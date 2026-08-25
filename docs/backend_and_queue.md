# Backend and Queue Design

This document describes the queue architecture and how to implement a custom backend for AJ.

## Queue Architecture

AJ uses a three-queue pattern inspired by industry best practices (Sidekiq, BullMQ):

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         Job Lifecycle                                   │
│                                                                         │
│   ┌─────────────┐                                                       │
│   │  Job Created │                                                       │
│   └──────┬──────┘                                                       │
│          │                                                              │
│          ▼                                                              │
│   ┌─────────────────────────────────────────────────────────────────┐   │
│   │                     DELAYED QUEUE (ZSET)                        │   │
│   │                                                                 │   │
│   │  Sorted by: run_at timestamp (score)                           │   │
│   │  Contains: Scheduled jobs, cron jobs, retry jobs               │   │
│   │                                                                 │   │
│   │  ┌─────────┬─────────┬─────────┬─────────┐                     │   │
│   │  │ job_id  │ job_id  │ job_id  │ job_id  │  ← sorted by time   │   │
│   │  │ t=1000  │ t=2000  │ t=3000  │ t=5000  │                     │   │
│   │  └─────────┴─────────┴─────────┴─────────┘                     │   │
│   └─────────────────────────┬───────────────────────────────────────┘   │
│                             │                                           │
│                             │ delayed_move_ready(now)                   │
│                             │ (moves jobs where run_at <= now)          │
│                             ▼                                           │
│   ┌─────────────────────────────────────────────────────────────────┐   │
│   │                     WAITING QUEUE (LIST)                        │   │
│   │                                                                 │   │
│   │  FIFO queue for jobs ready to execute                          │   │
│   │  Contains: Immediate jobs, jobs moved from delayed              │   │
│   │                                                                 │   │
│   │  ┌─────────┬─────────┬─────────┬─────────┐                     │   │
│   │  │ job_id  │ job_id  │ job_id  │ job_id  │  ← FIFO order       │   │
│   │  └─────────┴─────────┴─────────┴─────────┘                     │   │
│   │      ▲                                 │                        │   │
│   │      │                                 │                        │   │
│   └──────┼─────────────────────────────────┼────────────────────────┘   │
│          │                                 │                            │
│          │ requeue (retry/cron)            │ claim_job(worker_id)       │
│          │                                 │ (atomic: pop + lock + push)│
│          │                                 ▼                            │
│   ┌──────┴──────────────────────────────────────────────────────────┐   │
│   │                     ACTIVE QUEUE (LIST)                         │   │
│   │                                                                 │   │
│   │  Jobs currently being processed by workers                      │   │
│   │  Each job has an associated lock with TTL                       │   │
│   │                                                                 │   │
│   │  ┌─────────┬─────────┬─────────┐                               │   │
│   │  │ job_id  │ job_id  │ job_id  │  ← being processed            │   │
│   │  │ lock:w1 │ lock:w2 │ lock:w1 │                               │   │
│   │  └─────────┴─────────┴─────────┘                               │   │
│   └─────────────────────────┬───────────────────────────────────────┘   │
│                             │                                           │
│                             │ complete_job() / fail_job()               │
│                             ▼                                           │
│                      ┌─────────────┐                                    │
│                      │    Done     │                                    │
│                      └─────────────┘                                    │
└─────────────────────────────────────────────────────────────────────────┘
```

## Data Structures

### Redis Implementation

| Queue | Redis Type | Key Pattern | Description |
|-------|------------|-------------|-------------|
| Delayed | ZSET | `{queue}:delayed` | Score = run_at timestamp (ms) |
| Waiting | LIST | `{queue}:waiting` | FIFO queue (LPOP/RPUSH) |
| Active | LIST | `{queue}:active` | Currently processing |
| Storage | HASH | `{queue}:storage` | job_id → job_data (JSON) |
| Locks | STRING | `aj:lock:{job_id}` | Worker ID with TTL |

### In-Memory Implementation

| Queue | Rust Type | Description |
|-------|-----------|-------------|
| Delayed | `BTreeMap<i64, Vec<String>>` | Sorted by timestamp |
| Waiting | `VecDeque<String>` | FIFO queue |
| Active | `Vec<String>` | Currently processing |
| Storage | `HashMap<String, String>` | job_id → job_data |

### Postgres Implementation

Rather than four separate structures, all three queues plus storage live in one row per
`(queue, job_id)`, distinguished by a `state` column. Every object carries a configurable
prefix (default `aj_`).

| Queue | Representation | Description |
|-------|----------------|-------------|
| Delayed | `aj_job_queue` where `state = 'delayed'` | `ready_at_ms` = run_at timestamp (ms) |
| Waiting | `aj_job_queue` where `state = 'waiting'` | FIFO via `ORDER BY ready_at_ms, seq` |
| Active | `aj_job_queue` where `state = 'active'` | Currently processing |
| Storage | `aj_job_queue.payload` | Job JSON, as `TEXT` |
| Locks | `aj_job_lock` | `job_id` → worker_id + `expires_at_ms` |

Two consequences of the single-table shape:

- `state IS NULL` means the row exists but belongs to no queue, the equivalent of a job in
  Redis' storage hash but absent from all three lists. That is what a completed job leaves
  behind.
- `payload` is nullable, because the trait permits pushing a job id that was never
  `job_save`d.

`aj_job_lock` is keyed on `job_id` alone, deliberately **not** queue-scoped, mirroring Redis'
`aj:lock:{job_id}`. It has no foreign key to `aj_job_queue`, so `lock_acquire` works for a
job that has no row at all.

Postgres has no TTL eviction, so lock expiry is a predicate on `expires_at_ms` rather than
something the server does for you. Every read of a lock is guarded by `expires_at_ms > now`,
and `purge_expired_locks(grace_ms)` reclaims the dead rows.

## Backend Trait

To implement a custom backend, implement the `Backend` trait. It is asynchronous, so use
`#[async_trait]` (re-exported as `aj::async_trait`) and make every method an `async fn`:

