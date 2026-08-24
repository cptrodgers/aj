# Changelog

## 0.9.0

### Breaking

- **`Backend` is now an async trait.** Custom backend implementations need
  `#[async_trait]` (re-exported as `aj::async_trait`) and every method becomes an
  `async fn`. The bodies are otherwise unchanged.

  ```rust
  // before
  impl Backend for MyBackend {
      fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error> { .. }
  }

  // after
  #[async_trait]
  impl Backend for MyBackend {
      async fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error> { .. }
  }
  ```

- `WorkQueue<M>`'s methods are now `async` (`enqueue`, `re_enqueue`, `run_with_config`,
  `process_jobs`, `pick_jobs_to_process`, `cancel_job`, `get_job`, `read_job`, `retry_job`,
  `get_processing_job_ids`, and the `mark_job_is_*` family). `AJ::*` is the intended entry
  point and is unaffected.

- The `save_job` / `load_job` helpers in `aj_core::backend::types` are now `async`.

**Not affected:** `AJ::start`, `AJ::quick_start`, `Redis::new`, `InMemory::default`, every
`AJ::*` job method, `#[job]`-generated `run` / `just_run`, and all examples. If you use `aj`
through the `AJ` facade, no code changes are required.

No compatibility shim is provided for synchronous custom backends. If you need one,
`tokio::task::spawn_blocking` inside an `#[async_trait]` impl is the straightforward bridge.

### Changed

- The Redis backend uses the async driver and holds a single auto-reconnecting
  `ConnectionManager`, established on first use. It previously opened a **fresh TCP
  connection per operation** (14 call sites) and blocked a tokio worker on each one.
  `Redis::new` stays synchronous and does no I/O, so its signature is unchanged.
- The five Lua scripts are built once into `OnceLock` statics instead of being reconstructed,
  and rehashed, on every call.

### Added

- Test coverage for `WorkQueue`, which previously had none.
