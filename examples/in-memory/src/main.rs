pub mod cancel_job;
pub mod cron_job;
pub mod macro_job;
pub mod print_job;
pub mod retry_job;
pub mod schedule_job;
pub mod update_job;

use aj::{main, redis::Redis, AJ};

#[allow(dead_code)]
fn run_aj_redis_engine() {
    AJ::start(Redis::new("redis://localhost:6379"));
}

#[main]
async fn main() {
    // Run AJ engine with In Memory
    AJ::quick_start();
    // Or, run AJ engine with redis, un comment code below to test
    // run_aj_redis_engine();

    // Simple job, declare by macro
    macro_job::run().await;

    // Schedule job
    schedule_job::run().await;

    // Cron Job
    cron_job::run().await;

    // Cancel job
    cancel_job::run().await;

    // Update Job
    update_job::run().await;

    // Retry Job
    retry_job::run().await;
}
