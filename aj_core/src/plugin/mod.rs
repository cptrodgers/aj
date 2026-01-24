pub mod job_plugin;

pub use job_plugin::*;

use kameo::actor::ActorRef;
use kameo::message::{Context, Message};
use kameo::Actor;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::{Error, Executable, JobStatus};

static PLUGIN_CENTER: OnceLock<ActorRef<PluginCenter>> = OnceLock::new();

#[derive(Clone, Default, Actor)]
pub struct PluginCenter {
    plugins: Vec<Arc<JobPluginWrapper>>,
}

impl PluginCenter {
    /// Get or initialize the global PluginCenter actor
    fn get_or_init() -> ActorRef<Self> {
        PLUGIN_CENTER
            .get_or_init(|| kameo::spawn(PluginCenter::default()))
            .clone()
    }

    pub async fn register(plugin: JobPluginWrapper) -> Result<(), Error> {
        Self::get_or_init()
            .ask(RegisterPlugin { plugin })
            .await
            .map_err(|e| Error::ActorError(format!("{:?}", e)))?;
        Ok(())
    }

    pub(crate) fn change_status<M>(job_id: String, status: JobStatus)
    where
        M: Executable + Clone + Send + 'static,
    {
        let actor_ref = Self::get_or_init();
        let msg = ChangeStatusMsg { job_id, status };
        // Fire and forget - spawn a task to send the message
        tokio::spawn(async move {
            let _ = actor_ref.tell(msg).await;
        });
    }

    pub(crate) async fn before<M>(job_id: String)
    where
        M: Executable + Clone + Send + 'static,
    {
        let msg = RunHookMsg {
            job_id,
            before: true,
        };
        let _ = Self::get_or_init().ask(msg).await;
    }

    pub(crate) async fn after<M>(job_id: String)
    where
        M: Executable + Clone + Send + 'static,
    {
        let msg = RunHookMsg {
            job_id,
            before: false,
        };
        let _ = Self::get_or_init().ask(msg).await;
    }
}

// Message: RegisterPlugin
pub struct RegisterPlugin {
    pub plugin: JobPluginWrapper,
}

impl Message<RegisterPlugin> for PluginCenter {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RegisterPlugin,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.plugins.push(Arc::new(msg.plugin));
    }
}

// Message: RunHookMsg (non-generic version)
pub struct RunHookMsg {
    pub job_id: String,
    pub before: bool,
}

impl Message<RunHookMsg> for PluginCenter {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RunHookMsg,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        let job_id = msg.job_id;
        let before = msg.before;
        for plugin in &self.plugins {
            if before {
                plugin.hook.before_run(&job_id).await;
            } else {
                plugin.hook.after_run(&job_id).await;
            }
        }
    }
}

// Message: ChangeStatusMsg (non-generic version)
pub struct ChangeStatusMsg {
    pub job_id: String,
    pub status: JobStatus,
}

impl Message<ChangeStatusMsg> for PluginCenter {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ChangeStatusMsg,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        for plugin in &self.plugins {
            plugin.hook.change_status(&msg.job_id, msg.status).await;
        }
    }
}
