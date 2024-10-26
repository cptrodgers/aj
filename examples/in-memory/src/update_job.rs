use actix_rt::time::sleep;
use aj::{BackgroundJob, AJ};

use crate::print_job::Print;

pub async fn run() {
    // Run cron job every secs
    let job_id = Print { number: 1 }
        .job()
        .cron("* * * * * * *")
        .run()
        .await
        .unwrap();

    // Update print 2
    AJ::update_job(&job_id, Print { number: 2 }, None)
        .await
        .unwrap();

    sleep(std::time::Duration::from_secs(3)).await;
}
