use actix::*;
use dashmap::DashMap;
use lazy_static::lazy_static;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::any::{Any, TypeId};
use std::marker::PhantomData;
use std::sync::{Arc, RwLock};

use crate::job::Job;
use crate::mem::InMemory;
use crate::queue::{cancel_job, enqueue_job, WorkQueue};
use crate::types::Backend;
use crate::{get_job, retry_job, BackgroundJob, EnqueueConfig, Error, Executable, JobContext};

lazy_static! {
    static ref QUEUE_REGISTRY: Registry = Registry::default();
}

lazy_static! {
    static ref AJ_ADDR: Arc<RwLock<Option<Addr<AJ>>>> = Arc::new(RwLock::new(None));
}

#[derive(Debug, Default)]
pub struct Registry {
    registry: DashMap<TypeId, Box<dyn Any + Send + Sync>>,
    registry_by_name: DashMap<String, Box<dyn Any + Send + Sync>>,
}

pub fn get_work_queue_address<M>() -> Option<Addr<WorkQueue<M>>>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    let type_id = TypeId::of::<M>();
    if let Some(queue_addr) = QUEUE_REGISTRY.registry.get(&type_id) {
        if let Some(addr) = queue_addr.downcast_ref::<Addr<WorkQueue<M>>>() {
            return Some(addr.clone());
        }
    }

    None
}

pub fn get_aj_address() -> Option<Addr<AJ>> {
    if let Ok(addr) = AJ_ADDR.try_read() {
        addr.clone()
    } else {
        None
    }
}

pub struct AJ {
    backend: Arc<dyn Backend + Send + Sync + 'static>,
}

impl Actor for AJ {
    type Context = Context<Self>;
}

impl AJ {
    // Will use memory as Backend for AJ
    pub fn start(backend: impl Backend + Send + Sync + 'static) -> Addr<Self> {
        match System::try_current() {
            Some(_) => {
                info!("Found Actix Runtime, re-use it!");
            }
            None => {
                info!("No Actix Runtime, start new one!");
                let _ = System::new();
            }
        }
        let arbiter: Arbiter = Arbiter::new();
        let addr = <Self as Actor>::start_in_arbiter(&arbiter.handle(), |_| Self {
            backend: Arc::new(backend),
        });

        if let Ok(ref mut aj_addr) = AJ_ADDR.try_write() {
            **aj_addr = Some(addr.clone());
        }

        addr
    }

    pub fn quick_start() -> Addr<Self> {
        Self::start(InMemory::default())
    }

    pub fn register<M>(&self, queue_name: &str) -> Addr<WorkQueue<M>>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
        WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
    {
        let type_id = TypeId::of::<M>();

        let registry = &QUEUE_REGISTRY;
        if registry.registry_by_name.contains_key(queue_name) {
            panic!("You already register queue with name: {}", queue_name);
        }
        if registry.registry.contains_key(&type_id) {
            panic!("You already register queue with type: {:?}", type_id);
        }

        // Start Queue in an arbiter thread
        let queue_addr = WorkQueue::<M>::start_with_name(queue_name.into(), self.backend.clone());
        registry
            .registry
            .insert(type_id, Box::new(queue_addr.clone()));
        registry
            .registry_by_name
            .insert(queue_name.into(), Box::new(queue_addr.clone()));

        queue_addr
    }

    pub async fn enqueue_job<M>(
        job: Job<M>,
        config: EnqueueConfig,
        queue_name: &str,
    ) -> Result<(), Error>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
        WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
    {
        let addr = if let Some(addr) = get_work_queue_address() {
            addr
        } else {
            info!("Not found WorkQueue for {}", stringify!(M));
            let message = InitWorkQueue {
                queue_name: queue_name.into(),
                _type: PhantomData,
            };
            let aj_addr = get_aj_address().expect("AJ is not start, please start it via AJ::start");
            aj_addr.send(message).await?
        };
        enqueue_job(addr, job, config).await
    }

    pub async fn cancel_job<M>(job_id: String) -> Result<(), Error>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,

        WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
    {
        let addr: Option<Addr<WorkQueue<M>>> = get_work_queue_address();
        if let Some(queue_addr) = addr {
            cancel_job(queue_addr, job_id).await
        } else {
            Err(Error::NoQueueRegister)
        }
    }

    pub async fn get_job<M>(job_id: &str) -> Option<Job<M>>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
        WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
    {
        let addr: Option<Addr<WorkQueue<M>>> = get_work_queue_address();
        if let Some(queue_addr) = addr {
            get_job(queue_addr, job_id).await
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
        WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
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
        WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
    {
        let addr: Option<Addr<WorkQueue<M>>> = get_work_queue_address();
        if let Some(queue_addr) = addr {
            retry_job(queue_addr, job_id).await
        } else {
            Err(Error::NoQueueRegister)
        }
    }

    pub async fn add_job<M>(job: Job<M>, queue_name: &str) -> Result<String, Error>
    where
        M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
        WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
    {
        let job_id = job.id.clone();
        let config = EnqueueConfig::new_re_run();
        Self::enqueue_job(job, config, queue_name).await?;
        Ok(job_id)
    }
}

#[derive(Message)]
#[rtype(result = "Addr<WorkQueue<M>>")]
pub struct InitWorkQueue<M>
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    pub queue_name: String,
    _type: PhantomData<M>,
}

impl<M> Handler<InitWorkQueue<M>> for AJ
where
    M: Executable + Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
    WorkQueue<M>: Actor<Context = Context<WorkQueue<M>>>,
{
    type Result = Addr<WorkQueue<M>>;

    fn handle(&mut self, msg: InitWorkQueue<M>, _: &mut Self::Context) -> Self::Result {
        self.register(&msg.queue_name)
    }
}
