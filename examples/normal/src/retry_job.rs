use actix_rt::time::sleep;
use aj::{
    job::{JobStatus, Retry},
    BackgroundJob, AJ,
};

use crate::default_print_job::Print;

pub async fn run() {
    let max_retries = 3;
    // Retry 3 times -> Maximum do the job 4 times.
    let job = Print { number: 1 }.job().retry(Retry::new_interval_retry(
        Some(max_retries),
        chrono::Duration::seconds(1),
    ));
    let job_id = job.run().await.unwrap();

    sleep(std::time::Duration::from_secs(5)).await;

    // Get Job
    let job = AJ::get_job::<Print>(&job_id).await.unwrap();
    assert_eq!(job.context.job_status, JobStatus::Failed);
    assert_eq!(job.context.run_count, 4); // 3 tries, 1 original.

    // Manual Retry
    println!("Manual Retry");
    AJ::retry_job::<Print>(&job_id).await.unwrap();
    sleep(std::time::Duration::from_secs(1)).await;

    // Exponential Retry 3 times -> Maximum do the job 4 times.
    let job = Print { number: 3 }
        .job()
        .retry(Retry::new_exponential_backoff(
            Some(max_retries),
            chrono::Duration::seconds(1),
        ));
    let _ = job.run().await.unwrap();
    // Run at 0s, Retry at after first job 1s, after second job 2s,.... 4s
    sleep(std::time::Duration::from_secs(5)).await;
}
