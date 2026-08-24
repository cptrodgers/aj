

# aj
![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

AJ is a simple, customizable, and feature-rich background job processing library for Rust.
It runs on pure Tokio runtime using the Kameo actor framework.

## Install

```toml
aj = "0.9.0"
serde = { version = "1.0.64", features = ["derive"] } # Serialize and deserialize the job
tokio = { version = "1", features = ["rt-multi-thread", "macros"] } # Async runtime
```

### Enable Redis Backend

By default, AJ uses an in-memory backend. To use Redis as the backend, enable the `redis` feature:

```toml
aj = { version = "0.9.0", features = ["redis"] }
```

## Quick start

```rust
use aj::job;
use aj::AJ;

#[job]
async fn hello(name: String) {
    println!("Hello {name}");
}

#[tokio::main]
async fn main() {
    // Start AJ with in-memory backend (default, no feature flag needed)
    AJ::quick_start();

    // Or start with Redis backend (requires `redis` feature)
    // use aj::redis::Redis;
    // AJ::start(Redis::new("redis://localhost:6379"));

    // Fire and forget the job. No guarantee job is queued
    hello::just_run("Rodgers".into());
    // Or wait for job to be queued
    hello::run("AJ".into()).await;

    // Sleep 1 sec to view the result from the job (if you want to wait for the job to run)
    // tokio::time::sleep(std::time::Duration::from_secs(1)).await;
}
```

## Features & Usage

- [Create Job](#declare-a-job)
- [Schedule Job](#scheduled-job)
- [Cron Job](#cron-job)
- [Update Job](#update-job)
- [Cancel Job](#cancel-job)
- [Get Job](#get-job)
- [Retry](#retry)
  - Interval Retry
  - Backoff exponential retry
- [Plugin](#plugin)
- [Config Queue](#config)
- [Backends](#backends)
- DAG (Coming soon)
- Distributed Mode (Coming soon)
- Monitoring & Web Admin UI

### Declare a Job

We support 2 ways to define a job: macro and struct.

**Use macro #[job]** ([Full example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/macro_job.rs))

```rust
#[job]
async fn hello(name: String) {
    println!("Hello {name}");
}
```

**Structure**

You can declare a background job by using a struct and implementing the `Executable` trait for that struct.
[Full example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/default_print_job.rs)

```rust
#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = ();

    async fn execute(&mut self, _context: &JobContext) -> Self::Output {
        println!("Hello Job {}, {}", self.number, get_now());
    }
}

#[tokio::main]
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

Given that we have a `Print` job:

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
// Cron, run this job every second
let _ = Print { number: 3 }
    .job()
    .cron("* * * * * * *")
    .run()
    .await;
```

### Update Job

[Example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/update_job.rs)

```rust
// Run cron job every second
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

Update job context (such as retry logic, cron and schedule, etc.):

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

First, you should declare the failed output via the `is_failed_output` method.
If the result is true, the job will retry (following the retry strategy).

```rust
#[async_trait]
impl Executable for Print {
    type Output = Result<(), String>;

    async fn execute(&mut self, context: &JobContext) -> Self::Output {
        println!("Hello {}, {}", self.number, context.run_count);
        Err("I'm failing".into())
    }

    // Determine whether your job has failed.
    // For example, check if the job output returns an Err type
    async fn is_failed_output(&self, job_output: &Self::Output) -> bool {
        job_output.is_err()
    }
}
```

**Interval Strategy**

```rust
let max_retries = 3;
let job = Print { number: 1 }
    .job()
    // Try to retry 3 times, retry 1 sec after failed job
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
        // Initial backoff value
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

### Plugin

[Example](https://github.com/cptrodgers/aj/blob/master/examples/normal/src/plugin.rs)

```rust
use aj::{async_trait, job::JobStatus, JobPlugin};

pub struct SamplePlugin;

#[async_trait]
impl JobPlugin for SamplePlugin {
    async fn change_status(&self, job_id: &str, job_status: JobStatus) {
        println!("Hello, Job {job_id} changed status to {job_status:?}");
    }

    async fn before_run(&self, job_id: &str) {
        println!("Before job {job_id} runs");
    }

    async fn after_run(&self, job_id: &str) {
        println!("After job {job_id} runs");
    }
}

#[tokio::main]
async fn main() {
    AJ::register_plugin(SamplePlugin).await.unwrap();
}
```

### Config

```rust
AJ::update_work_queue(aj::WorkQueueConfig {
    // Fetch jobs every 50 ms
    process_tick_duration: Duration::from_millis(50),
    // Only process 10 jobs at a time
    max_processing_jobs: 10,
    // Lock TTL for distributed locking (default: 30 seconds)
    lock_ttl_ms: 30000,
}).await;
```

### Backends

For detailed backend architecture and implementation guide, see [Backend and Queue Design](docs/backend_and_queue.md).

#### In-Memory Backend (Default)

The in-memory backend is included by default and requires no feature flags. It's suitable for development and single-instance deployments.

```rust
use aj::mem::InMemory;

// Quick start uses in-memory backend
AJ::quick_start();

// Or explicitly
AJ::start(InMemory::default());
```

#### Redis Backend (Optional)

For production use with persistence and multi-instance support, enable the `redis` feature:

```toml
aj = { version = "0.9.0", features = ["redis"] }
```

```rust
use aj::redis::Redis;

AJ::start(Redis::new("redis://localhost:6379"));
```

#### Custom Backend

If you wish to customize the backend of AJ, such as using Postgres, MySQL, Kafka, RabbitMQ, etc.,
you can implement the `Backend` trait and then use it in AJ.

See [Backend and Queue Design](docs/backend_and_queue.md) for the full implementation guide.

```rust
use aj::async_trait;

pub struct YourBackend {
    // ...
}

#[async_trait]
impl Backend for YourBackend {
    // Implement required methods, all `async fn`...
}

// Use your custom backend
AJ::start(YourBackend::new());
```


### Distributed Mode (Run multiple AJ instances in many Rust applications)

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
for inclusion in AJ by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
</sub>
