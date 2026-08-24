use kameo::actor::ActorRef;
use kameo::message::{Context, Message};
use kameo::Actor;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::job::{Job, JobStatus, JobType};
use crate::types::{load_job, save_job, Backend};
use crate::util::get_now_as_ms;
use crate::{Error, Executable};

const DEFAULT_TICK_DURATION: Duration = Duration::from_millis(100);
const MAX_PROCESSING_JOBS: usize = 20;
const DEFAULT_LOCK_TTL_MS: u64 = 30000; // 30 seconds

#[derive(Debug, Clone)]
pub struct EnqueueConfig {
    // Will re run job if job is completed
    pub re_run: bool,
    // Will override data of current job if job is completed
    pub override_data: bool,
}

impl EnqueueConfig {
    pub fn new(re_run: bool, override_data: bool) -> Self {
        Self {
            re_run,
            override_data,
        }
    }

    pub fn new_re_run() -> Self {
        Self::new(true, true)
    }

    pub fn new_skip_if_finished() -> Self {
        Self::new(false, true)
    }
}

#[derive(Debug, Clone)]
pub struct WorkQueueConfig {
    pub process_tick_duration: Duration,
    pub max_processing_jobs: usize,
    pub lock_ttl_ms: u64,
}

impl WorkQueueConfig {
    pub fn init() -> Self {
        Self {
            max_processing_jobs: MAX_PROCESSING_JOBS,
            process_tick_duration: DEFAULT_TICK_DURATION,
            lock_ttl_ms: DEFAULT_LOCK_TTL_MS,
        }
    }
}

impl Default for WorkQueueConfig {
    fn default() -> Self {
        Self::init()
    }
}

#[derive(Actor)]
pub struct WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    name: Arc<String>,
    worker_id: String,
    config: WorkQueueConfig,
    _type: PhantomData<M>,
    backend: Arc<dyn Backend>,
}

impl<M> Clone for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            worker_id: self.worker_id.clone(),
            config: self.config.clone(),
            _type: PhantomData,
            backend: self.backend.clone(),
        }
    }
}

