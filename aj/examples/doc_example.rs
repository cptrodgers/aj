use std::time::Duration;

use aj::{
    async_trait::async_trait,
    serde::{Deserialize, Serialize},
    BackgroundJob, Executable, JobBuilder, JobContext, AJ,
    actix_rt::System,
    actix_rt::time::sleep,
};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct AJob;

#[async_trait]
impl Executable for AJob {
    type Output = ();

    async fn execute(&self, context: &JobContext) -> Self::Output {
        print!("AJob Context {context:?}")
    }
}

fn main() {
    AJ::quick_start();
    let message = AJob;

    std::thread::spawn(|| {
        System::new().block_on(async {
            let _ = message.job_builder().build().unwrap().run_background().await;
            sleep(Duration::from_secs(1)).await;
        })
    })
    .join()
    .expect("Cannot spawn thread");
}
