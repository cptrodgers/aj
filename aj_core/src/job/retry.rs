use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use crate::util::get_now;

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RetryStrategy {
    /// Retry the job with consistent time. Example: 2s, 4s, 6s, 8s ...
    Interval(#[serde_as(as = "serde_with::DurationMicroSeconds<i64>")] Duration), // ms
    /// Retry the job with exponential backoff time. Example: 1s, 2s, 4s, 8s, 16s ...
    /// This stratgy is a good fit retry solution if you fail to access http request with rate limit.
    ExponentialBackoff(#[serde_as(as = "serde_with::DurationMicroSeconds<i64>")] Duration), // ms
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retry {
    retried_times: i32,
    // None = never stop retry
    max_retries: Option<i32>,
    strategy: RetryStrategy,
}

impl Default for Retry {
    fn default() -> Self {
        Self::new_interval_retry(Some(3), chrono::TimeDelta::try_milliseconds(100).unwrap())
    }
}

impl Retry {
    pub fn new(retried_times: i32, max_retries: Option<i32>, strategy: RetryStrategy) -> Self {
        Self {
            retried_times,
            max_retries,
            strategy,
        }
    }

    pub fn new_interval_retry(max_retries: Option<i32>, interval: Duration) -> Self {
        Self::new(0, max_retries, RetryStrategy::Interval(interval))
    }

    pub fn new_exponential_backoff(max_retries: Option<i32>, initial_backoff: Duration) -> Self {
        let strategy = RetryStrategy::ExponentialBackoff(initial_backoff);
        Self::new(0, max_retries, strategy)
    }

    pub fn should_retry(&self) -> bool {
        if let Some(max_retries) = self.max_retries {
            self.retried_times < max_retries
        } else {
            true
        }
    }

    pub fn retry_at(&mut self, now: Option<DateTime<Utc>>) -> DateTime<Utc> {
        self.retried_times += 1;
        let final_now = now.unwrap_or(get_now());
        match &self.strategy {
            RetryStrategy::Interval(internal_time) => final_now + *internal_time,
            RetryStrategy::ExponentialBackoff(initial_backoff) => {
                final_now + *initial_backoff * 2_i32.pow(self.retried_times as u32)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    // Helper function to mock `get_now` and return a fixed time.
    fn mock_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn test_default_retry() {
        let retry = Retry::default();
        assert_eq!(retry.retried_times, 0);
        assert_eq!(retry.max_retries, Some(3));

        match retry.strategy {
            RetryStrategy::Interval(duration) => {
                assert_eq!(duration, Duration::try_milliseconds(100).unwrap());
            }
            _ => panic!("Default Retry should use Internal strategy"),
        }
    }

    #[test]
    fn test_exponential_backoff_retry() {
        let initial_backoff = Duration::seconds(5);
        let now = mock_now();
        let max_retries = 3;

        let mut retry = Retry::new_exponential_backoff(Some(max_retries), initial_backoff);

        for retry_times in 1..=max_retries {
            assert!(retry.should_retry());

            let first_retry = retry.retry_at(Some(now));
            assert_eq!(
                now + initial_backoff * 2_i32.pow(retry_times as u32),
                first_retry
            );
        }

        assert!(!retry.should_retry());
    }

    #[test]
    fn test_should_retry_with_max_retries() {
        let retry = Retry::new(
            2,
            Some(3),
            RetryStrategy::Interval(Duration::try_milliseconds(100).unwrap()),
        );
        assert!(retry.should_retry());

        let retry = Retry::new(
            3,
            Some(3),
            RetryStrategy::Interval(Duration::try_milliseconds(100).unwrap()),
        );
        assert!(!retry.should_retry());
    }

    #[test]
    fn test_should_retry_with_no_max_retries() {
        let retry = Retry::new(
            2,
            None,
            RetryStrategy::Interval(Duration::try_milliseconds(100).unwrap()),
        );
        assert!(retry.should_retry()); // Should retry indefinitely
    }

    #[test]
    fn test_retry_at_increments_retried_times() {
        let mut retry = Retry::new(
            0,
            Some(3),
            RetryStrategy::Interval(Duration::try_milliseconds(100).unwrap()),
        );
        let before = retry.retried_times;
        let next_retry_time = retry.retry_at(Some(mock_now()));

        // Check retried times has been incremented
        assert_eq!(retry.retried_times, before + 1);

        // Mock current time and check retry_at is calculated correctly
        let expected_retry_time = mock_now() + Duration::try_milliseconds(100).unwrap();
        assert_eq!(next_retry_time, expected_retry_time);
    }
}
