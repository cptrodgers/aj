# aj
![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

Aj is a simple, customizable, and feature-rich background job processing library for Rust. It can work with any runtime by running actix-rt in a new thread if it detects that an actix-rt runtime is not present.

- [x] No Async Runtime.
- [x] Actix rt
- [x] Tokio ([Example](/examples/tokio/))
- [x] Other async runtimes.

## Install

```toml
aj = "0.7.0"
serde = { version = "1.0.64", features = ["derive"] } # Serialize and deserialize the job
actix-rt = "2.2" # Actor model runtime engine
```

## Features & Usage

### Start AJ engine

```rust
// AJ will be backed by run in-memory backend.
// If you wish to use redis as the backend for aj.
// AJ::start(aj::Redis::new("redis://localhost:6379"));
AJ::quick_start();

/// Declare job
#[job]
async fn async_hello(name: String) {
    // We support async fn as well
    println!("Hello {name}");
}

// Run it
async_hello::just_run("AJ".into());
```

### Background Job

We support 2 ways to define a job. Macro and structure.

**#[job] macro**

Use `#[job]` macro ([Full example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/macro_job.rs))

```rust
#[job]
fn hello(name: String) {
    println!("Hello {name}");
}

#[job]
async fn async_hello(name: String) {
    // We support async fn as well
    println!("Hello {name}");
}

fn main() {
    AJ::quick_start();
    // Fire and forget the job
    hello::just_run("Rodgers".into());
    // Or waiting job completed
    hello::run("AJ".into()).await;
    // Fire and forget the job
    async_hello::just_run("Rodgers".into());
    // Or
    let _ = async_hello::run("Hien".into()).await;
}
```

**Structure and Executable Trait**

You can declare a Background Job by use Struct and implement trait `Executable` for that struct.
[Full example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/print_job.rs)

```rust
#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = ();

    async fn execute(&self, _context: &JobContext) -> Self::Output {
        println!("Hello Job {}, {}", self.number, get_now());
    }
}

#[main]
async fn main() {
    // Start AJ engine
    AJ::quick_start();

    let job_id = Print { number: 1 }
        .job()
        .run()
        .await
        .unwrap();
}
```

### Scheduled Job

[Example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/schedule_job.rs)
Given that we have `Print` job.

```rust
// Delay 1 sec and run
let _ = Print { number: 1 }
    .job()
    .delay(Duration::seconds(1))
    .run()
    .await;

// Schedule after 2 seconds
let _ = Print { number: 2 }
    .job()
    .schedule_at(get_now() + Duration::seconds(3))
    .run()
    .await;
```


### Cron Job
[Example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/cron_job.rs)

```rust
// Cron, run this job every seconds
let _ = Print { number: 3 }
    .job()
    .cron("* * * * * * *")
    .run()
    .await;
```

### Update Job

[Example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/update_job.rs)

```rust
// Run cron job every secs
let job_id = Print { number: 1 }
    .job()
    .cron("* * * * * * *")
    .run()
    .await
    .unwrap();

// Update print 1 -> 2
AJ::update_job(&job_id, Print { number: 2 }, None)
    .await
    .unwrap();
```

Update Job Context (Such as retry logic, cron and schedule, etc)

```rust
AJ::update_job(
  &job_id,
  Print { number: 2 },
  aj::JobContext::default(), // Change this to apply new context
)
  .await
  .unwrap();
```

### Cancel Job

[Example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/cancel_job.rs)

```rust
let result = AJ::cancel_job::<Print>(&job_id).await;
let success = result.is_ok();
```

### Get Job

```rust
let job = AJ::get_job::<Print>(&job_id).await;
```

### Retry

[Example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/retry_job.rs)

#### Auto Retry

First, you should declare the failed output via method `is_failed_output`.
If the result is true, then the job will retry (by following retry strategy)

```rust
#[async_trait]
impl Executable for Print {
    type Output = Result<(), String>;

    async fn execute(&self, context: &JobContext) -> Self::Output {
        println!("Hello {}, {}", self.number, context.run_count);
        Err("I'm failing".into())
    }

    // Determine where your job is failed.
    // For example, check job output is return Err type
    async fn is_failed_output(&self, job_output: Self::Output) -> bool {
        job_output.is_err()
    }
}
```

**Interval Strategy**

```rust
let max_retries = 3;
let job = Print { number: 1 }
    .job()
    // Try to retry 3 times, retry after failed job 1 sec.
    .retry(Retry::new_interval_retry(
        Some(max_retries),
        chrono::Duration::seconds(1),
    ));
let _ = job.run().await;
```

**Exponential Strategy**

```rust
let job = Print { number: 3 }
    .job()
    .retry(Retry::new_exponential_backoff(
        Some(max_retries),
        // Initial Backoff value
        chrono::Duration::seconds(1),
    ));
let _ = job.run().await.unwrap();
```

**Custom Strategy**

TBD

#### Manual Retry

You can also manually retry a 'Done' job (status: finished, failed, or cancelled).
This is useful for applications that have a UI allowing users to retry the job.

```rust
AJ::retry_job::<Print>(&job_id).await.unwrap();
```

### Custom Backend (Both Broker and Storage)
If you wish to customize the backend of AJ, such as using Postgres, MySQL, Kafka, RabbitMQ, etc.,
you can implement the `Backend` trait and then use it in AJ.

[In Memory Example](https://github.com/cptrodgers/aj/blob/master/aj_core/src/backend/mem.rs)

```rust
pub YourBackend {
...
}

impl Backend for YourBackend {
    ...
}

// Use it, just replace Redis by your backend.
AJ::start(YourBackend::new());
```


### Config Processing Speed

```rust
AJ::update_work_queue(aj::queue:WorkQueueConfig {
    // 50 ms will fetch job again
    process_tick_duration: choro::Duration::milliseconds(50),
    // Only process 10 jobs at time
    max_processing_jobs: 10,
}).await;
```

### Plugins / Extensions

[Example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/plugin.rs)

```rust
use aj::{async_trait, job::JobStatus, JobPlugin};

pub struct SamplePlugin;

#[async_trait]
impl JobPlugin for SamplePlugin {
    async fn change_status(&self, job_id: &str, job_status: JobStatus) {
        println!("Hello, Job {job_id} change status to {job_status:?}");
    }

    async fn before_run(&self, job_id: &str) {
        println!("Before job {job_id} run");
    }

    async fn after_run(&self, job_id: &str) {
        println!("After job {job_id} run");
    }
}

#[aj::main]
async fn main() {
    AJ::register_plugin(SamplePlugin).await.unwrap();
}
```

### Distributed Mode (Run multiple AJ in many rust applications)

In Roadmap

### DAG

In Roadmap

### Monitoring & APIs

In Roadmap


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
