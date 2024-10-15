use std::time::Duration;

use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        retry::Retry,
        serde::{Deserialize, Serialize},
    },
    main, BackgroundJob, Executable, JobBuilder, JobContext, AJ,
};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = Result<(), String>;

    async fn execute(&self, context: &JobContext) -> Self::Output {
        println!("Hello {}, {}", self.number, context.run_count);
        Err("I'm failing".into())
    }

    // Determine where your job is failed.
    // For example, check job output is return Err type
    async fn is_failed_output(&self, job_output: Self::Output) -> bool {
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
    let _ = job.run().await;

    sleep(Duration::from_secs(5)).await;
}