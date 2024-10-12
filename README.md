# aj
![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

Aj is a simple, flexible, and feature-rich background job processing library for Rust, backed by Actix (Actor Model).


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
    AJ::quick_start();
    let message = AJob;

    let _ = message
        .job_builder()
        .build()
        .unwrap()
        .run_background()
        .await;
    sleep(Duration::from_secs(1)).await;
}
```

## Features & Docs

- [x] Instant Job, Scheduled Job, and Cron Job
- [x] Update Job
- [x] Cancel Job
- [x] Get Job Information
- [x] Retry:
  - [x] Max Attempts
  - [x] Strategy:
    - [x] Interval Strategy
    - [ ] Exponential Strategy
  - [x] Custom Retry Logic: You can control when the job will retry by adjusting the `should_retry` logic.
  - [ ] Manual Retry
- [x] Flexible Backend and Broker:
  - [x] Native Support:
    - [x] Redis
    - [x] In-memory (Not recommended for production; does not support persisted jobs)
  - [x] `Backend` Trait: AJ can work with any storage that implements the `Backend` trait.
- [x] Custom Processing Speed of WorkQueue:
  - [x] Job Scan Period (tick)
  - [x] Number of Jobs per Tick
- [ ] DAG (Directed Acyclic Graph) (https://en.wikipedia.org/wiki/Directed_acyclic_graph)
- [ ] Multiple Node (Distributed Mode)
- [ ] APIs


## Using by:

- [ZenClass](https://zenclass.co) - ZenClass is an education platform that help you build your class.
- [Ikigai](https://ikigai.li) - Ikigai is an AI open assignment platform.
- [Record Wise](https://recordwise.app)

If you're using `aj`, please contact us to update the list.

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
