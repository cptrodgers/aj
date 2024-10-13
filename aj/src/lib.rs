//! Aj is a simple, flexible, and feature-rich background job processing library for Rust, backed by Actix (Actor Model).
//!
//! ```rust
//! use std::time::Duration;
//!
//! use aj::job;
//! use aj::{export::core::actix_rt::time::sleep, main, AJ};
//!
//! #[job]
//! fn hello(name: String) {
//!    println!("Hello {name}");
//! }
//!
//! #[job]
//! async fn async_hello(name: String) {
//!    // We support async fn as well
//!    println!("Hello async, {name}");
//! }
//!
//! #[main]
//! async fn main() {
//!    // Start AJ engine
//!    AJ::quick_start();
//!
//!    // Wait the job is registered in AJ
//!    let _ = hello::run("Rodgers".into()).await;
//!
//!    // Or fire and forget it
//!    let _ = async_hello::just_run("AJ".into());
//!
//!    // Sleep 1 ms to view the result from job
//!    sleep(Duration::from_secs(1)).await;
//! }
//! ```
//!
//! [More examples](https://github.com/cptrodgers/aj/tree/master/aj/examples)
//! [Features](https://github.com/cptrodgers/aj/?tab=readme-ov-file#features)
//!

extern crate aj_macro;

pub use aj_core::backend;
pub use aj_core::job;
pub use aj_core::retry;
pub use aj_core::{BackgroundJob, Error, Executable, Job, JobBuilder, JobContext, WorkQueue, AJ};
pub use aj_macro::job;
pub use aj_macro::BackgroundJob;

pub use actix_rt::main;
pub use aj_core::async_trait::async_trait;

#[doc(hidden)]
pub mod export {
    pub mod core {
        pub use aj_core::*;
        pub use serde;
    }
}
