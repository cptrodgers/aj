//! Backend in AJ support both storage and broker
//!
//! # Queue Design
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │                    Queue Architecture                        │
//! │                                                              │
//! │  {queue}:delayed  (Sorted Set)  ← Scheduled jobs            │
//! │                     score = run_at timestamp                 │
//! │                           │                                  │
//! │                           │ move when score <= now           │
//! │                           ▼                                  │
//! │  {queue}:waiting  (List)  ← Ready to process                │
//! │                           │                                  │
//! │                           │ claim (atomic pop + lock)        │
//! │                           ▼                                  │
//! │  {queue}:active   (List)  ← Currently processing            │
//! │                           │                                  │
//! │                           │ complete/fail                    │
//! │                           ▼                                  │
//! │  {queue}:storage  (Hash)  ← Job data (JSON)                 │
//! └──────────────────────────────────────────────────────────────┘
//! ```

#![allow(clippy::borrowed_box)]

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::Error;

/// Backend trait for queue and storage operations.
///
/// Implementations should be thread-safe (Send + Sync).
#[async_trait]
pub trait Backend: Send + Sync {
    // ========================================================================
    // Waiting Queue (LIST) - Jobs ready to be processed
    // ========================================================================

    /// Push a job ID to the waiting queue (adds to back).
    async fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error>;

    /// Pop a job ID from the waiting queue (removes from front).
    /// Returns None if queue is empty.
    async fn waiting_pop(&self, queue: &str) -> Result<Option<String>, Error>;

    /// Get the number of jobs in the waiting queue.
    async fn waiting_len(&self, queue: &str) -> Result<usize, Error>;

    // ========================================================================
    // Delayed Queue (SORTED SET) - Jobs scheduled for future execution
    // ========================================================================

    /// Add a job to the delayed queue with a scheduled execution time.
    /// The job will be moved to waiting queue when `run_at_ms <= now`.
    async fn delayed_push(&self, queue: &str, job_id: &str, run_at_ms: i64) -> Result<(), Error>;

    /// Move all jobs that are ready (run_at_ms <= now_ms) from delayed to waiting.
    /// Returns the number of jobs moved.
    async fn delayed_move_ready(&self, queue: &str, now_ms: i64) -> Result<usize, Error>;

    /// Remove a job from the delayed queue.
    async fn delayed_remove(&self, queue: &str, job_id: &str) -> Result<(), Error>;

    /// Get the number of jobs in the delayed queue.
    async fn delayed_len(&self, queue: &str) -> Result<usize, Error>;

    // ========================================================================
    // Active Queue (LIST) - Jobs currently being processed
    // ========================================================================

    /// Add a job ID to the active queue.
    async fn active_push(&self, queue: &str, job_id: &str) -> Result<(), Error>;

    /// Remove a job ID from the active queue.
    async fn active_remove(&self, queue: &str, job_id: &str) -> Result<(), Error>;

    /// Get the number of jobs in the active queue.
    async fn active_len(&self, queue: &str) -> Result<usize, Error>;

    /// Get all job IDs in the active queue (for reaper/recovery).
    async fn active_list(&self, queue: &str) -> Result<Vec<String>, Error>;

    // ========================================================================
    // Job Storage (HASH) - Stores job data as JSON
    // ========================================================================

    /// Save job data to storage.
    async fn job_save(&self, queue: &str, job_id: &str, data: &str) -> Result<(), Error>;

    /// Get job data from storage.
    async fn job_get(&self, queue: &str, job_id: &str) -> Result<Option<String>, Error>;

    /// Delete job data from storage.
    async fn job_delete(&self, queue: &str, job_id: &str) -> Result<(), Error>;

    // ========================================================================
    // Distributed Locking (Optional - for multi-worker setups)
    // ========================================================================

    /// Acquire a lock on a job. Returns true if acquired, false if already locked.
    /// Default implementation always succeeds (for single-process backends).
    async fn lock_acquire(
        &self,
        _job_id: &str,
        _worker_id: &str,
        _ttl_ms: u64,
    ) -> Result<bool, Error> {
        Ok(true)
    }

    /// Release a lock on a job. Returns true if released, false if not owner.
    async fn lock_release(&self, _job_id: &str, _worker_id: &str) -> Result<bool, Error> {
        Ok(true)
    }

    /// Extend a lock's TTL (heartbeat). Returns true if extended, false if lock lost.
    async fn lock_extend(
        &self,
        _job_id: &str,
        _worker_id: &str,
        _ttl_ms: u64,
    ) -> Result<bool, Error> {
        Ok(true)
    }

    // ========================================================================
    // Atomic Operations (Combines multiple operations atomically)
    // ========================================================================

    /// Atomically claim a job: pop from waiting, acquire lock, push to active.
    /// Returns the job ID if successful, None if no jobs available.
    ///
    /// Default implementation is NOT atomic (suitable for single-process only).
    /// Redis backend should use Lua script for atomicity.
    async fn claim_job(
        &self,
        queue: &str,
        worker_id: &str,
        lock_ttl_ms: u64,
    ) -> Result<Option<String>, Error> {
        // Default: simple pop + push (not atomic, but works for single process)
        if let Some(job_id) = self.waiting_pop(queue).await? {
            if self.lock_acquire(&job_id, worker_id, lock_ttl_ms).await? {
                self.active_push(queue, &job_id).await?;
                return Ok(Some(job_id));
            }
        }
        Ok(None)
    }

    /// Atomically complete a job: remove from active, release lock.
    /// Returns true if completed, false if job wasn't in active queue.
    async fn complete_job(
        &self,
        queue: &str,
        job_id: &str,
        worker_id: &str,
    ) -> Result<bool, Error> {
        self.active_remove(queue, job_id).await?;
        self.lock_release(job_id, worker_id).await?;
        Ok(true)
    }

    /// Atomically fail a job: remove from active, release lock.
    async fn fail_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        self.active_remove(queue, job_id).await?;
        self.lock_release(job_id, worker_id).await?;
        Ok(true)
    }

    /// Requeue orphaned jobs (jobs in active queue without valid locks).
    /// Returns list of job IDs that were requeued.
    /// Default implementation does nothing (single-process doesn't need this).
    async fn requeue_orphaned(&self, _queue: &str) -> Result<Vec<String>, Error> {
        Ok(vec![])
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Helper to serialize and save a job to storage.
pub async fn save_job<T: Serialize + Sync>(
    backend: &dyn Backend,
    queue: &str,
    job_id: &str,
    job: &T,
) -> Result<(), Error> {
    let data = serde_json::to_string(job).map_err(|e| {
        warn!("[Storage] Serialize failed for {}: {:?}", job_id, e);
        Error::SerializeError
    })?;
    backend.job_save(queue, job_id, &data).await
}

/// Helper to load a job from storage.
pub async fn load_job<T: DeserializeOwned>(
    backend: &dyn Backend,
    queue: &str,
    job_id: &str,
) -> Result<Option<T>, Error> {
    match backend.job_get(queue, job_id).await? {
        Some(data) => {
            let job = serde_json::from_str(&data).ok();
            Ok(job)
        }
        None => Ok(None),
    }
}
