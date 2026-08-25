

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

### Enable a Persistent Backend

By default, AJ uses an in-memory backend. To use Redis or Postgres instead, enable the
matching feature. Use `postgres-tls` rather than `postgres` if the database is not on
localhost:

```toml
aj = { version = "0.9.0", features = ["redis"] }
# or
aj = { version = "0.9.0", features = ["postgres"] }
# Postgres over TLS, which every managed provider requires
aj = { version = "0.9.0", features = ["postgres-tls"] }
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

    // Or Postgres (requires `postgres` feature)
    // use aj::postgres::Postgres;
    // AJ::start(Postgres::new("postgres://postgres:postgres@localhost:5432/mydb"));

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

#### Postgres Backend (Optional)

For deployments that already run Postgres and would rather not add Redis, enable the
`postgres` feature:

```toml
aj = { version = "0.9.0", features = ["postgres"] }
```

```rust
use aj::postgres::Postgres;

AJ::start(Postgres::new("postgres://postgres:postgres@localhost:5432/mydb"));
```

The schema is created on first use if it does not already exist: an `aj_job_queue` table, an
`aj_job_lock` table, and an `aj_job_queue_seq` sequence. Everything is prefixed so AJ can
share a database with your application tables.

To use a different prefix, a larger pool, or to manage the schema yourself:

```rust
let backend = Postgres::builder("postgres://localhost/mydb")
    .table_prefix("myapp_aj_")   // must match ^[a-z_][a-z0-9_]*$
    .pool_size(20)
    .auto_migrate(false)         // skip the CREATE TABLE IF NOT EXISTS
    .build()?;

// The DDL, if you would rather apply it as a migration:
println!("{}", Postgres::schema_sql("myapp_aj_")?);
```

##### TLS

`postgres` on its own connects in plaintext. That is fine for a database on localhost or
inside a private network, but it means `sslmode=require` cannot connect at all, and the
`sslmode=prefer` default **silently falls back to an unencrypted connection** carrying your
job payloads. Every managed provider — RDS, Neon, Supabase, Cloud SQL, Azure — mandates TLS.

Enable `postgres-tls` and `sslmode` in the URL does the rest:

```toml
aj = { version = "0.9.0", features = ["postgres-tls"] }
```

```rust
use aj::postgres::Postgres;

AJ::start(Postgres::new(
    "postgres://user:pw@db.example.com:5432/mydb?sslmode=require",
));
```

By default this trusts the operating system's certificate store, falling back to the bundled
Mozilla roots when there is no system store at all (scratch and distroless images).

**Verification is stricter than libpq.** Under `psql`, `sslmode=require` means "encrypt, do
not verify"; rustls always verifies the server certificate. A server using a private CA or a
self-signed certificate that `psql` connects to happily will be **rejected** here. Two other
requirements that trip up hand-rolled certificates: the certificate needs a `subjectAltName`
(rustls does not fall back to the Common Name), and the server certificate must not be marked
`CA:TRUE` — use a CA plus a leaf signed by it, not one self-signed certificate.

To trust a private CA, pin a self-signed certificate, or do client-certificate mTLS, pass
your own `rustls` configuration. `rustls` is re-exported so its version always matches:

```rust
use aj::postgres::{rustls, Postgres};
use aj::postgres::rustls::pki_types::{pem::PemObject, CertificateDer};

let mut roots = rustls::RootCertStore::empty();
for cert in CertificateDer::pem_file_iter("/etc/ssl/my-ca.crt")? {
    roots.add(cert?)?;
}

let backend = Postgres::builder("postgres://user:pw@db.internal:5432/mydb?sslmode=require")
    .tls_config(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
    .build()?;

AJ::start(backend);
```

Two operational notes:

- Job rows are kept after completion, matching how the Redis backend keeps its storage hash.
  Released locks are deleted, but a crashed worker leaves its lock row behind until it
  expires; call `purge_expired_locks(grace_ms)` periodically to reclaim them.
- Jobs moved off the delayed queue keep their scheduled time as the ordering key, so an
  overdue job is picked up ahead of one enqueued later. Redis appends them to the back of the
  waiting list instead.

#### Custom Backend

If you wish to customize the backend of AJ, such as using MySQL, Kafka, RabbitMQ, etc.,
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
