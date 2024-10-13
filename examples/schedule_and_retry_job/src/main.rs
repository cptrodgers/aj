use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        chrono::Duration,
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

    // Delay 1 sec and run
    let _ = Print { number: 1 }
        .job_builder()
        .delay(Duration::seconds(1))
        .build()
        .unwrap()
        .run()
        .await;

    // Schedule after 2 seconds
    let _ = Print { number: 2 }
        .job_builder()
        .schedule_at(get_now() + Duration::seconds(3))
        .build()
        .unwrap()
        .run()
        .await;

    // Cron
    let _ = Print { number: 3 }
        .job_builder()
        .cron("* * * * * * *")
        .build()
        .unwrap()
        .run()
        .await;

    sleep(std::time::Duration::from_secs(5)).await;
}
