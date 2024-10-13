use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        chrono::Duration,
        serde::{Deserialize, Serialize},
    },
    main, BackgroundJob, Executable, JobBuilder, JobContext, AJ,
};
use aj_core::{get_now, get_now_as_secs};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct AJob;

#[async_trait]
impl Executable for AJob {
    type Output = ();

    async fn execute(&self, _context: &JobContext) -> Self::Output {
        println!("Hello Job {}", get_now_as_secs());
    }
}

#[main]
async fn main() {
    AJ::quick_start();

    println!("Start time {}", get_now_as_secs());
    let _ = AJob
        .job_builder()
        .delay(Duration::seconds(1))
        .build()
        .unwrap()
        .run()
        .await;

    AJob.job_builder()
        .schedule_at(get_now() + Duration::seconds(2))
        .build()
        .unwrap()
        .just_run();

    sleep(std::time::Duration::from_secs(3)).await;
}
