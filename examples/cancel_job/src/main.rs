use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        serde::{Deserialize, Serialize},
    },
    main, BackgroundJob, Executable, JobBuilder, JobContext, AJ,
};
use chrono::{DateTime, Utc};

pub fn get_now() -> DateTime<Utc> {
    Utc::now()
}

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = ();

    async fn execute(&self, _context: &JobContext) -> Self::Output {
        println!("Hello Job {}, {}", self.number, get_now());
    }
}

#[main]
async fn main() {
    AJ::quick_start();

    // Cron
    let job_id = Print { number: 3 }
        .job_builder()
        .cron("* * * * * * *")
        .build()
        .unwrap()
        .run()
        .await
        .unwrap();

    let _ = AJ::cancel_job::<Print>(job_id).await;

    sleep(std::time::Duration::from_secs(5)).await;
}
