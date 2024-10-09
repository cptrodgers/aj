use chrono::{DateTime, TimeZone, Utc};

pub fn get_now() -> DateTime<Utc> {
    Utc::now()
}

pub fn get_now_as_secs() -> i64 {
    get_now().timestamp()
}

pub fn get_now_as_ms() -> i64 {
    get_now().timestamp_millis()
}

pub fn get_ms_as_datetime(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_nanos(ms * 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_get_now() {
        // Just ensure that `get_now` returns a valid DateTime that is close to the current time.
        let now = get_now();
        let system_now = Utc::now();
        // Allow for a slight difference in time
        assert!((system_now - now).num_seconds().abs() < 1);
    }

    #[test]
    fn test_get_now_as_secs() {
        let now_secs = get_now_as_secs();
        let system_now_secs = Utc::now().timestamp();
        // Check that the difference between system time and function output is small
        assert!((system_now_secs - now_secs).abs() < 1);
    }

    #[test]
    fn test_get_now_as_ms() {
        let now_ms = get_now_as_ms();
        let system_now_ms = Utc::now().timestamp_millis();
        // Check that the difference between system time in milliseconds is small
        assert!((system_now_ms - now_ms).abs() < 1000); // Allowing for a slight delay in ms
    }

    #[test]
    fn test_get_ms_as_datetime() {
        // Test conversion of milliseconds to DateTime
        let now_ms = get_now_as_ms();
        let datetime = get_ms_as_datetime(now_ms);
        let system_datetime = Utc::now();
        // Check if the difference is less than a second
        assert!((system_datetime.timestamp_millis() - datetime.timestamp_millis()).abs() < 1000);
    }

    #[test]
    fn test_get_ms_as_datetime_specific_value() {
        // Test conversion of a specific known timestamp (for reproducibility)
        let ms = 1_600_000_000_000; // Example timestamp in milliseconds
        let datetime = get_ms_as_datetime(ms);
        let expected_datetime = Utc.timestamp(1_600_000_000, 0); // Expected equivalent DateTime
        assert_eq!(datetime, expected_datetime);
    }
}