```rust
#[async_trait]
pub trait Backend: Send + Sync {
    // Waiting Queue (ready to process)
    async fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error>;
    async fn waiting_pop(&self, queue: &str) -> Result<Option<String>, Error>;
    async fn waiting_len(&self, queue: &str) -> Result<usize, Error>;

    // Delayed Queue (scheduled for future)
    async fn delayed_push(&self, queue: &str, job_id: &str, run_at_ms: i64) -> Result<(), Error>;
    async fn delayed_move_ready(&self, queue: &str, now_ms: i64) -> Result<usize, Error>;
    async fn delayed_remove(&self, queue: &str, job_id: &str) -> Result<(), Error>;
    async fn delayed_len(&self, queue: &str) -> Result<usize, Error>;

    // Active Queue (currently processing)
    async fn active_push(&self, queue: &str, job_id: &str) -> Result<(), Error>;
    async fn active_remove(&self, queue: &str, job_id: &str) -> Result<(), Error>;
    async fn active_len(&self, queue: &str) -> Result<usize, Error>;
    async fn active_list(&self, queue: &str) -> Result<Vec<String>, Error>;

    // Job Storage
    async fn job_save(&self, queue: &str, job_id: &str, data: &str) -> Result<(), Error>;
    async fn job_get(&self, queue: &str, job_id: &str) -> Result<Option<String>, Error>;
    async fn job_delete(&self, queue: &str, job_id: &str) -> Result<(), Error>;

    // Distributed Locking (optional - has default impl)
    async fn lock_acquire(&self, job_id: &str, worker_id: &str, ttl_ms: u64) -> Result<bool, Error>;
    async fn lock_release(&self, job_id: &str, worker_id: &str) -> Result<bool, Error>;
    async fn lock_extend(&self, job_id: &str, worker_id: &str, ttl_ms: u64) -> Result<bool, Error>;

    // Atomic Operations (optional - has default impl)
    async fn claim_job(&self, queue: &str, worker_id: &str, lock_ttl_ms: u64) -> Result<Option<String>, Error>;
    async fn complete_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error>;
    async fn fail_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error>;
    async fn requeue_orphaned(&self, queue: &str) -> Result<Vec<String>, Error>;
}
```

## Implementing a Custom Backend

### Built-in: the Postgres backend

Postgres is implemented in-tree at `aj_core/src/backend/postgres.rs`, behind the `postgres`
feature. Read it as the reference for a SQL-backed implementation. The parts worth copying:

**Atomic claim, with `FOR UPDATE SKIP LOCKED` in place of a Lua script.** One statement picks
the FIFO-head waiting row, takes the lock, and flips the row to `active`. If the lock is held
by a live worker the middle CTE returns nothing, the `UPDATE` matches nothing, and the row
stays `waiting`, which is the same put-back branch as `LUA_CLAIM_JOB`.

```sql
WITH picked AS MATERIALIZED (
    SELECT job_id FROM aj_job_queue
    WHERE queue = $1::text AND state = 'waiting'
    ORDER BY ready_at_ms, seq
    LIMIT 1
    FOR UPDATE SKIP LOCKED
), locked AS (
    INSERT INTO aj_job_lock (job_id, worker_id, expires_at_ms)
    SELECT job_id, $2::text, $3::bigint FROM picked
    ON CONFLICT (job_id) DO UPDATE
        SET worker_id = EXCLUDED.worker_id, expires_at_ms = EXCLUDED.expires_at_ms
        WHERE aj_job_lock.expires_at_ms <= $4::bigint
    RETURNING aj_job_lock.job_id
)
UPDATE aj_job_queue q SET state = 'active', worker_id = $2::text
WHERE q.queue = $1::text AND q.state = 'waiting'
  AND q.job_id IN (SELECT job_id FROM locked)
RETURNING q.job_id;
```

