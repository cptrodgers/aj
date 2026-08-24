//! Postgres Backend Implementation
//!
//! Maps the queue model onto two tables instead of Redis' four keys plus lock string:
//! - `{prefix}job_queue` - one row per (queue, job_id); a `state` column stands in for
//!   membership of the waiting / delayed / active queues, and `payload` holds the job JSON
//! - `{prefix}job_lock`  - worker locks, keyed by `job_id` alone
//! - `{prefix}job_queue_seq` - FIFO tiebreaker for rows sharing a `ready_at_ms`
//!
//! Atomic claiming uses `SELECT ... FOR UPDATE SKIP LOCKED` where the Redis backend uses
//! Lua scripts.
//!
//! # Why queries run on a dedicated thread pool
//!
//! [`Backend`] is synchronous, but the engine drives it from inside async message handlers.
//! The blocking `postgres` client is a wrapper over `tokio-postgres` that calls
//! `Runtime::block_on` internally, which panics with *"Cannot start a runtime from within a
//! runtime"* if it happens on a thread already inside one. `tokio::task::block_in_place`
//! would fix that on a multi-thread runtime but panics on a `current_thread` one, so every
//! query is instead handed to threads this backend owns and the caller blocks on a plain
//! std channel. That is safe from any context - async or not, either runtime flavor.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use postgres::NoTls;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;

use crate::types::Backend;
use crate::{get_now_as_ms, Error};

/// Default table-name prefix. Every object this backend creates is prefixed so that `aj`
/// can share a database with application tables without colliding.
pub const DEFAULT_TABLE_PREFIX: &str = "aj_";

/// Default connection pool size, and hence the number of worker threads.
pub const DEFAULT_POOL_SIZE: u32 = 10;

/// Longest accepted table prefix. Postgres truncates identifiers at 63 bytes and the
/// longest suffix generated here is `job_queue_pick_idx` (18 bytes).
const MAX_PREFIX_LEN: usize = 40;

type Manager = PostgresConnectionManager<NoTls>;
type Conn = PooledConnection<Manager>;

// ============================================================================
// State values
// ============================================================================

/// Job is in the waiting queue, ready to be claimed. Mirrors `{queue}:waiting`.
const STATE_WAITING: &str = "waiting";
/// Job is scheduled for the future. Mirrors `{queue}:delayed`.
const STATE_DELAYED: &str = "delayed";
/// Job is being processed. Mirrors `{queue}:active`.
const STATE_ACTIVE: &str = "active";
// A NULL state means the row exists but belongs to no queue - the equivalent of a job
// present in Redis' `{queue}:storage` hash but absent from all three lists.

// ============================================================================
// Backend
// ============================================================================

/// Postgres backend for distributed job queue.
///
/// # Example
/// ```ignore
/// let backend = Postgres::new("postgres://postgres:postgres@localhost:5432/aj");
///
/// // or, with a custom prefix and pool size:
/// let backend = Postgres::builder("postgres://localhost/aj")
///     .table_prefix("myapp_aj_")
///     .pool_size(20)
///     .build()?;
/// ```
#[derive(Clone)]
pub struct Postgres {
    handle: Arc<PoolHandle>,
    sql: Arc<Sql>,
}

/// Owns the pool and guarantees it is dropped on a worker thread.
///
/// `postgres::Client`'s *destructor* also drives its internal runtime, so teardown is
/// subject to the same restriction as queries: dropping the pool on a thread inside a tokio
/// runtime panics. Reaching the pool only through here makes that unrepresentable.
struct PoolHandle {
    /// `None` only during drop, after the pool has been handed to a worker.
    pool: Option<Pool<Manager>>,
    /// Dropped after `pool` - though by then `drop` has already taken the pool out, so what
    /// actually matters is that `Executor`'s own `Drop` (which joins the threads) runs only
    /// once the disposal task below has been submitted.
    executor: Arc<Executor>,
}

impl PoolHandle {
    fn pool(&self) -> &Pool<Manager> {
        self.pool
            .as_ref()
            .expect("Postgres pool accessed during shutdown")
    }
}

impl Drop for PoolHandle {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            // Nothing to do if the workers are already gone; the panic it would cause is
            // strictly worse than leaking the connections at process exit.
            let _ = self.executor.run(move || drop(pool));
        }
    }
}

impl std::fmt::Debug for Postgres {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Postgres")
            .field("table_prefix", &self.sql.prefix)
            .finish()
    }
}

impl Postgres {
    /// Create a new Postgres backend, creating the tables if they do not exist.
    ///
    /// Panics if the URL is invalid or the schema cannot be created, matching the
    /// convention of [`crate::backend::redis::Redis::new`]. Use [`Postgres::try_new`]
    /// to handle those failures.
    pub fn new(url: &str) -> Self {
        Self::try_new(url).expect("Failed to create Postgres backend")
    }

    /// Fallible variant of [`Postgres::new`].
    pub fn try_new(url: &str) -> Result<Self, Error> {
        Self::builder(url).build()
    }

    /// Builder for a backend with a custom table prefix or pool size.
    pub fn builder(url: impl Into<String>) -> PostgresBuilder {
        PostgresBuilder {
            url: url.into(),
            table_prefix: DEFAULT_TABLE_PREFIX.to_string(),
            pool_size: DEFAULT_POOL_SIZE,
            auto_migrate: true,
        }
    }

    /// The DDL this backend runs on startup, for teams that manage their schema
    /// out-of-band and build with `auto_migrate(false)`.
    pub fn schema_sql(table_prefix: &str) -> Result<String, Error> {
        Ok(Sql::new(table_prefix)?.ddl)
    }

    /// Delete lock rows whose TTL elapsed more than `grace_ms` ago.
    ///
    /// Postgres has no equivalent of Redis' key eviction, so expired locks linger as rows.
    /// They are already treated as free by `lock_acquire` and `claim_job`, so this is
    /// housekeeping rather than correctness. The grace period avoids racing a worker that
    /// is about to call `lock_extend`. Returns the number of rows removed.
    pub fn purge_expired_locks(&self, grace_ms: u64) -> Result<usize, Error> {
        let cutoff = get_now_as_ms().saturating_sub(clamp_ms(grace_ms));
        self.run(move |conn, sql| {
            let n = conn.execute(&sql.purge_expired_locks, &[&cutoff])?;
            Ok(n as usize)
        })
    }

