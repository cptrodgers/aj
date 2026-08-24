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
    pub(crate) hook: Box<dyn JobPlugin + Send + Sync + 'static>,
    // TODO: the per-job-type filter below is not reachable yet. `AJ::register_plugin` always
    // passes an empty `job_type_ids`, and `PluginCenter` invokes `hook` directly instead of
    // going through this wrapper, so `should_run` is never consulted. Kept until we decide
    // whether to wire it up or drop it.
    #[allow(dead_code)]
    job_type_ids: Vec<TypeId>,
}

// See the note on `job_type_ids`: these type-filtered wrappers have no production caller yet.
#[allow(dead_code)]
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

    use super::{JobPlugin, JobPluginWrapper};
    use crate::{Executable, Job, JobContext, JobStatus};

    /// Records the job id of the last hook invocation. Each test builds its own instance, so
    /// the tests stay independent when the suite runs in parallel. The state lives behind an
    /// `Arc`, so cloning before handing the plugin to `JobPluginWrapper` keeps a read handle.
    #[derive(Clone, Default)]
    pub struct SimplePlugin {
        last_job_id: Arc<Mutex<String>>,
    }

    impl SimplePlugin {
        fn record(&self, job_id: &str) {
            *self.last_job_id.lock().expect("lock poisoned") = job_id.to_string();
        }

        /// Clones the value out so no guard is held across an assertion; a failing assert
        /// would otherwise poison the mutex and cascade into the other tests.
        fn last_job_id(&self) -> String {
            self.last_job_id.lock().expect("lock poisoned").clone()
        }
    }

    #[async_trait]
    impl JobPlugin for SimplePlugin {
        async fn change_status(&self, job_id: &str, _status: JobStatus) {
            self.record(job_id);
        }

        async fn before_run(&self, job_id: &str) {
            self.record(job_id);
        }

        async fn after_run(&self, job_id: &str) {
            self.record(job_id);
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
        let plugin = JobPluginWrapper::new(SimplePlugin::default(), vec![]);
        assert!(plugin.should_run(TypeId::of::<JobA>()));
        assert!(plugin.should_run(TypeId::of::<B>()));

        // Only run for specific registered type
        let plugin_2 = JobPluginWrapper::new(SimplePlugin::default(), vec![TypeId::of::<JobA>()]);
        assert!(plugin_2.should_run(TypeId::of::<JobA>()));
        assert!(!plugin_2.should_run(TypeId::of::<B>()));
    }

    #[tokio::test]
    async fn test_change_status_hook() {
        let plugin = SimplePlugin::default();
        // Plugin apply for all Job
        let wrapper = JobPluginWrapper::new(plugin.clone(), vec![]);
        wrapper
            .change_status::<JobA>("job_status", JobStatus::Failed)
            .await;

        assert_eq!(plugin.last_job_id(), "job_status");
    }

    #[tokio::test]
    async fn test_change_before_run() {
        let plugin = SimplePlugin::default();
        // Plugin apply for all Job
        let wrapper = JobPluginWrapper::new(plugin.clone(), vec![]);
        wrapper.before_run::<JobA>("job_before").await;

        assert_eq!(plugin.last_job_id(), "job_before");
    }

    #[tokio::test]
    async fn test_change_after_run() {
        let plugin = SimplePlugin::default();
        // Plugin apply for all Job
        let wrapper = JobPluginWrapper::new(plugin.clone(), vec![]);
        wrapper.after_run::<JobA>("job_after").await;

        assert_eq!(plugin.last_job_id(), "job_after");
    }

    #[tokio::test]
    async fn test_hooks_skipped_for_unregistered_job_type() {
        #[derive(Clone)]
        struct JobB;

        #[async_trait]
        impl Executable for JobB {
            type Output = ();

            async fn execute(&mut self, _: &JobContext) {}
        }

        let plugin = SimplePlugin::default();
        // Registered for JobA only, so JobB must not reach the hook.
        let wrapper = JobPluginWrapper::new(plugin.clone(), vec![TypeId::of::<Job<JobA>>()]);

        wrapper.before_run::<JobB>("job_b").await;
        assert_eq!(plugin.last_job_id(), "");

        wrapper.before_run::<JobA>("job_a").await;
        assert_eq!(plugin.last_job_id(), "job_a");
    }
}
