#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_with;
#[macro_use]
extern crate derive_builder;

use actix_rt::System;

pub mod aj;
pub mod backend;
pub mod error;
pub mod queue;
pub mod util;
pub mod job;

pub use aj::*;
pub use backend::*;
pub use error::*;
pub use job::*;
pub use queue::*;
pub use util::*;

// External libs.
pub use async_trait;
pub use chrono;
pub use cron;
pub use serde;

pub fn start() {
    match System::try_current() {
        Some(_) => {
            info!("Found Actix Runtime, re-use it!");
        }
        None => {
            info!("No Actix Runtime, start new one!");
            let _ = System::new();
        }
    }
}
