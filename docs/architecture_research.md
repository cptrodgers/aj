# AJ Architecture Deep Analysis

## Executive Summary

AJ is a background job processing library for Rust built on the Actix actor model. This document provides a deep analysis of two critical aspects:

1. **Distributed Architecture** - How to evolve AJ to run in a distributed environment
2. **Macro Design** - Evaluation of current macro patterns and extension strategies like Diesel/Serde

---

## Part 1: Distributed Architecture Analysis

### Current Architecture

The current design uses queue names as the coordination key - workers register dynamically and coordinate via the Backend:

```
┌─────────────────────────────────────────────────────────────┐
│                     Current Architecture                     │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  AJ Singleton                                        │   │
│  │  └── WorkQueue<M> Actors (registered by queue name) │   │
│  └─────────────────────────────────────────────────────┘   │
│                           │                                 │
│                           ▼                                 │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  Backend (Redis/InMemory)                           │   │
│  │  - Queue coordination by name ✓                     │   │
│  │  - Shared storage ✓                                 │   │
│  │  - No distributed locking ✗                         │   │
│  │  - No worker heartbeat ✗                            │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

**Key Insight**: No static registry is needed. Workers register dynamically, and queue names serve as the coordination key across processes.

### Current Limitations

#### Problem 1: No Job Locking

In `queue.rs:233-270`, job picking uses a simple move operation:

```rust
pub fn try_pick_specific_ready_jobs(&self, total: usize) -> Result<Vec<Job<M>>, Error> {
    // ...
    self.backend.queue_move(
        &idle_queue_name,
        &processing_queue_name,
        1,
        QueueDirection::Back,
        QueueDirection::Front,
    )?;
}
```

**Issue**: If two workers call `queue_move` simultaneously, both may pick the same job before either completes the move. The read-check-move sequence is not atomic.

#### Problem 2: No Heartbeat/Lease Mechanism

Running jobs have no timeout. If a worker crashes mid-execution, the job stays in `Running` queue forever.

---

### Distributed Architecture Proposal

#### Design Principle: Backend Handles Distribution

The `Backend` trait should be responsible for all distributed coordination. This allows:
- **InMemory**: No-op locking (single process)
- **Redis**: Redis-based locking
- **PostgreSQL**: Advisory locks or SELECT FOR UPDATE
- **Custom**: Any distributed lock mechanism

This keeps the core WorkQueue logic clean and backend-agnostic.

#### Architecture: Competing Consumers with Backend-Managed Locking

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Competing Consumers Model                        │
│                                                                      │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐              │
│  │   Worker 1   │  │   Worker 2   │  │   Worker 3   │              │
│  │              │  │              │  │              │              │
│  │ WorkQueue<M> │  │ WorkQueue<M> │  │ WorkQueue<M> │              │
│  │  - Poll      │  │  - Poll      │  │  - Poll      │              │
│  │  - Claim     │  │  - Claim     │  │  - Claim     │              │
│  │  - Execute   │  │  - Execute   │  │  - Execute   │              │
│  │  - Heartbeat │  │  - Heartbeat │  │  - Heartbeat │              │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘              │
│         │                 │                 │                       │
│         └─────────────────┼─────────────────┘                       │
│                           ▼                                         │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │              Backend (with Distributed Locking)              │   │
│  │                                                              │   │
│  │  Backend Trait Methods:                                     │   │
│  │  ┌────────────────────────────────────────────────────────┐ │   │
│  │  │ - acquire_lock(key, owner, ttl) -> bool                │ │   │
│  │  │ - release_lock(key, owner) -> bool                     │ │   │
│  │  │ - extend_lock(key, owner, ttl) -> bool                 │ │   │
│  │  │ - claim_ready_job(queue, worker_id, ttl) -> Option<ID> │ │   │
│  │  │ - requeue_orphaned_jobs(queue, timeout) -> Vec<ID>     │ │   │
│  │  └────────────────────────────────────────────────────────┘ │   │
│  │                                                              │   │
│  │  Implementations:                                           │   │
│  │  - InMemory: Mutex-based (single process, no-op TTL)       │   │
│  │  - Redis: SET NX EX + Lua scripts                          │   │
│  │  - PostgreSQL: Advisory locks / SELECT FOR UPDATE SKIP LOCKED │
│  │  - Custom: User-defined                                     │   │
│  └─────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

**Key Changes Required:**

##### 1. Extend Backend Trait with Distributed Locking

```rust
// aj_core/src/backend/types.rs
pub trait Backend: Send + Sync {
    // ============ Existing Queue Methods ============
    fn queue_push(&self, queue_name: &str, item: &str) -> Result<(), Error>;
    fn queue_move(...) -> Result<Vec<String>, Error>;
    fn queue_remove(&self, queue: &str, item: &str) -> Result<(), Error>;
    fn queue_get(&self, queue: &str, count: usize, direction: QueueDirection) -> Result<Vec<String>, Error>;
    fn queue_count(&self, queue: &str) -> Result<usize, Error>;

    // ============ Existing Storage Methods ============
    fn storage_upsert(&self, hash: &str, key: &str, value: String) -> Result<(), Error>;
    fn storage_get(&self, hash: &str, key: &str) -> Result<Option<String>, Error>;

    // ============ NEW: Distributed Locking ============
    
