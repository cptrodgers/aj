//! In-Memory Backend Implementation
//!
//! Uses in-memory data structures:
//! - VecDeque for waiting and active queues (FIFO)
//! - BTreeMap for delayed queue (sorted by timestamp)
//! - HashMap for job storage

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::types::Backend;
use crate::Error;

type WaitingQueues = Arc<Mutex<HashMap<String, VecDeque<String>>>>;
type DelayedQueues = Arc<Mutex<HashMap<String, BTreeMap<i64, Vec<String>>>>>;
type ActiveQueues = Arc<Mutex<HashMap<String, Vec<String>>>>;
type JobStorage = Arc<Mutex<HashMap<String, HashMap<String, String>>>>;

/// In-memory backend for single-process job queue.
///
/// This backend stores all data in memory and is suitable for:
/// - Development and testing
/// - Single-process applications
/// - Scenarios where job persistence is not required
#[derive(Default, Clone)]
pub struct InMemory {
    /// Waiting queue: jobs ready to be processed (FIFO)
    waiting: WaitingQueues,

    /// Delayed queue: jobs scheduled for future (sorted by timestamp)
    delayed: DelayedQueues,

    /// Active queue: jobs currently being processed
    active: ActiveQueues,

    /// Job storage: job_id -> job_data (JSON)
    storage: JobStorage,
}

impl InMemory {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Backend for InMemory {
    // ========================================================================
    // Waiting Queue (LIST)
    // ========================================================================

    async fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut waiting = self.waiting.lock().unwrap();
        waiting
            .entry(queue.to_string())
            .or_default()
            .push_back(job_id.to_string());
        Ok(())
    }

    async fn waiting_pop(&self, queue: &str) -> Result<Option<String>, Error> {
        let mut waiting = self.waiting.lock().unwrap();
        Ok(waiting.get_mut(queue).and_then(|q| q.pop_front()))
    }

    async fn waiting_len(&self, queue: &str) -> Result<usize, Error> {
        let waiting = self.waiting.lock().unwrap();
        Ok(waiting.get(queue).map(|q| q.len()).unwrap_or(0))
    }

    // ========================================================================
    // Delayed Queue (ZSET equivalent using BTreeMap)
    // ========================================================================

    async fn delayed_push(&self, queue: &str, job_id: &str, run_at_ms: i64) -> Result<(), Error> {
        let mut delayed = self.delayed.lock().unwrap();
        delayed
            .entry(queue.to_string())
            .or_default()
            .entry(run_at_ms)
            .or_default()
            .push(job_id.to_string());
        Ok(())
    }

    async fn delayed_move_ready(&self, queue: &str, now_ms: i64) -> Result<usize, Error> {
        let mut delayed = self.delayed.lock().unwrap();
        let mut waiting = self.waiting.lock().unwrap();

        let delayed_queue = match delayed.get_mut(queue) {
            Some(q) => q,
            None => return Ok(0),
        };

        // Collect all timestamps <= now_ms
        let ready_timestamps: Vec<i64> =
            delayed_queue.range(..=now_ms).map(|(ts, _)| *ts).collect();

        let mut count = 0;
        let waiting_queue = waiting.entry(queue.to_string()).or_default();

        for ts in ready_timestamps {
            if let Some(job_ids) = delayed_queue.remove(&ts) {
                for job_id in job_ids {
                    waiting_queue.push_back(job_id);
                    count += 1;
                }
            }
        }

        Ok(count)
    }

    async fn delayed_remove(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut delayed = self.delayed.lock().unwrap();
        if let Some(delayed_queue) = delayed.get_mut(queue) {
            // Need to search all timestamps to find the job
            let mut empty_timestamps = vec![];
            for (ts, jobs) in delayed_queue.iter_mut() {
                jobs.retain(|id| id != job_id);
                if jobs.is_empty() {
                    empty_timestamps.push(*ts);
                }
            }
            // Clean up empty timestamp entries
            for ts in empty_timestamps {
                delayed_queue.remove(&ts);
            }
        }
        Ok(())
    }

