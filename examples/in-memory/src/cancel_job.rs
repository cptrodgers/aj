use aj::{
    export::core::actix_rt::time::sleep,
    BackgroundJob, AJ,
};

use crate::print_job::Print;

pub async fn run_cancel_job_examples() {
    // Cron
    let job_id = Print { number: 3 }
        .job()
        .cron("* * * * * * *")
        .run()
        .await
        .unwrap();

    let success = AJ::cancel_job::<Print>(job_id).await.is_ok();
    println!("Cancel: {success}");

    sleep(std::time::Duration::from_secs(1)).await;
}
