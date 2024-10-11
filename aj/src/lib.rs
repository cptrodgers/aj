extern crate aj_macro;

pub use aj_core::*;

pub use aj_core::{Executable, AJ, WorkQueue, Job, JobBuilder, Error};
pub use aj_macro::BackgroundJob;


#[doc(hidden)]
pub mod export {
    pub mod core {
        pub use aj_core::*;
    }
}