    async fn delayed_len(&self, queue: &str) -> Result<usize, Error> {
        let delayed = self.delayed.lock().unwrap();
        Ok(delayed
            .get(queue)
            .map(|q| q.values().map(|v| v.len()).sum())
            .unwrap_or(0))
    }

    // ========================================================================
    // Active Queue (LIST)
    // ========================================================================

    async fn active_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut active = self.active.lock().unwrap();
        active
            .entry(queue.to_string())
            .or_default()
            .push(job_id.to_string());
        Ok(())
    }

    async fn active_remove(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut active = self.active.lock().unwrap();
        if let Some(jobs) = active.get_mut(queue) {
            jobs.retain(|id| id != job_id);
        }
        Ok(())
    }

    async fn active_len(&self, queue: &str) -> Result<usize, Error> {
        let active = self.active.lock().unwrap();
        Ok(active.get(queue).map(|q| q.len()).unwrap_or(0))
    }

    async fn active_list(&self, queue: &str) -> Result<Vec<String>, Error> {
        let active = self.active.lock().unwrap();
        Ok(active.get(queue).cloned().unwrap_or_default())
    }

    // ========================================================================
    // Job Storage (HASH)
    // ========================================================================

    async fn job_save(&self, queue: &str, job_id: &str, data: &str) -> Result<(), Error> {
        let mut storage = self.storage.lock().unwrap();
        storage
            .entry(queue.to_string())
            .or_default()
            .insert(job_id.to_string(), data.to_string());
        Ok(())
    }

    async fn job_get(&self, queue: &str, job_id: &str) -> Result<Option<String>, Error> {
        let storage = self.storage.lock().unwrap();
        Ok(storage.get(queue).and_then(|h| h.get(job_id)).cloned())
    }

    async fn job_delete(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut storage = self.storage.lock().unwrap();
        if let Some(hash) = storage.get_mut(queue) {
            hash.remove(job_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_waiting_queue() {
        let backend = InMemory::new();
        let queue = "test";

        backend.waiting_push(queue, "job1").await.unwrap();
        backend.waiting_push(queue, "job2").await.unwrap();

        assert_eq!(backend.waiting_len(queue).await.unwrap(), 2);
        assert_eq!(
            backend.waiting_pop(queue).await.unwrap(),
            Some("job1".to_string())
        );
        assert_eq!(
            backend.waiting_pop(queue).await.unwrap(),
            Some("job2".to_string())
        );
        assert_eq!(backend.waiting_pop(queue).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_delayed_queue() {
        let backend = InMemory::new();
        let queue = "test";

        backend.delayed_push(queue, "job1", 1000).await.unwrap();
        backend.delayed_push(queue, "job2", 2000).await.unwrap();
        backend.delayed_push(queue, "job3", 3000).await.unwrap();

        assert_eq!(backend.delayed_len(queue).await.unwrap(), 3);

        // Move ready jobs (job1 and job2)
        let moved = backend.delayed_move_ready(queue, 2500).await.unwrap();
        assert_eq!(moved, 2);

        assert_eq!(backend.delayed_len(queue).await.unwrap(), 1);
        assert_eq!(backend.waiting_len(queue).await.unwrap(), 2);

        // Check order (FIFO)
        assert_eq!(
            backend.waiting_pop(queue).await.unwrap(),
            Some("job1".to_string())
        );
        assert_eq!(
            backend.waiting_pop(queue).await.unwrap(),
            Some("job2".to_string())
        );
    }

    #[tokio::test]
    async fn test_delayed_remove() {
        let backend = InMemory::new();
        let queue = "test";

        backend.delayed_push(queue, "job1", 1000).await.unwrap();
        backend.delayed_push(queue, "job2", 2000).await.unwrap();

        assert_eq!(backend.delayed_len(queue).await.unwrap(), 2);

        backend.delayed_remove(queue, "job1").await.unwrap();
        assert_eq!(backend.delayed_len(queue).await.unwrap(), 1);

        // Move remaining and verify job2 is still there
        backend.delayed_move_ready(queue, 3000).await.unwrap();
        assert_eq!(
            backend.waiting_pop(queue).await.unwrap(),
            Some("job2".to_string())
        );
    }

    #[tokio::test]
    async fn test_active_queue() {
        let backend = InMemory::new();
        let queue = "test";

        backend.active_push(queue, "job1").await.unwrap();
        backend.active_push(queue, "job2").await.unwrap();

        assert_eq!(backend.active_len(queue).await.unwrap(), 2);
        assert_eq!(
            backend.active_list(queue).await.unwrap(),
            vec!["job1".to_string(), "job2".to_string()]
        );

        backend.active_remove(queue, "job1").await.unwrap();
        assert_eq!(backend.active_len(queue).await.unwrap(), 1);
        assert_eq!(
            backend.active_list(queue).await.unwrap(),
            vec!["job2".to_string()]
        );
    }

    #[tokio::test]
    async fn test_job_storage() {
        let backend = InMemory::new();
        let queue = "test";

        backend
            .job_save(queue, "job1", r#"{"data": 1}"#)
            .await
            .unwrap();

        let data = backend.job_get(queue, "job1").await.unwrap();
        assert_eq!(data, Some(r#"{"data": 1}"#.to_string()));

        backend.job_delete(queue, "job1").await.unwrap();
        assert_eq!(backend.job_get(queue, "job1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_claim_job() {
        let backend = InMemory::new();
        let queue = "test";

        backend.waiting_push(queue, "job1").await.unwrap();
        backend.waiting_push(queue, "job2").await.unwrap();

        // Claim job1
        let job = backend.claim_job(queue, "worker1", 30000).await.unwrap();
        assert_eq!(job, Some("job1".to_string()));
        assert_eq!(backend.waiting_len(queue).await.unwrap(), 1);
        assert_eq!(backend.active_len(queue).await.unwrap(), 1);

        // Claim job2
        let job = backend.claim_job(queue, "worker1", 30000).await.unwrap();
        assert_eq!(job, Some("job2".to_string()));
        assert_eq!(backend.waiting_len(queue).await.unwrap(), 0);
        assert_eq!(backend.active_len(queue).await.unwrap(), 2);

        // No more jobs
        let job = backend.claim_job(queue, "worker1", 30000).await.unwrap();
        assert_eq!(job, None);
    }

    #[tokio::test]
    async fn test_complete_and_fail_job() {
        let backend = InMemory::new();
        let queue = "test";

        backend.active_push(queue, "job1").await.unwrap();
        backend.active_push(queue, "job2").await.unwrap();

        backend
            .complete_job(queue, "job1", "worker1")
            .await
            .unwrap();
        assert_eq!(backend.active_len(queue).await.unwrap(), 1);

        backend.fail_job(queue, "job2", "worker1").await.unwrap();
        assert_eq!(backend.active_len(queue).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_full_flow() {
        let backend = InMemory::new();
        let queue = "test";

        // 1. Save job data
        backend
            .job_save(queue, "job1", r#"{"task": "test"}"#)
            .await
            .unwrap();

        // 2. Add to delayed queue (scheduled for future)
        backend.delayed_push(queue, "job1", 1000).await.unwrap();
        assert_eq!(backend.delayed_len(queue).await.unwrap(), 1);

        // 3. Move ready jobs to waiting
        let moved = backend.delayed_move_ready(queue, 2000).await.unwrap();
        assert_eq!(moved, 1);
        assert_eq!(backend.waiting_len(queue).await.unwrap(), 1);

        // 4. Claim job for processing
        let job_id = backend.claim_job(queue, "worker1", 30000).await.unwrap();
        assert_eq!(job_id, Some("job1".to_string()));
        assert_eq!(backend.active_len(queue).await.unwrap(), 1);

        // 5. Get job data
        let data = backend.job_get(queue, "job1").await.unwrap();
        assert_eq!(data, Some(r#"{"task": "test"}"#.to_string()));

        // 6. Complete job
        backend
            .complete_job(queue, "job1", "worker1")
            .await
            .unwrap();
        assert_eq!(backend.active_len(queue).await.unwrap(), 0);
    }
}