    /// Acquire a distributed lock
    /// Returns true if lock acquired, false if already held by another owner
    fn lock_acquire(
        &self,
        key: &str,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<bool, Error> {
        // Default: no-op for single-process backends
        Ok(true)
    }

    /// Release a distributed lock
    /// Returns true if released, false if not owned by this owner
    fn lock_release(&self, key: &str, owner: &str) -> Result<bool, Error> {
        Ok(true)
    }

    /// Extend lock TTL (heartbeat)
    /// Returns true if extended, false if lock lost
    fn lock_extend(
        &self,
        key: &str,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<bool, Error> {
        Ok(true)
    }

    // ============ NEW: Atomic Job Operations ============

    /// Atomically claim a ready job from the queue
    /// This should be an atomic operation: check ready + acquire lock + move to running
    fn claim_ready_job(
        &self,
        queue_name: &str,
        worker_id: &str,
        lock_ttl_ms: u64,
        now_ms: i64,
    ) -> Result<Option<String>, Error> {
        // Default implementation for non-distributed backends
        // Uses existing methods - not atomic but works for single-process
        let queued = self.format_queue_name(queue_name, "Queued");
        let running = self.format_queue_name(queue_name, "Running");
        
        let job_ids = self.queue_get(&queued, 1, QueueDirection::Back)?;
        if let Some(job_id) = job_ids.first() {
            self.queue_move(&queued, &running, 1, QueueDirection::Back, QueueDirection::Front)?;
            return Ok(Some(job_id.clone()));
        }
        Ok(None)
    }

    /// Find and requeue jobs that have been running longer than timeout
    /// (their locks have expired - worker likely crashed)
    fn requeue_orphaned_jobs(
        &self,
        queue_name: &str,
        timeout_ms: u64,
    ) -> Result<Vec<String>, Error> {
        // Default: no-op for single-process backends
        Ok(vec![])
    }

    /// Helper to format queue names consistently
    fn format_queue_name(&self, base: &str, status: &str) -> String {
        format!("{}:queue:{}", base, status)
    }
}
```

##### 2. InMemory Backend Implementation (Single Process)

```rust
// aj_core/src/backend/mem.rs
impl Backend for InMemory {
    // ... existing methods ...

    // Default implementations work for single-process
    // Lock methods return Ok(true) - no actual locking needed
    
    fn claim_ready_job(
        &self,
        queue_name: &str,
        _worker_id: &str,
        _lock_ttl_ms: u64,
        _now_ms: i64,
    ) -> Result<Option<String>, Error> {
        // Single process: just move the job, no lock needed
        let queued = self.format_queue_name(queue_name, "Queued");
        let running = self.format_queue_name(queue_name, "Running");
        
        let moved = self.queue_move(&queued, &running, 1, 
            QueueDirection::Back, QueueDirection::Front)?;
        Ok(moved.into_iter().next())
    }
}
```

##### 3. Redis Backend Implementation (Distributed)

```rust
// aj_core/src/backend/redis.rs

impl Redis {
    const CLAIM_JOB_SCRIPT: &'static str = r#"
        local queued_queue = KEYS[1]
        local running_queue = KEYS[2]
        local storage_hash = KEYS[3]
        local worker_id = ARGV[1]
        local ttl_ms = tonumber(ARGV[2])
        local now_ms = tonumber(ARGV[3])
        
        -- Peek jobs from back of queue (oldest first)
        local job_ids = redis.call('LRANGE', queued_queue, -20, -1)
        
        for i, job_id in ipairs(job_ids) do
            local lock_key = 'aj:lock:' .. job_id
            
            -- Try to acquire lock atomically
            local acquired = redis.call('SET', lock_key, worker_id, 'NX', 'PX', ttl_ms)
            
            if acquired then
                -- Get job data to check if ready
                local job_json = redis.call('HGET', storage_hash, job_id)
                if job_json then
                    local job = cjson.decode(job_json)
                    local scheduled_at = job.context and job.context.enqueue_at or 0
                    
                    if scheduled_at <= now_ms then
                        -- Job is ready! Move to running queue
                        redis.call('LREM', queued_queue, 1, job_id)
                        redis.call('LPUSH', running_queue, job_id)
                        return job_id
                    end
                end
                -- Job not ready, release lock
                redis.call('DEL', lock_key)
            end
        end
        
        return nil
    "#;

    const RELEASE_LOCK_SCRIPT: &'static str = r#"
        local lock_key = KEYS[1]
        local owner = ARGV[1]
        
        if redis.call('GET', lock_key) == owner then
            return redis.call('DEL', lock_key)
        end
        return 0
    "#;

    const EXTEND_LOCK_SCRIPT: &'static str = r#"
        local lock_key = KEYS[1]
        local owner = ARGV[1]
        local ttl_ms = tonumber(ARGV[2])
        
        if redis.call('GET', lock_key) == owner then
            return redis.call('PEXPIRE', lock_key, ttl_ms)
        end
        return 0
    "#;

    const REQUEUE_ORPHANED_SCRIPT: &'static str = r#"
        local running_queue = KEYS[1]
        local queued_queue = KEYS[2]
        local orphaned = {}
        
        local job_ids = redis.call('LRANGE', running_queue, 0, -1)
        
        for i, job_id in ipairs(job_ids) do
            local lock_key = 'aj:lock:' .. job_id
            -- If lock doesn't exist, job is orphaned
            if not redis.call('EXISTS', lock_key) then
                redis.call('LREM', running_queue, 1, job_id)
                redis.call('RPUSH', queued_queue, job_id)
                table.insert(orphaned, job_id)
            end
        end
        
        return orphaned
    "#;
}

impl Backend for Redis {
    // ... existing methods ...

    fn lock_acquire(&self, key: &str, owner: &str, ttl_ms: u64) -> Result<bool, Error> {
        let mut conn = self.client.get_connection()?;
        let lock_key = format!("aj:lock:{}", key);
        let result: Option<String> = redis::cmd("SET")
            .arg(&lock_key)
            .arg(owner)
            .arg("NX")
            .arg("PX")
            .arg(ttl_ms)
            .query(&mut conn)?;
        Ok(result.is_some())
    }

    fn lock_release(&self, key: &str, owner: &str) -> Result<bool, Error> {
        let mut conn = self.client.get_connection()?;
        let lock_key = format!("aj:lock:{}", key);
        let script = redis::Script::new(Self::RELEASE_LOCK_SCRIPT);
        let result: i32 = script.key(&lock_key).arg(owner).invoke(&mut conn)?;
        Ok(result == 1)
    }

    fn lock_extend(&self, key: &str, owner: &str, ttl_ms: u64) -> Result<bool, Error> {
        let mut conn = self.client.get_connection()?;
        let lock_key = format!("aj:lock:{}", key);
        let script = redis::Script::new(Self::EXTEND_LOCK_SCRIPT);
        let result: i32 = script.key(&lock_key).arg(owner).arg(ttl_ms).invoke(&mut conn)?;
        Ok(result == 1)
    }

    fn claim_ready_job(
        &self,
        queue_name: &str,
        worker_id: &str,
        lock_ttl_ms: u64,
        now_ms: i64,
    ) -> Result<Option<String>, Error> {
        let mut conn = self.client.get_connection()?;
        let queued = self.format_queue_name(queue_name, "Queued");
        let running = self.format_queue_name(queue_name, "Running");
        let storage = format!("{}:storage", queue_name);
        
        let script = redis::Script::new(Self::CLAIM_JOB_SCRIPT);
        let result: Option<String> = script
            .key(&queued)
            .key(&running)
            .key(&storage)
            .arg(worker_id)
            .arg(lock_ttl_ms)
            .arg(now_ms)
            .invoke(&mut conn)?;
        
        Ok(result)
    }

    fn requeue_orphaned_jobs(
        &self,
        queue_name: &str,
        _timeout_ms: u64,
    ) -> Result<Vec<String>, Error> {
        let mut conn = self.client.get_connection()?;
        let running = self.format_queue_name(queue_name, "Running");
        let queued = self.format_queue_name(queue_name, "Queued");
        
        let script = redis::Script::new(Self::REQUEUE_ORPHANED_SCRIPT);
        let result: Vec<String> = script
            .key(&running)
            .key(&queued)
            .invoke(&mut conn)?;
        
        Ok(result)
    }
}
```

##### 4. PostgreSQL Backend Example (Optional Future Implementation)

```rust
// aj_core/src/backend/postgres.rs (future)
impl Backend for Postgres {
    fn claim_ready_job(
        &self,
        queue_name: &str,
        worker_id: &str,
        lock_ttl_ms: u64,
        now_ms: i64,
    ) -> Result<Option<String>, Error> {
        // Use SELECT FOR UPDATE SKIP LOCKED for atomic claim
        let result = sqlx::query!(
            r#"
            UPDATE aj_jobs
            SET status = 'running',
                locked_by = $1,
                locked_until = NOW() + interval '$2 milliseconds'
            WHERE id = (
                SELECT id FROM aj_jobs
                WHERE queue_name = $3
                  AND status = 'queued'
                  AND scheduled_at <= $4
                ORDER BY created_at ASC
                LIMIT 1
                FOR UPDATE SKIP LOCKED
            )
            RETURNING id
            "#,
            worker_id,
            lock_ttl_ms as i64,
            queue_name,
            now_ms
        )
        .fetch_optional(&self.pool)
        .await?;
        
        Ok(result.map(|r| r.id))
    }
}
```

##### 5. Worker Identity and Heartbeat

```rust
// aj_core/src/worker.rs (new file)
use std::sync::Arc;
use chrono::{DateTime, Utc};
use uuid::Uuid;

pub struct WorkerIdentity {
    pub id: String,
    pub hostname: String,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
}

impl WorkerIdentity {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            hostname: hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            pid: std::process::id(),
            started_at: Utc::now(),
        }
    }
    
