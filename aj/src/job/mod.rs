pub mod retry;

use async_trait::async_trait;
use chrono::Duration;
use chrono::{serde::ts_microseconds, serde::ts_microseconds_option, DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::str::FromStr;

use super::retry::*;
use crate::types::{upsert_to_storage, Backend};
use crate::util::{get_now, get_now_as_ms};
use crate::Error;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CronContext {
    pub tick_number: i32,
    pub max_repeat: Option<i32>,
    #[serde(with = "ts_microseconds_option")]
    pub end_at: Option<DateTime<Utc>>,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobType {
    // DateTime<Utc> background job
    Normal,
    // Schedule Job: (Next Tick At)
    ScheduledAt(#[serde(with = "ts_microseconds")] DateTime<Utc>),
    // Cron Job: Cron Expression, Next Tick At, total repeat, Cron Context
    Cron(
        String,
        #[serde(with = "ts_microseconds")] DateTime<Utc>,
        #[serde(default)] i32,
        #[serde(default)] CronContext,
    ),
}

impl Default for JobType {
    fn default() -> Self {
        Self::Normal
    }
}

impl JobType {
    pub fn new_schedule(schedule_at: DateTime<Utc>) -> Self {
        JobType::ScheduledAt(schedule_at)
    }

    pub fn new_cron(expression: &str, context: CronContext) -> Result<Self, Error> {
        let schedule = Schedule::from_str(expression)?;
        let now = get_now();
        let next_tick = schedule.after(&now).next().unwrap_or(now);
        Ok(Self::Cron(expression.into(), next_tick, 1, context))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub enum JobStatus {
    Queued = 0,
    Running = 1,
    Finished = 2,
    Failed = 3,
    Canceled = 4,
}

impl Default for JobStatus {
    fn default() -> Self {
        Self::Queued
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Builder, Default)]
pub struct JobContext {
    #[builder(default = "JobType::Normal")]
    pub job_type: JobType,
    #[builder(default = "JobStatus::Queued")]
    pub job_status: JobStatus,
    #[builder(setter(into, strip_option), default)]
    pub retry: Option<Retry>,
    #[builder(default = "get_now_as_ms()", setter(skip))]
    pub created_at: i64,
    #[builder(default, setter(skip))]
    pub enqueue_at: Option<i64>,
    #[builder(default, setter(skip))]
    pub run_at: Option<i64>,
    #[builder(default, setter(skip))]
    pub complete_at: Option<i64>,
    #[builder(default, setter(skip))]
    pub cancel_at: Option<i64>,
    #[builder(default, setter(skip))]
    pub run_count: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone, Builder)]
pub struct Job<M: Executable + Clone> {
    #[builder(default = "uuid::Uuid::new_v4().to_string()")]
    pub id: String,
    #[builder(default)]
    pub context: JobContext,
    pub data: M,
}

impl<M> JobBuilder<M>
where
    M: Executable + Clone,
{
    pub fn schedule_at(&mut self, schedule_at: DateTime<Utc>) -> &mut Self {
        if let Some(context) = &mut self.context {
            context.job_type = JobType::new_schedule(schedule_at);
        } else {
            let mut context = JobContext::default();
            context.job_type = JobType::new_schedule(schedule_at);
            self.context(context);
        }

        self
    }

    pub fn delay(&mut self, after: Duration) -> &mut Self {
        let schedule_at = get_now() + after;
        self.schedule_at(schedule_at)
    }

    pub fn cron(&mut self, expression: &str) -> &mut Self {
        let cron = JobType::new_cron(expression, CronContext::default()).unwrap();
        if let Some(context) = &mut self.context {
            context.job_type = cron;
        } else {
            let mut context = JobContext::default();
            context.job_type = cron;
            self.context(context);
        }

        self
    }

    pub fn retry(&mut self, retry: Retry) -> &mut Self {
        if let Some(context) = &mut self.context {
            context.retry = Some(retry);
        } else {
            let mut context = JobContext::default();
            context.retry = Some(retry);
            self.context(context);
        }

        self
    }
}

#[async_trait]
pub trait Executable {
    type Output: Debug + Send;

    async fn pre_execute(&self, _context: &'_ JobContext) {}

    async fn execute(&self, _context: &'_ JobContext) -> Self::Output;

    async fn post_execute(&self, output: Self::Output, _context: &'_ JobContext) -> Self::Output {
        output
    }

    // Identify job is failed or not. Default is false
    // You can change id_failed_output logic to handle retry logic
    async fn is_failed_output(&self, _job_output: Self::Output) -> bool {
        false
    }

    // Job will re-run if should_retry return a specific time in the future
    async fn should_retry(
        &self,
        retry_context: &mut Retry,
        job_output: Self::Output,
    ) -> Option<DateTime<Utc>> {
        let should_retry = self.is_failed_output(job_output).await && retry_context.should_retry();

        if should_retry {
            Some(retry_context.retry_at(None))
        } else {
            None
        }
    }
}

impl<M> Job<M>
where
    M: Executable + Clone + Serialize + Sync,
{
    pub async fn execute(&mut self) -> <M as Executable>::Output {
        self.data.pre_execute(&self.context).await;
        let output = self.data.execute(&self.context).await;
        let output = self.data.post_execute(output, &self.context).await;
        self.context.run_count += 1;

        output
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

    pub fn next_tick(&mut self) -> Option<Self> {
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

    pub fn is_running(&self) -> bool {
        self.context.job_status == JobStatus::Running
    }

    pub fn is_cancelled(&self) -> bool {
        self.context.job_status == JobStatus::Canceled
    }

    pub fn is_done(&self) -> bool {
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

    pub fn run(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Run {}", self.id);
        self.context.job_status = JobStatus::Running;
        self.context.run_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }

    pub fn finish(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Finish {}", self.id);
        self.context.job_status = JobStatus::Finished;
        self.context.complete_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }

    pub fn cancel(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Cancel {}", self.id);
        self.context.job_status = JobStatus::Canceled;
        self.context.cancel_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }

    pub fn fail(&mut self, backend: &dyn Backend) -> Result<(), Error> {
        debug!("[Job] Failed {}", self.id);
        self.context.job_status = JobStatus::Failed;
        self.context.complete_at = Some(get_now_as_ms());
        upsert_to_storage(backend, &self.id, self.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[derive(Default, Debug, Clone, Serialize)]
    pub struct TestJob {
        number: i32,
    }

    #[async_trait::async_trait]
    impl Executable for TestJob {
        type Output = i32;

        async fn execute(&self, _context: &JobContext) -> Self::Output {
            self.number
        }
    }

    fn default_job(number: i32) -> Job<TestJob> {
        JobBuilder::default()
            .data(TestJob { number })
            .build()
            .unwrap()
    }

    #[actix::test]
    async fn test_normal_job() {
        let number = 1;
        let mut default_job = default_job(number);

        assert!(default_job.is_ready());
        assert!(default_job.context.job_status == JobStatus::Queued);
        assert!(default_job.context.job_type == JobType::Normal);

        let output = default_job.execute().await;
        assert_eq!(output, number);
    }

    #[actix::test]
    async fn test_schedule_job() {
        let number = 1;
        let schedule_at = get_now() + Duration::from_secs(1);
        let schedule_job = JobBuilder::default()
            .data(TestJob { number })
            .schedule_at(schedule_at)
            .build()
            .unwrap();

        assert!(!schedule_job.is_ready());
        assert!(schedule_job.context.job_type == JobType::ScheduledAt(schedule_at))
    }

    #[actix::test]
    async fn test_cron_job() {
        let number = 1;
        let expression = "0 1 1 1 * * *";
        let schedule_job = JobBuilder::default()
            .data(TestJob { number })
            .cron(expression)
            .build()
            .unwrap();

        assert!(!schedule_job.is_ready());
        let expected_cron = JobType::new_cron(expression, CronContext::default()).unwrap();
        assert!(schedule_job.context.job_type == expected_cron);
    }
}