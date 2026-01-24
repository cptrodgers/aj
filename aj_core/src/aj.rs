use dashmap::DashMap;
use kameo::actor::ActorRef;
use kameo::message::{Context, Message};
use kameo::Actor;
use lazy_static::lazy_static;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::any::{Any, TypeId};
use std::sync::{Arc, RwLock};

use crate::job::Job;
use crate::mem::InMemory;
use crate::queue::{cancel_job, enqueue_job, WorkQueue};
use crate::types::Backend;
use crate::{
    get_job, retry_job, update_work_queue_config, BackgroundJob, EnqueueConfig, Error, Executable,
    JobContext, JobPlugin, JobPluginWrapper, PluginCenter, WorkQueueConfig,
};

lazy_static! {
    static ref QUEUE_REGISTRY: Registry = Registry::default();
}

lazy_static! {
    static ref AJ_BACKEND: Arc<RwLock<Option<Arc<dyn Backend + Send + Sync + 'static>>>> =
        Arc::new(RwLock::new(None));
}

lazy_static! {
    static ref AJ_ADDR: Arc<RwLock<Option<ActorRef<AJ>>>> = Arc::new(RwLock::new(None));
}

#[derive(Debug, Default)]
pub struct Registry {
    registry: DashMap<TypeId, Box<dyn Any + Send + Sync>>,
    registry_by_name: DashMap<String, Box<dyn Any + Send + Sync>>,
}

fn get_backend() -> Option<Arc<dyn Backend + Send + Sync + 'static>> {
    if let Ok(backend) = AJ_BACKEND.try_read() {
        backend.clone()
    } else {
        None
    }
}

pub fn get_work_queue_address<M>() -> Option<ActorRef<WorkQueue<M>>>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    let type_id = TypeId::of::<M>();
    if let Some(queue_addr) = QUEUE_REGISTRY.registry.get(&type_id) {
        if let Some(addr) = queue_addr.downcast_ref::<ActorRef<WorkQueue<M>>>() {
            return Some(addr.clone());
        }
    }

    None
}

fn register_work_queue<M>(queue_name: &str) -> ActorRef<WorkQueue<M>>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    let type_id = TypeId::of::<M>();
    let registry = &QUEUE_REGISTRY;

    // Check if already registered
    if let Some(queue_addr) = registry.registry.get(&type_id) {
        if let Some(addr) = queue_addr.downcast_ref::<ActorRef<WorkQueue<M>>>() {
            return addr.clone();
        }
    }

    let backend = get_backend().expect("AJ is not started, please start it via AJ::start");

    // Start Queue actor
    let queue_ref = WorkQueue::<M>::start_with_name(queue_name.into(), backend);
    registry
        .registry
        .insert(type_id, Box::new(queue_ref.clone()));
    registry
        .registry_by_name
        .insert(queue_name.into(), Box::new(queue_ref.clone()));

    queue_ref
}

pub fn get_aj_address() -> Option<ActorRef<AJ>> {
    if let Ok(addr) = AJ_ADDR.try_read() {
        addr.clone()
    } else {
        None
    }
}

#[derive(Actor)]
pub struct AJ {}

impl AJ {
    /// Start AJ with a custom backend
    pub fn start(backend: impl Backend + Send + Sync + 'static) -> ActorRef<Self> {
        if let Some(aj_addr) = get_aj_address() {
            warn!("AJ is running. Return current AJ");
            return aj_addr;
        }

        // Store backend globally
        if let Ok(ref mut backend_ref) = AJ_BACKEND.try_write() {
            **backend_ref = Some(Arc::new(backend));
        }

        let actor = AJ {};
        let actor_ref = kameo::spawn(actor);

        if let Ok(ref mut aj_addr) = AJ_ADDR.try_write() {
            **aj_addr = Some(actor_ref.clone());
        }

        actor_ref
    }

    /// Quick start AJ with in-memory backend
    pub fn quick_start() -> ActorRef<Self> {
        Self::start(InMemory::default())
    }

    pub async fn enqueue_job<M>(
        job: Job<M>,
        config: EnqueueConfig,
        queue_name: &str,
    ) -> Result<(), Error>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    {
        let actor_ref = if let Some(actor_ref) = get_work_queue_address() {
            actor_ref
        } else {
            info!("Not found WorkQueue for {}, creating...", queue_name);
            register_work_queue::<M>(queue_name)
        };
        enqueue_job(actor_ref, job, config).await
    }

    pub async fn cancel_job<M>(job_id: String) -> Result<(), Error>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    {
        let actor_ref: Option<ActorRef<WorkQueue<M>>> = get_work_queue_address();
        if let Some(queue_ref) = actor_ref {
            cancel_job(queue_ref, job_id).await
        } else {
            Err(Error::NoQueueRegister)
        }
    }

    pub async fn get_job<M>(job_id: &str) -> Option<Job<M>>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    {
        let actor_ref: Option<ActorRef<WorkQueue<M>>> = get_work_queue_address();
        if let Some(queue_ref) = actor_ref {
            get_job(queue_ref, job_id).await
        } else {
            None
        }
    }

    pub async fn update_job<M>(
        job_id: &str,
        data: M,
        context: Option<JobContext>,
    ) -> Result<(), Error>
    where
        M: Executable
            + BackgroundJob
            + Send
            + Sync
            + Clone
            + Serialize
            + DeserializeOwned
            + 'static,
    {
        let job = Self::get_job::<M>(job_id).await;
        if let Some(mut job) = job {
            job.data = data;
            if let Some(context) = context {
                job.context = context;
            }
            Self::add_job(job, M::queue_name()).await?;
        } else {
            warn!("Cannot update non existing job {job_id}");
        }

        Ok(())
    }

    pub async fn retry_job<M>(job_id: &str) -> Result<bool, Error>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    {
        let actor_ref: Option<ActorRef<WorkQueue<M>>> = get_work_queue_address();
        if let Some(queue_ref) = actor_ref {
            retry_job(queue_ref, job_id).await
        } else {
            Err(Error::NoQueueRegister)
        }
    }

    pub async fn add_job<M>(job: Job<M>, queue_name: &str) -> Result<String, Error>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    {
        let job_id = job.id().to_string();
        let config = EnqueueConfig::new_re_run();
        Self::enqueue_job(job, config, queue_name).await?;
        Ok(job_id)
    }

    pub async fn update_work_queue<M>(config: WorkQueueConfig) -> Result<(), Error>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    {
        let actor_ref: Option<ActorRef<WorkQueue<M>>> = get_work_queue_address();
        if let Some(queue_ref) = actor_ref {
            update_work_queue_config(queue_ref, config).await
        } else {
            Err(Error::NoQueueRegister)
        }
    }

    pub async fn register_plugin(
        plugin: impl JobPlugin + Send + Sync + 'static,
    ) -> Result<(), Error> {
        let wrapper = JobPluginWrapper::new(plugin, vec![]);
        PluginCenter::register(wrapper).await
    }
}

// Message: JustRunJob (fire and forget style)
pub struct JustRunJob<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    pub job: Job<M>,
    pub queue_name: String,
}

impl<M> Message<JustRunJob<M>> for AJ
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    type Reply = ();

    async fn handle(
        &mut self,
        msg: JustRunJob<M>,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        if let Err(reason) = AJ::add_job(msg.job, &msg.queue_name).await {
            error!("Cannot start job {reason:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{get_aj_address, AJ};

    #[tokio::test]
    async fn test_start_aj_under_tokio_runtime() {
        let _actor_ref = AJ::quick_start();
        let register_ref = get_aj_address();

        assert!(register_ref.is_some());
    }
}
