use std::time::Duration;

use aj::{
    async_trait::async_trait,
    serde::{Deserialize, Serialize},
    BackgroundJob, Executable, JobBuilder, JobContext, AJ,
    actix_rt::time::sleep,
    main,
};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct AJob;

#[async_trait]
impl Executable for AJob {
    type Output = ();

    async fn execute(&self, _context: &JobContext) -> Self::Output {
        println!("Hello Job");
    }
}

#[main]
async fn main() {
    AJ::quick_start();
    let message = AJob;

    // Spawn a thread and wait 1 sec to view
    let _ = message.job_builder().build().unwrap().run_background().await;
    sleep(Duration::from_secs(1)).await;
}