    pub fn unique_id(&self) -> String {
        format!("{}:{}:{}", self.hostname, self.pid, self.id)
    }
}

/// Actor that periodically extends locks on running jobs
pub struct HeartbeatActor {
    worker_id: String,
    backend: Arc<dyn Backend>,
    interval: Duration,
    job_ids: Arc<RwLock<HashSet<String>>>,
}

impl Actor for HeartbeatActor {
    type Context = Context<Self>;
    
    fn started(&mut self, ctx: &mut Self::Context) {
        self.schedule_heartbeat(ctx);
    }
}

impl HeartbeatActor {
    fn schedule_heartbeat(&self, ctx: &mut Context<Self>) {
        ctx.run_interval(self.interval, |actor, _ctx| {
            let job_ids = actor.job_ids.read().unwrap();
            for job_id in job_ids.iter() {
                if let Err(e) = actor.backend.lock_extend(
                    job_id,
                    &actor.worker_id,
                    actor.interval.as_millis() as u64 * 3,
                ) {
                    warn!("Failed to extend lock for job {}: {:?}", job_id, e);
                }
            }
        });
    }
}
```

##### 6. Dead Worker Detection (Reaper Actor)

```rust
// aj_core/src/reaper.rs (new file)

/// Actor that periodically scans for orphaned jobs and requeues them
pub struct ReaperActor {
    backend: Arc<dyn Backend>,
    queue_names: Vec<String>,
    scan_interval: Duration,
    job_timeout: Duration,
}

impl Actor for ReaperActor {
    type Context = Context<Self>;
    
    fn started(&mut self, ctx: &mut Self::Context) {
        self.schedule_reap(ctx);
    }
}

