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

fn run_a_job() {
    let message = AJob;
    // use `do_run` can start your background job in non async function
    message.job_builder().build().unwrap().do_run();
}

async fn run_a_job_in_async() {
    let message = AJob;
    let _ = message.job_builder().build().unwrap().run().await;
}

#[main]
async fn main() {
    AJ::quick_start();

    run_a_job_in_async().await;
    run_a_job();

    sleep(Duration::from_secs(1)).await;
}
