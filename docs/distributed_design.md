# Distributed Mode Design

This document describes the design and requirements for running AJ in distributed/multi-node mode.

## Table of Contents

1. [Why Multi-Node](#why-multi-node)
2. [Current Architecture](#current-architecture)
3. [Distributed Requirements](#distributed-requirements)
   - [Phase 1: Safety](#phase-1-safety-critical)
   - [Phase 2: Visibility](#phase-2-visibility)
   - [Phase 3: Control](#phase-3-control)
4. [Configuration](#configuration)
5. [Deployment Guide](#deployment-guide)
6. [Migration Path](#migration-path)

---

## Why Multi-Node

### 1. Horizontal Scalability

Single-node processing has inherent limits:

```
Single Node Capacity:
┌─────────────────────────────────────┐
│  max_processing_jobs = 20 (default) │
│  process_tick = 100ms               │
│  Theoretical max: ~200 jobs/second  │
└─────────────────────────────────────┘

Multi-Node Scaling:
┌─────────┐ ┌─────────┐ ┌─────────┐
│ Node 1  │ │ Node 2  │ │ Node N  │
│ 20 jobs │ │ 20 jobs │ │ 20 jobs │
└─────────┘ └─────────┘ └─────────┘
     │           │           │
     └───────────┴───────────┘
              ↓
    Total: N × 20 concurrent jobs
```

**Use Cases:**
- High job volume (thousands/second)
- CPU-intensive jobs requiring parallel processing
- Burst handling during peak loads

### 2. High Availability

Single point of failure is unacceptable for production:

```
Single Node (Fragile):
┌─────────┐
│ Node 1  │ ──── Node dies → All processing stops
└─────────┘

Multi-Node (Resilient):
┌─────────┐ ┌─────────┐ ┌─────────┐
│ Node 1  │ │ Node 2  │ │ Node 3  │
└─────────┘ └─────────┘ └─────────┘
     │           │           │
     └───────────┴───────────┘
              ↓
    Node 1 dies → Nodes 2,3 continue
    Orphaned jobs recovered automatically
```

**Benefits:**
- Zero downtime during deployments (rolling updates)
- Automatic failover on node crashes
- Meet SLA requirements for critical systems

### 3. Geographic Distribution

Process jobs closer to data or users:

```
┌─────────────────────────────────────────────────────────────┐
│                      Global Deployment                       │
│                                                             │
│   US-East          EU-West           AP-South               │
│  ┌───────┐        ┌───────┐        ┌───────┐               │
│  │ AJ    │        │ AJ    │        │ AJ    │               │
│  │ Node  │        │ Node  │        │ Node  │               │
│  └───┬───┘        └───┬───┘        └───┬───┘               │
│      │                │                │                    │
│      └────────────────┴────────────────┘                    │
│                       │                                     │
│              ┌────────┴────────┐                           │
│              │  Redis Cluster  │                           │
│              │  (or replicas)  │                           │
│              └─────────────────┘                           │
└─────────────────────────────────────────────────────────────┘
```

**Benefits:**
- Reduced latency for region-specific jobs
- Data locality compliance (GDPR, etc.)
- Follow-the-sun processing

### 4. Resource Isolation

Separate workloads by characteristics:

```
┌─────────────────────────────────────────────────────────────┐
│                    Workload Isolation                        │
│                                                             │
│   CPU-Heavy Nodes         I/O-Heavy Nodes                   │
│  ┌─────────────────┐    ┌─────────────────┐                │
│  │ • Image resize  │    │ • API calls     │                │
│  │ • Video encode  │    │ • DB queries    │                │
│  │ • ML inference  │    │ • File uploads  │                │
│  │                 │    │                 │                │
│  │ max_jobs: 4     │    │ max_jobs: 100   │                │
│  │ (CPU bound)     │    │ (I/O bound)     │                │
│  └─────────────────┘    └─────────────────┘                │
└─────────────────────────────────────────────────────────────┘
```

**Benefits:**
- Prevent resource starvation between job types
- Optimize node resources for specific workloads
- Cost efficiency (right-size infrastructure)

---

## Current Architecture

### What's Already Built

The Redis backend provides solid distributed primitives:

| Component | Implementation | Status |
|-----------|---------------|--------|
| Atomic job claiming | Lua script `LUA_CLAIM_JOB` | Ready |
| Distributed locks | `SET NX PX` with worker ID | Ready |
| Lock release (owner-only) | Lua script `LUA_LOCK_RELEASE` | Ready |
| Lock extension | Lua script `LUA_LOCK_EXTEND` | Ready |
| Orphan detection | Lua script `LUA_REQUEUE_ORPHANED` | Ready |
| Shared job storage | Redis HASH | Ready |
| Shared queue state | Redis LIST/ZSET | Ready |

### Current Gaps

| Component | Current State | Impact |
|-----------|--------------|--------|
| Orphan recovery loop | Script exists, never invoked | Jobs lost on crash |
| Lock heartbeat | No automatic extension | Long jobs stolen |
| Worker registry | None | No visibility |
| Global concurrency | None | Resource exhaustion |
| Graceful shutdown | None | Jobs abandoned |
| Health checks | None | Silent failures |

### Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         Current Architecture                            │
│                                                                         │
│   Node 1                    Node 2                    Node N            │
│  ┌──────────────┐          ┌──────────────┐          ┌──────────────┐  │
│  │ WorkQueue    │          │ WorkQueue    │          │ WorkQueue    │  │
│  │ (Kameo Actor)│          │ (Kameo Actor)│          │ (Kameo Actor)│  │
│  │              │          │              │          │              │  │
│  │ worker_id: A │          │ worker_id: B │          │ worker_id: N │  │
│  │ max_jobs: 20 │          │ max_jobs: 20 │          │ max_jobs: 20 │  │
│  └──────┬───────┘          └──────┬───────┘          └──────┬───────┘  │
│         │                         │                         │          │
│         └─────────────────────────┴─────────────────────────┘          │
│                                   │                                     │
│                                   ▼                                     │
│                    ┌──────────────────────────────┐                    │
│                    │         Redis Backend        │                    │
│                    │                              │                    │
│                    │  {queue}:waiting  (LIST)    │                    │
│                    │  {queue}:delayed  (ZSET)    │                    │
│                    │  {queue}:active   (LIST)    │                    │
│                    │  {queue}:storage  (HASH)    │                    │
│                    │  aj:lock:{job_id} (STRING)  │                    │
│                    └──────────────────────────────┘                    │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Distributed Requirements

### Phase 1: Safety (Critical)

These features prevent data loss and must be implemented first.

#### 1.1 Orphan Job Recovery

**Problem:** Worker crashes mid-job → job stuck in active queue forever.

**Current state:**
- `requeue_orphaned()` Lua script exists in `redis.rs:220-237`
- Never called in production code

**Solution: Orphan Reaper**

```rust
/// Periodically scans for orphaned jobs and requeues them.
/// An orphaned job is in the active queue but has no valid lock.
pub struct OrphanReaper {
    backend: Arc<dyn Backend>,
    queues: Vec<String>,
    check_interval: Duration,
}

impl OrphanReaper {
    pub fn new(
        backend: Arc<dyn Backend>,
        queues: Vec<String>,
        check_interval: Duration,
    ) -> Self {
        Self { backend, queues, check_interval }
    }

    /// Start the reaper loop. Should run on at least one node.
    pub async fn run(&self) {
        let mut interval = tokio::time::interval(self.check_interval);
        loop {
            interval.tick().await;
            for queue in &self.queues {
                match self.backend.requeue_orphaned(queue) {
                    Ok(orphaned) if !orphaned.is_empty() => {
                        log::warn!(
                            "[OrphanReaper] Requeued {} orphaned jobs in queue '{}'",
                            orphaned.len(),
                            queue
                        );
                        for job_id in &orphaned {
                            log::info!("[OrphanReaper] Requeued job: {}", job_id);
                        }
                    }
                    Err(e) => {
                        log::error!("[OrphanReaper] Failed to check queue '{}': {:?}", queue, e);
                    }
                    _ => {}
                }
            }
        }
    }
}
```

**Deployment options:**
1. **All nodes run reaper** - Safe because Lua script is idempotent
2. **Leader-only** - More efficient, requires leader election (see 3.2)

**Configuration:**
```rust
pub struct ReaperConfig {
    /// How often to scan for orphans (default: 60s)
    pub check_interval: Duration,
    /// Queues to monitor (default: all registered)
    pub queues: Option<Vec<String>>,
}
```

---

#### 1.2 Lock Heartbeat / Extension

**Problem:** Default lock TTL is 30 seconds. Long-running jobs lose their lock.

**Current state:**
- `lock_extend()` exists but never called
- No automatic heartbeat mechanism

**Failure scenario:**
```
Timeline:
0s    - Worker A claims job, lock acquired (TTL=30s)
25s   - Job still processing...
30s   - Lock expires!
31s   - Worker B claims same job (duplicate execution)
35s   - Worker A completes, tries to release lock → fails (not owner)
40s   - Worker B completes → duplicate result
```

**Solution: Automatic Lock Heartbeat**

```rust
impl<M> WorkQueue<M> {
    /// Execute job with automatic lock heartbeat
    pub async fn execute_job(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        
        // Start heartbeat task before execution
        let heartbeat_handle = self.start_lock_heartbeat(&job_id);
        
        // Execute the job
        let result = job.execute().await;
        
        // Stop heartbeat (job completed or failed)
        heartbeat_handle.abort();
        
        // Continue with completion logic...
        self.handle_job_result(job, result).await
    }

    fn start_lock_heartbeat(&self, job_id: &str) -> tokio::task::JoinHandle<()> {
        let backend = self.backend.clone();
        let worker_id = self.worker_id.clone();
        let job_id = job_id.to_string();
        let ttl = self.config.lock_ttl_ms;
        
        // Extend at 1/3 of TTL to ensure safety margin
        let interval_ms = ttl / 3;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
            loop {
                interval.tick().await;
                match backend.lock_extend(&job_id, &worker_id, ttl) {
                    Ok(true) => {
                        log::trace!("[Heartbeat] Extended lock for job {}", job_id);
                    }
                    Ok(false) => {
                        log::error!(
                            "[Heartbeat] Lost lock for job {}! Another worker may process it.",
                            job_id
                        );
                        break;
                    }
                    Err(e) => {
                        log::error!("[Heartbeat] Failed to extend lock for {}: {:?}", job_id, e);
                    }
                }
            }
        })
    }
}
```

**Safe timeline with heartbeat:**
```
Timeline:
0s    - Worker A claims job, lock acquired (TTL=30s)
10s   - Heartbeat extends lock → TTL reset to 30s
20s   - Heartbeat extends lock → TTL reset to 30s
30s   - Heartbeat extends lock → TTL reset to 30s
45s   - Job completes, heartbeat stopped, lock released
```

---

#### 1.3 Graceful Shutdown

**Problem:** Node termination (deploy, scale-down) abandons active jobs.

**Solution: Shutdown Handler**

```rust
pub struct GracefulShutdown {
    timeout: Duration,
}

impl GracefulShutdown {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub async fn shutdown<M>(&self, work_queues: &[ActorRef<WorkQueue<M>>]) 
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    {
        log::info!("[Shutdown] Initiating graceful shutdown...");
        
        // Step 1: Stop accepting new jobs
        for wq in work_queues {
            let _ = wq.tell(StopAccepting).await;
        }
        log::info!("[Shutdown] Stopped accepting new jobs");

        // Step 2: Wait for active jobs to complete (with timeout)
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            let mut total_active = 0;
            for wq in work_queues {
                if let Ok(count) = wq.ask(GetActiveCount).await {
                    total_active += count;
                }
            }
            
            if total_active == 0 {
                log::info!("[Shutdown] All jobs completed");
                break;
            }
            
            if tokio::time::Instant::now() >= deadline {
                log::warn!(
                    "[Shutdown] Timeout reached with {} jobs still active",
                    total_active
                );
                break;
            }
            
            log::info!("[Shutdown] Waiting for {} active jobs...", total_active);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Step 3: Release locks for remaining jobs (reaper will recover them)
        for wq in work_queues {
            let _ = wq.tell(ReleaseAllLocks).await;
        }
        
        log::info!("[Shutdown] Graceful shutdown complete");
    }
}

// Integration with tokio signals
pub async fn run_with_graceful_shutdown<M>(
    work_queues: Vec<ActorRef<WorkQueue<M>>>,
    shutdown_timeout: Duration,
) where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    let shutdown = GracefulShutdown::new(shutdown_timeout);
    
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            log::info!("Received SIGINT");
            shutdown.shutdown(&work_queues).await;
        }
        // Also handle SIGTERM for container environments
    }
}
```

---

### Phase 2: Visibility

These features provide operational insight into the distributed system.

#### 2.1 Worker Registry

**Problem:** No visibility into active workers, their status, or what they're processing.

**Solution: Redis-based Worker Registry**

```rust
/// Redis keys:
/// - aj:workers:{worker_id}           -> WorkerInfo (JSON)
/// - aj:workers:{worker_id}:heartbeat -> "1" with TTL (presence indicator)
/// - aj:workers:index                 -> SET of active worker IDs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInfo {
    pub worker_id: String,
    pub node_name: String,
    pub hostname: String,
    pub started_at: i64,
    pub queues: Vec<String>,
    pub max_jobs: usize,
    pub version: String,
}

pub struct WorkerRegistry {
    backend: Arc<Redis>,
    worker_id: String,
    info: WorkerInfo,
    heartbeat_interval: Duration,
    heartbeat_ttl: Duration,
}

impl WorkerRegistry {
    const WORKERS_PREFIX: &'static str = "aj:workers";
    const WORKERS_INDEX: &'static str = "aj:workers:index";

    pub async fn register(&self) -> Result<(), Error> {
        let info_key = format!("{}:{}", Self::WORKERS_PREFIX, self.worker_id);
        let heartbeat_key = format!("{}:{}:heartbeat", Self::WORKERS_PREFIX, self.worker_id);
        
        // Store worker info
        let info_json = serde_json::to_string(&self.info)?;
        self.backend.set(&info_key, &info_json)?;
        
        // Set heartbeat with TTL
        self.backend.set_ex(&heartbeat_key, "1", self.heartbeat_ttl)?;
        
        // Add to index
        self.backend.sadd(Self::WORKERS_INDEX, &self.worker_id)?;
        
        log::info!("[Registry] Worker {} registered", self.worker_id);
        Ok(())
    }

    pub async fn heartbeat(&self) -> Result<(), Error> {
        let heartbeat_key = format!("{}:{}:heartbeat", Self::WORKERS_PREFIX, self.worker_id);
        self.backend.expire(&heartbeat_key, self.heartbeat_ttl)?;
        Ok(())
    }

    pub async fn deregister(&self) -> Result<(), Error> {
        let info_key = format!("{}:{}", Self::WORKERS_PREFIX, self.worker_id);
        let heartbeat_key = format!("{}:{}:heartbeat", Self::WORKERS_PREFIX, self.worker_id);
        
        self.backend.del(&info_key)?;
        self.backend.del(&heartbeat_key)?;
        self.backend.srem(Self::WORKERS_INDEX, &self.worker_id)?;
        
        log::info!("[Registry] Worker {} deregistered", self.worker_id);
        Ok(())
    }

    pub async fn list_active_workers(&self) -> Result<Vec<WorkerInfo>, Error> {
        let worker_ids: Vec<String> = self.backend.smembers(Self::WORKERS_INDEX)?;
        let mut active_workers = Vec::new();
        
        for worker_id in worker_ids {
            let heartbeat_key = format!("{}:{}:heartbeat", Self::WORKERS_PREFIX, worker_id);
            
            // Only include workers with valid heartbeat
            if self.backend.exists(&heartbeat_key)? {
                let info_key = format!("{}:{}", Self::WORKERS_PREFIX, worker_id);
                if let Some(info_json) = self.backend.get(&info_key)? {
                    if let Ok(info) = serde_json::from_str(&info_json) {
                        active_workers.push(info);
                    }
                }
            } else {
                // Clean up stale entry
                self.backend.srem(Self::WORKERS_INDEX, &worker_id)?;
            }
        }
        
        Ok(active_workers)
    }

    /// Start background heartbeat task
    pub fn start_heartbeat_loop(&self) -> tokio::task::JoinHandle<()> {
        let registry = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(registry.heartbeat_interval);
            loop {
                interval.tick().await;
                if let Err(e) = registry.heartbeat().await {
                    log::error!("[Registry] Heartbeat failed: {:?}", e);
                }
            }
        })
    }
}
```

---

#### 2.2 Metrics & Observability

**Problem:** No visibility into queue depth, processing rates, or failure rates.

**Solution: Metrics Collection**

```rust
use prometheus::{Counter, Gauge, Histogram, Registry};

pub struct QueueMetrics {
    // Counters (monotonically increasing)
    pub jobs_enqueued: Counter,
    pub jobs_completed: Counter,
    pub jobs_failed: Counter,
    pub jobs_retried: Counter,
    pub orphans_recovered: Counter,
    
    // Gauges (point-in-time values)
    pub waiting_queue_size: Gauge,
    pub active_queue_size: Gauge,
    pub delayed_queue_size: Gauge,
    pub active_workers: Gauge,
    
    // Histograms (distributions)
    pub job_duration_seconds: Histogram,
    pub job_wait_seconds: Histogram,
}

impl QueueMetrics {
    pub fn new(registry: &Registry, queue_name: &str) -> Self {
        let labels = &[("queue", queue_name)];
        
        Self {
            jobs_enqueued: Counter::with_opts(
                Opts::new("aj_jobs_enqueued_total", "Total jobs enqueued")
                    .const_labels(labels.iter().cloned().collect())
            ).unwrap(),
            
            jobs_completed: Counter::with_opts(
                Opts::new("aj_jobs_completed_total", "Total jobs completed successfully")
                    .const_labels(labels.iter().cloned().collect())
            ).unwrap(),
            
            // ... more metrics
            
            job_duration_seconds: Histogram::with_opts(
                HistogramOpts::new("aj_job_duration_seconds", "Job execution duration")
                    .const_labels(labels.iter().cloned().collect())
                    .buckets(vec![0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0])
            ).unwrap(),
        }
    }
    
    /// Expose metrics endpoint for Prometheus scraping
    pub fn expose_endpoint(registry: &Registry, port: u16) {
        // HTTP server at /metrics
    }
}
```

**Key metrics to track:**

| Metric | Type | Description |
|--------|------|-------------|
| `aj_jobs_enqueued_total` | Counter | Jobs added to queue |
| `aj_jobs_completed_total` | Counter | Jobs finished successfully |
| `aj_jobs_failed_total` | Counter | Jobs that failed |
| `aj_waiting_queue_size` | Gauge | Jobs waiting to be processed |
| `aj_active_queue_size` | Gauge | Jobs currently processing |
| `aj_job_duration_seconds` | Histogram | Execution time distribution |
| `aj_job_wait_seconds` | Histogram | Time in queue before processing |
| `aj_active_workers` | Gauge | Number of active workers |

---

### Phase 3: Control

These features provide fine-grained control over distributed behavior.

#### 3.1 Global Concurrency Control

**Problem:** No global limit across nodes. 10 nodes × 20 jobs = 200 concurrent jobs could overwhelm downstream services.

**Solution: Queue-Level Semaphore**

```rust
/// Redis keys:
/// - aj:semaphore:{queue}:count -> current active count
/// - aj:semaphore:{queue}:limit -> configured limit

const LUA_SEMAPHORE_ACQUIRE: &str = r#"
    local count_key = KEYS[1]
    local limit = tonumber(ARGV[1])
    
    local current = tonumber(redis.call('GET', count_key) or 0)
    if current < limit then
        redis.call('INCR', count_key)
        return 1
    end
    return 0
"#;

const LUA_SEMAPHORE_RELEASE: &str = r#"
    local count_key = KEYS[1]
    local current = tonumber(redis.call('GET', count_key) or 0)
    if current > 0 then
        redis.call('DECR', count_key)
    end
    return redis.call('GET', count_key)
"#;

pub trait Backend {
    // Add to Backend trait
    fn semaphore_acquire(&self, queue: &str, limit: usize) -> Result<bool, Error>;
    fn semaphore_release(&self, queue: &str) -> Result<(), Error>;
    fn semaphore_count(&self, queue: &str) -> Result<usize, Error>;
}

// Usage in WorkQueue
fn pick_jobs_to_process(&self) -> Result<Vec<Job<M>>, Error> {
    // Check global semaphore before claiming
    if let Some(global_limit) = self.config.global_max_jobs {
        if !self.backend.semaphore_acquire(&self.name, global_limit)? {
            log::debug!("[WorkQueue] Global limit reached, waiting...");
            return Ok(vec![]);
        }
    }
    
    // ... existing claim logic
}
```

---

#### 3.2 Leader Election

**Problem:** Some tasks (orphan reaper, scheduled job mover) should only run on ONE node.

**Solution: Redis-based Leader Election**

```rust
const LUA_TRY_ACQUIRE_LEADERSHIP: &str = r#"
    local key = KEYS[1]
    local worker_id = ARGV[1]
    local ttl = tonumber(ARGV[2])
    
    local current = redis.call('GET', key)
    if current == false or current == worker_id then
        redis.call('SET', key, worker_id, 'PX', ttl)
        return 1
    end
    return 0
"#;

pub struct LeaderElection {
    backend: Arc<Redis>,
    key: String,
    worker_id: String,
    ttl: Duration,
    renew_interval: Duration,
}

impl LeaderElection {
    pub fn new(
        backend: Arc<Redis>,
        election_name: &str,
        worker_id: String,
        ttl: Duration,
    ) -> Self {
        Self {
            backend,
            key: format!("aj:leader:{}", election_name),
            worker_id,
            ttl,
            renew_interval: ttl / 3,
        }
    }

    pub async fn try_become_leader(&self) -> Result<bool, Error> {
        // SET key worker_id NX PX ttl
        self.backend.set_nx_px(&self.key, &self.worker_id, self.ttl)
    }

    pub async fn is_leader(&self) -> Result<bool, Error> {
        match self.backend.get(&self.key)? {
            Some(current) => Ok(current == self.worker_id),
            None => Ok(false),
        }
    }

    pub async fn renew_leadership(&self) -> Result<bool, Error> {
        // Only extend if we're the leader (Lua script)
        self.backend.leader_renew(&self.key, &self.worker_id, self.ttl)
    }

    pub async fn resign(&self) -> Result<(), Error> {
        // DEL key if we're the leader
        if self.is_leader().await? {
            self.backend.del(&self.key)?;
        }
        Ok(())
    }

    /// Run as leader with automatic renewal
    pub async fn run_as_leader<F, Fut>(&self, task: F) -> Result<(), Error>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        // Try to become leader
        if !self.try_become_leader().await? {
            log::info!("[Leader] Another node is leader for {}", self.key);
            return Ok(());
        }
        
        log::info!("[Leader] Became leader for {}", self.key);
        
        // Start renewal loop
        let renewal_handle = self.start_renewal_loop();
        
        // Run the task
        task().await;
        
        // Resign leadership
        renewal_handle.abort();
        self.resign().await?;
        
        Ok(())
    }
}
```

**Usage:**
```rust
let election = LeaderElection::new(backend, "orphan-reaper", worker_id, Duration::from_secs(30));

// Only one node will run the reaper
election.run_as_leader(|| async {
    orphan_reaper.run().await;
}).await;
```

---

#### 3.3 Job Uniqueness / Deduplication

**Problem:** Same job could be enqueued from multiple nodes simultaneously.

**Solution: Atomic Unique Enqueue**

```rust
const LUA_ENQUEUE_UNIQUE: &str = r#"
    local storage_key = KEYS[1]
    local queue_key = KEYS[2]
    local job_id = ARGV[1]
    local job_data = ARGV[2]
    
    -- Check if job already exists
    if redis.call('HEXISTS', storage_key, job_id) == 1 then
        return 0  -- Job already exists, skip
    end
    
    -- Atomically save and enqueue
    redis.call('HSET', storage_key, job_id, job_data)
    redis.call('RPUSH', queue_key, job_id)
    return 1  -- Successfully enqueued
"#;

impl Backend for Redis {
    fn enqueue_unique(
        &self,
        queue: &str,
        job_id: &str,
        job_data: &str,
    ) -> Result<bool, Error> {
        let mut conn = self.client.get_connection()?;
        let storage_key = self.key_storage(queue);
        let waiting_key = self.key_waiting(queue);
        
        let script = Script::new(LUA_ENQUEUE_UNIQUE);
        let result: i32 = script
            .key(&storage_key)
            .key(&waiting_key)
            .arg(job_id)
            .arg(job_data)
            .invoke(&mut conn)?;
        
        Ok(result == 1)
    }
}
```

---

## Configuration

### Multi-Node Configuration

```rust
#[derive(Debug, Clone)]
pub struct DistributedConfig {
    // === Required ===
    
    /// Redis connection URL
    pub redis_url: String,
    
    // === Node Identity ===
    
    /// Unique node identifier (auto-generated UUID if not provided)
    pub node_id: Option<String>,
    
    /// Human-readable node name for debugging
    pub node_name: Option<String>,
    
    // === Lock Settings ===
    
    /// Lock TTL - MUST be longer than max job duration
    /// Default: 5 minutes
    pub lock_ttl: Duration,
    
    /// Heartbeat interval (recommended: lock_ttl / 3)
    /// Default: 100 seconds
    pub heartbeat_interval: Duration,
    
    // === Recovery Settings ===
    
    /// Orphan reaper check interval
    /// Default: 60 seconds
    pub reaper_interval: Duration,
    
    /// Enable orphan reaper on this node
    /// Default: true (all nodes run reaper, Lua script is idempotent)
    pub enable_reaper: bool,
    
    // === Shutdown Settings ===
    
    /// Graceful shutdown timeout
    /// Default: 30 seconds
    pub shutdown_timeout: Duration,
    
    // === Concurrency Settings ===
    
    /// Global max concurrent jobs across all nodes (None = unlimited)
    /// Default: None
    pub global_max_jobs: Option<usize>,
    
    /// Per-node max concurrent jobs
    /// Default: 20
    pub node_max_jobs: usize,
    
    // === Advanced ===
    
    /// Enable leader election for singleton tasks
    /// Default: true
    pub enable_leader_election: bool,
    
    /// Enable worker registry
    /// Default: true
    pub enable_worker_registry: bool,
}

impl Default for DistributedConfig {
    fn default() -> Self {
        Self {
            redis_url: "redis://localhost:6379".into(),
            node_id: None,
            node_name: None,
            lock_ttl: Duration::from_secs(300),           // 5 minutes
            heartbeat_interval: Duration::from_secs(100), // ~1/3 of TTL
            reaper_interval: Duration::from_secs(60),
            enable_reaper: true,
            shutdown_timeout: Duration::from_secs(30),
            global_max_jobs: None,
            node_max_jobs: 20,
            enable_leader_election: true,
            enable_worker_registry: true,
        }
    }
}
```

### Configuration Guidelines

| Setting | Guidance |
|---------|----------|
| `lock_ttl` | Must be > longest job duration. If jobs run 10 min, use 15+ min |
| `heartbeat_interval` | Use `lock_ttl / 3` for safety margin |
| `reaper_interval` | Balance between quick recovery and Redis load |
| `shutdown_timeout` | Long enough for jobs to complete, short enough for deploys |
| `global_max_jobs` | Based on downstream service capacity |

---

## Deployment Guide

### Prerequisites

1. **Redis 6.0+** (for Lua script support)
2. **Network connectivity** between all nodes and Redis
3. **Synchronized clocks** (NTP) for accurate scheduling

### Basic Multi-Node Setup

```rust
use aj::{AJ, DistributedConfig};
use aj::redis::Redis;

#[tokio::main]
async fn main() {
    // Configure for distributed mode
    let config = DistributedConfig {
        redis_url: std::env::var("REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost:6379".into()),
        node_name: Some(std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())),
        lock_ttl: Duration::from_secs(300),
        ..Default::default()
    };
    
    // Start AJ with distributed mode
    AJ::start_distributed(Redis::new(&config.redis_url), config).await;
    
    // Register job types (must be same on all nodes)
    AJ::register::<MyJob>().await;
    
    // Run with graceful shutdown
    AJ::run_until_shutdown().await;
}
```

### Kubernetes Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: aj-workers
spec:
  replicas: 3  # Multiple nodes
  template:
    spec:
      terminationGracePeriodSeconds: 60  # Allow graceful shutdown
      containers:
      - name: aj-worker
        env:
        - name: REDIS_URL
          value: "redis://redis-master:6379"
        - name: HOSTNAME
          valueFrom:
            fieldRef:
              fieldPath: metadata.name
        - name: AJ_LOCK_TTL_SECS
          value: "300"
        - name: AJ_SHUTDOWN_TIMEOUT_SECS
          value: "30"
        lifecycle:
          preStop:
            exec:
              command: ["/bin/sh", "-c", "kill -SIGTERM 1 && sleep 35"]
```

### Health Checks

```rust
// HTTP health endpoint
async fn health_check() -> impl IntoResponse {
    let status = AJ::health_status().await;
    
    if status.is_healthy {
        (StatusCode::OK, Json(status))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(status))
    }
}

#[derive(Serialize)]
struct HealthStatus {
    is_healthy: bool,
    worker_id: String,
    active_jobs: usize,
    redis_connected: bool,
    last_heartbeat: Option<i64>,
}
```

---

## Migration Path

### From Single-Node to Multi-Node

1. **Enable Redis backend** (if using in-memory)
   ```toml
   aj = { version = "0.7.2", features = ["redis"] }
   ```

2. **Make jobs idempotent** - Critical for recovery scenarios
   ```rust
   impl Executable for MyJob {
       async fn execute(&mut self, ctx: &JobContext) -> Self::Output {
           // Use job_id for idempotency key
           if self.already_processed(ctx.job_id).await {
               return Ok(());
           }
           // ... process
       }
   }
   ```

3. **Increase lock TTL** for safety
   ```rust
   AJ::update_work_queue::<MyJob>(WorkQueueConfig {
       lock_ttl_ms: 300_000, // 5 minutes
       ..Default::default()
   }).await;
   ```

4. **Deploy gradually**
   - Start with 2 nodes
   - Monitor for duplicate executions
   - Add more nodes as confidence grows

5. **Enable additional features**
   - Orphan reaper
   - Metrics
   - Worker registry

---

## Summary

### Implementation Priority

| Phase | Feature | Priority | Effort | Risk if Missing |
|-------|---------|----------|--------|-----------------|
| 1 | Orphan Reaper | Critical | Medium | Jobs permanently lost |
| 1 | Lock Heartbeat | Critical | Low | Duplicate execution |
| 1 | Graceful Shutdown | High | Medium | Jobs abandoned on deploy |
| 2 | Worker Registry | Medium | Medium | No visibility |
| 3 | Global Concurrency | Medium | Low | Resource exhaustion |
| 3 | Leader Election | Low | Low | Duplicate singleton tasks |
| 3 | Job Uniqueness | Low | Low | Duplicate jobs |

### Redis Key Schema

```
# Queue data (per queue)
{queue}:waiting           LIST    Jobs ready to process
{queue}:delayed           ZSET    Scheduled jobs (score = run_at_ms)
{queue}:active            LIST    Jobs being processed
{queue}:storage           HASH    Job ID -> Job data (JSON)

# Distributed locking
aj:lock:{job_id}          STRING  Worker ID (with TTL)

# Worker registry (Phase 2)
aj:workers:index          SET     All worker IDs
aj:workers:{id}           STRING  WorkerInfo (JSON)
aj:workers:{id}:heartbeat STRING  "1" with TTL (presence indicator)

# Control (Phase 3)
aj:leader:{name}          STRING  Leader worker ID (with TTL)
aj:semaphore:{queue}      STRING  Current concurrent job count
```

### Current Status

The Redis backend already has the distributed primitives needed:
- Atomic job claiming (Lua scripts)
- Distributed locking
- Orphan detection script

**Main work remaining:** Adding the orchestration layer (reaper loop, heartbeat, shutdown handler) on top of existing primitives.
