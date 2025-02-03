use aj::{
    async_trait,
    export::core::serde::{Deserialize, Serialize},
    BackgroundJob, Executable, JobContext,
};
use chrono::{DateTime, Utc};

pub fn get_now() -> DateTime<Utc> {
    Utc::now()
}

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    pub number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = Result<(), ()>;

    async fn execute(&mut self, _context: &JobContext) -> Self::Output {
        println!("Hello Job {}, {}", self.number, get_now());
        Err(())
    }

    // Optional: Determine where your job is failed.
    // This function will be useful to control retry logic.
    async fn is_failed_output(&self, job_output: &Self::Output) -> bool {
        job_output.is_err()
    }
}
