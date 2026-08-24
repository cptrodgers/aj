//! Redis Backend Implementation
//!
//! Uses Redis data structures optimized for job queue operations:
//! - LIST for waiting and active queues (FIFO)
//! - ZSET (Sorted Set) for delayed queue (sorted by timestamp)
//! - HASH for job storage
//! - Lua scripts for atomic operations

use std::sync::OnceLock;

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Client, IntoConnectionInfo, RedisResult, Script};
use tokio::sync::OnceCell;

use crate::types::Backend;
use crate::Error;

/// Redis backend for distributed job queue.
///
/// # Key Naming Convention
/// - `{queue}:waiting` - LIST of job IDs ready to process
/// - `{queue}:delayed` - ZSET of job IDs with score = run_at timestamp
/// - `{queue}:active` - LIST of job IDs currently being processed
/// - `{queue}:storage` - HASH of job_id -> job_data (JSON)
/// - `aj:lock:{job_id}` - Lock key for distributed locking
///
/// # Connection
///
/// A single [`ConnectionManager`] is established on first use and shared. It is preferred
/// over `MultiplexedConnection` because it reconnects with exponential backoff: a worker is
/// long-lived, and one dropped socket must not disable it. It has no `Debug` impl, so `Debug`
/// is written by hand below rather than derived.
#[derive(Clone)]
pub struct Redis {
    client: Client,
    /// Established on first use so that `new` stays synchronous and does no I/O.
    conn: OnceCell<ConnectionManager>,
}

impl std::fmt::Debug for Redis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redis")
            .field("connection_info", &self.client.get_connection_info().addr)
            .finish()
    }
}

impl Redis {
    /// Create a new Redis backend.
    ///
    /// # Example
    /// ```ignore
    /// let backend = Redis::new("redis://localhost:6379/");
    /// ```
    pub fn new<T: IntoConnectionInfo>(connection_params: T) -> Self {
        let client = Client::open(connection_params).expect("Failed to create Redis client");
        Self::new_with_client(client)
    }

    /// Create a Redis backend from an existing client.
    pub fn new_with_client(client: Client) -> Self {
        Self {
            client,
            conn: OnceCell::new(),
        }
    }

    /// The shared connection, established on first use.
    ///
    /// `get_or_try_init` leaves the cell empty on failure, so a Redis that is unreachable at
    /// startup is retried on the next call rather than poisoning the backend. The manager is
    /// cheap to clone and commands need `&mut`, so each caller takes its own clone.
    async fn conn(&self) -> Result<ConnectionManager, Error> {
        self.conn
            .get_or_try_init(|| ConnectionManager::new(self.client.clone()))
            .await
            .cloned()
            .map_err(Into::into)
    }

    fn key_waiting(&self, queue: &str) -> String {
        format!("{}:waiting", queue)
    }

    fn key_delayed(&self, queue: &str) -> String {
        format!("{}:delayed", queue)
    }

    fn key_active(&self, queue: &str) -> String {
        format!("{}:active", queue)
    }

    fn key_storage(&self, queue: &str) -> String {
        format!("{}:storage", queue)
    }

    fn key_lock(&self, job_id: &str) -> String {
        format!("aj:lock:{}", job_id)
    }
}

// ============================================================================
// Lua Scripts for Atomic Operations
// ============================================================================

/// Move ready jobs from delayed (ZSET) to waiting (LIST).
/// KEYS[1] = delayed queue (ZSET)
/// KEYS[2] = waiting queue (LIST)
/// ARGV[1] = current timestamp (ms)
/// Returns: number of jobs moved
const LUA_DELAYED_MOVE_READY: &str = r#"
local delayed_key = KEYS[1]
local waiting_key = KEYS[2]
local now_ms = tonumber(ARGV[1])

local ready = redis.call('ZRANGEBYSCORE', delayed_key, '-inf', now_ms)
local count = 0

for i, job_id in ipairs(ready) do
    redis.call('ZREM', delayed_key, job_id)
    redis.call('RPUSH', waiting_key, job_id)
    count = count + 1
end

return count
"#;