    /// The resolved table prefix.
    pub fn table_prefix(&self) -> &str {
        &self.sql.prefix
    }

    /// Hand a unit of work to the backend's own threads and block until it finishes.
    ///
    /// See the module docs for why this indirection exists. The closure is `'static`, so
    /// callers pass owned parameters; the pool handle and statement set are cheap clones.
    fn run<T, F>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Conn, &Sql) -> Result<T, Error> + Send + 'static,
        T: Send + 'static,
    {
        let pool = self.handle.pool().clone();
        let sql = self.sql.clone();
        self.handle.executor.run(move || {
            let mut conn = pool.get()?;
            f(&mut conn, &sql)
        })?
    }

    /// Who holds the lock on `job_id`, if anyone. Test assertions only.
    #[cfg(test)]
    fn lock_owner(&self, job_id: &str) -> Result<Option<String>, Error> {
        let job_id = job_id.to_string();
        self.run(move |conn, sql| {
            let row = conn.query_opt(
                &format!(
                    "SELECT worker_id FROM {}job_lock WHERE job_id = $1",
                    sql.prefix
                ),
                &[&job_id],
            )?;
            Ok(row.map(|r| r.get(0)))
        })
    }

    /// Delete everything a test may have touched. Test teardown only.
    #[cfg(test)]
    fn purge_for_test(&self, queue: &str, job_ids: Vec<String>) -> Result<(), Error> {
        let queue = queue.to_string();
        self.run(move |conn, sql| {
            conn.execute(
                &format!("DELETE FROM {}job_queue WHERE queue = $1", sql.prefix),
                &[&queue],
            )?;
            conn.execute(
                &format!("DELETE FROM {}job_lock WHERE job_id = ANY($1)", sql.prefix),
                &[&job_ids],
            )?;
            Ok(())
        })
    }
}

/// Builder for [`Postgres`].
///
/// A builder rather than `with_*` setters on the backend itself, because the table names
/// have to be known before the schema can be created.
pub struct PostgresBuilder {
    url: String,
    table_prefix: String,
    pool_size: u32,
    auto_migrate: bool,
}

impl PostgresBuilder {
    /// Prefix for every table, index, and sequence. Default `aj_`.
    ///
    /// Must match `^[a-z_][a-z0-9_]*$` and be at most 40 characters - table names cannot be
    /// bound as query parameters, so the prefix is interpolated into SQL and has to be
    /// validated.
    pub fn table_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.table_prefix = prefix.into();
        self
    }

    /// Maximum pooled connections, and the number of worker threads. Default 10.
    pub fn pool_size(mut self, size: u32) -> Self {
        self.pool_size = size.max(1);
        self
    }

    /// Whether to create the schema on build. Default true.
    pub fn auto_migrate(mut self, auto_migrate: bool) -> Self {
        self.auto_migrate = auto_migrate;
        self
    }

    pub fn build(self) -> Result<Postgres, Error> {
        let sql = Arc::new(Sql::new(&self.table_prefix)?);

        let config: postgres::Config = self
            .url
            .parse()
            .map_err(|e| Error::Postgres(format!("invalid Postgres url: {e}")))?;
        let executor = Arc::new(Executor::new(self.pool_size as usize));

        // Built on the executor's threads, not here: r2d2 establishes connections eagerly
        // during `build`, and connection setup goes through the same `block_on` that must
        // never run on a caller's runtime thread.
        let pool_size = self.pool_size;
        let pool = executor.run(move || {
            let manager = PostgresConnectionManager::new(config, NoTls);
            Pool::builder()
                .max_size(pool_size)
                .build(manager)
                .map_err(|e| Error::Postgres(format!("failed to build Postgres pool: {e}")))
        })??;

        let pg = Postgres {
            handle: Arc::new(PoolHandle {
                pool: Some(pool),
                executor,
            }),
            sql,
        };

        if self.auto_migrate {
            pg.migrate()?;
        }

        Ok(pg)
    }
}

impl Postgres {
    /// Create the schema if it is absent.
    ///
    /// `CREATE TABLE IF NOT EXISTS` is explicitly *not* race-free - concurrent creators can
    /// get `duplicate key value violates unique constraint "pg_type_typname_nsp_index"` -
    /// so the whole bootstrap runs under a transaction-scoped advisory lock keyed on the
    /// prefix. Two different prefixes still bootstrap concurrently.
    fn migrate(&self) -> Result<(), Error> {
        self.run(|conn, sql| {
            let mut tx = conn.transaction()?;
            tx.execute("SELECT pg_advisory_xact_lock($1)", &[&sql.lock_key])?;
            tx.batch_execute(&sql.ddl)?;
            tx.commit()?;
            Ok(())
        })
    }
}

// ============================================================================
// Worker threads
// ============================================================================

type Task = Box<dyn FnOnce() + Send + 'static>;

/// A fixed set of OS threads that own every interaction with the connection pool.
struct Executor {
    /// `None` only while shutting down, so that dropping the sender closes the channel and
    /// lets the workers exit before they are joined.
    tx: Option<Sender<Task>>,
    workers: Vec<JoinHandle<()>>,
}

impl Executor {
    fn new(threads: usize) -> Self {
        let (tx, rx) = channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));

        let workers = (0..threads)
            .map(|i| {
                let rx = Arc::clone(&rx);
                std::thread::Builder::new()
                    .name(format!("aj-postgres-{i}"))
                    .spawn(move || worker_loop(rx))
                    .expect("failed to spawn aj postgres worker thread")
            })
            .collect();

        Self {
            tx: Some(tx),
            workers,
        }
    }

    fn run<T, F>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (done_tx, done_rx) = channel();
        let task: Task = Box::new(move || {
            // A send failure means the caller gave up; nothing to do about it.
            let _ = done_tx.send(f());
        });

        self.tx
            .as_ref()
            .ok_or_else(|| Error::Postgres("Postgres backend is shutting down".into()))?
            .send(task)
            .map_err(|_| Error::Postgres("Postgres worker threads have stopped".into()))?;

        // Blocking on a std channel is safe from inside a tokio runtime; blocking on the
        // postgres client directly is not. That is the whole point of this hop.
        done_rx
            .recv()
            .map_err(|_| Error::Postgres("Postgres worker thread panicked".into()))
    }
}

