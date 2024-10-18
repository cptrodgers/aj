# aj
![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

Aj is a simple, customize-able, and feature-rich background job processing library for Rust, backed by Actix (Actor Model).

## Install

```toml
aj = "0.6.3"
serde = { version = "1.0.64", features = ["derive"] } # Serialize and deserialize the job
actix-rt = "2.2" # Actor model runtime engine
```

## Features & Usage

### Start AJ engine

```rust
// AJ will be backed by run in-memory backend
AJ::quick_start();

// Or, redis.
// https://github.com/cptrodgers/aj/tree/master/examples/redis_backend
AJ::start(aj::Redis::new("redis://localhost:6379"));
```

### Background Job

We support 2 ways to define a job. Macro and structure.

**#[job] macro**

Use `#[job]` macro ([Full example](https://github.com/cptrodgers/aj/tree/master/examples/simple_job))

```rust
use aj::job;

#[job]
fn hello(name: String) {
    println!("Hello {name}");
}

fn main() {
    AJ::quick_start();
    // Fire and forget the job
    hello::just_run("Rodgers".into());
    // Or waiting job completed
    hello::run("AJ".into()).await;
}
```

We also support async

```rust
use aj::job;

#[job]
async fn async_hello(name: String) {
    // We support async fn as well
    println!("Hello {name}");
}

#[main]
async fn main() {
    // Start AJ engine
    AJ::quick_start();
    // Fire and forget the job
    async_hello::just_run("Rodgers".into());
    // Or
    let _ = async_hello::run("Hien".into()).await;
}
```

**Structure way**

[Full example](https://github.com/cptrodgers/aj/tree/master/examples/update_job)

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
        .job_builder()
        .build()
        .unwrap()
        .run()
        .await
        .unwrap();
}
```

### Scheduled Job

[Example](https://github.com/cptrodgers/aj/blob/master/examples/cron_and_schedule/src/main.rs#L35)
Given that we have `Print` job.

```rust
// Delay 1 sec and run
let _ = Print { number: 1 }
    .job_builder()
    .delay(Duration::seconds(1))
    .build()
    .unwrap()
    .run()
    .await;

// Schedule after 2 seconds
let _ = Print { number: 2 }
    .job_builder()
    .schedule_at(get_now() + Duration::seconds(3))
    .build()
    .unwrap()
    .run()
    .await;
```


### Cron Job
[Example](https://github.com/cptrodgers/aj/blob/master/examples/cron_and_schedule/src/main.rs#L53)

```rust
// Cron, run this job every seconds
let _ = Print { number: 3 }
    .job_builder()
    .cron("* * * * * * *")
    .build()
    .unwrap()
    .run()
    .await;
```

### Update Job

[Example](https://github.com/cptrodgers/aj/blob/master/examples/update_job/src/main.rs#L44)


Update Job Data

```rust
// Run cron job every secs
let job_id = Print { number: 1 }
    .job_builder()
    .cron("* * * * * * *")
    .build()
    .unwrap()
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

[Example](https://github.com/cptrodgers/aj/blob/master/examples/cancel_job/src/main.rs#L43)

```rust
let result = AJ::cancel_job::<Print>(&job_id).await;
let success = result.is_ok();
```

### Get Job

```rust
let job = AJ::get_job::<Print>(&job_id).await;
```

### Retry

[Example](https://github.com/cptrodgers/aj/tree/master/examples/retry)

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
    .job_builder()
    // Try to retry 3 times, retry after failed job 1 sec.
    .retry(Retry::new_interval_retry(
        Some(max_retries),
        chrono::Duration::seconds(1),
    ))
    .build()
    .unwrap();
let _ = job.run().await;
```

**Exponential Strategy**

```rust
let job = Print { number: 3 }
    .job_builder()
    .retry(Retry::new_exponential_backoff(
        Some(max_retries),
        // Initial Backoff value
        chrono::Duration::seconds(1),
    ))
    .build()
    .unwrap();
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

TBD

### Distributed Mode

In Roadmap

### Plugins / Extensions

With plugin, you can write hook functions (before job run and after job run).

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
