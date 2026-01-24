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

## Backend Trait

To implement a custom backend, implement the `Backend` trait:

```rust
pub trait Backend: Send + Sync {
    // Waiting Queue (ready to process)
    fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error>;
    fn waiting_pop(&self, queue: &str) -> Result<Option<String>, Error>;
    fn waiting_len(&self, queue: &str) -> Result<usize, Error>;

    // Delayed Queue (scheduled for future)
    fn delayed_push(&self, queue: &str, job_id: &str, run_at_ms: i64) -> Result<(), Error>;
    fn delayed_move_ready(&self, queue: &str, now_ms: i64) -> Result<usize, Error>;
    fn delayed_remove(&self, queue: &str, job_id: &str) -> Result<(), Error>;
    fn delayed_len(&self, queue: &str) -> Result<usize, Error>;

    // Active Queue (currently processing)
    fn active_push(&self, queue: &str, job_id: &str) -> Result<(), Error>;
    fn active_remove(&self, queue: &str, job_id: &str) -> Result<(), Error>;
    fn active_len(&self, queue: &str) -> Result<usize, Error>;
    fn active_list(&self, queue: &str) -> Result<Vec<String>, Error>;

    // Job Storage
    fn job_save(&self, queue: &str, job_id: &str, data: &str) -> Result<(), Error>;
    fn job_get(&self, queue: &str, job_id: &str) -> Result<Option<String>, Error>;
    fn job_delete(&self, queue: &str, job_id: &str) -> Result<(), Error>;

    // Distributed Locking (optional - has default impl)
    fn lock_acquire(&self, job_id: &str, worker_id: &str, ttl_ms: u64) -> Result<bool, Error>;
    fn lock_release(&self, job_id: &str, worker_id: &str) -> Result<bool, Error>;
    fn lock_extend(&self, job_id: &str, worker_id: &str, ttl_ms: u64) -> Result<bool, Error>;

    // Atomic Operations (optional - has default impl)
    fn claim_job(&self, queue: &str, worker_id: &str, lock_ttl_ms: u64) -> Result<Option<String>, Error>;
    fn complete_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error>;
    fn fail_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error>;
    fn requeue_orphaned(&self, queue: &str) -> Result<Vec<String>, Error>;
}
```

## Implementing a Custom Backend

### Example: PostgreSQL Backend

```rust
use aj_core::{Backend, Error};

pub struct PostgresBackend {
    pool: PgPool,
}

impl Backend for PostgresBackend {
    fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        // INSERT INTO waiting_queue (queue_name, job_id, created_at)
        // VALUES ($1, $2, NOW())
        todo!()
    }

    fn waiting_pop(&self, queue: &str) -> Result<Option<String>, Error> {
        // DELETE FROM waiting_queue
        // WHERE id = (SELECT id FROM waiting_queue WHERE queue_name = $1 ORDER BY created_at LIMIT 1)
        // RETURNING job_id
        todo!()
    }

    fn delayed_push(&self, queue: &str, job_id: &str, run_at_ms: i64) -> Result<(), Error> {
        // INSERT INTO delayed_queue (queue_name, job_id, run_at)
        // VALUES ($1, $2, to_timestamp($3 / 1000.0))
        todo!()
    }

    fn delayed_move_ready(&self, queue: &str, now_ms: i64) -> Result<usize, Error> {
        // WITH moved AS (
        //     DELETE FROM delayed_queue
        //     WHERE queue_name = $1 AND run_at <= to_timestamp($2 / 1000.0)
        //     RETURNING job_id
        // )
        // INSERT INTO waiting_queue (queue_name, job_id, created_at)
        // SELECT $1, job_id, NOW() FROM moved
        todo!()
    }

    fn claim_job(&self, queue: &str, worker_id: &str, lock_ttl_ms: u64) -> Result<Option<String>, Error> {
        // Use advisory locks or SELECT FOR UPDATE SKIP LOCKED
        // BEGIN;
        // SELECT job_id FROM waiting_queue WHERE queue_name = $1 FOR UPDATE SKIP LOCKED LIMIT 1;
        // DELETE FROM waiting_queue WHERE job_id = $2;
        // INSERT INTO active_queue (queue_name, job_id, worker_id, locked_until) VALUES (...);
        // COMMIT;
        todo!()
    }

    // ... implement remaining methods
}
```

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
fn requeue_orphaned(&self, queue: &str) -> Result<Vec<String>, Error> {
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

| Feature | InMemory | Redis | PostgreSQL* |
|---------|----------|-------|-------------|
| Persistence | No | Optional | Yes |
| Distributed | No | Yes | Yes |
| Atomic ops | Mutex | Lua scripts | Transactions |
| Performance | Fastest | Fast | Moderate |
| Scaling | Single process | Multi-process | Multi-process |

*PostgreSQL backend not included, shown as implementation example.

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