impl<M> WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    pub fn new(job_name: String, backend: Arc<dyn Backend>) -> Self {
        Self {
            name: Arc::new(job_name),
            worker_id: Uuid::new_v4().to_string(),
            config: WorkQueueConfig::default(),
            _type: PhantomData,
            backend,
        }
    }

    /// Get the queue name (used as prefix for all queue keys)
    pub fn queue_name(&self) -> &str {
        &self.name
    }

    pub fn start_with_name(
        name: String,
        backend: Arc<dyn Backend + Sync + Send>,
    ) -> ActorRef<Self> {
        let queue = WorkQueue::<M>::new(name, backend);
        let actor_ref = kameo::spawn(queue);

        // Start the processing loop in a separate task
        let actor_ref_clone = actor_ref.clone();
        tokio::spawn(async move {
            Self::processing_loop(actor_ref_clone).await;
        });

        actor_ref
    }

    /// Background processing loop that sends ProcessTick messages periodically
    async fn processing_loop(actor_ref: ActorRef<Self>) {
        let mut interval = tokio::time::interval(DEFAULT_TICK_DURATION);
        loop {
            interval.tick().await;
            // Send ProcessTick message to self
            if actor_ref.tell(ProcessTick).await.is_err() {
                // Actor stopped, exit the loop
                break;
            }
        }
    }

    // ========================================================================
    // Job Lifecycle Operations
    // ========================================================================

    pub async fn run_with_config(&self, job: Job<M>, config: EnqueueConfig) -> Result<(), Error> {
        let job_id = job.id();
        let existing_job = load_job::<Job<M>>(self.backend.as_ref(), &self.name, job_id).await?;

        if let Some(existing_job) = existing_job {
            if config.override_data && !existing_job.is_running() {
                log::info!(
                    "[WorkQueue] Update existing job with new job data: {}",
                    job.id()
                );
                save_job(self.backend.as_ref(), &self.name, job_id, &job).await?;
            } else {
                log::info!(
                    "[WorkQueue] Job is running, skip update job data: {}",
                    job.id()
                );
            }

            if config.re_run && existing_job.is_done() {
                log::info!("[WorkQueue] Re run job {}", existing_job.id());
                self.enqueue(job).await?;
            }

            return Ok(());
        }

        self.enqueue(job).await
    }

    pub async fn enqueue(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        log::info!("[WorkQueue] New Job {}", job_id);

        // Update job status
        job.context.job_status = JobStatus::Queued;
        job.context.enqueue_at = Some(get_now_as_ms());

        // Save job data
        save_job(self.backend.as_ref(), &self.name, &job_id, &job).await?;

        // Determine which queue to add to based on job type
        match &job.context.job_type {
            JobType::Normal => {
                // Immediate job -> waiting queue
                self.backend.waiting_push(&self.name, &job_id).await?;
            }
            JobType::ScheduledAt(schedule_at) => {
                // Scheduled job -> delayed queue
                let run_at_ms = schedule_at.timestamp_millis();
                self.backend
                    .delayed_push(&self.name, &job_id, run_at_ms)
                    .await?;
            }
            JobType::Cron(_, next_slot, _, _) => {
                // Cron job -> delayed queue with next slot time
                let run_at_ms = next_slot.timestamp_millis();
                self.backend
                    .delayed_push(&self.name, &job_id, run_at_ms)
                    .await?;
            }
        }

        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Queued);
        Ok(())
    }

    pub async fn re_enqueue(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        log::debug!("[WorkQueue] Re-run job {}", job_id);

        // Remove from active queue
        self.backend.active_remove(&self.name, &job_id).await?;
        self.backend.lock_release(&job_id, &self.worker_id).await?;

        // Update job status
        job.context.job_status = JobStatus::Queued;
        save_job(self.backend.as_ref(), &self.name, &job_id, &job).await?;

        // Add to appropriate queue based on job type
        match &job.context.job_type {
            JobType::Normal => {
                self.backend.waiting_push(&self.name, &job_id).await?;
            }
            JobType::ScheduledAt(schedule_at) => {
                let run_at_ms = schedule_at.timestamp_millis();
                self.backend
                    .delayed_push(&self.name, &job_id, run_at_ms)
                    .await?;
            }
            JobType::Cron(_, next_slot, _, _) => {
                let run_at_ms = next_slot.timestamp_millis();
                self.backend
                    .delayed_push(&self.name, &job_id, run_at_ms)
                    .await?;
            }
        }

        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Queued);
        Ok(())
    }

    pub async fn mark_job_is_canceled(&self, job_id: &str) {
        log::info!("Cancel job {}", job_id);
        // Remove from active and release lock
        if let Err(e) = self.backend.active_remove(&self.name, job_id).await {
            log::error!("[WorkQueue] Cannot remove from active {}: {:?}", job_id, e);
        }
        if let Err(e) = self.backend.lock_release(job_id, &self.worker_id).await {
            log::error!("[WorkQueue] Cannot release lock {}: {:?}", job_id, e);
        }
    }

    pub async fn mark_job_is_finished(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        log::info!("Finish job {}", job_id);

        // Update job status
        job.context.job_status = JobStatus::Finished;
        job.context.complete_at = Some(get_now_as_ms());
        save_job(self.backend.as_ref(), &self.name, &job_id, &job).await?;

        // Remove from active and release lock
        self.backend
            .complete_job(&self.name, &job_id, &self.worker_id)
            .await?;

        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Finished);
        Ok(())
    }

    pub async fn mark_job_is_failed(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        log::info!("Failed job {}", job_id);

        // Update job status
        job.context.job_status = JobStatus::Failed;
        job.context.complete_at = Some(get_now_as_ms());
        save_job(self.backend.as_ref(), &self.name, &job_id, &job).await?;

        // Remove from active and release lock
        self.backend
            .fail_job(&self.name, &job_id, &self.worker_id)
            .await?;

        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Failed);
        Ok(())
    }

    // ========================================================================
    // Job Processing
    // ========================================================================

    pub async fn process_jobs(&self) {
        // First, move ready delayed jobs to waiting queue
        let now_ms = get_now_as_ms();
        if let Err(e) = self.backend.delayed_move_ready(&self.name, now_ms).await {
            log::error!("[WorkQueue] Failed to move delayed jobs: {:?}", e);
        }

        // Then pick and process jobs
        match self.pick_jobs_to_process().await {
            Ok(jobs) => {
                for job in jobs {
                    self.execute_job_task(job);
                }
            }
            Err(err) => {
                log::error!("[WorkQueue]: Cannot pick jobs to process {err:?}");
            }
        }
    }

    pub async fn pick_jobs_to_process(&self) -> Result<Vec<Job<M>>, Error> {
        let active_count = self.backend.active_len(&self.name).await.unwrap_or(0);
        if active_count >= self.config.max_processing_jobs {
            return Ok(vec![]);
        }

        let slots_available = self.config.max_processing_jobs - active_count;
        let mut ready_jobs = Vec::new();

        for _ in 0..slots_available {
            // Atomically claim a job from waiting queue
            match self
                .backend
                .claim_job(&self.name, &self.worker_id, self.config.lock_ttl_ms)
                .await?
            {
                Some(job_id) => {
                    // Load job data
                    if let Some(mut job) =
                        load_job::<Job<M>>(self.backend.as_ref(), &self.name, &job_id).await?
                    {
                        // Update job status to running
                        job.context.job_status = JobStatus::Running;
                        job.context.run_at = Some(get_now_as_ms());
                        save_job(self.backend.as_ref(), &self.name, &job_id, &job).await?;

                        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Running);
                        ready_jobs.push(job);
                    } else {
                        // Job data not found, remove from active
                        log::warn!("[WorkQueue] Job data not found for {}, removing", job_id);
                        self.backend.active_remove(&self.name, &job_id).await?;
                        self.backend.lock_release(&job_id, &self.worker_id).await?;
                    }
                }
                None => {
                    // No more jobs available
                    break;
                }
            }
        }

        Ok(ready_jobs)
    }

    pub fn execute_job_task(&self, job: Job<M>) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(err) = this.execute_job(job.clone()).await {
                log::error!("[WorkQueue] Execute job {} fail: {:?}", job.id(), err);
                let _ = this.mark_job_is_failed(job).await;
            }
        });
    }

    pub async fn execute_job(&self, mut job: Job<M>) -> Result<(), Error> {
        // If job is cancelled, handle it
        if job.is_cancelled() {
            self.mark_job_is_canceled(job.id()).await;
            return Ok(());
        }

        let job_output = job.execute().await;
        let is_failed_output = job.data.is_failed_output(&job_output).await;

        log::info!(
            "[WorkQueue] Execution complete. Job {} - Result: {job_output:?}",
            job.id()
        );

        // Check for retry
        if let Some(retry_context) = job.context.retry.as_mut() {
            if let Some(next_retry_at) = job.data.retry_at(retry_context, job_output).await {
                log::info!("[WorkQueue] Retry this job. {}", job.id());
                job.context.job_type = JobType::ScheduledAt(next_retry_at);
                return self.re_enqueue(job).await;
            }
        }

        // If this is interval job (has next tick) -> re_enqueue it
        if let Some(next_job) = job.next_tick() {
            return self.re_enqueue(next_job).await;
        }

        if is_failed_output {
            self.mark_job_is_failed(job).await
        } else {
            self.mark_job_is_finished(job).await
        }
    }

    // ========================================================================
    // Job Management
    // ========================================================================

    pub async fn cancel_job(&self, job_id: &str) -> Result<(), Error> {
        if let Some(mut job) = load_job::<Job<M>>(self.backend.as_ref(), &self.name, job_id).await?
        {
            // Only cancel queued job
            if job.is_queued() {
                job.context.job_status = JobStatus::Canceled;
                job.context.cancel_at = Some(get_now_as_ms());
                save_job(self.backend.as_ref(), &self.name, job_id, &job).await?;

                // Remove from waiting or delayed queue
                self.backend.waiting_pop(&self.name).await?; // Try waiting
                self.backend.delayed_remove(&self.name, job_id).await?; // Try delayed

                crate::PluginCenter::change_status::<M>(job_id.to_string(), JobStatus::Canceled);
            } else {
                log::warn!("[WorkQueue] Cannot cancel {:?} job", job.context.job_status);
            }
        }

        Ok(())
    }

    pub async fn get_job(&self, job_id: &str) -> Result<Option<Job<M>>, Error> {
        load_job(self.backend.as_ref(), &self.name, job_id).await
    }

    pub async fn retry_job(&self, job_id: &str) -> Result<bool, Error> {
        let job = load_job::<Job<M>>(self.backend.as_ref(), &self.name, job_id).await?;

        if let Some(job) = job {
            // Only allow retry done job (cancelled, failed, finished)
            if job.is_done() {
                self.re_enqueue(job).await?;
                return Ok(true);
            } else {
                log::debug!(
                    "[WorkQueue] Cannot retry job {} in status {:?}",
                    job_id,
                    job.context.job_status
                );
                return Ok(false);
            }
        }

        log::debug!("[WorkQueue] Don't found job {} to retry", job_id);
        Ok(false)
    }

    pub async fn read_job(&self, job_id: &str) -> Result<Option<Job<M>>, Error> {
        load_job(self.backend.as_ref(), &self.name, job_id).await
    }

    pub async fn get_processing_job_ids(&self, _count: usize) -> Result<Vec<String>, Error> {
        self.backend.active_list(&self.name).await
    }
}

