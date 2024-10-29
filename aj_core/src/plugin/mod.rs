pub mod job_plugin;

pub use job_plugin::*;

use actix::*;
use std::{marker::PhantomData, sync::Arc};

use crate::{Executable, JobStatus};

#[derive(Clone, Default)]
pub struct PluginCenter {
    plugins: Vec<Arc<JobPluginWrapper>>,
}

impl PluginCenter {
    pub async fn register(plugin: JobPluginWrapper) {
        Self::from_registry()
            .send(RegisterPlugin { plugin })
            .await
            .unwrap();
    }

    pub(crate) fn change_status<M>(job_id: String, status: JobStatus)
    where
        M: Executable + Clone + Send + 'static,
    {
        let msg: ChangeStatus<M> = ChangeStatus {
            job_id,
            status,
            phantom: PhantomData,
        };
        Self::from_registry().do_send(msg);
    }

    pub(crate) async fn before<M>(job_id: String)
    where
        M: Executable + Clone + Send + 'static,
    {
        let msg: RunHook<M> = RunHook {
            job_id,
            before: true,
            phantom: PhantomData,
        };
        let _ = Self::from_registry().send(msg).await;
    }

    pub(crate) async fn after<M>(job_id: String)
    where
        M: Executable + Clone + Send + 'static,
    {
        let msg: RunHook<M> = RunHook {
            job_id,
            before: true,
            phantom: PhantomData,
        };
        let _ = Self::from_registry().send(msg).await;
    }
}

impl Actor for PluginCenter {
    type Context = Context<Self>;
}

impl SystemService for PluginCenter {}
impl Supervised for PluginCenter {}

#[derive(Message)]
#[rtype(result = "()")]
pub struct RegisterPlugin {
    pub plugin: JobPluginWrapper,
}

impl Handler<RegisterPlugin> for PluginCenter {
    type Result = ();

    fn handle(&mut self, msg: RegisterPlugin, _: &mut Self::Context) -> Self::Result {
        self.plugins.push(Arc::new(msg.plugin))
    }
}

#[derive(Message)]
#[rtype(result = "()")]
pub struct RunHook<M>
where
    M: Executable + Clone + 'static,
{
    pub job_id: String,
    pub before: bool,
    phantom: PhantomData<M>,
}

impl<M: Executable + Clone> Handler<RunHook<M>> for PluginCenter {
    type Result = ResponseFuture<()>;

    fn handle(&mut self, msg: RunHook<M>, _ctx: &mut Self::Context) -> Self::Result {
        let plugins = self.plugins.clone();
        Box::pin(async move {
            for plugin in plugins {
                if msg.before {
                    plugin.before_run::<M>(&msg.job_id).await;
                } else {
                    plugin.after_run::<M>(&msg.job_id).await;
                }
            }
        })
    }
}

#[derive(Message)]
#[rtype(result = "()")]
pub struct ChangeStatus<M>
where
    M: Executable + Clone + 'static,
{
    pub job_id: String,
    pub status: JobStatus,
    phantom: PhantomData<M>,
}

impl<M: Executable + Clone> Handler<ChangeStatus<M>> for PluginCenter {
    type Result = ResponseFuture<()>;

    fn handle(&mut self, msg: ChangeStatus<M>, _ctx: &mut Self::Context) -> Self::Result {
        let plugins = self.plugins.clone();
        Box::pin(async move {
            for plugin in plugins {
                plugin.change_status::<M>(&msg.job_id, msg.status).await;
            }
        })
    }
}
