# aj (Actix Background Job)

![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

aj is a one-stop solution for your background (includes schedule, cron) job needs in Rust system.
Based on [Actor Model (Actix)](https://actix.rs).

- `Simple`:
  - Easy to integrate into your application.
  - Freedom to choose your backend (Redis by default). You just need to implement the `Backend` trait to your storage and plug it into the worker.
- `Flexible`:
  - Supports rich features like `update`, `cancel`, `schedule`, `cron` that can fulfill all your needs. You don't need to find other crates for your system.
  - Control velocity/throughput of your worker/queue. (In Roadmap)
  - Web Interface to control (In roadmap)
- `Reliable`:
  - Persistent by default (Redis by default).
  - No unsafe code.


[Docs](https://github.com/cptrodgers/aj/blob/master/docs)

## Usage ([examples](https://github.com/cptrodgers/aj/tree/master/examples))

```rust
use std::str::FromStr;
use std::time::Duration;
use actix_rt::time::sleep;

use aj::async_trait::async_trait;
use aj::AJ;
use aj::{Executable, JobBuilder};
use aj::serde::{Serialize, Deserialize};
use aj::chrono::Utc;
use aj::cron::Schedule;
use aj::rt;
use aj::mem::InMemory;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintJob {
    pub number: i32,
}

#[async_trait]
impl Executable for PrintJob {
    async fn execute(&self) {
        // Do your stuff here in async mode
        print!("Hello in background {}", self.number);
    }
}

fn run_job_instantly() {
    let job = JobBuilder::new(PrintJob { number: 1 }).build();
    AJ::add_job(job);
}

fn run_job_at() {
    // Now in seconds
    let now = Utc::now().timestamp();
    let job = JobBuilder::new(PrintJob { number: 2 })
        .set_schedule_at(now + 30)  // Run after now 30 secs
        .build();
    AJ::add_job(job);
}

fn run_simple_cron_job() {
    let expression = "* * * * * * *";
    let schedule = Schedule::from_str(expression).unwrap();
    let job = JobBuilder::new(PrintJob { number: 3 })
        .set_cron(schedule, CronContext::default())
        .build();
    AJ::add_job(job);
}

#[rt]
async fn main() {
    let mem = InMemory::default();
    AJ::register::<PrintJob>("print_job", mem);
    run_job_instantly();
    run_job_at();
    run_simple_cron_job();
    sleep(Duration::from_secs(10)).await;
}
```

## `aj` in Production

- [ZenClass](https://zenclass.co): uses `aj` to build their reminder system (reminder at specific time or monthly, weekly, daily repeat reminder).

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
