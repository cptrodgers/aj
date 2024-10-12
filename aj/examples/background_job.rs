use std::time::Duration;

use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        serde::{Deserialize, Serialize},
    },
    main, BackgroundJob, Executable, JobBuilder, JobContext, AJ,
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

    let _ = message
        .job_builder()
        .build()
        .unwrap()
        .run_background()
        .await;
    sleep(Duration::from_secs(1)).await;
}
