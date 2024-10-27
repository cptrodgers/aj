use actix_rt::time::sleep;
use aj::BackgroundJob;

use crate::print_job::Print;

pub async fn run() {
    // Cron
    let _ = Print { number: 3 }.job().cron("* * * * * * *").run().await;

    sleep(std::time::Duration::from_secs(3)).await;
}
