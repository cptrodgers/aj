//! Aj is a simple, flexible, and feature-rich background job processing library for Rust, backed by Actix (Actor Model).
//!
//! ```rust
//! use std::time::Duration;
//!
//! use aj::{
//!    async_trait::async_trait,
//!    serde::{Deserialize, Serialize},
//!    BackgroundJob, Executable, JobBuilder, JobContext, AJ,
//!    actix_rt::time::sleep,
//!    main,
//! };
//!
//! #[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
//! pub struct Print {
//!     number: i32,
//! }
//!
//! #[async_trait]
//! impl Executable for Print {
//!    type Output = ();
//!
//!    async fn execute(&self, _context: &JobContext) -> Self::Output {
//!        print!("Hello, AJ {}", self.number);
//!    }
//! }
//!
//! #[main]
//! async fn main() {
//!     AJ::quick_start();
//!
//!     let message = Print {
//!         number: 1,
//!     };
//!     let _ = message.job_builder().build().unwrap().run_background().await;
//!     sleep(Duration::from_secs(1)).await;
//! }
//! ```
//!


extern crate aj_macro;

pub use aj_core::*;

pub use aj_core::{Executable, AJ, WorkQueue, Job, JobBuilder, Error};
pub use aj_macro::BackgroundJob;

pub use actix_rt::main;

#[doc(hidden)]
pub mod export {
    pub mod core {
        pub use aj_core::*;
    }
}
