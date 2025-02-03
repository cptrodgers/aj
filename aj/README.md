# a# aj
![ci status](https://github.com/cptrodgers/aj/actions/workflows/test-and-build.yml/badge.svg)

Aj is a simple, customizable, and feature-rich background job processing library for Rust.
It can work with any runtime by running new actix-rt in a separated thread if it detects that an actix-rt runtime is not present.

## Install

```toml
aj = "0.7.0"
serde = { version = "1.0.64", features = ["derive"] } # Serialize and deserialize the job
actix-rt = "2.2" # Actor model runtime engine
```

## Quick start

```rust
use aj::job;

#[job]
async fn hello(name: String) {
    println!("Hello {name}");
}

#[aj::main]
async fn main() {
    // AJ will be backed by run in-memory backend.
    // If you wish to use redis as the backend for aj.
    // AJ::start(aj::Redis::new("redis://localhost:6379"));
    AJ::quick_start();
    // Fire and forget the job. No gruantee job is queued
    hello::just_run("Rodgers".into());
    // Or waiting job is queued
    hello::run("AJ".into()).await;

    // Sleep 1 sec to view the result from job (if you want to wait the job run)
    // sleep(Duration::from_secs(1)).await;
}
```

[More examples](https://github.com/cptrodgers/aj)

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
