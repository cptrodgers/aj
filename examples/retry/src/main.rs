use std::time::Duration;

use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        retry::Retry,
        serde::{Deserialize, Serialize},
    },
    job::JobStatus,
    main, BackgroundJob, Executable, JobBuilder, JobContext, AJ,
};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = Result<(), String>;

    async fn execute(&mut self, context: &JobContext) -> Self::Output {
        println!("Hello {}, {}", self.number, context.run_count);
        Err("I'm failing".into())
    }

    // Determine where your job is failed.
    // For example, check job output is return Err type
    async fn is_failed_output(&self, job_output: &Self::Output) -> bool {
        job_output.is_err()
    }
}

#[main]
async fn main() {
    AJ::quick_start();

    let max_retries = 3;
    // Retry 3 times -> Maximum do the job 4 times.
    let job = Print { number: 1 }
        .job_builder()
        .retry(Retry::new_interval_retry(
            Some(max_retries),
            chrono::Duration::seconds(1),
        ))
        .build()
        .unwrap();
    let job_id = job.run().await.unwrap();

    sleep(Duration::from_secs(5)).await;

    // Get Job
    let job = AJ::get_job::<Print>(&job_id).await.unwrap();
    assert_eq!(job.context.job_status, JobStatus::Failed);
    assert_eq!(job.context.run_count, 4); // 3 tries, 1 original.

    // Manual Retry
    println!("Manual Retry");
    AJ::retry_job::<Print>(&job_id).await.unwrap();
    sleep(Duration::from_secs(1)).await;

    // Exponential Retry 3 times -> Maximum do the job 4 times.
    let job = Print { number: 3 }
        .job_builder()
        .retry(Retry::new_exponential_backoff(
            Some(max_retries),
            chrono::Duration::seconds(1),
        ))
        .build()
        .unwrap();
    let _ = job.run().await.unwrap();
    // Run at 0s, Retry at after first job 1s, after second job 2s,.... 4s
    sleep(Duration::from_secs(5)).await;
}
