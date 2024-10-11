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

    async fn execute(&self, _context: &JobContext) -> Self::Output {
        print!("Hello, this is Rodgers");
    }
}

fn main() {
    AJ::quick_start();
    let job = JobBuilder::default().data(AJob).build().unwrap();

    std::thread::spawn(|| {
        System::new().block_on(async {
            let _ = job.apply().await;
            sleep(Duration::from_secs(1)).await;
        })
    })
    .join()
    .expect("Cannot spawn thread");
}
