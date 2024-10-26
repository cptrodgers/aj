use chrono::{serde::ts_microseconds, serde::ts_microseconds_option, DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::str::FromStr;

use super::retry::*;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobContext {
    pub job_type: JobType,
    pub job_status: JobStatus,
    pub retry: Option<Retry>,
    pub created_at: i64,
    pub enqueue_at: Option<i64>,
    pub run_at: Option<i64>,
    pub complete_at: Option<i64>,
    pub cancel_at: Option<i64>,
    pub run_count: usize,
}

impl Default for JobContext {
    fn default() -> Self {
        Self {
            job_type: JobType::Normal,
            job_status: JobStatus::Queued,
            retry: None,
            created_at: get_now_as_ms(),
            enqueue_at: None,
            run_at: None,
            complete_at: None,
            cancel_at: None,
            run_count: 0,
        }
    }
}