/// Atomically claim a job: pop from waiting, lock, push to active.
/// KEYS[1] = waiting queue (LIST)
/// KEYS[2] = active queue (LIST)
/// ARGV[1] = worker_id
/// ARGV[2] = lock TTL (ms)
/// Returns: job_id or nil
const LUA_CLAIM_JOB: &str = r#"
local waiting_key = KEYS[1]
local active_key = KEYS[2]
local worker_id = ARGV[1]
local lock_ttl = tonumber(ARGV[2])

local job_id = redis.call('LPOP', waiting_key)
if not job_id then
    return nil
end

local lock_key = 'aj:lock:' .. job_id
local acquired = redis.call('SET', lock_key, worker_id, 'NX', 'PX', lock_ttl)

if acquired then
    redis.call('RPUSH', active_key, job_id)
    return job_id
else
    -- Failed to acquire lock, put job back
    redis.call('LPUSH', waiting_key, job_id)
    return nil
end
"#;

/// Release lock only if owned by worker.
/// KEYS[1] = lock key
/// ARGV[1] = worker_id
/// Returns: 1 if released, 0 if not owner
const LUA_LOCK_RELEASE: &str = r#"
local lock_key = KEYS[1]
local worker_id = ARGV[1]

if redis.call('GET', lock_key) == worker_id then
    return redis.call('DEL', lock_key)
end
return 0
"#;

/// Extend lock TTL only if owned by worker.
/// KEYS[1] = lock key
/// ARGV[1] = worker_id
/// ARGV[2] = new TTL (ms)
/// Returns: 1 if extended, 0 if not owner
const LUA_LOCK_EXTEND: &str = r#"
local lock_key = KEYS[1]
local worker_id = ARGV[1]
local ttl_ms = tonumber(ARGV[2])

if redis.call('GET', lock_key) == worker_id then
    return redis.call('PEXPIRE', lock_key, ttl_ms)
end
return 0
"#;

/// Find and requeue orphaned jobs (in active but lock expired).
/// KEYS[1] = active queue (LIST)
/// KEYS[2] = waiting queue (LIST)
/// Returns: list of requeued job IDs
const LUA_REQUEUE_ORPHANED: &str = r#"
local active_key = KEYS[1]
local waiting_key = KEYS[2]
local orphaned = {}

local job_ids = redis.call('LRANGE', active_key, 0, -1)

for i, job_id in ipairs(job_ids) do
    local lock_key = 'aj:lock:' .. job_id
    if redis.call('EXISTS', lock_key) == 0 then
        redis.call('LREM', active_key, 1, job_id)
        redis.call('RPUSH', waiting_key, job_id)
        table.insert(orphaned, job_id)
    end
end

return orphaned
"#;

/// Scripts are built once and reused. `Script::new` hashes the body, and these were
/// previously reconstructed on every single call.
fn script(cell: &'static OnceLock<Script>, body: &'static str) -> &'static Script {
    cell.get_or_init(|| Script::new(body))
}

fn lua_delayed_move_ready() -> &'static Script {
    static S: OnceLock<Script> = OnceLock::new();
    script(&S, LUA_DELAYED_MOVE_READY)
}

fn lua_claim_job() -> &'static Script {
    static S: OnceLock<Script> = OnceLock::new();
    script(&S, LUA_CLAIM_JOB)
}

fn lua_lock_release() -> &'static Script {
    static S: OnceLock<Script> = OnceLock::new();
    script(&S, LUA_LOCK_RELEASE)
}

fn lua_lock_extend() -> &'static Script {
    static S: OnceLock<Script> = OnceLock::new();
    script(&S, LUA_LOCK_EXTEND)
}

fn lua_requeue_orphaned() -> &'static Script {
    static S: OnceLock<Script> = OnceLock::new();
    script(&S, LUA_REQUEUE_ORPHANED)
}

// ============================================================================
// Backend Implementation
// ============================================================================

#[async_trait]
impl Backend for Redis {
    // ========================================================================
    // Waiting Queue (LIST)
    // ========================================================================

