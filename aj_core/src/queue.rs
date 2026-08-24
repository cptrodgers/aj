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

    pub fn run_with_config(&self, job: Job<M>, config: EnqueueConfig) -> Result<(), Error> {
        let job_id = job.id();
        let existing_job = load_job::<Job<M>>(self.backend.as_ref(), &self.name, job_id)?;

        if let Some(existing_job) = existing_job {
            if config.override_data && !existing_job.is_running() {
                log::info!(
                    "[WorkQueue] Update existing job with new job data: {}",
                    job.id()
                );
                save_job(self.backend.as_ref(), &self.name, job_id, &job)?;
            } else {
                log::info!(
                    "[WorkQueue] Job is running, skip update job data: {}",
                    job.id()
                );
            }

            if config.re_run && existing_job.is_done() {
                log::info!("[WorkQueue] Re run job {}", existing_job.id());
                self.enqueue(job)?;
            }

            return Ok(());
        }

        self.enqueue(job)
    }

    pub fn enqueue(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        log::info!("[WorkQueue] New Job {}", job_id);

        // Update job status
        job.context.job_status = JobStatus::Queued;
        job.context.enqueue_at = Some(get_now_as_ms());

        // Save job data
        save_job(self.backend.as_ref(), &self.name, &job_id, &job)?;

        // Determine which queue to add to based on job type
        match &job.context.job_type {
            JobType::Normal => {
                // Immediate job -> waiting queue
                self.backend.waiting_push(&self.name, &job_id)?;
            }
            JobType::ScheduledAt(schedule_at) => {
                // Scheduled job -> delayed queue
                let run_at_ms = schedule_at.timestamp_millis();
                self.backend.delayed_push(&self.name, &job_id, run_at_ms)?;
            }
            JobType::Cron(_, next_slot, _, _) => {
                // Cron job -> delayed queue with next slot time
                let run_at_ms = next_slot.timestamp_millis();
                self.backend.delayed_push(&self.name, &job_id, run_at_ms)?;
            }
        }

        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Queued);
        Ok(())
    }

    pub fn re_enqueue(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        log::debug!("[WorkQueue] Re-run job {}", job_id);

        // Remove from active queue
        self.backend.active_remove(&self.name, &job_id)?;
        self.backend.lock_release(&job_id, &self.worker_id)?;

        // Update job status
        job.context.job_status = JobStatus::Queued;
        save_job(self.backend.as_ref(), &self.name, &job_id, &job)?;

        // Add to appropriate queue based on job type
        match &job.context.job_type {
            JobType::Normal => {
                self.backend.waiting_push(&self.name, &job_id)?;
            }
            JobType::ScheduledAt(schedule_at) => {
                let run_at_ms = schedule_at.timestamp_millis();
                self.backend.delayed_push(&self.name, &job_id, run_at_ms)?;
            }
            JobType::Cron(_, next_slot, _, _) => {
                let run_at_ms = next_slot.timestamp_millis();
                self.backend.delayed_push(&self.name, &job_id, run_at_ms)?;
            }
        }

        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Queued);
        Ok(())
    }

    pub fn mark_job_is_canceled(&self, job_id: &str) {
        log::info!("Cancel job {}", job_id);
        // Remove from active and release lock
        if let Err(e) = self.backend.active_remove(&self.name, job_id) {
            log::error!("[WorkQueue] Cannot remove from active {}: {:?}", job_id, e);
        }
        if let Err(e) = self.backend.lock_release(job_id, &self.worker_id) {
            log::error!("[WorkQueue] Cannot release lock {}: {:?}", job_id, e);
        }
    }

    pub fn mark_job_is_finished(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        log::info!("Finish job {}", job_id);

        // Update job status
        job.context.job_status = JobStatus::Finished;
        job.context.complete_at = Some(get_now_as_ms());
        save_job(self.backend.as_ref(), &self.name, &job_id, &job)?;

        // Remove from active and release lock
        self.backend
            .complete_job(&self.name, &job_id, &self.worker_id)?;

        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Finished);
        Ok(())
    }

    pub fn mark_job_is_failed(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        log::info!("Failed job {}", job_id);

        // Update job status
        job.context.job_status = JobStatus::Failed;
        job.context.complete_at = Some(get_now_as_ms());
        save_job(self.backend.as_ref(), &self.name, &job_id, &job)?;

        // Remove from active and release lock
        self.backend
            .fail_job(&self.name, &job_id, &self.worker_id)?;

        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Failed);
        Ok(())
    }

    // ========================================================================
    // Job Processing
    // ========================================================================

    pub fn process_jobs(&self) {
        // First, move ready delayed jobs to waiting queue
        let now_ms = get_now_as_ms();
        if let Err(e) = self.backend.delayed_move_ready(&self.name, now_ms) {
            log::error!("[WorkQueue] Failed to move delayed jobs: {:?}", e);
        }

        // Then pick and process jobs
        match self.pick_jobs_to_process() {
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

    pub fn pick_jobs_to_process(&self) -> Result<Vec<Job<M>>, Error> {
        let active_count = self.backend.active_len(&self.name).unwrap_or(0);
        if active_count >= self.config.max_processing_jobs {
            return Ok(vec![]);
        }

        let slots_available = self.config.max_processing_jobs - active_count;
        let mut ready_jobs = Vec::new();

        for _ in 0..slots_available {
            // Atomically claim a job from waiting queue
            match self
                .backend
                .claim_job(&self.name, &self.worker_id, self.config.lock_ttl_ms)?
            {
                Some(job_id) => {
                    // Load job data
                    if let Some(mut job) =
                        load_job::<Job<M>>(self.backend.as_ref(), &self.name, &job_id)?
                    {
                        // Update job status to running
                        job.context.job_status = JobStatus::Running;
                        job.context.run_at = Some(get_now_as_ms());
                        save_job(self.backend.as_ref(), &self.name, &job_id, &job)?;

                        crate::PluginCenter::change_status::<M>(job_id, JobStatus::Running);
                        ready_jobs.push(job);
                    } else {
                        // Job data not found, remove from active
                        log::warn!("[WorkQueue] Job data not found for {}, removing", job_id);
                        self.backend.active_remove(&self.name, &job_id)?;
                        self.backend.lock_release(&job_id, &self.worker_id)?;
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
                let _ = this.mark_job_is_failed(job);
            }
        });
    }

    pub async fn execute_job(&self, mut job: Job<M>) -> Result<(), Error> {
        // If job is cancelled, handle it
        if job.is_cancelled() {
            self.mark_job_is_canceled(job.id());
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
                return self.re_enqueue(job);
            }
        }

        // If this is interval job (has next tick) -> re_enqueue it
        if let Some(next_job) = job.next_tick() {
            return self.re_enqueue(next_job);
        }

        if is_failed_output {
            self.mark_job_is_failed(job)
        } else {
            self.mark_job_is_finished(job)
        }
    }

    // ========================================================================
    // Job Management
    // ========================================================================

    pub fn cancel_job(&self, job_id: &str) -> Result<(), Error> {
        if let Some(mut job) = load_job::<Job<M>>(self.backend.as_ref(), &self.name, job_id)? {
            // Only cancel queued job
            if job.is_queued() {
                job.context.job_status = JobStatus::Canceled;
                job.context.cancel_at = Some(get_now_as_ms());
                save_job(self.backend.as_ref(), &self.name, job_id, &job)?;

                // Remove from waiting or delayed queue
                self.backend.waiting_pop(&self.name)?; // Try waiting
                self.backend.delayed_remove(&self.name, job_id)?; // Try delayed

                crate::PluginCenter::change_status::<M>(job_id.to_string(), JobStatus::Canceled);
            } else {
                log::warn!("[WorkQueue] Cannot cancel {:?} job", job.context.job_status);
            }
        }

        Ok(())
    }

    pub fn get_job(&self, job_id: &str) -> Result<Option<Job<M>>, Error> {
        load_job(self.backend.as_ref(), &self.name, job_id)
    }

    pub fn retry_job(&self, job_id: &str) -> Result<bool, Error> {
        let job = load_job::<Job<M>>(self.backend.as_ref(), &self.name, job_id)?;

        if let Some(job) = job {
            // Only allow retry done job (cancelled, failed, finished)
            if job.is_done() {
                self.re_enqueue(job)?;
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

    pub fn read_job(&self, job_id: &str) -> Result<Option<Job<M>>, Error> {
        load_job(self.backend.as_ref(), &self.name, job_id)
    }

    pub fn get_processing_job_ids(&self, _count: usize) -> Result<Vec<String>, Error> {
        self.backend.active_list(&self.name)
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
        self.process_jobs();
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
        self.run_with_config(msg.0, msg.1)
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
        self.cancel_job(&msg.job_id)
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
        self.get_job(&msg.job_id).ok().flatten()
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
        self.retry_job(&msg.job_id)
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
