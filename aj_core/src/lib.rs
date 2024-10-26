#![allow(clippy::field_reassign_with_default)]

#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_with;
#[macro_use]
extern crate derive_builder;

pub mod aj;
pub mod backend;
pub mod error;
pub mod job;
pub mod queue;
pub mod util;

pub use aj::*;
pub use backend::*;
pub use error::*;
pub use job::*;
pub use queue::*;
use serde::{de::DeserializeOwned, Serialize};
pub use util::*;

// External libs.
pub use actix_rt;
pub use async_trait;
pub use chrono;
pub use cron;
pub use serde;

impl<M> Job<M>
where
    M: BackgroundJob
        + Executable
        + Unpin
        + Send
        + Sync
        + Clone
        + Serialize
        + DeserializeOwned
        + 'static,
{
    /// It will wait to WorkQueue received and insert job into backend
    pub async fn run(self) -> Result<String, Error> {
        AJ::add_job(self, M::queue_name()).await
    }

    /// It will just send message to WorkQueue and no gurantee job is inserted to backend
    pub fn just_run(self) {
        if let Some(aj_addr) = get_aj_address() {
            aj_addr.do_send(JustRunJob {
                job: self,
                queue_name: M::queue_name().to_string(),
            });
        }
    }
}
