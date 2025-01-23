use actix_rt::time::sleep;
use aj::BackgroundJob;
use chrono::Duration;

use crate::default_print_job::{get_now, Print};

pub async fn run() {
    // Delay 1 sec and run
    let _ = Print { number: 1 }
        .job()
        .delay(Duration::seconds(1))
        .run()
        .await;

    // Schedule after 2 seconds
    let _ = Print { number: 2 }
        .job()
        .schedule_at(get_now() + Duration::seconds(3))
        .run()
        .await;

    // Cron
    let _ = Print { number: 3 }.job().cron("* * * * * * *").run().await;

    sleep(std::time::Duration::from_secs(5)).await;
}
