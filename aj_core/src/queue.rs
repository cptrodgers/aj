use actix::fut::wrap_future;
use actix::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use crate::job::{Job, JobStatus};
use crate::types::{get_from_storage, upsert_to_storage, Backend, QueueDirection};
use crate::{Error, Executable, JobType};

const DEFAULT_TICK_DURATION: Duration = Duration::from_millis(100);
const MAX_PROCESSING_JOBS: usize = 20;

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
}

impl WorkQueueConfig {
    pub fn init() -> Self {
        Self {
            max_processing_jobs: MAX_PROCESSING_JOBS,
            process_tick_duration: DEFAULT_TICK_DURATION,
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
            config: WorkQueueConfig::default(),
            _type: PhantomData,
            backend,
        }
    }

    pub fn format_queue_name(&self, status: JobStatus) -> String {
        format!("{}:queue:{:?}", self.name, status)
    }

    pub fn format_failed_queue_name(&self) -> String {
        format!("{}:queue:failed", self.name)
    }

    pub fn storage_name(&self) -> String {
        format!("{}:storage", self.name)
    }

    pub fn start_with_name(name: String, backend: Arc<dyn Backend + Sync + Send>) -> Addr<Self> {
        let arbiter: Arbiter = Arbiter::new();

        <Self as Actor>::start_in_arbiter(&arbiter.handle(), |ctx| {
            let mut q = WorkQueue::<M>::new(name, backend);
            q.process_jobs(ctx);
            q
        })
    }

    pub fn run_with_config(&self, job: Job<M>, config: EnqueueConfig) -> Result<(), Error> {
        let key = job.id.clone();
        let existing_job = get_from_storage::<Job<M>>(self.backend.deref(), &key)?;
        if let Some(existing_job) = existing_job {
            if config.override_data && !existing_job.is_running() {
                info!(
                    "[WorkQueue] Update exising job with new job data: {}",
                    job.id
                );
                upsert_to_storage(self.backend.deref(), &key, &job)?;
            } else {
                info!(
                    "[WorkQueue] Job is running, skip update job data: {}",
                    job.id
                );
            }

            if config.re_run && existing_job.is_done() {
                info!("[WorkQueue] Re run job {}", existing_job.id);
                self.enqueue(job)?;
            }

            return Ok(());
        }

        self.enqueue(job)
    }

    pub fn enqueue(&self, mut job: Job<M>) -> Result<(), Error> {
        let key = job.id.clone();
        info!("[WorkQueue] New Job {}", key);
        self.backend
            .queue_push(&self.format_queue_name(JobStatus::Queued), &key)?;
        job.enqueue(self.backend.deref())
    }

    // Push processing job if
    // 1. Job is done and still have next tick (cron job).
    // 2. Job ran failed and have retry logic
    pub fn re_queue_processing_job(&self, mut job: Job<M>) -> Result<(), Error> {
        debug!("[WorkQueue] Re-run job {}", job.id);

        self.remove_processing_job(&job.id);
        job.enqueue(self.backend.deref())?;

        let queued_queue = self.format_queue_name(JobStatus::Queued);
        if let Err(e) = self.backend.queue_push(&queued_queue, job.id.as_str()) {
            error!("[WorkQueue] Cannot re enqueue {}: {:?}", job.id, e);
        };
        Ok(())
    }

    pub fn mark_job_is_canceled(&self, job_id: &str) {
        info!("Cancel job {}", job_id);
        let cancelled_queue = self.format_queue_name(JobStatus::Canceled);
        if let Err(e) = self.backend.queue_push(&cancelled_queue, job_id) {
            error!("[WorkQueue] Cannot re enqueue {}: {:?}", job_id, e);
        };
    }

    pub fn mark_job_is_finished(&self, mut job: Job<M>) -> Result<(), Error> {
        info!("Finish job {}", job.id);
        self.remove_processing_job(&job.id);
        job.finish(self.backend.deref())?;

        let finished_queue = self.format_queue_name(JobStatus::Finished);
        if let Err(e) = self.backend.queue_push(&finished_queue, job.id.as_str()) {
            error!("[WorkQueue] Cannot finish {}: {:?}", job.id, e);
        };
        Ok(())
    }

    pub fn mark_job_is_failed(&self, mut job: Job<M>) -> Result<(), Error> {
        info!("Failed job {}", job.id);
        job.fail(self.backend.deref())?;
        self.push_failed_job(job.id.as_str());
        Ok(())
    }

    pub fn push_failed_job(&self, job_id: &str) {
        self.remove_processing_job(job_id);
        let failed_queue = self.format_queue_name(JobStatus::Failed);
        if let Err(e) = self.backend.queue_push(&failed_queue, job_id) {
            error!(
                "[WorkQueue] Cannot move to failed queue {}: {:?}",
                job_id, e
            );
        };
    }

    pub fn remove_processing_job(&self, job_id: &str) {
        let processing_queue = self.format_queue_name(JobStatus::Running);
        if let Err(reason) = self.backend.queue_remove(&processing_queue, job_id) {
            error!(
                "[WorkQueue] Cannot remove job {} in processing queue: {:?}",
                job_id, reason
            );
        };
    }

