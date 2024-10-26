pub mod print_job;
pub mod macro_job;
pub mod cancel_job;
pub mod cron_job;
pub mod schedule_job;
pub mod update_job;
pub mod retry_job;

use aj::main;

#[main]
async fn main() {
    macro_job::run();
}
