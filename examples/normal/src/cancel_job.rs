use aj::{BackgroundJob, AJ};

use crate::default_print_job::Print;

pub async fn run() {
    // Cron
    let job_id = Print { number: 3 }
        .job()
        .cron("* * * * * * *")
        .run()
        .await
        .unwrap();

    let success = AJ::cancel_job::<Print>(job_id).await.is_ok();
    println!("Cancel: {success}");

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
}
