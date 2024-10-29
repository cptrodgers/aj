use std::any::TypeId;

use async_trait::async_trait;

use crate::{Executable, Job, JobStatus};

#[async_trait]
pub trait JobPlugin {
    // Status Change
    async fn change_status(&self, _job_id: &str, _status: JobStatus) {}

    // Run before queue start
    async fn before_run(&self, _job_id: &str) {}

    // Run after queue start
    async fn after_run(&self, _job_id: &str) {}
}

pub struct JobPluginWrapper {
    hook: Box<dyn JobPlugin + Send + Sync + 'static>,
    job_type_ids: Vec<TypeId>,
}

impl JobPluginWrapper {
    pub(crate) async fn change_status<M: Executable + Clone + 'static>(
        &self,
        job_id: &str,
        status: JobStatus,
    ) {
        let type_id = TypeId::of::<Job<M>>();
        if self.should_run(type_id) {
            self.hook.change_status(job_id, status).await;
        }
    }

    pub(crate) async fn before_run<M: Executable + Clone + 'static>(&self, job_id: &str) {
        let type_id = TypeId::of::<Job<M>>();
        if self.should_run(type_id) {
            self.hook.before_run(job_id).await;
        }
    }

    pub(crate) async fn after_run<M: Executable + Clone + 'static>(&self, job_id: &str) {
        let type_id = TypeId::of::<Job<M>>();
        if self.should_run(type_id) {
            self.hook.after_run(job_id).await;
        }
    }

    pub(crate) fn should_run(&self, job_type_id: TypeId) -> bool {
        if self.job_type_ids.is_empty() {
            return true;
        }

        self.job_type_ids.contains(&job_type_id)
    }
}