impl ReaperActor {
    fn schedule_reap(&self, ctx: &mut Context<Self>) {
        ctx.run_interval(self.scan_interval, |actor, _ctx| {
            for queue_name in &actor.queue_names {
                match actor.backend.requeue_orphaned_jobs(
                    queue_name,
                    actor.job_timeout.as_millis() as u64,
                ) {
                    Ok(orphaned) if !orphaned.is_empty() => {
                        info!("Requeued {} orphaned jobs from {}", orphaned.len(), queue_name);
                    }
                    Err(e) => {
                        error!("Failed to reap orphaned jobs from {}: {:?}", queue_name, e);
                    }
                    _ => {}
                }
            }
        });
    }
}
```

---

### Distributed Mode Configuration

```rust
// aj_core/src/config.rs (new file)
use std::time::Duration;

/// Configuration for distributed mode
#[derive(Debug, Clone)]
pub struct DistributedConfig {
    /// How long a job lock is held before expiring (default: 30s)
    /// Should be longer than the longest expected job execution time
    pub job_lease_duration: Duration,
    
    /// How often to extend job leases (default: 10s)
    /// Should be less than job_lease_duration / 3 for safety
    pub heartbeat_interval: Duration,
    
    /// How often to scan for orphaned jobs (default: 60s)
    pub reaper_interval: Duration,
    
    /// Whether this worker should run the reaper
    /// (only one worker needs to run it, but multiple is safe)
    pub enable_reaper: bool,
}

impl Default for DistributedConfig {
    fn default() -> Self {
        Self {
            job_lease_duration: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(10),
            reaper_interval: Duration::from_secs(60),
            enable_reaper: true,
        }
    }
}

impl AJ {
    /// Start AJ in distributed mode with the given backend and config
    pub fn start_distributed(
        backend: impl Backend + Send + Sync + 'static,
        config: DistributedConfig,
    ) -> Addr<Self> {
        let worker = WorkerIdentity::new();
        info!("Starting AJ worker: {}", worker.unique_id());
        
        let backend = Arc::new(backend);
        
        // Start the main AJ actor
        let aj_addr = Self::start_with_backend(backend.clone());
        
        // Start heartbeat actor
        let _heartbeat = HeartbeatActor::new(
            worker.unique_id(),
            backend.clone(),
            config.heartbeat_interval,
        ).start();
        
        // Optionally start reaper actor
        if config.enable_reaper {
            let _reaper = ReaperActor::new(
                backend.clone(),
                config.reaper_interval,
                config.job_lease_duration,
            ).start();
        }
        
        aj_addr
    }
}
```

##### 7. Updated WorkQueue to Use Backend Locking

```rust
// aj_core/src/queue.rs (modifications)

impl<M> WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    /// Pick jobs using backend's atomic claim operation
    pub fn pick_jobs_to_process_distributed(
        &self,
        worker_id: &str,
        lock_ttl_ms: u64,
    ) -> Result<Vec<Job<M>>, Error> {
        let mut ready_jobs = vec![];
        let now_ms = Utc::now().timestamp_millis();
        
        let slots = self.config.max_processing_jobs - self.current_processing_count();
        
        for _ in 0..slots {
            // Use backend's atomic claim operation
            match self.backend.claim_ready_job(
                &self.name,
                worker_id,
                lock_ttl_ms,
                now_ms,
            )? {
                Some(job_id) => {
                    if let Some(job) = self.read_job(&job_id)? {
                        ready_jobs.push(job);
                    }
                }
                None => break, // No more ready jobs
            }
        }
        
        Ok(ready_jobs)
    }
}
```

---

### Migration Path

| Phase | Changes | Breaking? |
|-------|---------|-----------|
| 1 | Add lock methods to Backend trait with default no-op implementations | No |
| 2 | Add `claim_ready_job` and `requeue_orphaned_jobs` to Backend trait | No |
| 3 | Implement locking in Redis backend | No |
| 4 | Add `DistributedConfig` and `start_distributed()` | No |
| 5 | Add HeartbeatActor and ReaperActor | No |
| 6 | Update WorkQueue to use `claim_ready_job` when in distributed mode | No |

---

## Distributed Locking: Industry Analysis

### How Popular Job Queues Handle Locking

#### 1. Celery (Python)

Celery itself has **no built-in locking**. It relies on external libraries:

- **[celery-once](https://github.com/cameronmaske/celery-once)**: Uses Redis `SET NX EX` for distributed locks
- **[celery-singleton](https://github.com/steinitzu/celery-singleton)**: Uses task name + args hash as lock key
- **Default behavior**: Jobs can run in parallel on multiple workers (no deduplication)

**Key insight**: Celery delegates locking to the broker (Redis/RabbitMQ), not the application.

#### 2. Sidekiq (Ruby)

Uses Redis with multiple locking strategies:

- **[sidekiq-unique-jobs](https://github.com/mhenrixon/sidekiq-unique-jobs)**: Creates Redis keys based on `queue + class + args`
- **Sidekiq Enterprise**: Built-in rate limiting API with distributed mutex
- **Lock mechanism**: `SET NX EX` with configurable TTL

**Key insight**: Sidekiq recommends `maxmemory-policy noeviction` - Redis should be persistent storage, not a cache.

#### 3. BullMQ (Node.js) - **Best Practice Example**

BullMQ is considered the gold standard for Redis-based job queues:

- **Atomic Lua Scripts**: All state transitions use Lua scripts for atomicity
- **Lock Token System**: Each job gets a unique lock token; only the token holder can complete/fail the job
- **Heartbeat via `QRELOCK`**: Workers can extend locks while processing
- **At-least-once delivery**: Guarantees delivery even if workers crash

```
Job Flow:
waiting → active (with lock) → completed/failed (lock released)
           ↓
    lock expires → job returns to waiting
