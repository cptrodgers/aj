use actix::fut::wrap_future;
use actix::*;
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

#[derive(Clone)]
pub struct WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    Self: Actor<Context = Context<Self>>,
{
    name: Arc<String>,
    worker_id: String,
    config: WorkQueueConfig,
    _type: PhantomData<M>,
    backend: Arc<dyn Backend>,
}

impl<M> Actor for WorkQueue<M>
where
    M: Executable + Unpin + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    type Context = Context<Self>;
}

impl<M> WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    Self: Actor<Context = Context<Self>>,
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

    pub fn start_with_name(name: String, backend: Arc<dyn Backend + Sync + Send>) -> Addr<Self> {
        let arbiter: Arbiter = Arbiter::new();

        <Self as Actor>::start_in_arbiter(&arbiter.handle(), |ctx| {
            let mut q = WorkQueue::<M>::new(name, backend);
            q.process_jobs(ctx);
            q
        })
    }

    // ========================================================================
    // Job Lifecycle Operations
    // ========================================================================

    pub fn run_with_config(&self, job: Job<M>, config: EnqueueConfig) -> Result<(), Error> {
        let job_id = job.id();
        let existing_job = load_job::<Job<M>>(self.backend.as_ref(), &self.name, job_id)?;

        if let Some(existing_job) = existing_job {
            if config.override_data && !existing_job.is_running() {
                info!(
                    "[WorkQueue] Update existing job with new job data: {}",
                    job.id()
                );
                save_job(self.backend.as_ref(), &self.name, job_id, &job)?;
            } else {
                info!(
                    "[WorkQueue] Job is running, skip update job data: {}",
                    job.id()
                );
            }

            if config.re_run && existing_job.is_done() {
                info!("[WorkQueue] Re run job {}", existing_job.id());
                self.enqueue(job)?;
            }

            return Ok(());
        }

        self.enqueue(job)
    }

    pub fn enqueue(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        info!("[WorkQueue] New Job {}", job_id);

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
        debug!("[WorkQueue] Re-run job {}", job_id);

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
        info!("Cancel job {}", job_id);
        // Remove from active and release lock
        if let Err(e) = self.backend.active_remove(&self.name, job_id) {
            error!("[WorkQueue] Cannot remove from active {}: {:?}", job_id, e);
        }
        if let Err(e) = self.backend.lock_release(job_id, &self.worker_id) {
            error!("[WorkQueue] Cannot release lock {}: {:?}", job_id, e);
        }
    }

    pub fn mark_job_is_finished(&self, mut job: Job<M>) -> Result<(), Error> {
        let job_id = job.id().to_string();
        info!("Finish job {}", job_id);

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
        info!("Failed job {}", job_id);

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

    pub fn process_jobs(&mut self, ctx: &mut Context<WorkQueue<M>>) {
        // First, move ready delayed jobs to waiting queue
        let now_ms = get_now_as_ms();
        if let Err(e) = self.backend.delayed_move_ready(&self.name, now_ms) {
            error!("[WorkQueue] Failed to move delayed jobs: {:?}", e);
        }

        // Then pick and process jobs
        match self.pick_jobs_to_process() {
            Ok(jobs) => {
                for job in jobs {
                    self.execute_job_task(job, ctx);
                }
            }
            Err(err) => {
                error!("[WorkQueue]: Cannot pick jobs to process {err:?}",);
            }
        }

        ctx.run_later(self.config.process_tick_duration, |work_queue, ctx| {
            work_queue.process_jobs(ctx);
        });
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
                        warn!("[WorkQueue] Job data not found for {}, removing", job_id);
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

    pub fn execute_job_task(&self, job: Job<M>, ctx: &mut Context<WorkQueue<M>>) {
        let this = self.clone();
        let task = async move {
            if let Err(err) = this.execute_job(job.clone()).await {
                error!("[WorkQueue] Execute job {} fail: {:?}", job.id(), err);
                let _ = this.mark_job_is_failed(job);
            }
        };
        wrap_future::<_, Self>(task).spawn(ctx);
    }

    pub async fn execute_job(&self, mut job: Job<M>) -> Result<(), Error> {
        // If job is cancelled, handle it
        if job.is_cancelled() {
            self.mark_job_is_canceled(job.id());
            return Ok(());
        }

        let job_output = job.execute().await;
        let is_failed_output = job.data.is_failed_output(&job_output).await;

        info!(
            "[WorkQueue] Execution complete. Job {} - Result: {job_output:?}",
            job.id()
        );

        // Check for retry
        if let Some(retry_context) = job.context.retry.as_mut() {
            if let Some(next_retry_at) = job.data.retry_at(retry_context, job_output).await {
                info!("[WorkQueue] Retry this job. {}", job.id());
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
                warn!("[WorkQueue] Cannot cancel {:?} job", job.context.job_status);
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
                debug!(
                    "[WorkQueue] Cannot retry job {} in status {:?}",
                    job_id, job.context.job_status
                );
                return Ok(false);
            }
        }

        debug!("[WorkQueue] Don't found job {} to retry", job_id);
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
// Actix Message Handlers
// ============================================================================

#[derive(Message, Debug)]
#[rtype(result = "Result<(), Error>")]
pub struct Enqueue<M: Executable + Clone + Send + Sync + 'static>(pub Job<M>, pub EnqueueConfig);

impl<M> Handler<Enqueue<M>> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    Self: Actor<Context = Context<Self>>,
{
    type Result = Result<(), Error>;

    fn handle(&mut self, msg: Enqueue<M>, _: &mut Self::Context) -> Self::Result {
        self.run_with_config(msg.0, msg.1)
    }
}

pub async fn enqueue_job<M>(
    addr: Addr<WorkQueue<M>>,
    job: Job<M>,
    config: EnqueueConfig,
) -> Result<(), Error>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    addr.send::<Enqueue<M>>(Enqueue(job, config)).await?
}

#[derive(Message, Debug)]
#[rtype(result = "Result<(), Error>")]
pub struct CancelJob {
    pub job_id: String,
}

impl<M> Handler<CancelJob> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    Self: Actor<Context = Context<Self>>,
{
    type Result = Result<(), Error>;

    fn handle(&mut self, msg: CancelJob, _: &mut Self::Context) -> Self::Result {
        let job_id = msg.job_id;
        self.cancel_job(&job_id)
    }
}

pub async fn cancel_job<M>(addr: Addr<WorkQueue<M>>, job_id: String) -> Result<(), Error>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    addr.send::<CancelJob>(CancelJob { job_id }).await?
}

#[derive(Message, Debug)]
#[rtype(result = "Option<Job<M>>")]
pub struct GetJob<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    pub job_id: String,
    _phantom: PhantomData<M>,
}