// ============================================================================
// Kameo Message Handlers
// ============================================================================

// Message: ProcessTick (internal message for periodic processing)
pub struct ProcessTick;

impl<M> Message<ProcessTick> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: ProcessTick,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.process_jobs().await;
    }
}

// Message: Enqueue
#[derive(Debug)]
pub struct Enqueue<M: Executable + Clone + Send + Sync + 'static>(pub Job<M>, pub EnqueueConfig);

impl<M> Message<Enqueue<M>> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    type Reply = Result<(), Error>;

    async fn handle(
        &mut self,
        msg: Enqueue<M>,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.run_with_config(msg.0, msg.1).await
    }
}

pub async fn enqueue_job<M>(
    actor_ref: ActorRef<WorkQueue<M>>,
    job: Job<M>,
    config: EnqueueConfig,
) -> Result<(), Error>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    actor_ref
        .ask(Enqueue(job, config))
        .await
        .map_err(Error::from)
}

// Message: CancelJob
#[derive(Debug)]
pub struct CancelJob {
    pub job_id: String,
}

impl<M> Message<CancelJob> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    type Reply = Result<(), Error>;

    async fn handle(
        &mut self,
        msg: CancelJob,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.cancel_job(&msg.job_id).await
    }
}

pub async fn cancel_job<M>(actor_ref: ActorRef<WorkQueue<M>>, job_id: String) -> Result<(), Error>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    actor_ref
        .ask(CancelJob { job_id })
        .await
        .map_err(Error::from)
}