```

**Key insight**: BullMQ stores only job IDs in queues (lists), job data in hashes - efficient and atomic.

#### 4. Temporal (Workflow Engine)

Takes a completely different approach - **no explicit locking needed**:

- **Server-mediated execution**: Temporal Server guarantees only one worker executes a task
- **Durable event sourcing**: All events are logged; state can be reconstructed after failure
- **Sharded architecture**: Workflow IDs hash to shards; each shard operates independently
- **Pull-based model**: Workers poll for tasks only when they have capacity

**Key insight**: Temporal eliminates locking by making the server the single coordinator.

### Locking Pattern Comparison

| Pattern | Used By | Pros | Cons |
|---------|---------|------|------|
| **Single Redis SET NX EX** | Celery-once, Sidekiq | Simple, fast, low latency | Single point of failure |
| **Lua Scripts (Atomic)** | BullMQ, Bull | Atomic multi-step operations, no race conditions | Requires Lua knowledge |
| **Redlock (Multi-node)** | High-availability setups | Fault tolerant (N/2+1 nodes) | Complex, clock skew issues, [criticized by Kleppmann](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html) |
| **Fencing Tokens** | ZooKeeper, etcd | Prevents stale lock holders from corrupting data | Requires token validation in all operations |
| **Server Coordination** | Temporal, AWS SQS | No client-side locking needed | Requires dedicated infrastructure |
| **Database Locks** | PostgreSQL FOR UPDATE SKIP LOCKED | ACID guarantees, no external dependencies | Higher latency than Redis |

### The Redlock Controversy

[Martin Kleppmann's famous critique](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html) argues:

1. **For efficiency** (preventing duplicate work): Single Redis instance is sufficient
2. **For correctness** (protecting critical data): Use proper consensus (ZooKeeper, etcd) with fencing tokens

**Redlock problems**:
- Relies on synchronized clocks across nodes
- Process pauses (GC, scheduling) can cause lock expiry while holder thinks it's valid
- No fencing tokens to detect stale lock holders

### Recommendation for AJ

Based on industry analysis, **BullMQ's approach is the best fit** for AJ:

```
┌─────────────────────────────────────────────────────────────────┐
│                 Recommended: BullMQ-style Locking               │
│                                                                 │
│  1. Atomic Lua Scripts for all state transitions               │
│  2. Lock tokens (worker_id) stored with job                    │
│  3. Heartbeat to extend locks during long jobs                 │
│  4. Automatic requeue when locks expire (reaper)               │
│  5. Single Redis instance for most use cases                   │
│                                                                 │
│  Why not Redlock?                                              │
│  - Job queues are "efficiency" not "correctness" (Kleppmann)   │
│  - Duplicate job execution is recoverable (retry/idempotency)  │
│  - Added complexity not worth it for most use cases            │
│                                                                 │
│  When to consider Redlock or consensus systems:                │
│  - Financial transactions requiring exactly-once               │
│  - Distributed coordination beyond job queuing                 │
│  - Multi-region deployments with strict consistency needs      │
└─────────────────────────────────────────────────────────────────┘
```

### Backend Implementation Checklist

| Backend | Locking Method | Atomic Claim | Notes |
|---------|---------------|--------------|-------|
| InMemory | Mutex (default) | Simple move | Single-process only |
| Redis | SET NX EX + Lua | Lua script (BullMQ-style) | **Recommended for production** |
| PostgreSQL | Advisory locks | SELECT FOR UPDATE SKIP LOCKED | Good for existing PG infrastructure |
| DynamoDB | Conditional writes | UpdateItem with conditions | AWS-native option |
| Etcd | Lease-based | Transaction | For strict consistency needs |

### Sources

- [Distributed Locks with Redis](https://redis.io/docs/latest/develop/clients/patterns/distributed-locks/)
- [How to do distributed locking - Martin Kleppmann](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html)
- [BullMQ Documentation](https://docs.bullmq.io)
- [Sidekiq Wiki - Using Redis](https://github.com/sidekiq/sidekiq/wiki/Using-Redis)
- [Temporal Task Queues](https://docs.temporal.io/task-queue)
- [Celery-once](https://github.com/cameronmaske/celery-once)

---

## DAG/Workflow Capabilities: BullMQ vs Airflow

### Short Answer

**No, BullMQ cannot do full DAGs like Airflow.** BullMQ supports **trees only** (one parent per job), not true DAGs (multiple parents per job).

### BullMQ Flows: Tree-Based Dependencies

BullMQ has a [Flows feature](https://docs.bullmq.io/guide/flows) that supports parent-child job relationships:

```typescript
// BullMQ Flow Example - Tree Structure
const flow = await flowProducer.add({
  name: 'build-app',           // Parent - waits for all children
  queueName: 'build',
  children: [
    { name: 'compile-frontend', queueName: 'compile' },
    { name: 'compile-backend', queueName: 'compile' },
    { name: 'run-tests', queueName: 'test' },
  ],
});
// Execution: children run first (parallel) → parent runs after all complete
```

**What BullMQ Flows Support:**
- Parent waits for all children to complete
- Children can have their own children (arbitrary depth)
- Parent can access children's results via `getChildrenValues()`
- Atomic flow addition (all-or-nothing)
- `failParentOnFailure` option for strict dependencies

**What BullMQ Flows DON'T Support:**
- **Multiple parents per child** (true DAG requirement)
- **Diamond/merge patterns** (A → B, A → C, B+C → D)
- Built-in scheduling (cron)
- Visual DAG editor

```
BullMQ (Tree):              Airflow (DAG):
     A                           A
    /|\                         / \
   B C D                       B   C
   |                            \ /
   E                             D    ← D has TWO parents (not supported in BullMQ)
