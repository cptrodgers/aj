# Changelog

## 0.9.1 - 2026-08-25

### Added

- **Postgres backend**, behind a new `postgres` feature, at the same level of support as
  `redis`:

  ```rust
  use aj::postgres::Postgres;
  AJ::start(Postgres::new("postgres://postgres:postgres@localhost:5432/mydb"));
  ```

  Two prefixed tables (`aj_job_queue`, `aj_job_lock`) plus a sequence, created on first use.
  Claiming is a single `FOR UPDATE SKIP LOCKED` statement, so jobs are handed to exactly one
  worker without advisory locks or polling races. `Postgres::new` is synchronous and does no
  I/O, so it drops into `AJ::start` exactly like `Redis::new`.

  Configurable via `Postgres::builder(url).table_prefix(..).pool_size(..).auto_migrate(..)`,
  with `Postgres::schema_sql(prefix)` for teams that apply DDL out-of-band and
  `purge_expired_locks(grace_ms)` to reclaim locks left by crashed workers.

- **TLS for the Postgres backend**, behind a new `postgres-tls` feature that implies
  `postgres`:

  ```toml
  aj = { version = "0.9.0", features = ["postgres-tls"] }
  ```

  ```rust
  AJ::start(Postgres::new("postgres://user:pw@db.example.com:5432/mydb?sslmode=require"));
  ```

  Without it the pool is built with `NoTls`, which means `sslmode=require` cannot connect at
  all and the `sslmode=prefer` default **silently falls back to an unencrypted connection**.
  That ruled out every managed provider — RDS, Neon, Supabase, Cloud SQL, Azure. With the
  feature enabled, `sslmode` in the URL selects the behaviour as libpq users expect.

  The default trusts the system certificate store and falls back to the bundled Mozilla roots
  when there is no system store, so scratch and distroless images work unchanged.

  **Verification is stricter than libpq.** rustls always verifies the server certificate,
  where libpq's `sslmode=require` means "encrypt, do not verify". A private-CA or self-signed
  server that `psql` accepts will be rejected. Also, rustls requires a `subjectAltName` rather
  than falling back to the Common Name, and refuses a `CA:TRUE` certificate presented as the
  server leaf. Supply your own configuration for those cases:

  ```rust
  Postgres::builder(url).tls_config(my_rustls_client_config).build()?
  ```

  `rustls` is re-exported as `aj::postgres::rustls` so the version cannot skew.

### Changed

- Dependency bumps: tokio 1.35.0 -> 1.43.1, time 0.3.36 -> 0.3.55, hashbrown 0.15.0 -> 0.15.5.

**Upgrading from 0.9.0:** nothing to do. Both additions are new opt-in features; no existing
API changed.

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
