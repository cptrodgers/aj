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
pub use async_trait;
pub use chrono;
pub use cron;
pub use serde;
pub use actix_rt;

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
    pub async fn apply(self) -> Result<(), Error> {
        AJ::add_job(self, M::queue_name()).await
    }
}