```

### Apache Airflow: Full DAG Support

Airflow is designed specifically for DAG workflows:

```python
# Airflow DAG Example - True DAG with Diamond Pattern
with DAG('etl_pipeline', schedule='@daily') as dag:
    extract = PythonOperator(task_id='extract', ...)
    transform_a = PythonOperator(task_id='transform_a', ...)
    transform_b = PythonOperator(task_id='transform_b', ...)
    load = PythonOperator(task_id='load', ...)
    
    extract >> [transform_a, transform_b] >> load  # Diamond pattern
```

### Comparison Table

| Feature | BullMQ Flows | Apache Airflow | Temporal |
|---------|-------------|----------------|----------|
| **Dependency Type** | Tree only | Full DAG | Full DAG (via workflows) |
| **Multiple Parents** | No | Yes | Yes |
| **Diamond/Merge Pattern** | No (workaround needed) | Native | Native |
| **Scheduling (Cron)** | Basic (separate feature) | Built-in, powerful | Built-in |
| **Visual UI** | Requires add-on | Built-in web UI | Built-in web UI |
| **Performance** | Very fast (Redis) | Slower (DB + scheduler) | Fast (event-sourced) |
| **Use Case** | Job queues, simple pipelines | Data pipelines, ETL | Long-running workflows |
| **Language** | Node.js | Python | Any (polyglot) |

### When to Use What

| Use Case | Best Choice | Why |
|----------|-------------|-----|
| Simple background jobs | BullMQ / AJ | Fast, lightweight |
| Fan-out/fan-in (tree) | BullMQ Flows | Native support |
| ETL data pipelines | Airflow | Full DAG, scheduling, UI |
| Complex DAG with merges | Airflow / Temporal | True DAG support |
| Long-running workflows | Temporal | Durable execution, retries |
| Microservices orchestration | Temporal | Polyglot, strongly typed |

### BullMQ Workaround for Diamond Pattern

From [GitHub Discussion #2279](https://github.com/taskforcesh/bullmq/discussions/2279), the workaround is to restructure:

```typescript
// Instead of: A → B, A → C, B+C → D (diamond)
// Use: A → [B, C, D-wrapper] where D-wrapper waits for B and C manually

// Or use a "coordinator" job pattern:
const flow = await flowProducer.add({
  name: 'coordinator',  // This acts as the merge point
  queueName: 'main',
  children: [
    { 
      name: 'branch-b', 
      queueName: 'process',
      children: [{ name: 'shared-A', queueName: 'extract' }]
    },
    { 
      name: 'branch-c', 
      queueName: 'process',
      children: [{ name: 'shared-A-copy', queueName: 'extract' }]  // Duplicate!
    },
  ],
});
```

This is clunky and duplicates work.

### Recommendation for AJ

For AJ's roadmap:

1. **Current**: Simple job queue (like Celery/Sidekiq) ✓
2. **Next**: Add tree-based flows (like BullMQ) - relatively simple
3. **Future**: Consider DAG support only if there's strong demand

**Tree-based flows** would cover 80% of use cases:
- Build pipelines (compile → test → deploy)
- Data processing (extract → transform → load)
- Batch operations (process items → summarize)

**Full DAG support** is significantly more complex and may be better served by dedicated tools like Airflow or Temporal.

### Sources

- [BullMQ Flows Documentation](https://docs.bullmq.io/guide/flows)
- [BullMQ DAG Discussion](https://github.com/taskforcesh/bullmq/discussions/2279)
- [Why Eppo Replaced Airflow](https://www.geteppo.com/blog/why-we-replaced-airflow-in-our-experimentation-platform)
- [Airflow Alternatives Comparison](https://hevodata.com/learn/airflow-alternatives/)

---

## Part 2: Macro Design Analysis

### Current Macro Implementation Review

#### The `#[derive(BackgroundJob)]` Macro

**Location**: `aj_macro/src/lib.rs:8-25`

```rust
#[proc_macro_derive(BackgroundJob)]
pub fn background_job_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;

    let expanded = quote! {
        impl BackgroundJob for #name {
            fn queue_name() -> &'static str {
                stringify!(#name)
            }

            fn job(self) -> aj::Job<Self> {
                aj::Job::new(self)
            }
        }
    };

    TokenStream::from(expanded)
}
```

**Assessment**: 

| Aspect | Rating | Notes |
|--------|--------|-------|
| Simplicity | Good | Single responsibility, easy to understand |
| Extensibility | Poor | No attribute support, no customization |
| Error Handling | Poor | No validation, no helpful error messages |
| Documentation | Poor | No doc generation |

#### The `#[job]` Attribute Macro

**Location**: `aj_macro/src/lib.rs:28-172`

**Assessment**:

| Aspect | Rating | Notes |
|--------|--------|-------|
| Code Generation | Good | Generates struct, impl, and helper functions |
| Type Handling | Moderate | Handles async/sync, but clones all args |
| Extensibility | Poor | No attributes for customization |
| Error Handling | Poor | Panics on invalid input, no span info |

---

### Comparison with Diesel and Serde

#### Serde's Approach