// Message: GetJob
#[derive(Debug)]
pub struct GetJob<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    pub job_id: String,
    _phantom: PhantomData<M>,
}

impl<M> Message<GetJob<M>> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    type Reply = Option<Job<M>>;

    async fn handle(
        &mut self,
        msg: GetJob<M>,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.get_job(&msg.job_id).await.ok().flatten()
    }
}

pub async fn get_job<M>(actor_ref: ActorRef<WorkQueue<M>>, job_id: &str) -> Option<Job<M>>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    let msg: GetJob<M> = GetJob {
        job_id: job_id.to_string(),
        _phantom: PhantomData,
    };
    actor_ref.ask(msg).await.ok().flatten()
}

// Message: RetryJob
#[derive(Debug)]
pub struct RetryJob<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    pub job_id: String,
    _phantom: PhantomData<M>,
}

impl<M> Message<RetryJob<M>> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    type Reply = Result<bool, Error>;

    async fn handle(
        &mut self,
        msg: RetryJob<M>,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.retry_job(&msg.job_id).await
    }
}

pub async fn retry_job<M>(actor_ref: ActorRef<WorkQueue<M>>, job_id: &str) -> Result<bool, Error>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    let msg: RetryJob<M> = RetryJob {
        job_id: job_id.to_string(),
        _phantom: PhantomData,
    };
    actor_ref.ask(msg).await.map_err(Error::from)
}

