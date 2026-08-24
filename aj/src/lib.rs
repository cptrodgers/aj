//! Aj is a simple, flexible, and feature-rich background job processing library for Rust.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use std::time::Duration;
//!
//! use aj::job;
//! use aj::AJ;
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
//! #[tokio::main]
//! async fn main() {
//!    // Start AJ engine with in-memory backend
//!    AJ::quick_start();
//!
//!    // Wait the job is registered in AJ
//!    let _ = hello::run("Rodgers".into()).await;
//!
//!    // Or fire and forget it
//!    let _ = async_hello::just_run("AJ".into());
//!
//!    // Sleep 1 ms to view the result from job
//!    tokio::time::sleep(Duration::from_secs(1)).await;
//! }
//! ```
//!
//! # Feature Flags
//!
//! - `redis` - Enables Redis backend support for production use with persistence
//!
//! ```toml
//! # Enable Redis backend
//! aj = { version = "0.8.0", features = ["redis"] }
//! ```
//!
//! # Using Redis Backend
//!
//! When the `redis` feature is enabled:
//!
//! ```ignore
//! use aj::redis::Redis;
//! use aj::AJ;
//!
//! AJ::start(Redis::new("redis://localhost:6379"));
//! ```
//!
//! [More examples](https://github.com/cptrodgers/aj/tree/master/aj/examples)
//! [Features](https://github.com/cptrodgers/aj/?tab=readme-ov-file#features)
//!
pub use aj_core::backend;
pub use aj_core::backend::mem;
#[cfg(feature = "redis")]
pub use aj_core::backend::redis;
pub use aj_core::job;
pub use aj_core::plugin::*;
pub use aj_core::queue;
pub use aj_core::retry;
pub use aj_core::{BackgroundJob, Error, Executable, Job, JobContext, WorkQueue, AJ};
pub use aj_macro::job;
pub use aj_macro::BackgroundJob;

pub use aj_core::async_trait::async_trait;
pub use aj_core::chrono;

#[doc(hidden)]
pub mod export {
    pub mod core {
        pub use aj_core::*;
        pub use serde;
    }
}
