//! Aj is a simple, flexible, and feature-rich background job processing library for Rust, backed by Actix (Actor Model).
//!
//! ```rust
//! use std::time::Duration;
//!
//! use aj::{
//!    async_trait,
//!    main,
//!    BackgroundJob, Executable, JobBuilder, JobContext, AJ,
//!    export::core:: {
//!        actix_rt::time::sleep,
//!        serde::{Deserialize, Serialize},
//!    }
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
//!     // Run with async function
//!     let message = Print {
//!         number: 1,
//!     };
//!     let _ = message.job_builder().build().unwrap().run().await;
//!
//!     // Run with non async function, no need .await
//!     let message = Print {
//!         number: 2,
//!     };
//!     message.job_builder().build().unwrap().do_run();
//!
//!     sleep(Duration::from_secs(1)).await;
//! }
//! ```
//!

extern crate aj_macro;

pub use aj_core::{BackgroundJob, Error, Executable, Job, JobBuilder, JobContext, WorkQueue, AJ};
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