// Message: UpdateWorkQueue
pub struct UpdateWorkQueue {
    pub config: WorkQueueConfig,
}

impl<M> Message<UpdateWorkQueue> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    type Reply = ();

    async fn handle(
        &mut self,
        msg: UpdateWorkQueue,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.config = msg.config;
    }
}

pub async fn update_work_queue_config<M>(
    actor_ref: ActorRef<WorkQueue<M>>,
    config: WorkQueueConfig,
) -> Result<(), Error>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    let msg = UpdateWorkQueue { config };
    actor_ref
        .ask(msg)
        .await
        .map_err(|e| Error::ActorError(format!("{:?}", e)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde::Deserialize;

    use super::*;
    use crate::backend::mem::InMemory;
    use crate::job::JobContext;
    use crate::util::get_now;

    #[derive(Default, Debug, Clone, Serialize, Deserialize)]
    struct TestJob {
        number: i32,
    }

    #[async_trait::async_trait]
    impl Executable for TestJob {
        type Output = i32;

        async fn execute(&mut self, _context: &JobContext) -> Self::Output {
            self.number
        }
    }

    /// A queue wired to a private in-memory backend, so tests never share state.
    fn queue() -> WorkQueue<TestJob> {
        WorkQueue::new(
            format!("test:{}", Uuid::new_v4()),
            Arc::new(InMemory::default()),
        )
    }

    fn job(number: i32) -> Job<TestJob> {
        Job::new(TestJob { number })
    }

    async fn status(q: &WorkQueue<TestJob>, job_id: &str) -> JobStatus {
        q.get_job(job_id).await.unwrap().unwrap().context.job_status
    }

    // ========================================================================
    // Enqueue routing
    // ========================================================================

    #[tokio::test]
    async fn test_enqueue_normal_job_lands_in_waiting() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();

        q.enqueue(j).await.unwrap();

        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 1);
        assert_eq!(q.backend.delayed_len(&q.name).await.unwrap(), 0);
        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 0);
        assert_eq!(status(&q, &id).await, JobStatus::Queued);
    }

    #[tokio::test]
    async fn test_enqueue_scheduled_job_lands_in_delayed() {
        let q = queue();
        let j = job(1).schedule_at(get_now() + Duration::from_secs(60));
        let id = j.id().to_string();

        q.enqueue(j).await.unwrap();

        assert_eq!(q.backend.delayed_len(&q.name).await.unwrap(), 1);
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 0);
        assert_eq!(status(&q, &id).await, JobStatus::Queued);
    }

    /// `process_jobs` moves due jobs out of the delayed queue before picking. Only the
    /// delayed side is asserted: picking spawns execution, so anything downstream is racy.
    #[tokio::test]
    async fn test_process_jobs_moves_due_delayed_job() {
        let q = queue();
        let j = job(1).schedule_at(get_now() - Duration::from_secs(1));
        q.enqueue(j).await.unwrap();
        assert_eq!(q.backend.delayed_len(&q.name).await.unwrap(), 1);

        q.process_jobs().await;

        assert_eq!(q.backend.delayed_len(&q.name).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_not_due_delayed_job_is_left_alone() {
        let q = queue();
        q.enqueue(job(1).schedule_at(get_now() + Duration::from_secs(300)))
            .await
            .unwrap();

        q.process_jobs().await;

        assert_eq!(q.backend.delayed_len(&q.name).await.unwrap(), 1);
        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 0);
    }

    // ========================================================================
    // Claim
    // ========================================================================

    #[tokio::test]
    async fn test_pick_jobs_claims_and_marks_running() {
        let q = queue();
        let j = job(7);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();

        let picked = q.pick_jobs_to_process().await.unwrap();

        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].id(), id);
        assert_eq!(picked[0].data.number, 7);
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 0);
        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 1);
        assert_eq!(status(&q, &id).await, JobStatus::Running);
    }

    #[tokio::test]
    async fn test_pick_jobs_on_empty_queue() {
        let q = queue();
        assert!(q.pick_jobs_to_process().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_pick_jobs_respects_max_processing_jobs() {
        let mut q = queue();
        q.config.max_processing_jobs = 2;
        for n in 0..5 {
            q.enqueue(job(n)).await.unwrap();
        }

        let picked = q.pick_jobs_to_process().await.unwrap();

        assert_eq!(picked.len(), 2);
        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 2);
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn test_pick_jobs_is_fifo() {
        let q = queue();
        let mut ids = Vec::new();
        for n in 0..3 {
            let j = job(n);
            ids.push(j.id().to_string());
            q.enqueue(j).await.unwrap();
        }

        let picked = q.pick_jobs_to_process().await.unwrap();

        let picked_ids: Vec<String> = picked.iter().map(|j| j.id().to_string()).collect();
        assert_eq!(picked_ids, ids);
    }

    // ========================================================================
    // Terminal transitions
    // ========================================================================

    #[tokio::test]
    async fn test_mark_job_is_finished() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();
        let picked = q.pick_jobs_to_process().await.unwrap();

        q.mark_job_is_finished(picked[0].clone()).await.unwrap();

        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 0);
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 0);
        assert_eq!(status(&q, &id).await, JobStatus::Finished);
        assert!(q
            .get_job(&id)
            .await
            .unwrap()
            .unwrap()
            .context
            .complete_at
            .is_some());
    }

    /// Guards the path behind `execute_job_task`'s `let _ = this.mark_job_is_failed(job);`.
    /// If that call is ever dropped rather than driven, the job silently stays Running.
    #[tokio::test]
    async fn test_mark_job_is_failed() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();
        let picked = q.pick_jobs_to_process().await.unwrap();
        assert_eq!(status(&q, &id).await, JobStatus::Running);

        q.mark_job_is_failed(picked[0].clone()).await.unwrap();

        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 0);
        assert_eq!(status(&q, &id).await, JobStatus::Failed);
    }

    #[tokio::test]
    async fn test_mark_job_is_canceled_clears_active() {
        let q = queue();
        q.enqueue(job(1)).await.unwrap();
        let picked = q.pick_jobs_to_process().await.unwrap();
        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 1);

        q.mark_job_is_canceled(picked[0].id()).await;

        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 0);
    }

    // ========================================================================
    // Re-enqueue / retry
    // ========================================================================

    #[tokio::test]
    async fn test_re_enqueue_returns_job_to_waiting() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();
        let picked = q.pick_jobs_to_process().await.unwrap();

        q.re_enqueue(picked[0].clone()).await.unwrap();

        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 0);
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 1);
        assert_eq!(status(&q, &id).await, JobStatus::Queued);
    }

    #[tokio::test]
    async fn test_re_enqueue_scheduled_job_goes_back_to_delayed() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();
        let mut picked = q.pick_jobs_to_process().await.unwrap().remove(0);
        picked.context.job_type = JobType::ScheduledAt(get_now() + Duration::from_secs(60));

        q.re_enqueue(picked).await.unwrap();

        assert_eq!(q.backend.delayed_len(&q.name).await.unwrap(), 1);
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 0);
        assert_eq!(status(&q, &id).await, JobStatus::Queued);
    }

    #[tokio::test]
    async fn test_retry_job_only_when_done() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();

        // Queued, not done -> refused.
        assert!(!q.retry_job(&id).await.unwrap());

        let picked = q.pick_jobs_to_process().await.unwrap();
        q.mark_job_is_failed(picked[0].clone()).await.unwrap();

        // Failed counts as done -> re-queued.
        assert!(q.retry_job(&id).await.unwrap());
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 1);
        assert_eq!(status(&q, &id).await, JobStatus::Queued);
    }

    #[tokio::test]
    async fn test_retry_unknown_job_is_false() {
        let q = queue();
        assert!(!q.retry_job("nope").await.unwrap());
    }

    // ========================================================================
    // run_with_config
    // ========================================================================

    #[tokio::test]
    async fn test_run_with_config_skips_requeue_of_finished_job() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j.clone()).await.unwrap();
        let picked = q.pick_jobs_to_process().await.unwrap();
        q.mark_job_is_finished(picked[0].clone()).await.unwrap();

        q.run_with_config(j, EnqueueConfig::new_skip_if_finished())
            .await
            .unwrap();

        // re_run = false, so it is not put back on the queue.
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 0);
        // But skip_if_finished still carries override_data = true, so the stored copy is
        // replaced by the incoming job and its status resets to Queued.
        assert_eq!(status(&q, &id).await, JobStatus::Queued);
    }

    #[tokio::test]
    async fn test_run_with_config_re_runs_finished_job() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j.clone()).await.unwrap();
        let picked = q.pick_jobs_to_process().await.unwrap();
        q.mark_job_is_finished(picked[0].clone()).await.unwrap();

        q.run_with_config(j, EnqueueConfig::new_re_run())
            .await
            .unwrap();

        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 1);
        assert_eq!(status(&q, &id).await, JobStatus::Queued);
    }

    #[tokio::test]
    async fn test_run_with_config_enqueues_unknown_job() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();

        q.run_with_config(j, EnqueueConfig::new_re_run())
            .await
            .unwrap();

        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 1);
        assert_eq!(status(&q, &id).await, JobStatus::Queued);
    }

    // ========================================================================
    // Reads and cancel
    // ========================================================================

    #[tokio::test]
    async fn test_get_and_read_job() {
        let q = queue();
        let j = job(42);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();

        assert_eq!(q.get_job(&id).await.unwrap().unwrap().data.number, 42);
        assert_eq!(q.read_job(&id).await.unwrap().unwrap().data.number, 42);
        assert!(q.get_job("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_processing_job_ids() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();
        assert!(q.get_processing_job_ids(10).await.unwrap().is_empty());

        q.pick_jobs_to_process().await.unwrap();

        assert_eq!(q.get_processing_job_ids(10).await.unwrap(), vec![id]);
    }

    #[tokio::test]
    async fn test_cancel_queued_job() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();

        q.cancel_job(&id).await.unwrap();

        assert_eq!(status(&q, &id).await, JobStatus::Canceled);
        assert_eq!(q.backend.waiting_len(&q.name).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_cancel_running_job_is_refused() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();
        q.pick_jobs_to_process().await.unwrap();

        q.cancel_job(&id).await.unwrap();

        // Only queued jobs may be cancelled.
        assert_eq!(status(&q, &id).await, JobStatus::Running);
        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 1);
    }

    // ========================================================================
    // End to end through the real execution path
    // ========================================================================

    #[tokio::test]
    async fn test_execute_job_finishes_normal_job() {
        let q = queue();
        let j = job(5);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();
        let picked = q.pick_jobs_to_process().await.unwrap();

        q.execute_job(picked[0].clone()).await.unwrap();

        assert_eq!(status(&q, &id).await, JobStatus::Finished);
        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_execute_job_cancelled_job_short_circuits() {
        let q = queue();
        let j = job(1);
        let id = j.id().to_string();
        q.enqueue(j).await.unwrap();
        let mut picked = q.pick_jobs_to_process().await.unwrap().remove(0);
        picked.context.job_status = JobStatus::Canceled;

        q.execute_job(picked).await.unwrap();

        assert_eq!(q.backend.active_len(&q.name).await.unwrap(), 0);
        // The stored copy still reads Running: execute_job only clears the queues.
        assert_eq!(status(&q, &id).await, JobStatus::Running);
    }
}