Serde uses a **three-layer architecture**:

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 1: Derive Macro Entry Point                          │
│  #[derive(Serialize, Deserialize)]                         │
│  - Parses struct/enum                                      │
│  - Extracts field attributes                               │
│  - Delegates to internals                                  │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  Layer 2: Container/Field Attributes (serde_derive_internals) │
│  #[serde(rename = "...")]                                  │
│  #[serde(skip)]                                            │
│  #[serde(default)]                                         │
│  - Parsed into structured types                            │
│  - Validated with helpful errors                           │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  Layer 3: Code Generation (quote!)                          │
│  - Conditional generation based on attributes              │
│  - Handles generics properly                               │
│  - Generates impl blocks                                   │
└─────────────────────────────────────────────────────────────┘
```

**Key Serde Patterns**:

1. **Container Attributes** - Apply to entire struct/enum
2. **Field Attributes** - Apply to individual fields  
3. **Variant Attributes** - Apply to enum variants
4. **Attribute Validation** - Errors with span information
5. **Conditional Code Generation** - Different code paths based on attributes

#### Diesel's Approach

Diesel uses **schema-driven code generation**:

```
┌─────────────────────────────────────────────────────────────┐
│  diesel::table! macro (declarative)                         │
│  - Defines schema at compile time                          │
│  - Generates types for queries                             │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  #[derive(Queryable, Insertable, etc.)]                     │
│  - Maps struct fields to columns                           │
│  - Uses #[diesel(table_name = ...)] for configuration      │
│  - Validates against schema                                │
└─────────────────────────────────────────────────────────────┘
```

**Key Diesel Patterns**:

1. **Schema as Source of Truth** - `table!` macro defines structure
2. **Multiple Derive Macros** - `Queryable`, `Insertable`, `Selectable`, etc.
3. **Association Attributes** - `#[diesel(belongs_to(...))]`
4. **Column Mapping** - `#[diesel(column_name = "...")]`

---

### Recommended Macro Extensions for AJ

#### 1. Add Attribute Support to `#[derive(BackgroundJob)]`

```rust
// Current (limited)
#[derive(BackgroundJob)]
pub struct MyJob { ... }

// Proposed (extensible like Serde/Diesel)
#[derive(BackgroundJob)]
#[aj(queue_name = "high_priority")]
#[aj(max_retries = 3)]
#[aj(retry_strategy = "exponential")]
#[aj(timeout = "30s")]
pub struct MyJob {
    #[aj(skip)]           // Don't serialize this field
    temp_data: String,
    
    #[aj(rename = "uid")]  // Rename in serialization
    user_id: i32,
}
```

**Implementation**:

```rust
// aj_macro/src/lib.rs

use syn::{Attribute, Meta, NestedMeta, Lit};

struct ContainerAttrs {
    queue_name: Option<String>,
    max_retries: Option<u32>,
    retry_strategy: Option<RetryStrategy>,
    timeout: Option<Duration>,
}

struct FieldAttrs {
    skip: bool,
    rename: Option<String>,
}

fn parse_container_attrs(attrs: &[Attribute]) -> ContainerAttrs {
    let mut container = ContainerAttrs::default();
    
    for attr in attrs {
        if !attr.path.is_ident("aj") {
            continue;
        }
        
        match attr.parse_meta() {
            Ok(Meta::List(list)) => {
                for nested in list.nested {
                    match nested {
                        NestedMeta::Meta(Meta::NameValue(nv)) => {
                            if nv.path.is_ident("queue_name") {
                                if let Lit::Str(s) = nv.lit {
                                    container.queue_name = Some(s.value());
                                }
                            }
                            // ... handle other attributes
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    
    container
}

#[proc_macro_derive(BackgroundJob, attributes(aj))]
pub fn background_job_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    
    // Parse container attributes
    let container = parse_container_attrs(&input.attrs);
    
    let queue_name = container.queue_name
        .map(|s| quote! { #s })
        .unwrap_or_else(|| quote! { stringify!(#name) });
    
    let expanded = quote! {
        impl BackgroundJob for #name {
            fn queue_name() -> &'static str {
                #queue_name
            }

            fn job(self) -> aj::Job<Self> {
                aj::Job::new(self)
            }
        }
    };

    TokenStream::from(expanded)
}
```

#### 2. Add Attributes to `#[job]` Macro

```rust
// Current (limited)
#[job]
async fn process_order(order_id: i32) { ... }

// Proposed (rich configuration)
#[job(
    queue = "orders",
    retries = 5,
    retry_delay = "1s",
    timeout = "5m",
    unique_key = "order_id",  // Deduplicate by this field
    priority = "high"
)]
async fn process_order(
    #[job(rename = "oid")] order_id: i32,
    #[job(default)] notify: bool,
) { ... }
```

**Implementation**:

```rust
// aj_macro/src/job_attrs.rs (new file)

use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, LitInt, Token};

pub struct JobAttrs {
    pub queue: Option<String>,
    pub retries: Option<u32>,
    pub retry_delay: Option<String>,
    pub timeout: Option<String>,
    pub unique_key: Option<String>,
    pub priority: Option<String>,
}

impl Parse for JobAttrs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut attrs = JobAttrs::default();
        
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            
            match key.to_string().as_str() {
                "queue" => {
                    let val: LitStr = input.parse()?;
                    attrs.queue = Some(val.value());
                }
                "retries" => {
                    let val: LitInt = input.parse()?;
                    attrs.retries = Some(val.base10_parse()?);
                }
                // ... other attributes
                unknown => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown attribute: {}", unknown)
                    ));
                }
            }
            
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        
        Ok(attrs)
    }
}
```

#### 3. Implement Error Reporting Like Serde

```rust
// aj_macro/src/error.rs (new file)

use proc_macro2::Span;
use syn::Error;

pub fn duplicate_attr(span: Span, name: &str) -> Error {
    Error::new(span, format!("duplicate `{}` attribute", name))
}

pub fn invalid_attr_value(span: Span, name: &str, expected: &str) -> Error {
    Error::new(
        span,
        format!(
            "invalid value for `{}` attribute, expected {}",
            name, expected
        ),
    )
}

pub fn unknown_attr(span: Span, name: &str, valid: &[&str]) -> Error {
    let valid_str = valid.join(", ");
    Error::new(
        span,
        format!(
            "unknown attribute `{}`, expected one of: {}",
            name, valid_str
        ),
    )
}

// Usage in macro:
// return Err(error::unknown_attr(key.span(), &key_str, &["queue", "retries"]));
```

