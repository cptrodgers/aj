use std::time::Duration;

use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        serde::{Deserialize, Serialize},
    },
    main, BackgroundJob, Executable, JobContext, AJ,
};
use aj_core::retry::Retry;

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = Result<(), String>;

    async fn execute(&mut self, _context: &JobContext) -> Self::Output {
        println!("Hello {}", self.number);
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
    let job = Print { number: 1 }.job().retry(Retry::new_interval_retry(
        Some(max_retries),
        aj_core::chrono::Duration::seconds(1),
    ));
    let _ = job.run().await;

    sleep(Duration::from_secs(5)).await;
}
