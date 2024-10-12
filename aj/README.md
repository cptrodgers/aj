# AJ
![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

Aj is a simple, customize-able, and feature-rich background job processing library for Rust, backed by Actix (Actor Model).


```rust
use std::time::Duration;

use aj::{
    async_trait,
    export::core::{
        actix_rt::time::sleep,
        serde::{Deserialize, Serialize},
    },
    main, BackgroundJob, Executable, JobBuilder, JobContext, AJ,
};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct AJob;

#[async_trait]
impl Executable for AJob {
    type Output = ();

    async fn execute(&self, _context: &JobContext) -> Self::Output {
        println!("Hello Job");
    }
}

#[main]
async fn main() {
    AJ::quick_start();
    let message = AJob;

    // Normal Job
    let _ = message
        .job_builder()
        .build()
        .unwrap()
        .run()
        .await;

    // Or just run, work in non async function
    let _ = message
        .job_builder()
        .build()
        .unwrap()
        .do_run();

    sleep(Duration::from_secs(1)).await;
}
```