#### 4. Support Generics Properly

Current limitation: No generic support

```rust
// This should work but may not:
#[derive(BackgroundJob)]
pub struct GenericJob<T: Serialize + DeserializeOwned> {
    data: T,
}
```

**Fix**:

```rust
#[proc_macro_derive(BackgroundJob, attributes(aj))]
pub fn background_job_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    
    // Add required bounds
    let where_clause = if let Some(wc) = where_clause {
        let predicates = &wc.predicates;
        quote! { where #predicates, Self: Clone + serde::Serialize + serde::de::DeserializeOwned }
    } else {
        quote! { where Self: Clone + serde::Serialize + serde::de::DeserializeOwned }
    };

    let expanded = quote! {
        impl #impl_generics BackgroundJob for #name #ty_generics #where_clause {
            fn queue_name() -> &'static str {
                stringify!(#name)
            }

            fn job(self) -> aj::Job<Self> {
                aj::Job::new(self)
            }
        }
    };

    TokenStream::from(expanded)
}
```

#### 5. Add Multiple Derive Macros (Diesel-style)

Split responsibilities into focused derives:

```rust
// Like Diesel's Queryable, Insertable, etc.
#[derive(BackgroundJob)]     // Core trait
#[derive(Retryable)]         // Adds retry configuration
#[derive(Schedulable)]       // Adds cron/delay support  
#[derive(Traceable)]         // Adds tracing/metrics hooks
pub struct MyJob { ... }

// Or combine them:
#[derive(BackgroundJob, Retryable, Schedulable)]
pub struct MyJob { ... }
```

**Implementation**:

```rust
// aj_macro/src/lib.rs

#[proc_macro_derive(Retryable, attributes(aj))]
pub fn retryable_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let attrs = parse_retry_attrs(&input.attrs);
    
    let max_retries = attrs.max_retries.unwrap_or(3);
    let strategy = match attrs.strategy.as_deref() {
        Some("exponential") => quote! { RetryStrategy::ExponentialBackoff },
        _ => quote! { RetryStrategy::Interval },
    };
    
    let expanded = quote! {
        impl Retryable for #name {
            fn default_retry() -> Option<Retry> {
                Some(Retry::new(#max_retries, #strategy))
            }
        }
    };

    TokenStream::from(expanded)
}
```

#### 6. Add Helper Attribute Macros

```rust
// Validate job parameters at compile time
#[aj::validate]
#[job]
async fn send_email(
    #[validate(email)] to: String,
    #[validate(non_empty)] subject: String,
) { ... }

// Auto-generate OpenTelemetry spans
#[aj::traced]
#[job]
async fn process_payment(amount: f64) { ... }
```

---

### Macro Architecture Recommendations

#### Recommended Project Structure

```
aj_macro/
├── Cargo.toml
├── src/
│   ├── lib.rs              # Entry points only
│   ├── background_job.rs   # BackgroundJob derive
│   ├── job.rs              # #[job] attribute
│   ├── retryable.rs        # Retryable derive
│   ├── schedulable.rs      # Schedulable derive
│   ├── attrs/
│   │   ├── mod.rs
│   │   ├── container.rs    # Container-level attributes
│   │   ├── field.rs        # Field-level attributes
│   │   └── parse.rs        # Parsing utilities
│   ├── codegen/
│   │   ├── mod.rs
│   │   ├── impl_block.rs   # Generate impl blocks
│   │   └── helpers.rs      # Generate helper functions
│   └── error.rs            # Error utilities with spans
```

#### Key Principles from Serde/Diesel

1. **Separate Parsing from Generation** - Parse attributes into structured types first, then generate code
2. **Validate Early with Good Errors** - Check attribute combinations and values with helpful span-based errors
3. **Support Composition** - Allow multiple derives to work together
4. **Handle Generics Correctly** - Properly propagate generic parameters and bounds
5. **Document Generated Code** - Add `#[doc]` attributes to generated items
6. **Use Span Information** - Preserve source spans for error messages

---

## Summary

### Distributed Architecture

The key insight is that **Backend should own distributed coordination**. This keeps WorkQueue logic clean and allows different backends to implement locking appropriately for their technology.

| Change | Priority | Effort |
|--------|----------|--------|
| Add `lock_*` methods to Backend trait (with default no-ops) | High | Low |
| Add `claim_ready_job` to Backend trait | High | Medium |
| Add `requeue_orphaned_jobs` to Backend trait | High | Low |
| Implement Lua scripts in Redis backend | High | Medium |
| Add HeartbeatActor for lock extension | Medium | Low |
| Add ReaperActor for orphan recovery | Medium | Low |
| Add `DistributedConfig` and `start_distributed()` | Medium | Low |

**No static registry changes needed** - queue names already serve as coordination keys.

### Macro Improvements

| Change | Priority | Effort |
|--------|----------|--------|
| Add attribute parsing (`#[aj(...)]`) | High | Medium |
| Improve error messages with spans | High | Low |
| Support generics properly | Medium | Low |
| Split into multiple focused derives | Low | Medium |
| Add validation attribute macros | Low | High |

### Key Design Decisions

1. **Backend-owned locking**: Each backend implements locking appropriate to its technology
   - InMemory: No-op (single process)
   - Redis: SET NX EX + Lua scripts
   - PostgreSQL: SELECT FOR UPDATE SKIP LOCKED
   
2. **Queue names as coordination keys**: Workers register dynamically, no TypeId sharing needed

3. **Backward compatible**: All new Backend methods have default implementations

4. **Macro extensibility**: Follow Serde/Diesel patterns for attribute parsing and error handling

Both areas have clear paths forward that maintain backward compatibility while enabling significant new capabilities.