    async fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut conn = self.conn().await?;
        let key = self.key_waiting(queue);
        conn.rpush::<_, _, ()>(&key, job_id).await?;
        Ok(())
    }

    async fn waiting_pop(&self, queue: &str) -> Result<Option<String>, Error> {
        let mut conn = self.conn().await?;
        let key = self.key_waiting(queue);
        let result: Option<String> = conn.lpop(&key, None).await?;
        Ok(result)
    }

    async fn waiting_len(&self, queue: &str) -> Result<usize, Error> {
        let mut conn = self.conn().await?;
        let key = self.key_waiting(queue);
        let len: usize = conn.llen(&key).await?;
        Ok(len)
    }

    // ========================================================================
    // Delayed Queue (ZSET)
    // ========================================================================

    async fn delayed_push(&self, queue: &str, job_id: &str, run_at_ms: i64) -> Result<(), Error> {
        let mut conn = self.conn().await?;
        let key = self.key_delayed(queue);
        conn.zadd::<_, _, _, ()>(&key, job_id, run_at_ms).await?;
        Ok(())
    }

    async fn delayed_move_ready(&self, queue: &str, now_ms: i64) -> Result<usize, Error> {
        let mut conn = self.conn().await?;
        let delayed_key = self.key_delayed(queue);
        let waiting_key = self.key_waiting(queue);

        let script = lua_delayed_move_ready();
        let count: usize = script
            .key(&delayed_key)
            .key(&waiting_key)
            .arg(now_ms)
            .invoke_async(&mut conn)
            .await?;

        Ok(count)
    }

    async fn delayed_remove(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut conn = self.conn().await?;
        let key = self.key_delayed(queue);
        conn.zrem::<_, _, ()>(&key, job_id).await?;
        Ok(())
    }

    async fn delayed_len(&self, queue: &str) -> Result<usize, Error> {
        let mut conn = self.conn().await?;
        let key = self.key_delayed(queue);
        let len: usize = conn.zcard(&key).await?;
        Ok(len)
    }

    // ========================================================================
    // Active Queue (LIST)
    // ========================================================================

    async fn active_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut conn = self.conn().await?;
        let key = self.key_active(queue);
        conn.rpush::<_, _, ()>(&key, job_id).await?;
        Ok(())
    }

    async fn active_remove(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut conn = self.conn().await?;
        let key = self.key_active(queue);
        conn.lrem::<_, _, ()>(&key, 1, job_id).await?;
        Ok(())
    }

    async fn active_len(&self, queue: &str) -> Result<usize, Error> {
        let mut conn = self.conn().await?;
        let key = self.key_active(queue);
        let len: usize = conn.llen(&key).await?;
        Ok(len)
    }

    async fn active_list(&self, queue: &str) -> Result<Vec<String>, Error> {
        let mut conn = self.conn().await?;
        let key = self.key_active(queue);
        let jobs: Vec<String> = conn.lrange(&key, 0, -1).await?;
        Ok(jobs)
    }

    // ========================================================================
    // Job Storage (HASH)
    // ========================================================================

    async fn job_save(&self, queue: &str, job_id: &str, data: &str) -> Result<(), Error> {
        let mut conn = self.conn().await?;
        let key = self.key_storage(queue);
        conn.hset::<_, _, _, ()>(&key, job_id, data).await?;
        Ok(())
    }

    async fn job_get(&self, queue: &str, job_id: &str) -> Result<Option<String>, Error> {
        let mut conn = self.conn().await?;
        let key = self.key_storage(queue);
        let data: Option<String> = conn.hget(&key, job_id).await?;
        Ok(data)
    }

    async fn job_delete(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut conn = self.conn().await?;
        let key = self.key_storage(queue);
        conn.hdel::<_, _, ()>(&key, job_id).await?;
        Ok(())
    }

    // ========================================================================
    // Distributed Locking
    // ========================================================================

    async fn lock_acquire(
        &self,
        job_id: &str,
        worker_id: &str,
        ttl_ms: u64,
    ) -> Result<bool, Error> {
        let mut conn = self.conn().await?;
        let lock_key = self.key_lock(job_id);

        // SET key value NX PX ttl
        let result: RedisResult<Option<String>> = redis::cmd("SET")
            .arg(&lock_key)
            .arg(worker_id)
            .arg("NX")
            .arg("PX")
            .arg(ttl_ms)
            .query_async(&mut conn)
            .await;

        match result {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    async fn lock_release(&self, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        let mut conn = self.conn().await?;
        let lock_key = self.key_lock(job_id);

        let script = lua_lock_release();
        let result: i32 = script
            .key(&lock_key)
            .arg(worker_id)
            .invoke_async(&mut conn)
            .await?;

        Ok(result == 1)
    }

    async fn lock_extend(&self, job_id: &str, worker_id: &str, ttl_ms: u64) -> Result<bool, Error> {
        let mut conn = self.conn().await?;
        let lock_key = self.key_lock(job_id);

        let script = lua_lock_extend();
        let result: i32 = script
            .key(&lock_key)
            .arg(worker_id)
            .arg(ttl_ms)
            .invoke_async(&mut conn)
            .await?;

        Ok(result == 1)
    }

    // ========================================================================
    // Atomic Operations
    // ========================================================================

    async fn claim_job(
        &self,
        queue: &str,
        worker_id: &str,
        lock_ttl_ms: u64,
    ) -> Result<Option<String>, Error> {
        let mut conn = self.conn().await?;
        let waiting_key = self.key_waiting(queue);
        let active_key = self.key_active(queue);

        let script = lua_claim_job();
        let result: Option<String> = script
            .key(&waiting_key)
            .key(&active_key)
            .arg(worker_id)
            .arg(lock_ttl_ms)
            .invoke_async(&mut conn)
            .await?;

        Ok(result)
    }

    async fn complete_job(
        &self,
        queue: &str,
        job_id: &str,
        worker_id: &str,
    ) -> Result<bool, Error> {
        // Remove from active and release lock
        self.active_remove(queue, job_id).await?;
        self.lock_release(job_id, worker_id).await?;
        Ok(true)
    }

    async fn fail_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        // Same as complete for now - remove from active and release lock
        self.active_remove(queue, job_id).await?;
        self.lock_release(job_id, worker_id).await?;
        Ok(true)
    }

    async fn requeue_orphaned(&self, queue: &str) -> Result<Vec<String>, Error> {
        let mut conn = self.conn().await?;
        let active_key = self.key_active(queue);
        let waiting_key = self.key_waiting(queue);

        let script = lua_requeue_orphaned();
        let orphaned: Vec<String> = script
            .key(&active_key)
            .key(&waiting_key)
            .invoke_async(&mut conn)
            .await?;

        Ok(orphaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_redis() -> Redis {
        Redis::new("redis://localhost:6379/")
    }

    fn unique_queue() -> String {
        format!("test:{}", Uuid::new_v4())
    }

    /// Job ids must be unique per test. Lock keys are `aj:lock:{job_id}` (see `key_lock`) and
    /// are *not* queue-scoped, so two tests sharing a job id contend for the same lock when
    /// the suite runs in parallel against one Redis instance.
    fn unique_job() -> String {
        format!("job:{}", Uuid::new_v4())
    }

    /// Deletes every key a test may have touched, including the global lock keys for
    /// `job_ids`, so a panicking test cannot leak a lock into the next run.
    async fn cleanup(redis: &Redis, queue: &str, job_ids: &[&str]) {
        let mut conn = redis.conn().await.unwrap();
        let mut cmd = redis::cmd("DEL");
        cmd.arg(redis.key_waiting(queue))
            .arg(redis.key_delayed(queue))
            .arg(redis.key_active(queue))
            .arg(redis.key_storage(queue));
        for job_id in job_ids {
            cmd.arg(redis.key_lock(job_id));
        }
        let _: () = cmd.query_async(&mut conn).await.unwrap();
    }

    #[tokio::test]
    async fn test_waiting_queue() {
        let redis = test_redis();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        redis.waiting_push(&queue, &job1).await.unwrap();
        redis.waiting_push(&queue, &job2).await.unwrap();

        assert_eq!(redis.waiting_len(&queue).await.unwrap(), 2);
        assert_eq!(redis.waiting_pop(&queue).await.unwrap(), Some(job1.clone()));
        assert_eq!(redis.waiting_pop(&queue).await.unwrap(), Some(job2.clone()));
        assert_eq!(redis.waiting_pop(&queue).await.unwrap(), None);

        cleanup(&redis, &queue, &[&job1, &job2]).await;
    }

    #[tokio::test]
    async fn test_delayed_queue() {
        let redis = test_redis();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();
        let job3 = unique_job();

        redis.delayed_push(&queue, &job1, 1000).await.unwrap();
        redis.delayed_push(&queue, &job2, 2000).await.unwrap();
        redis.delayed_push(&queue, &job3, 3000).await.unwrap();

        assert_eq!(redis.delayed_len(&queue).await.unwrap(), 3);

        // Move ready jobs (job1 and job2)
        let moved = redis.delayed_move_ready(&queue, 2500).await.unwrap();
        assert_eq!(moved, 2);

        assert_eq!(redis.delayed_len(&queue).await.unwrap(), 1);
        assert_eq!(redis.waiting_len(&queue).await.unwrap(), 2);

        // Check order
        assert_eq!(redis.waiting_pop(&queue).await.unwrap(), Some(job1.clone()));
        assert_eq!(redis.waiting_pop(&queue).await.unwrap(), Some(job2.clone()));

        cleanup(&redis, &queue, &[&job1, &job2, &job3]).await;
    }

    #[tokio::test]
    async fn test_claim_job() {
        let redis = test_redis();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        redis.waiting_push(&queue, &job1).await.unwrap();
        redis.waiting_push(&queue, &job2).await.unwrap();

        // Claim job1
        let job = redis.claim_job(&queue, "worker1", 30000).await.unwrap();
        assert_eq!(job, Some(job1.clone()));
        assert_eq!(redis.waiting_len(&queue).await.unwrap(), 1);
        assert_eq!(redis.active_len(&queue).await.unwrap(), 1);

        // Verify lock exists
        let mut conn = redis.conn().await.unwrap();
        let lock_value: Option<String> = conn.get(redis.key_lock(&job1)).await.unwrap();
        assert_eq!(lock_value, Some("worker1".to_string()));

        cleanup(&redis, &queue, &[&job1, &job2]).await;
    }

    #[tokio::test]
    async fn test_lock_operations() {
        let redis = test_redis();
        let job_id = unique_job();

        // Acquire lock
        assert!(redis.lock_acquire(&job_id, "worker1", 30000).await.unwrap());

        // Try to acquire again (should fail)
        assert!(!redis.lock_acquire(&job_id, "worker2", 30000).await.unwrap());

        // Extend lock (by owner)
        assert!(redis.lock_extend(&job_id, "worker1", 60000).await.unwrap());

        // Extend lock (by non-owner - should fail)
        assert!(!redis.lock_extend(&job_id, "worker2", 60000).await.unwrap());

        // Release lock (by non-owner - should fail)
        assert!(!redis.lock_release(&job_id, "worker2").await.unwrap());

        // Release lock (by owner)
        assert!(redis.lock_release(&job_id, "worker1").await.unwrap());

        // Now worker2 can acquire
        assert!(redis.lock_acquire(&job_id, "worker2", 30000).await.unwrap());
        redis.lock_release(&job_id, "worker2").await.unwrap();
    }

    #[tokio::test]
    async fn test_requeue_orphaned() {
        let redis = test_redis();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        // Simulate orphaned jobs (in active but no lock)
        redis.active_push(&queue, &job1).await.unwrap();
        redis.active_push(&queue, &job2).await.unwrap();

        // job1 has a lock, job2 doesn't (orphaned)
        assert!(redis.lock_acquire(&job1, "worker1", 30000).await.unwrap());

        let orphaned = redis.requeue_orphaned(&queue).await.unwrap();
        assert_eq!(orphaned, vec![job2.clone()]);

        assert_eq!(redis.active_len(&queue).await.unwrap(), 1);
        assert_eq!(redis.waiting_len(&queue).await.unwrap(), 1);
        assert_eq!(redis.waiting_pop(&queue).await.unwrap(), Some(job2.clone()));

        cleanup(&redis, &queue, &[&job1, &job2]).await;
    }

    #[tokio::test]
    async fn test_job_storage() {
        let redis = test_redis();
        let queue = unique_queue();
        let job1 = unique_job();

        redis
            .job_save(&queue, &job1, r#"{"data": 1}"#)
            .await
            .unwrap();

        let data = redis.job_get(&queue, &job1).await.unwrap();
        assert_eq!(data, Some(r#"{"data": 1}"#.to_string()));

        redis.job_delete(&queue, &job1).await.unwrap();
        assert_eq!(redis.job_get(&queue, &job1).await.unwrap(), None);

        cleanup(&redis, &queue, &[&job1]).await;
    }
}
