# aj
![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

Aj is a simple, customize-able, and feature-rich background job processing library for Rust, backed by Actix (Actor Model).

## Usage

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

[More examples](https://github.com/cptrodgers/aj/tree/master/aj/examples)

## Features

**Job Types**:
- [x] Instant Jobs
- [x] Scheduled Jobs,
- [x] Cron Jobs

**Manage Job**:
- [x] Update Jobs
- [x] Cancel Jobs
- [x] Get Job Information

**Retry**
- [ ] Manual Retry
- [x] Maximum Retries
- [x] Retry Strategy:
  - [x] Interval Strategy
  - [ ] Exponential Strategy
  - [x] Custom Strategy: Control when the job retries by adjusting the `should_retry` logic.

**Backend (Broker + Storage)**
- [x] [Backend](https://github.com/cptrodgers/aj/blob/master/aj_core/src/backend/types.rs#L16) Trait: AJ can work with any database or storage that implements the `Backend` trait. [In memory Example](https://github.com/cptrodgers/aj/blob/master/aj_core/src/backend/mem.rs)
- [x] Native Support:
  - [x] In-memory
  - [x] Redis

**Processing Speed Customization**
- [x] Job Scan Period (tick)
- [x] Number of Jobs can run at same time

**DAG**
- [ ] DAG (Directed Acyclic Graph)

**Distributed**
- [ ] Distributed Mode

**Dashboard & Other**
- [ ] Monitorting
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
