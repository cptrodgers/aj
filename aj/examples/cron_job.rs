use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        serde::{Deserialize, Serialize},
    },
    main, BackgroundJob, Executable, JobBuilder, JobContext, AJ,
};
use aj_core::get_now_as_secs;

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct AJob;

#[async_trait]
impl Executable for AJob {
    type Output = ();

    async fn execute(&mut self, _context: &JobContext) -> Self::Output {
        println!("Hello Job {}", get_now_as_secs());
    }
}

#[main]
async fn main() {
    AJ::quick_start();

    println!("Start time {}", get_now_as_secs());
    let _ = AJob
        .job_builder()
        .cron("* * * * * * *")
        .build()
        .unwrap()
        .run()
        .await;

    sleep(std::time::Duration::from_secs(5)).await;
}
