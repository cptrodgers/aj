use std::fmt::Debug;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::{job_context::JobContext, retry::Retry};

#[async_trait]
pub trait Executable {
    type Output: Debug + Send;

    async fn pre_execute(&mut self, _context: &'_ JobContext) {}

    async fn execute(&mut self, _context: &'_ JobContext) -> Self::Output;

    async fn post_execute(
        &mut self,
        output: Self::Output,
        _context: &'_ JobContext,
    ) -> Self::Output {
        output
    }

    // Identify job is failed or not. Default is false
    // You can change id_failed_output logic to handle retry logic
    async fn is_failed_output(&self, _job_output: &Self::Output) -> bool {
        false
    }

    // Job will re-run if should_retry return a specific time in the future
    async fn retry_at(
        &self,
        retry_context: &mut Retry,
        job_output: Self::Output,
    ) -> Option<DateTime<Utc>> {
        let should_retry = self.is_failed_output(&job_output).await && retry_context.should_retry();

        if should_retry {
            Some(retry_context.retry_at(None))
        } else {
            None
        }
    }
}