fn worker_loop(rx: Arc<Mutex<Receiver<Task>>>) {
    loop {
        let task = {
            // Recover from poisoning: one panicking task must not wedge the pool.
            let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
            guard.recv()
        };
        match task {
            Ok(task) => task(),
            // Channel closed: the backend was dropped.
            Err(_) => break,
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        // Close the channel first, otherwise the joins below never return.
        self.tx.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

// ============================================================================
// Statements
// ============================================================================

/// Every statement, pre-formatted with the resolved table names at construction time.
struct Sql {
    prefix: String,
    ddl: String,
    /// Advisory-lock key for the bootstrap, derived from the prefix.
    lock_key: i64,

    waiting_push: String,
    waiting_pop: String,
    waiting_len: String,

    delayed_push: String,
    delayed_move_ready: String,
    delayed_remove: String,
    delayed_len: String,

    active_push: String,
    active_remove: String,
    active_len: String,
    active_list: String,

    job_save: String,
    job_get: String,
    job_clear_payload: String,
    job_delete_unqueued: String,

    lock_acquire: String,
    lock_release: String,
    lock_extend: String,
    purge_expired_locks: String,

    claim_job: String,
    requeue_orphaned: String,
}

/// Reject anything that is not a plain lowercase identifier fragment. This is the only
/// place a caller-supplied string reaches SQL without being a bind parameter.
fn validate_prefix(prefix: &str) -> Result<(), Error> {
    let invalid = |reason: &str| {
        Err(Error::Postgres(format!(
            "invalid table prefix {prefix:?}: {reason}"
        )))
    };

    let mut chars = prefix.chars();
    match chars.next() {
        None => return invalid("must not be empty"),
        Some(c) if c.is_ascii_lowercase() || c == '_' => {}
        Some(_) => return invalid("must start with a lowercase letter or underscore"),
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return invalid("may only contain lowercase letters, digits, and underscores");
    }
    if prefix.len() > MAX_PREFIX_LEN {
        return invalid("must be at most 40 characters");
    }
    Ok(())
}

/// FNV-1a. Spelled out rather than using `DefaultHasher` so the advisory-lock key is
/// stable across Rust versions - two processes on different builds must agree on it, or
/// the bootstrap race the lock exists to prevent comes back.
fn advisory_lock_key(prefix: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in b"aj:schema:".iter().chain(prefix.as_bytes()) {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as i64
}

/// Saturating `u64` -> `i64` for millisecond values.
///
/// An unchecked `as i64` on an absurd TTL wraps negative, which would make the lock
/// permanently expired and silently disable mutual exclusion.
fn clamp_ms(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// `(now_ms, expires_at_ms)` from a single clock reading.
fn lock_window(ttl_ms: u64) -> (i64, i64) {
    let now = get_now_as_ms();
    (now, now.saturating_add(clamp_ms(ttl_ms)))
}

impl Sql {
    fn new(prefix: &str) -> Result<Self, Error> {
        validate_prefix(prefix)?;

        let q = format!("{prefix}job_queue");
        let l = format!("{prefix}job_lock");
        let seq = format!("{prefix}job_queue_seq");
        let idx = format!("{prefix}job_queue_pick_idx");
        // An explicit sequence rather than BIGSERIAL: the implicit name BIGSERIAL derives
        // gets truncated at 63 bytes and silently suffixed if taken, so nextval() could
        // end up pointing at the wrong sequence.
        let next = format!("nextval('{seq}')");

        let ddl = format!(
            "CREATE SEQUENCE IF NOT EXISTS {seq};

             CREATE TABLE IF NOT EXISTS {q} (
                 queue       TEXT   NOT NULL,
                 job_id      TEXT   NOT NULL,
                 state       TEXT   CHECK (state IN ('{STATE_WAITING}', '{STATE_DELAYED}', '{STATE_ACTIVE}')),
                 ready_at_ms BIGINT NOT NULL DEFAULT 0,
                 seq         BIGINT NOT NULL,
                 payload     TEXT,
                 worker_id   TEXT,
                 PRIMARY KEY (queue, job_id)
             );

             CREATE INDEX IF NOT EXISTS {idx}
                 ON {q} (queue, state, ready_at_ms, seq);

             CREATE TABLE IF NOT EXISTS {l} (
                 job_id        TEXT PRIMARY KEY,
                 worker_id     TEXT   NOT NULL,
                 expires_at_ms BIGINT NOT NULL
             );"
        );

        Ok(Self {
            prefix: prefix.to_string(),
            ddl,
            lock_key: advisory_lock_key(prefix),

            // Upsert: the trait allows pushing a job id that was never `job_save`d, and a
            // re-push has to land at the *back* of the queue (Redis RPUSH). Both parts of
            // the sort key must be reset, or an old `ready_at_ms` keeps it at the front.
            waiting_push: format!(
                "INSERT INTO {q} (queue, job_id, state, ready_at_ms, seq)
                 VALUES ($1::text, $2::text, '{STATE_WAITING}', $3::bigint, {next})
                 ON CONFLICT (queue, job_id) DO UPDATE
                     SET state = '{STATE_WAITING}', ready_at_ms = $3::bigint,
                         seq = {next}, worker_id = NULL"
            ),
            // Removes the job from waiting without putting it anywhere else, matching LPOP.
            waiting_pop: format!(
                "UPDATE {q} SET state = NULL, worker_id = NULL
                 WHERE queue = $1::text AND state = '{STATE_WAITING}' AND job_id = (
                     SELECT job_id FROM {q}
                     WHERE queue = $1::text AND state = '{STATE_WAITING}'
                     ORDER BY ready_at_ms, seq
                     LIMIT 1
                     FOR UPDATE SKIP LOCKED
                 )
                 RETURNING job_id"
            ),
            waiting_len: format!(
                "SELECT count(*) FROM {q}
                 WHERE queue = $1::text AND state = '{STATE_WAITING}'"
            ),

            // `ready_at_ms` doubles as the ZSET score and the waiting-queue ordering key,
            // which is what lets `delayed_move_ready` be a plain unordered UPDATE.
            delayed_push: format!(
                "INSERT INTO {q} (queue, job_id, state, ready_at_ms, seq)
                 VALUES ($1::text, $2::text, '{STATE_DELAYED}', $3::bigint, {next})
                 ON CONFLICT (queue, job_id) DO UPDATE
                     SET state = '{STATE_DELAYED}', ready_at_ms = $3::bigint,
                         seq = {next}, worker_id = NULL"
            ),
            delayed_move_ready: format!(
                "UPDATE {q} SET state = '{STATE_WAITING}'
                 WHERE queue = $1::text AND state = '{STATE_DELAYED}'
                   AND ready_at_ms <= $2::bigint"
            ),
            // Every removal is guarded by the state it expects. `cancel_job` calls
            // `delayed_remove` for jobs that may be sitting in `waiting`; unguarded, that
            // would silently dequeue them.
            delayed_remove: format!(
                "UPDATE {q} SET state = NULL
                 WHERE queue = $1::text AND job_id = $2::text AND state = '{STATE_DELAYED}'"
            ),
            delayed_len: format!(
                "SELECT count(*) FROM {q}
                 WHERE queue = $1::text AND state = '{STATE_DELAYED}'"
            ),

            active_push: format!(
                "INSERT INTO {q} (queue, job_id, state, ready_at_ms, seq)
                 VALUES ($1::text, $2::text, '{STATE_ACTIVE}', $3::bigint, {next})
                 ON CONFLICT (queue, job_id) DO UPDATE
                     SET state = '{STATE_ACTIVE}', ready_at_ms = $3::bigint, seq = {next}"
            ),
            active_remove: format!(
                "UPDATE {q} SET state = NULL, worker_id = NULL
                 WHERE queue = $1::text AND job_id = $2::text AND state = '{STATE_ACTIVE}'"
            ),
            active_len: format!(
                "SELECT count(*) FROM {q}
                 WHERE queue = $1::text AND state = '{STATE_ACTIVE}'"
            ),
            active_list: format!(
                "SELECT job_id FROM {q}
                 WHERE queue = $1::text AND state = '{STATE_ACTIVE}'
                 ORDER BY ready_at_ms, seq"
            ),

            // Must not disturb `state`, `ready_at_ms`, or `seq`: `save_job` is called both
            // before enqueueing and again while the job is already active.
            job_save: format!(
                "INSERT INTO {q} (queue, job_id, ready_at_ms, seq, payload)
                 VALUES ($1::text, $2::text, 0, {next}, $3::text)
                 ON CONFLICT (queue, job_id) DO UPDATE SET payload = EXCLUDED.payload"
            ),
            job_get: format!(
                "SELECT payload FROM {q} WHERE queue = $1::text AND job_id = $2::text"
            ),
            // Redis' HDEL drops the stored data but leaves the id in whatever list holds
            // it, so only rows that belong to no queue are removed outright.
            job_clear_payload: format!(
                "UPDATE {q} SET payload = NULL WHERE queue = $1::text AND job_id = $2::text"
            ),
            job_delete_unqueued: format!(
                "DELETE FROM {q}
                 WHERE queue = $1::text AND job_id = $2::text AND state IS NULL"
            ),

            // Locks are keyed on job_id alone, *not* queue-scoped, mirroring Redis'
            // `aj:lock:{job_id}`. An expired row counts as free, which is how the
            // `SET NX PX` semantics survive the absence of TTL eviction.
            lock_acquire: format!(
                "INSERT INTO {l} (job_id, worker_id, expires_at_ms)
                 VALUES ($1::text, $2::text, $3::bigint)
                 ON CONFLICT (job_id) DO UPDATE
                     SET worker_id = EXCLUDED.worker_id,
                         expires_at_ms = EXCLUDED.expires_at_ms
                     WHERE {l}.expires_at_ms <= $4::bigint"
            ),
            // No expiry check: releasing a lock whose TTL lapsed should still clear the
            // row. The worker_id match is what prevents stealing someone else's lock.
            lock_release: format!(
                "DELETE FROM {l} WHERE job_id = $1::text AND worker_id = $2::text"
            ),
            // The expiry check *is* required here, for parity with LUA_LOCK_EXTEND: an
            // expired lock is gone in Redis, so extending it must fail rather than
            // resurrect a claim another worker is entitled to take.
            lock_extend: format!(
                "UPDATE {l} SET expires_at_ms = $3::bigint
                 WHERE job_id = $1::text AND worker_id = $2::text
                   AND expires_at_ms > $4::bigint"
            ),
            purge_expired_locks: format!("DELETE FROM {l} WHERE expires_at_ms <= $1::bigint"),

            // Replaces LUA_CLAIM_JOB. `FOR UPDATE SKIP LOCKED` keeps two claimers off the
            // same row; the lock table's conflict predicate rejects a job another worker
            // still holds. When it does, `locked` yields nothing, the UPDATE touches
            // nothing, and the row stays waiting - the Lua script's put-back branch.
            //
            // Lock order is always {q} then {l}, in every statement, so no cycle exists.
            claim_job: format!(
                "WITH picked AS MATERIALIZED (
                     SELECT job_id FROM {q}
                     WHERE queue = $1::text AND state = '{STATE_WAITING}'
                     ORDER BY ready_at_ms, seq
                     LIMIT 1
                     FOR UPDATE SKIP LOCKED
                 ), locked AS (
                     INSERT INTO {l} (job_id, worker_id, expires_at_ms)
                     SELECT job_id, $2::text, $3::bigint FROM picked
                     ON CONFLICT (job_id) DO UPDATE
                         SET worker_id = EXCLUDED.worker_id,
                             expires_at_ms = EXCLUDED.expires_at_ms
                         WHERE {l}.expires_at_ms <= $4::bigint
                     RETURNING {l}.job_id
                 )
                 UPDATE {q} q SET state = '{STATE_ACTIVE}', worker_id = $2::text
                 WHERE q.queue = $1::text AND q.state = '{STATE_WAITING}'
                   AND q.job_id IN (SELECT job_id FROM locked)
                 RETURNING q.job_id"
            ),
            // Replaces LUA_REQUEUE_ORPHANED. Redis detects orphans by the lock key having
            // been evicted; here the equivalent test is that no *unexpired* lock row
            // exists. {l} is only read, never locked, so this cannot deadlock against
            // claim_job - and the {q} row lock is the serializing token that stops it
            // stealing a job mid-claim.
            requeue_orphaned: format!(
                "UPDATE {q} q
                 SET state = '{STATE_WAITING}', worker_id = NULL,
                     ready_at_ms = $2::bigint, seq = {next}
                 WHERE q.queue = $1::text AND q.state = '{STATE_ACTIVE}'
                   AND NOT EXISTS (
                       SELECT 1 FROM {l} l
                       WHERE l.job_id = q.job_id AND l.expires_at_ms > $2::bigint
                   )
                 RETURNING q.job_id"
            ),
        })
    }
}

// ============================================================================
// Backend Implementation
// ============================================================================

impl Backend for Postgres {
    // ========================================================================
    // Waiting Queue
    // ========================================================================

    fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let (queue, job_id) = (queue.to_string(), job_id.to_string());
        self.run(move |conn, sql| {
            conn.execute(&sql.waiting_push, &[&queue, &job_id, &get_now_as_ms()])?;
            Ok(())
        })
    }

    fn waiting_pop(&self, queue: &str) -> Result<Option<String>, Error> {
        let queue = queue.to_string();
        self.run(move |conn, sql| {
            let row = conn.query_opt(&sql.waiting_pop, &[&queue])?;
            Ok(row.map(|r| r.get(0)))
        })
    }

    fn waiting_len(&self, queue: &str) -> Result<usize, Error> {
        let queue = queue.to_string();
        self.run(move |conn, sql| {
            let row = conn.query_one(&sql.waiting_len, &[&queue])?;
            Ok(to_usize(row.get::<_, i64>(0)))
        })
    }

    // ========================================================================
    // Delayed Queue
    // ========================================================================

    fn delayed_push(&self, queue: &str, job_id: &str, run_at_ms: i64) -> Result<(), Error> {
        let (queue, job_id) = (queue.to_string(), job_id.to_string());
        self.run(move |conn, sql| {
            conn.execute(&sql.delayed_push, &[&queue, &job_id, &run_at_ms])?;
            Ok(())
        })
    }

    fn delayed_move_ready(&self, queue: &str, now_ms: i64) -> Result<usize, Error> {
        let queue = queue.to_string();
        self.run(move |conn, sql| {
            let n = conn.execute(&sql.delayed_move_ready, &[&queue, &now_ms])?;
            Ok(n as usize)
        })
    }

    fn delayed_remove(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let (queue, job_id) = (queue.to_string(), job_id.to_string());
        self.run(move |conn, sql| {
            conn.execute(&sql.delayed_remove, &[&queue, &job_id])?;
            Ok(())
        })
    }

    fn delayed_len(&self, queue: &str) -> Result<usize, Error> {
        let queue = queue.to_string();
        self.run(move |conn, sql| {
            let row = conn.query_one(&sql.delayed_len, &[&queue])?;
            Ok(to_usize(row.get::<_, i64>(0)))
        })
    }

    // ========================================================================
    // Active Queue
    // ========================================================================

    fn active_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let (queue, job_id) = (queue.to_string(), job_id.to_string());
        self.run(move |conn, sql| {
            conn.execute(&sql.active_push, &[&queue, &job_id, &get_now_as_ms()])?;
            Ok(())
        })
    }

    fn active_remove(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let (queue, job_id) = (queue.to_string(), job_id.to_string());
        self.run(move |conn, sql| {
            conn.execute(&sql.active_remove, &[&queue, &job_id])?;
            Ok(())
        })
    }

    fn active_len(&self, queue: &str) -> Result<usize, Error> {
        let queue = queue.to_string();
        self.run(move |conn, sql| {
            let row = conn.query_one(&sql.active_len, &[&queue])?;
            Ok(to_usize(row.get::<_, i64>(0)))
        })
    }

    fn active_list(&self, queue: &str) -> Result<Vec<String>, Error> {
        let queue = queue.to_string();
        self.run(move |conn, sql| {
            let rows = conn.query(&sql.active_list, &[&queue])?;
            Ok(rows.iter().map(|r| r.get(0)).collect())
        })
    }

    // ========================================================================
    // Job Storage
    // ========================================================================

    fn job_save(&self, queue: &str, job_id: &str, data: &str) -> Result<(), Error> {
        let (queue, job_id, data) = (queue.to_string(), job_id.to_string(), data.to_string());
        self.run(move |conn, sql| {
            conn.execute(&sql.job_save, &[&queue, &job_id, &data])?;
            Ok(())
        })
    }

    fn job_get(&self, queue: &str, job_id: &str) -> Result<Option<String>, Error> {
        let (queue, job_id) = (queue.to_string(), job_id.to_string());
        self.run(move |conn, sql| {
            let row = conn.query_opt(&sql.job_get, &[&queue, &job_id])?;
            // Outer None: no row. Inner None: row exists with no payload.
            Ok(row.and_then(|r| r.get::<_, Option<String>>(0)))
        })
    }

    fn job_delete(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let (queue, job_id) = (queue.to_string(), job_id.to_string());
        self.run(move |conn, sql| {
            let mut tx = conn.transaction()?;
            tx.execute(&sql.job_clear_payload, &[&queue, &job_id])?;
            tx.execute(&sql.job_delete_unqueued, &[&queue, &job_id])?;
            tx.commit()?;
            Ok(())
        })
    }

    // ========================================================================
    // Distributed Locking
    // ========================================================================

    fn lock_acquire(&self, job_id: &str, worker_id: &str, ttl_ms: u64) -> Result<bool, Error> {
        let (job_id, worker_id) = (job_id.to_string(), worker_id.to_string());
        let (now_ms, expires_at_ms) = lock_window(ttl_ms);
        self.run(move |conn, sql| {
            let n = conn.execute(
                &sql.lock_acquire,
                &[&job_id, &worker_id, &expires_at_ms, &now_ms],
            )?;
            Ok(n == 1)
        })
    }

    fn lock_release(&self, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        let (job_id, worker_id) = (job_id.to_string(), worker_id.to_string());
        self.run(move |conn, sql| {
            let n = conn.execute(&sql.lock_release, &[&job_id, &worker_id])?;
            Ok(n == 1)
        })
    }

    fn lock_extend(&self, job_id: &str, worker_id: &str, ttl_ms: u64) -> Result<bool, Error> {
        let (job_id, worker_id) = (job_id.to_string(), worker_id.to_string());
        let (now_ms, expires_at_ms) = lock_window(ttl_ms);
        self.run(move |conn, sql| {
            let n = conn.execute(
                &sql.lock_extend,
                &[&job_id, &worker_id, &expires_at_ms, &now_ms],
            )?;
            Ok(n == 1)
        })
    }

    // ========================================================================
    // Atomic Operations
    // ========================================================================

    fn claim_job(
        &self,
        queue: &str,
        worker_id: &str,
        lock_ttl_ms: u64,
    ) -> Result<Option<String>, Error> {
        let (queue, worker_id) = (queue.to_string(), worker_id.to_string());
        let (now_ms, expires_at_ms) = lock_window(lock_ttl_ms);
        self.run(move |conn, sql| {
            let row = conn.query_opt(
                &sql.claim_job,
                &[&queue, &worker_id, &expires_at_ms, &now_ms],
            )?;
            Ok(row.map(|r| r.get(0)))
        })
    }

    fn complete_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        self.finish_job(queue, job_id, worker_id)
    }

    fn fail_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        self.finish_job(queue, job_id, worker_id)
    }

    fn requeue_orphaned(&self, queue: &str) -> Result<Vec<String>, Error> {
        let queue = queue.to_string();
        self.run(move |conn, sql| {
            let rows = conn.query(&sql.requeue_orphaned, &[&queue, &get_now_as_ms()])?;
            Ok(rows.iter().map(|r| r.get(0)).collect())
        })
    }
}

