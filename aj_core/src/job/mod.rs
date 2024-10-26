pub mod retry;
pub mod executable;
pub mod job_context;

pub use executable::*;
pub use job_context::*;
pub use retry::*;

use chrono::Duration;
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::fmt::Debug;
use std::str::FromStr;

use crate::types::{upsert_to_storage, Backend};
use crate::util::{get_now, get_now_as_ms};
use crate::Error;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Job<M: Executable + Clone> {
    pub id: String,
    pub context: JobContext,
    pub data: M,
}

impl<M> Job<M>
where M: Executable + Clone
{
    /// Create a new job.
    /// ```no_run
    /// let job = Job::new(Print {
    ///     number: 1,
    /// })
    /// ```
    pub fn new(job: M) -> Job<M> {
        let id = Uuid::new_v4().to_string();
        Self {
            id,
            context: JobContext::default(),
            data: job,
        }
    }

    pub fn set_context(mut self, context: JobContext) -> Self {
        self.context = context;
        self
    }

    pub fn context(self, context: JobContext) -> Self {
        self.set_context(context)
    }

    /// Set Job Type
    pub fn set_job_type(mut self, job_type: JobType) -> Self {
        self.context.job_type = job_type;
        self
    }

    pub fn job_type(self, job_type: JobType) -> Self {
        self.set_job_type(job_type)
    }

    /// Schedule job to run at specific time
    /// ```no_run
    /// job.schedule_at(get_now() + Duration::seconds(2));
    /// ```
    pub fn schedule_at(self, schedule_at: DateTime<Utc>) -> Self {
        self.job_type(JobType::new_schedule(schedule_at))
    }

    /// Schedule job to run after specific time
    /// ```no_run
    /// job.delay(Duration::seconds(2));
    /// ```
    pub fn delay(self, after: Duration) -> Self {
        let schedule_at = get_now() + after;
        self.schedule_at(schedule_at)
    }

    /// Schedule job run by Cron Pattern
    /// ```no_run
    /// job.cron("* * * * * * *");
    /// ```
    pub fn cron(self, cron_expression: &str) -> Self {
        let cron = JobType::new_cron(cron_expression, CronContext::default()).unwrap();
        self.job_type(cron)
    }

    /// Set Retry logic for job
    /// `job.set_retry(aj::Retry::default())`
    pub fn set_retry(mut self, retry: Retry) -> Self {
        self.context.retry = Some(retry);
        self
    }

    pub fn retry(self, retry: Retry) -> Self {
        self.set_retry(retry)
    }
}

