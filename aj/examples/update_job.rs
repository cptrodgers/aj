use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        chrono::Duration,
        serde::{Deserialize, Serialize},
    },
    main, BackgroundJob, Executable, JobContext, AJ,
};
use aj_core::get_now_as_secs;

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = ();

    async fn execute(&mut self, _context: &JobContext) -> Self::Output {
        println!("Print {} - {}", self.number, get_now_as_secs());
    }
}

#[main]
async fn main() {
    AJ::quick_start();

    println!("Start time {}", get_now_as_secs());
    // Start a job, get job_id back
    let job_id = Print { number: 1 }
        .job()
        .delay(Duration::seconds(1))
        .run()
        .await
        .unwrap();

    // Update job data by re using job_id
    AJ::update_job(&job_id, Print { number: 2 }, None)
        .await
        .unwrap();

    sleep(std::time::Duration::from_secs(2)).await;
}
