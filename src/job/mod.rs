pub mod retry;

use async_trait::async_trait;
use chrono::{serde::ts_microseconds, serde::ts_microseconds_option, DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::str::FromStr;

use crate::types::{upsert_to_storage, Backend};
use crate::util::{get_now, get_now_as_ms};
use crate::Error;
use super::retry::*;

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
    pub fn init_cron(expression: &str, context: CronContext) -> Result<Self, Error> {
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
}

#[derive(Debug, Serialize, Deserialize, Clone, Builder)]
pub struct Job<M: Executable + Clone> {
    #[builder(default = "uuid::Uuid::new_v4().to_string()")]
    pub id: String,
    #[builder(default)]
    pub context: JobContext,
    pub data: M,
}

#[async_trait]
pub trait Executable {
    type Output: Debug + Send;

    async fn pre_execute(&self) {}

    async fn execute(&self) -> Self::Output;

    async fn post_execute(&self, output: Self::Output) -> Self::Output {
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
    M: Executable + Clone + Serialize,
{
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

    pub fn is_cancelled(&self) -> bool {
        self.context.job_status == JobStatus::Canceled
    }

    pub fn is_done(&self) -> bool {
        self.context.job_status == JobStatus::Finished ||
        self.context.job_status == JobStatus::Canceled ||
        self.context.job_status == JobStatus::Failed
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
    use super::*;

    #[derive(Default, Debug, Clone, Serialize)]
    pub struct TestJob {
        number: i32,
    }

    #[async_trait::async_trait]
    impl Executable for TestJob {
        type Output = i32;

        async fn execute(&self) -> Self::Output {
            self.number
        }

        async fn post_execute(&self, output: Self::Output) -> Self::Output {
            output + 1
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
        let default_job = default_job(number);

        assert!(default_job.is_ready());
        assert!(default_job.context.job_status == JobStatus::Queued);
        assert!(default_job.context.job_type == JobType::Normal);

        let output = default_job.data.execute().await;
        assert_eq!(output, number);
        assert_eq!(default_job.data.post_execute(output).await, 2);
    }
}
