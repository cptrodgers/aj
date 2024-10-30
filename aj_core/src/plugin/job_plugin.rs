use std::any::TypeId;

use async_trait::async_trait;

use crate::{Executable, Job, JobStatus};

/// Implement Plugin to catch job hook event
/// ```ignore
/// pub struct MyHook;
///
/// #[async_trait]
/// impl JobPlugin for MyHook {
///     async fn change_status(&self, job_id: &str, status: JobStatus) {
///         println!("Job {job_id} status: {status}");
///     }
///
///     async fn before_run(&self, job_id: &str) {
///         println!("Before Job {job_id} run");
///     }
///
///     async fn after_run(&self, job_id: &str) {
///         println!("After Job {job_id} run");
///     }
/// }
///
/// AJ::register_plugin(MyHook);
/// ```
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
    pub(crate) fn new(
        plugin: impl JobPlugin + Send + Sync + 'static,
        job_type_ids: Vec<TypeId>,
    ) -> Self {
        let hook = Box::new(plugin);
        Self { hook, job_type_ids }
    }

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