impl<M> Handler<GetJob<M>> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    type Result = Option<Job<M>>;

    fn handle(&mut self, msg: GetJob<M>, _: &mut Self::Context) -> Self::Result {
        self.get_job(&msg.job_id).ok().flatten()
    }
}

pub async fn get_job<M>(addr: Addr<WorkQueue<M>>, job_id: &str) -> Option<Job<M>>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    let msg: GetJob<M> = GetJob {
        job_id: job_id.to_string(),
        _phantom: PhantomData,
    };
    addr.send::<GetJob<M>>(msg).await.ok().flatten()
}

#[derive(Message, Debug)]
#[rtype(result = "Result<bool, Error>")]
pub struct RetryJob<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    pub job_id: String,
    _phantom: PhantomData<M>,
}

impl<M> Handler<RetryJob<M>> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    type Result = Result<bool, Error>;

    fn handle(&mut self, msg: RetryJob<M>, _: &mut Self::Context) -> Self::Result {
        self.retry_job(&msg.job_id)
    }
}

pub async fn retry_job<M>(addr: Addr<WorkQueue<M>>, job_id: &str) -> Result<bool, Error>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    let msg: RetryJob<M> = RetryJob {
        job_id: job_id.to_string(),
        _phantom: PhantomData,
    };
    addr.send::<RetryJob<M>>(msg).await?
}

#[derive(Message)]
#[rtype(result = "()")]
pub struct UpdateWorkQueue {
    pub config: WorkQueueConfig,
}

impl<M> Handler<UpdateWorkQueue> for WorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    type Result = ();

    fn handle(&mut self, msg: UpdateWorkQueue, _: &mut Self::Context) -> Self::Result {
        self.config = msg.config;
    }
}

pub async fn update_work_queue_config<M>(
    addr: Addr<WorkQueue<M>>,
    config: WorkQueueConfig,
) -> Result<(), Error>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    let msg = UpdateWorkQueue { config };
    addr.send::<UpdateWorkQueue>(msg).await?;
    Ok(())
}