    pub fn get_processing_job_ids(&self, count: usize) -> Result<Vec<String>, Error> {
        let processing_queue_name = self.format_queue_name(JobStatus::Running);
        let job_ids =
            self.backend
                .queue_get(&processing_queue_name, count, QueueDirection::Front)?;
        Ok(job_ids)
    }

    pub fn read_job(&self, job_id: &str) -> Result<Option<Job<M>>, Error> {
        let item = get_from_storage(self.backend.deref(), job_id)?;
        Ok(item)
    }

    pub fn process_jobs(&mut self, ctx: &mut Context<WorkQueue<M>>) {
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
        let processing_queue = self.format_queue_name(JobStatus::Running);
        let total_processing_jobs = self.backend.queue_count(&processing_queue).unwrap_or(0);
        if total_processing_jobs >= self.config.max_processing_jobs {
            return Ok(vec![]);
        }

        self.try_pick_specific_ready_jobs(self.config.max_processing_jobs - total_processing_jobs)
    }

    pub fn try_pick_specific_ready_jobs(&self, total: usize) -> Result<Vec<Job<M>>, Error> {
        info!("Try Picking {total} jobs in queue");
        let idle_queue_name = self.format_queue_name(JobStatus::Queued);
        let processing_queue_name = self.format_queue_name(JobStatus::Running);

        let idle_queue_length = self.backend.queue_count(&idle_queue_name)?;
        let mut ready_jobs = vec![];
        let mut current_cursor = idle_queue_length as i32;

        loop {
            let job_ids = self
                .backend
                .queue_get(&idle_queue_name, 20, QueueDirection::Back)?;

            // Update current cursor
            current_cursor -= job_ids.len() as i32;

            for job_id in job_ids {
                let job = get_from_storage::<Job<M>>(self.backend.deref(), &job_id)?;
                if let Some(job) = job {
                    if job.is_ready()
                        && ready_jobs.len() < total
                        && !ready_jobs
                            .iter()
                            .any(|ready_job: &Job<M>| ready_job.id == job.id)
                    {
                        // Job is read to process. Put it in the ready list
                        ready_jobs.push(job);
                        self.backend.queue_move(
                            &idle_queue_name,
                            &processing_queue_name,
                            1,
                            QueueDirection::Back,
                            QueueDirection::Front,
                        )?;
                        continue;
                    }
                }

                // Job is not ready to process, move it back to front of queue
                self.backend.queue_move(
                    &idle_queue_name,
                    &idle_queue_name,
                    1,
                    QueueDirection::Back,
                    QueueDirection::Front,
                )?;
            }

            if ready_jobs.len() >= total {
                break;
            }

            if current_cursor <= 0 {
                break;
            }
        }

        Ok(ready_jobs)
    }

    pub fn execute_job_task(&self, job: Job<M>, ctx: &mut Context<WorkQueue<M>>) {
        let this = self.clone();
        let task = async move {
            // TODO: Consider Smart Pointer to wrap job instead of clone
            if let Err(err) = this.execute_job(job.clone()).await {
                error!("[WorkQueue] Execute job {} fail: {:?}", job.id, err);
                let _ = this.mark_job_is_failed(job);
            }
        };
        wrap_future::<_, Self>(task).spawn(ctx);
    }

    pub async fn execute_job(&self, mut job: Job<M>) -> Result<(), Error> {
        // If job is cancelled, move to cancel queued
        if job.is_cancelled() {
            self.mark_job_is_canceled(job.id.as_str());
            return Ok(());
        }

        let job_output = job.execute().await;
        info!(
            "[WorkQueue] Execution complete. Job {} - Result: {job_output:?}",
            job.id
        );
        if let Some(retry_context) = job.context.retry.as_mut() {
            if let Some(next_retry_ms) = job.data.should_retry(retry_context, job_output).await {
                info!("[WorkQueue] Retry this job. {}", job.id);
                job.context.job_type = JobType::ScheduledAt(next_retry_ms);
                return self.re_queue_processing_job(job);
            }
        }

        // If this is interval job (has next tick) -> re_enqueue it
        if let Some(next_job) = job.next_tick() {
            return self.re_queue_processing_job(next_job);
        }

        self.mark_job_is_finished(job)
    }

    pub fn cancel_job(&self, job_id: &str) -> Result<(), Error> {
        if let Some(mut job) = get_from_storage::<Job<M>>(self.backend.deref(), job_id)? {
            // Only cancel queued job
            if job.is_queued() {
                job.cancel(self.backend.deref())?;
                self.backend
                    .queue_remove(&self.format_queue_name(JobStatus::Queued), job_id)?;
            } else {
                warn!("[WorkQueue] Cannot cancel {:?} job", job.context.job_status);
            }
        }

        Ok(())
    }

    pub fn get_job(&self, job_id: &str) -> Result<Option<Job<M>>, Error> {
        let job = get_from_storage::<Job<M>>(self.backend.deref(), job_id)?;
        Ok(job)
    }
}

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