**Lock ordering.** Every statement touches `aj_job_queue` before `aj_job_lock`, with no
exceptions, which is what rules out a deadlock cycle. `complete_job` / `fail_job` therefore
use an explicit transaction rather than a data-modifying CTE: CTE sub-statements have no
guaranteed execution order, so the lock order would become nondeterministic.

**Ordering without a list.** `(ready_at_ms, seq)` stands in for Redis list order. `seq` comes
from a prefixed sequence and breaks ties within the same millisecond, so re-pushing a job
assigns a fresh `ready_at_ms` *and* `seq` to move it to the back. Because `delayed_push`
writes `run_at` into the same `ready_at_ms` column, `delayed_move_ready` is a plain
`UPDATE ... SET state = 'waiting'` with no per-row sequencing.

**Guard every transition.** With one `state` column, an unguarded `SET state = NULL` would
clobber unrelated membership: `cancel_job` calls `delayed_remove` for jobs that may be sitting
in `waiting`. Every removal carries `AND state = '<expected>'`.

**Lazy connection, lazy schema.** The pool is built synchronously and opens no sockets, so
`Postgres::new` stays sync like `Redis::new`. The schema bootstrap does need a connection, so
it runs once on first use, under a transaction-scoped advisory lock because
`CREATE TABLE IF NOT EXISTS` is not race-free.

### Writing your own

### Key Implementation Considerations

#### 1. Atomicity

For distributed backends, ensure atomic operations:

```rust
// Redis: Use Lua scripts
const LUA_CLAIM_JOB: &str = r#"
    local job_id = redis.call('LPOP', KEYS[1])
    if not job_id then return nil end
    
    local lock_key = 'aj:lock:' .. job_id
    local acquired = redis.call('SET', lock_key, ARGV[1], 'NX', 'PX', ARGV[2])
    
    if acquired then
        redis.call('RPUSH', KEYS[2], job_id)
        return job_id
    else
        redis.call('LPUSH', KEYS[1], job_id)
        return nil
    end
"#;

// PostgreSQL: Use transactions with FOR UPDATE SKIP LOCKED
// MongoDB: Use findAndModify with appropriate write concern
```

#### 2. Lock Safety

Implement safe lock release (only owner can release):

```rust
// Redis Lua script
const LUA_LOCK_RELEASE: &str = r#"
    if redis.call('GET', KEYS[1]) == ARGV[1] then
        return redis.call('DEL', KEYS[1])
    end
    return 0
"#;

// PostgreSQL: Use WHERE clause
// DELETE FROM locks WHERE job_id = $1 AND worker_id = $2
```

#### 3. Orphan Recovery

Handle crashed workers by requeuing orphaned jobs:

```rust
async fn requeue_orphaned(&self, queue: &str) -> Result<Vec<String>, Error> {
    // Find jobs in active queue without valid locks
    // Move them back to waiting queue
}
```

#### 4. FIFO Ordering

Maintain job ordering in waiting queue:
- Use LPOP/RPUSH for Redis LIST
- Use ORDER BY created_at for SQL databases
- Use VecDeque for in-memory

## Backend Comparison

| Feature | InMemory | Redis | Postgres |
|---------|----------|-------|----------|
| Feature flag | none (default) | `redis` | `postgres`, or `postgres-tls` for TLS |
| Persistence | No | Optional | Yes |
| Distributed | No | Yes | Yes |
| Atomic ops | Mutex | Lua scripts | `FOR UPDATE SKIP LOCKED` |
| Performance | Fastest | Fast | Moderate |
| Scaling | Single process | Multi-process | Multi-process |

## WorkQueue Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                      WorkQueue::process_jobs()                  │
│                                                                 │
│  1. delayed_move_ready(now_ms)                                  │
│     └─ Move scheduled jobs that are ready to waiting queue      │
│                                                                 │
│  2. For each available slot:                                    │
│     └─ claim_job(worker_id, lock_ttl)                          │
│        └─ Atomically: pop from waiting → lock → push to active │
│                                                                 │
│  3. For each claimed job:                                       │
│     └─ execute_job()                                           │
│        ├─ Success → complete_job() (remove from active)         │
│        ├─ Retry   → re_enqueue() (back to delayed/waiting)      │
│        └─ Failure → fail_job() (remove from active)             │
└─────────────────────────────────────────────────────────────────┘
```

## Configuration

```rust
pub struct WorkQueueConfig {
    pub process_tick_duration: Duration,  // How often to check for jobs (default: 100ms)
    pub max_processing_jobs: usize,       // Max concurrent jobs (default: 20)
    pub lock_ttl_ms: u64,                 // Lock timeout (default: 30000ms)
}
```

## Best Practices

1. **Lock TTL**: Set lock TTL longer than your longest expected job execution time
2. **Tick Duration**: Balance between responsiveness and resource usage
3. **Max Processing Jobs**: Consider your system resources and job characteristics
4. **Orphan Recovery**: Run `requeue_orphaned()` periodically in production
5. **Idempotency**: Design jobs to be idempotent in case of retries
