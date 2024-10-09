use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use crate::util::get_now;

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RetryStrategy {
    Interval(#[serde_as(as = "serde_with::DurationMicroSeconds<i64>")] Duration), // ms
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
            RetryStrategy::Interval(ms) => final_now + *ms,
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
        
        if let RetryStrategy::Interval(duration) = retry.strategy {
            assert_eq!(duration, Duration::milliseconds(100));
        } else {
            panic!("Unexpected retry strategy");
        }
    }

    #[test]
    fn test_should_retry_with_max_retries() {
        let retry = Retry::new(2, Some(3), RetryStrategy::Interval(Duration::milliseconds(100)));
        assert!(retry.should_retry());
        
        let retry = Retry::new(3, Some(3), RetryStrategy::Interval(Duration::milliseconds(100)));
        assert!(!retry.should_retry());
    }

    #[test]
    fn test_should_retry_with_no_max_retries() {
        let retry = Retry::new(2, None, RetryStrategy::Interval(Duration::milliseconds(100)));
        assert!(retry.should_retry()); // Should retry indefinitely
    }

    #[test]
    fn test_retry_at_increments_retried_times() {
        let mut retry = Retry::new(0, Some(3), RetryStrategy::Interval(Duration::milliseconds(100)));
        let before = retry.retried_times;
        let next_retry_time = retry.retry_at(Some(mock_now()));

        // Check retried times has been incremented
        assert_eq!(retry.retried_times, before + 1);

        // Mock current time and check retry_at is calculated correctly
        let expected_retry_time = mock_now() + Duration::milliseconds(100);
        assert_eq!(next_retry_time, expected_retry_time);
    }
}
