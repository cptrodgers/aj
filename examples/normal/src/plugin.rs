use aj::{async_trait, job::JobStatus, JobPlugin};

pub struct SamplePlugin;

#[async_trait]
impl JobPlugin for SamplePlugin {
    async fn change_status(&self, job_id: &str, job_status: JobStatus) {
        println!("Hello, Job {job_id} change status to {job_status:?}");
    }

    async fn before_run(&self, job_id: &str) {
        println!("Before job {job_id} run");
    }

    async fn after_run(&self, job_id: &str) {
        println!("After job {job_id} run");
    }
}
