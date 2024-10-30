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

#[cfg(test)]
mod tests {
    use std::{
        any::TypeId,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use lazy_static::lazy_static;

    use super::{JobPlugin, JobPluginWrapper};
    use crate::{Executable, JobContext, JobStatus};

    lazy_static! {
        static ref NUMBER: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));
        static ref JOB_ID: Arc<Mutex<String>> = Arc::new(Mutex::new("".into()));
    }

    pub struct SimplePlugin;

    #[async_trait]
    impl JobPlugin for SimplePlugin {
        async fn change_status(&self, job_id: &str, _status: JobStatus) {
            if let Ok(job_id_ref) = JOB_ID.lock().as_mut() {
                **job_id_ref = job_id.to_string();
            }
        }

        async fn before_run(&self, job_id: &str) {
            if let Ok(job_id_ref) = JOB_ID.lock().as_mut() {
                **job_id_ref = job_id.to_string();
            }
        }

        async fn after_run(&self, job_id: &str) {
            if let Ok(job_id_ref) = JOB_ID.lock().as_mut() {
                **job_id_ref = job_id.to_string();
            }
        }
    }

    #[derive(Clone)]
    pub struct JobA;

    #[async_trait]
    impl Executable for JobA {
        type Output = ();

        async fn execute(&mut self, _: &JobContext) {}
    }

    #[test]
    fn test_should_run() {
        pub struct B;
        // Plugin apply for all Job
        let plugin = JobPluginWrapper::new(SimplePlugin, vec![]);
        assert!(plugin.should_run(TypeId::of::<JobA>()));
        assert!(plugin.should_run(TypeId::of::<B>()));

        // Only run for specific registered type
        let plugin_2 = JobPluginWrapper::new(SimplePlugin, vec![TypeId::of::<JobA>()]);
        assert!(plugin_2.should_run(TypeId::of::<JobA>()));
        assert_eq!(plugin_2.should_run(TypeId::of::<B>()), false);
    }

    #[actix_rt::test]
    async fn test_change_status_hook() {
        // Plugin apply for all Job
        let plugin = JobPluginWrapper::new(SimplePlugin, vec![]);
        plugin
            .change_status::<JobA>("job_status", JobStatus::Failed)
            .await;

        assert_eq!(*JOB_ID.lock().unwrap(), "job_status");
    }

    #[actix_rt::test]
    async fn test_change_before_run() {
        // Plugin apply for all Job
        let plugin = JobPluginWrapper::new(SimplePlugin, vec![]);
        plugin
            .change_status::<JobA>("job_before", JobStatus::Failed)
            .await;

        assert_eq!(*JOB_ID.lock().unwrap(), "job_before");
    }

    #[actix_rt::test]
    async fn test_change_after_run() {
        // Plugin apply for all Job
        let plugin = JobPluginWrapper::new(SimplePlugin, vec![]);
        plugin
            .change_status::<JobA>("job_after", JobStatus::Failed)
            .await;

        assert_eq!(*JOB_ID.lock().unwrap(), "job_after");
    }
}
