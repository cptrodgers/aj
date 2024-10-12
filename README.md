# aj
![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

Aj is a simple, flexible, and feature-rich background job processing library for Rust, backed by Actix (Actor Model).

## Usage

```rust
use std::time::Duration;

use aj::{
    async_trait,
    main,
    BackgroundJob, Executable, JobBuilder, JobContext, AJ,
    export::core:: {
        actix_rt::time::sleep,
        serde::{Deserialize, Serialize},
    }
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
    // Start AJ engine
    AJ::quick_start();

    // Run a Job in Background
    let message = AJob;
    let _ = message
        .job_builder()
        .build()
        .unwrap()
        .run_background()
        .await;

    // Wait this thread in 1 sec to view result in background job
    sleep(Duration::from_secs(1)).await;
}
```

[More examples](https://github.com/cptrodgers/aj/tree/master/aj/examples)

## Features & Docs

- [x] Instant Jobs, Scheduled Jobs, and Cron Jobs
- [x] Update Jobs
- [x] Cancel Jobs
- [x] Retrieve Job Information
- [x] Retry Options:
  - [x] Maximum Retries
  - [x] Retry Strategy:
    - [x] Interval Strategy
    - [ ] Exponential Strategy
  - [x] Custom Retry Logic: Control when the job retries by adjusting the `should_retry` logic.
  - [ ] Manual Retry
- [x] Flexible Backend and Broker Support:
  - [x] Native Support:
    - [x] Redis
    - [x] In-memory (Not recommended for production; does not support persisted jobs)
  - [x] `Backend` Trait: AJ can work with any storage that implements the `Backend` trait.
- [x] Custom Processing Speed for the WorkQueue:
  - [x] Job Scan Period (tick)
  - [x] Number of Jobs per Tick
- [ ] DAG (Directed Acyclic Graph)
- [ ] Distributed Mode
- [ ] APIs

## LICENSE

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in aj by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
</sub>