impl Postgres {
    /// Remove from active and release the lock in one transaction.
    ///
    /// The Redis backend does these as two round trips because it has no cheap way to
    /// combine them; here they are atomic, so a crash cannot leave a job out of the active
    /// queue while its lock is still held. An explicit transaction rather than a
    /// data-modifying CTE, because CTE sub-statements have no guaranteed execution order -
    /// which would make the {q}-before-{l} lock ordering nondeterministic and open a
    /// deadlock against `claim_job`.
    fn finish_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        let (queue, job_id, worker_id) =
            (queue.to_string(), job_id.to_string(), worker_id.to_string());
        self.run(move |conn, sql| {
            let mut tx = conn.transaction()?;
            tx.execute(&sql.active_remove, &[&queue, &job_id])?;
            tx.execute(&sql.lock_release, &[&job_id, &worker_id])?;
            tx.commit()?;
            Ok(true)
        })
    }
}

/// `count(*)` comes back as `bigint` and is never negative, but avoid an unchecked cast.
fn to_usize(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;
    use uuid::Uuid;

    const DEFAULT_TEST_URL: &str = "postgres://postgres:postgres@localhost:5432/aj_test";

    /// One backend per test process. Concurrent `CREATE TABLE IF NOT EXISTS` from parallel
    /// tests can trip over Postgres' catalog uniqueness, so the DDL runs exactly once.
    fn test_postgres() -> &'static Postgres {
        static PG: OnceLock<Postgres> = OnceLock::new();
        PG.get_or_init(|| {
            let url =
                std::env::var("AJ_TEST_POSTGRES_URL").unwrap_or_else(|_| DEFAULT_TEST_URL.into());
            Postgres::new(&url)
        })
    }

    fn unique_queue() -> String {
        format!("test:{}", Uuid::new_v4())
    }

    /// Job ids must be unique per test. Lock rows are keyed by `job_id` alone (see the
    /// `{prefix}job_lock` table) and are *not* queue-scoped, so two tests sharing a job id
    /// contend for the same lock when the suite runs in parallel against one database.
    fn unique_job() -> String {
        format!("job:{}", Uuid::new_v4())
    }

    /// Removes every row a test may have touched, including the global lock rows for
    /// `job_ids`, so a panicking test cannot leak a lock into the next run.
    fn cleanup(pg: &Postgres, queue: &str, job_ids: &[&str]) {
        let ids: Vec<String> = job_ids.iter().map(|s| s.to_string()).collect();
        pg.purge_for_test(queue, ids).unwrap();
    }

    // ========================================================================
    // Parity with the Redis backend's suite
    // ========================================================================

    #[test]
    fn test_waiting_queue() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        pg.waiting_push(&queue, &job1).unwrap();
        pg.waiting_push(&queue, &job2).unwrap();

        assert_eq!(pg.waiting_len(&queue).unwrap(), 2);
        assert_eq!(pg.waiting_pop(&queue).unwrap(), Some(job1.clone()));
        assert_eq!(pg.waiting_pop(&queue).unwrap(), Some(job2.clone()));
        assert_eq!(pg.waiting_pop(&queue).unwrap(), None);

        cleanup(pg, &queue, &[&job1, &job2]);
    }

    #[test]
    fn test_delayed_queue() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();
        let job3 = unique_job();

        pg.delayed_push(&queue, &job1, 1000).unwrap();
        pg.delayed_push(&queue, &job2, 2000).unwrap();
        pg.delayed_push(&queue, &job3, 3000).unwrap();

        assert_eq!(pg.delayed_len(&queue).unwrap(), 3);

        // Move ready jobs (job1 and job2)
        let moved = pg.delayed_move_ready(&queue, 2500).unwrap();
        assert_eq!(moved, 2);

        assert_eq!(pg.delayed_len(&queue).unwrap(), 1);
        assert_eq!(pg.waiting_len(&queue).unwrap(), 2);

        // Check order
        assert_eq!(pg.waiting_pop(&queue).unwrap(), Some(job1.clone()));
        assert_eq!(pg.waiting_pop(&queue).unwrap(), Some(job2.clone()));

        cleanup(pg, &queue, &[&job1, &job2, &job3]);
    }

    #[test]
    fn test_delayed_remove() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();

        pg.delayed_push(&queue, &job1, 1000).unwrap();
        assert_eq!(pg.delayed_len(&queue).unwrap(), 1);

        pg.delayed_remove(&queue, &job1).unwrap();
        assert_eq!(pg.delayed_len(&queue).unwrap(), 0);
        // Removal must not resurrect it into waiting.
        assert_eq!(pg.waiting_len(&queue).unwrap(), 0);

        cleanup(pg, &queue, &[&job1]);
    }

    #[test]
    fn test_claim_job() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        pg.waiting_push(&queue, &job1).unwrap();
        pg.waiting_push(&queue, &job2).unwrap();

        // Claim job1
        let job = pg.claim_job(&queue, "worker1", 30000).unwrap();
        assert_eq!(job, Some(job1.clone()));
        assert_eq!(pg.waiting_len(&queue).unwrap(), 1);
        assert_eq!(pg.active_len(&queue).unwrap(), 1);
        assert_eq!(pg.active_list(&queue).unwrap(), vec![job1.clone()]);

        // Verify the lock is held by worker1
        assert_eq!(pg.lock_owner(&job1).unwrap().as_deref(), Some("worker1"));

        cleanup(pg, &queue, &[&job1, &job2]);
    }

    #[test]
    fn test_lock_operations() {
        let pg = test_postgres();
        // Note: no queue row exists for this job id at all. Locks are independent of the
        // job table, exactly as Redis' `aj:lock:{job_id}` is independent of the queues.
        let job_id = unique_job();

        // Acquire lock
        assert!(pg.lock_acquire(&job_id, "worker1", 30000).unwrap());

        // Try to acquire again (should fail)
        assert!(!pg.lock_acquire(&job_id, "worker2", 30000).unwrap());

        // Extend lock (by owner)
        assert!(pg.lock_extend(&job_id, "worker1", 60000).unwrap());

        // Extend lock (by non-owner - should fail)
        assert!(!pg.lock_extend(&job_id, "worker2", 60000).unwrap());

        // Release lock (by non-owner - should fail)
        assert!(!pg.lock_release(&job_id, "worker2").unwrap());

        // Release lock (by owner)
        assert!(pg.lock_release(&job_id, "worker1").unwrap());

        // Now worker2 can acquire
        assert!(pg.lock_acquire(&job_id, "worker2", 30000).unwrap());
        pg.lock_release(&job_id, "worker2").unwrap();
    }

    #[test]
    fn test_requeue_orphaned() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        // Simulate orphaned jobs (in active but no lock)
        pg.active_push(&queue, &job1).unwrap();
        pg.active_push(&queue, &job2).unwrap();

        // job1 has a lock, job2 doesn't (orphaned)
        assert!(pg.lock_acquire(&job1, "worker1", 30000).unwrap());

        let orphaned = pg.requeue_orphaned(&queue).unwrap();
        assert_eq!(orphaned, vec![job2.clone()]);

        assert_eq!(pg.active_len(&queue).unwrap(), 1);
        assert_eq!(pg.waiting_len(&queue).unwrap(), 1);
        assert_eq!(pg.waiting_pop(&queue).unwrap(), Some(job2.clone()));

        cleanup(pg, &queue, &[&job1, &job2]);
    }

    #[test]
    fn test_job_storage() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();

        pg.job_save(&queue, &job1, r#"{"data": 1}"#).unwrap();

        let data = pg.job_get(&queue, &job1).unwrap();
        assert_eq!(data, Some(r#"{"data": 1}"#.to_string()));

        pg.job_delete(&queue, &job1).unwrap();
        assert_eq!(pg.job_get(&queue, &job1).unwrap(), None);

        cleanup(pg, &queue, &[&job1]);
    }

    #[test]
    fn test_complete_and_fail_job() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        pg.waiting_push(&queue, &job1).unwrap();
        pg.waiting_push(&queue, &job2).unwrap();

        let claimed = pg.claim_job(&queue, "worker1", 30000).unwrap().unwrap();
        assert!(pg.complete_job(&queue, &claimed, "worker1").unwrap());
        assert_eq!(pg.active_len(&queue).unwrap(), 0);
        // The lock must be gone, so the id is claimable again after a re-push.
        assert!(pg.lock_acquire(&claimed, "worker2", 30000).unwrap());
        pg.lock_release(&claimed, "worker2").unwrap();

        let claimed2 = pg.claim_job(&queue, "worker1", 30000).unwrap().unwrap();
        assert!(pg.fail_job(&queue, &claimed2, "worker1").unwrap());
        assert_eq!(pg.active_len(&queue).unwrap(), 0);

        cleanup(pg, &queue, &[&job1, &job2]);
    }

    #[test]
    fn test_full_flow() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();

        pg.job_save(&queue, &job1, r#"{"id":"1"}"#).unwrap();
        pg.delayed_push(&queue, &job1, 1000).unwrap();
        assert_eq!(pg.delayed_move_ready(&queue, 2000).unwrap(), 1);

        let claimed = pg.claim_job(&queue, "worker1", 30000).unwrap();
        assert_eq!(claimed, Some(job1.clone()));
        // Claiming must not disturb the stored payload.
        assert_eq!(
            pg.job_get(&queue, &job1).unwrap(),
            Some(r#"{"id":"1"}"#.to_string())
        );

        assert!(pg.complete_job(&queue, &job1, "worker1").unwrap());
        assert_eq!(pg.active_len(&queue).unwrap(), 0);
        assert_eq!(pg.waiting_len(&queue).unwrap(), 0);
        assert_eq!(pg.delayed_len(&queue).unwrap(), 0);

        cleanup(pg, &queue, &[&job1]);
    }

    // ========================================================================
    // Postgres-specific behaviour, with no Redis counterpart
    // ========================================================================

    /// The core guarantee of `FOR UPDATE SKIP LOCKED`: under concurrency every job is
    /// handed to exactly one claimer.
    #[test]
    fn test_concurrent_claim_is_exclusive() {
        const JOBS: usize = 40;
        const THREADS: usize = 8;

        let pg = test_postgres();
        let queue = unique_queue();
        let jobs: Vec<String> = (0..JOBS).map(|_| unique_job()).collect();
        for job in &jobs {
            pg.waiting_push(&queue, job).unwrap();
        }

        let claimed_count = AtomicUsize::new(0);
        let all: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|t| {
                    let queue = queue.as_str();
                    let claimed_count = &claimed_count;
                    scope.spawn(move || {
                        let worker = format!("worker{t}");
                        let mut mine = Vec::new();
                        // Bounded so a bug cannot hang the suite. SKIP LOCKED means a
                        // thread can get None while work remains, so keep trying until
                        // every job is accounted for.
                        for _ in 0..JOBS * 20 {
                            if claimed_count.load(Ordering::SeqCst) >= JOBS {
                                break;
                            }
                            if let Some(id) = pg.claim_job(queue, &worker, 30_000).unwrap() {
                                claimed_count.fetch_add(1, Ordering::SeqCst);
                                mine.push(id);
                            }
                        }
                        mine
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });

        assert_eq!(all.len(), JOBS, "every job should be claimed exactly once");
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), JOBS, "a job was claimed by two workers");
        assert_eq!(pg.waiting_len(&queue).unwrap(), 0);
        assert_eq!(pg.active_len(&queue).unwrap(), JOBS);

        let refs: Vec<&str> = jobs.iter().map(|s| s.as_str()).collect();
        cleanup(pg, &queue, &refs);
    }

    /// Redis gets lock expiry for free from `PX`. Here it is a predicate on
    /// `expires_at_ms`, so it needs explicit coverage.
    #[test]
    fn test_expired_lock_can_be_stolen() {
        let pg = test_postgres();
        let job_id = unique_job();

        assert!(pg.lock_acquire(&job_id, "worker1", 1).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Expired, so another worker may take it...
        assert!(pg.lock_acquire(&job_id, "worker2", 30000).unwrap());
        // ...and the original holder can no longer extend it.
        assert!(!pg.lock_extend(&job_id, "worker1", 30000).unwrap());

        pg.lock_release(&job_id, "worker2").unwrap();
    }

    /// An expired lock leaves a row behind, since Postgres has no TTL eviction.
    #[test]
    fn test_purge_expired_locks() {
        let pg = test_postgres();
        let job_id = unique_job();

        assert!(pg.lock_acquire(&job_id, "worker1", 1).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(20));

        // grace 0: purge anything already expired.
        assert!(pg.purge_expired_locks(0).unwrap() >= 1);
        assert!(pg.lock_owner(&job_id).unwrap().is_none());
    }

    /// Re-pushing a queued job moves it to the back, matching Redis' RPUSH.
    #[test]
    fn test_waiting_repush_moves_to_back() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        pg.waiting_push(&queue, &job1).unwrap();
        pg.waiting_push(&queue, &job2).unwrap();
        pg.waiting_push(&queue, &job1).unwrap();

        assert_eq!(
            pg.waiting_len(&queue).unwrap(),
            2,
            "re-push must not duplicate"
        );
        assert_eq!(pg.waiting_pop(&queue).unwrap(), Some(job2.clone()));
        assert_eq!(pg.waiting_pop(&queue).unwrap(), Some(job1.clone()));

        cleanup(pg, &queue, &[&job1, &job2]);
    }

    /// The prefix is interpolated into SQL rather than bound, so it must be rejected
    /// before it ever reaches the database. Needs no live connection.
    #[test]
    fn test_invalid_table_prefix_is_rejected() {
        for bad in [
            "",
            "1aj_",
            "AJ_",
            "aj-",
            "aj_\"; DROP TABLE users; --",
            "aj_'x",
            "aj_ x",
        ] {
            assert!(
                Sql::new(bad).is_err(),
                "prefix {bad:?} should have been rejected"
            );
        }
        for good in ["aj_", "_", "myapp_aj_", "aj2_"] {
            assert!(Sql::new(good).is_ok(), "prefix {good:?} should be accepted");
        }
    }

    /// Regression test for the reason the executor exists.
    ///
    /// The blocking `postgres` client calls `Runtime::block_on` internally, so calling it
    /// straight from a runtime thread panics with "Cannot start a runtime from within a
    /// runtime". `block_in_place` would paper over the multi-thread case only, so both
    /// flavors are covered here: a `current_thread` runtime is exactly where that shortcut
    /// would fail.
    #[test]
    fn test_usable_from_inside_a_tokio_runtime() {
        let multi = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let current = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        for (flavor, rt) in [("multi_thread", multi), ("current_thread", current)] {
            rt.block_on(async {
                // Construction connects, so it exercises the same path as every query.
                let url = std::env::var("AJ_TEST_POSTGRES_URL")
                    .unwrap_or_else(|_| DEFAULT_TEST_URL.into());
                let pg = Postgres::try_new(&url)
                    .unwrap_or_else(|e| panic!("{flavor}: construction failed: {e:?}"));

                let queue = unique_queue();
                let job = unique_job();
                pg.waiting_push(&queue, &job).unwrap();
                assert_eq!(
                    pg.claim_job(&queue, "worker1", 30_000).unwrap(),
                    Some(job.clone()),
                    "{flavor}: claim inside runtime"
                );
                assert!(pg.complete_job(&queue, &job, "worker1").unwrap());
                cleanup(&pg, &queue, &[&job]);
            });
        }
    }

    #[test]
    fn test_schema_sql_is_prefixed() {
        let ddl = Postgres::schema_sql("myapp_aj_").unwrap();
        assert!(ddl.contains("myapp_aj_job_queue"));
        assert!(ddl.contains("myapp_aj_job_lock"));
        assert!(ddl.contains("myapp_aj_job_queue_seq"));
        assert!(!ddl.contains(" aj_job_queue "));
    }
}