impl<M> Job<M>
where
    M: Executable + Clone + Serialize + Sync + Send,
{
    pub(crate) async fn execute(&mut self) -> <M as Executable>::Output {
        // Log Count
        self.context.run_count += 1;

        self.data.pre_execute(&self.context).await;
        let output = self.data.execute(&self.context).await;
        self.data.post_execute(output, &self.context).await
    }

    pub fn is_ready(&self) -> bool {
        let now = get_now();
        match &self.context.job_type {
            JobType::ScheduledAt(schedule_at) => &now > schedule_at,
            JobType::Cron(_, next_slot, total_repeat, context) => {
                if now < *next_slot {
                    return false;
                }

                if let Some(max_repeat) = context.max_repeat {
                    if max_repeat < *total_repeat {
                        return false;
                    }
                }

                if let Some(end_at) = context.end_at {
                    if now > end_at {
                        return false;
                    }
                }

                true
            }
            _ => true,
        }
    }

    pub(crate) fn next_tick(&mut self) -> Option<Self> {
        let now = get_now();
        match &self.context.job_type {
            JobType::Cron(cron_expression, _, total_repeat, context) => {
                let mut job = self.clone();
                let schedule = Schedule::from_str(cron_expression);
                if schedule.is_err() {
                    error!(
                        "[Job] Cannot parse schedule {cron_expression} of job {}",
                        job.id
                    );
                    return None;
                }

                let schedule = schedule.unwrap();
                if let Some(upcoming_event) = schedule.after(&get_now()).next() {
                    job.context.job_type = JobType::Cron(
                        cron_expression.clone(),
                        upcoming_event,
                        *total_repeat + 1,
                        context.clone(),
                    );
                }

                if let Some(max_repeat) = context.max_repeat {
                    if max_repeat < *total_repeat {
                        return None;
                    }
                }

                if let Some(end_at) = context.end_at {
                    if end_at < now {
                        return None;
                    }
                }

                Some(job)
            }
            _ => None,
        }
    }

    pub fn is_queued(&self) -> bool {
        self.context.job_status == JobStatus::Queued
    }

    pub fn is_running(&self) -> bool {
        self.context.job_status == JobStatus::Running
    }

    pub fn is_cancelled(&self) -> bool {
        self.context.job_status == JobStatus::Canceled
    }

    pub(crate) fn is_done(&self) -> bool {
        self.context.job_status == JobStatus::Finished
            || self.context.job_status == JobStatus::Canceled
            || self.context.job_status == JobStatus::Failed
    }

    pub fn enqueue(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Enqueue {}", self.id);
        self.context.job_status = JobStatus::Queued;
        self.context.enqueue_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }

    pub fn process(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Run {}", self.id);
        self.context.job_status = JobStatus::Running;
        self.context.run_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }

    pub(crate) fn finish(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Finish {}", self.id);
        self.context.job_status = JobStatus::Finished;
        self.context.complete_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }

    pub(crate) fn cancel(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Cancel {}", self.id);
        self.context.job_status = JobStatus::Canceled;
        self.context.cancel_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }

    pub(crate) fn fail(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Failed {}", self.id);
        self.context.job_status = JobStatus::Failed;
        self.context.complete_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;

    #[derive(Default, Debug, Clone, Serialize)]
    pub struct TestJob {
        number: i32,
    }

    #[async_trait::async_trait]
    impl Executable for TestJob {
        type Output = i32;

        async fn execute(&mut self, _context: &JobContext) -> Self::Output {
            self.number
        }
    }

    fn default_job(number: i32) -> Job<TestJob> {
        Job::new(TestJob { number })
    }

    #[actix::test]
    async fn test_job() {
        let number = 1;
        let mut default_job = default_job(number);

        assert!(default_job.is_ready());
        assert!(default_job.context.job_status == JobStatus::Queued);
        assert!(default_job.context.job_type == JobType::Normal);

        let output = default_job.execute().await;
        assert_eq!(output, number);

        assert_eq!(default_job.context.run_count, 1);
    }

    #[actix::test]
    async fn test_schedule_job() {
        let number = 1;
        let schedule_at = get_now() + Duration::from_secs(1);
        let schedule_job = Job::new(TestJob { number })
            .schedule_at(schedule_at);

        assert!(!schedule_job.is_ready());
        assert!(schedule_job.context.job_type == JobType::ScheduledAt(schedule_at))
    }

    #[actix::test]
    async fn test_cron_job() {
        let number = 1;
        let expression = "0 1 1 1 * * *";
        let schedule_job = Job::new(TestJob { number }).cron(expression);

        assert!(!schedule_job.is_ready());
        let expected_cron = JobType::new_cron(expression, CronContext::default()).unwrap();
        assert!(schedule_job.context.job_type == expected_cron);
    }

    #[actix::test]
    async fn test_retry() {
        #[derive(Default, Debug, Clone, Serialize)]
        pub struct TestRetryJob {
            number: i32,
        }

        #[async_trait]
        impl Executable for TestRetryJob {
            type Output = i32;

            async fn execute(&mut self, _: &JobContext) -> Self::Output {
                self.number
            }

            async fn is_failed_output(&self, output: &Self::Output) -> bool {
                output % 2 == 0
            }
        }

        // Job with interval retry
        let max_retries = 3;
        let retry = Retry::new_interval_retry(Some(max_retries), chrono::Duration::seconds(1));
        let mut internal_retry_job = Job::new(TestRetryJob { number: 2 })
            .retry(retry);

        for _ in 1..=max_retries {
            let output = internal_retry_job.execute().await;
            assert!(internal_retry_job.data.is_failed_output(&output).await);

            let should_retry_at = internal_retry_job
                .data
                .retry_at(internal_retry_job.context.retry.as_mut().unwrap(), output)
                .await;
            assert!(should_retry_at.is_some());
        }
    }
}

pub trait BackgroundJob: Executable + Clone {
    fn queue_name() -> &'static str;
    fn job(self) -> Job<Self>;
}
