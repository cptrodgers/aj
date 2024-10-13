use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        redis::Redis,
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
    AJ::start(Redis::new("redis://localhost:6379"));

    // Run cron job every secs
    let job_id = Print { number: 1 }
        .job_builder()
        .cron("* * * * * * *")
        .build()
        .unwrap()
        .run()
        .await
        .unwrap();

    // Update print 2
    AJ::update_job(&job_id, Print { number: 2 }, None)
        .await
        .unwrap();

    sleep(std::time::Duration::from_secs(5)).await;
}
