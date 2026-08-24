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

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use deadpool_postgres::tokio_postgres::{Config as PgConfig, NoTls};
use deadpool_postgres::{Client, Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio::sync::OnceCell;

use crate::types::Backend;
use crate::{get_now_as_ms, Error};

/// Default table-name prefix. Every object this backend creates is prefixed so that `aj`
/// can share a database with application tables without colliding.
pub const DEFAULT_TABLE_PREFIX: &str = "aj_";

/// Default maximum pooled connections.
pub const DEFAULT_POOL_SIZE: usize = 10;

/// How long to wait for a free connection before giving up.
const POOL_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest accepted table prefix. Postgres truncates identifiers at 63 bytes and the
/// longest suffix generated here is `job_queue_pick_idx` (18 bytes).
const MAX_PREFIX_LEN: usize = 40;

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
///
/// # Connection
///
/// `Pool` is lazy: building it opens no sockets, which is what lets `new` stay synchronous
/// like [`crate::backend::redis::Redis::new`]. The schema bootstrap does need a connection,
/// so it runs once on first use rather than during construction.
#[derive(Clone)]
pub struct Postgres {
    pool: Pool,
    sql: Arc<Sql>,
    auto_migrate: bool,
    /// The bootstrap runs at most once per backend instance.
    migrated: Arc<OnceCell<()>>,
}

impl std::fmt::Debug for Postgres {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Postgres")
            .field("table_prefix", &self.sql.prefix)
            .field("pool", &self.pool.status())
            .finish()
    }
}

impl Postgres {
    /// Create a new Postgres backend.
    ///
    /// Panics if the URL is malformed, matching the convention of
    /// [`crate::backend::redis::Redis::new`]. Use [`Postgres::try_new`] to handle that.
    /// A database that is merely unreachable is *not* an error here: the pool connects
    /// lazily, so the failure surfaces on first use.
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

    /// The resolved table prefix.
    pub fn table_prefix(&self) -> &str {
        &self.sql.prefix
    }

    /// Delete lock rows whose TTL elapsed more than `grace_ms` ago.
    ///
    /// Postgres has no equivalent of Redis' key eviction, so expired locks linger as rows.
    /// They are already treated as free by `lock_acquire` and `claim_job`, so this is
    /// housekeeping rather than correctness. The grace period avoids racing a worker that
    /// is about to call `lock_extend`. Returns the number of rows removed.
    pub async fn purge_expired_locks(&self, grace_ms: u64) -> Result<usize, Error> {
        let cutoff = get_now_as_ms().saturating_sub(clamp_ms(grace_ms));
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.purge_expired_locks).await?;
        let n = client.execute(&stmt, &[&cutoff]).await?;
        Ok(n as usize)
    }

    /// Create the schema now, rather than on first use.
    ///
    /// `CREATE TABLE IF NOT EXISTS` is explicitly *not* race-free - concurrent creators can
    /// get `duplicate key value violates unique constraint "pg_type_typname_nsp_index"` -
    /// so the whole bootstrap runs under a transaction-scoped advisory lock keyed on the
    /// prefix. Two different prefixes still bootstrap concurrently.
    pub async fn migrate(&self) -> Result<(), Error> {
        let mut client = self.pool.get().await?;
        let txn = client.transaction().await?;
        txn.execute("SELECT pg_advisory_xact_lock($1)", &[&self.sql.lock_key])
            .await?;
        txn.batch_execute(&self.sql.ddl).await?;
        txn.commit().await?;
        Ok(())
    }

    /// A pooled connection, with the schema guaranteed to exist.
    ///
    /// The `OnceCell` is left empty if the bootstrap fails, so a database that is briefly
    /// unavailable is retried on the next call instead of poisoning the backend.
    async fn client(&self) -> Result<Client, Error> {
        if self.auto_migrate {
            self.migrated.get_or_try_init(|| self.migrate()).await?;
        }
        Ok(self.pool.get().await?)
    }
}

/// Builder for [`Postgres`].
///
/// A builder rather than `with_*` setters on the backend itself, because the table names
/// have to be known before any statement can be built.
pub struct PostgresBuilder {
    url: String,
    table_prefix: String,
    pool_size: usize,
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

    /// Maximum pooled connections. Default 10.
    pub fn pool_size(mut self, size: usize) -> Self {
        self.pool_size = size.max(1);
        self
    }

    /// Whether to create the schema on first use. Default true.
    pub fn auto_migrate(mut self, auto_migrate: bool) -> Self {
        self.auto_migrate = auto_migrate;
        self
    }

    pub fn build(self) -> Result<Postgres, Error> {
        let sql = Arc::new(Sql::new(&self.table_prefix)?);

        // Parsing here rather than handing the raw URL to deadpool's own `Config`, so that a
        // malformed URL is rejected by `new` instead of surfacing on the first query.
        let pg_config = PgConfig::from_str(&self.url)
            .map_err(|e| Error::Postgres(format!("invalid Postgres url: {e}")))?;

        let manager = Manager::from_config(
            pg_config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );

        // Sync and lazy: no socket is opened here.
        let pool = Pool::builder(manager)
            .max_size(self.pool_size)
            .wait_timeout(Some(POOL_WAIT_TIMEOUT))
            // Required whenever a timeout is set, else `BuildError::NoRuntimeSpecified`.
            .runtime(Runtime::Tokio1)
            .build()
            .map_err(|e| Error::Postgres(format!("failed to build Postgres pool: {e}")))?;

        Ok(Postgres {
            pool,
            sql,
            auto_migrate: self.auto_migrate,
            migrated: Arc::new(OnceCell::new()),
        })
    }
}

// ============================================================================
// Statements
// ============================================================================

/// Every statement, pre-formatted with the resolved table names at construction time.
///
/// The text is formatted once; `prepare_cached` then keeps the parsed plan per connection,
/// so the repeated statements in the tick loop cost no extra round trips once warm.
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
/// Postgres has no unsigned integers, and an unchecked `as i64` on an absurd TTL wraps
/// negative, which would make the lock permanently expired and silently disable mutual
/// exclusion.
fn clamp_ms(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// `(now_ms, expires_at_ms)` from a single clock reading.
fn lock_window(ttl_ms: u64) -> (i64, i64) {
    let now = get_now_as_ms();
    (now, now.saturating_add(clamp_ms(ttl_ms)))
}

/// `count(*)` comes back as `bigint` and is never negative, but avoid an unchecked cast.
fn to_usize(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
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

#[async_trait]
impl Backend for Postgres {
    // ========================================================================
    // Waiting Queue
    // ========================================================================

    async fn waiting_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.waiting_push).await?;
        client
            .execute(&stmt, &[&queue, &job_id, &get_now_as_ms()])
            .await?;
        Ok(())
    }

    async fn waiting_pop(&self, queue: &str) -> Result<Option<String>, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.waiting_pop).await?;
        let row = client.query_opt(&stmt, &[&queue]).await?;
        row.map(|r| r.try_get(0)).transpose().map_err(Into::into)
    }

    async fn waiting_len(&self, queue: &str) -> Result<usize, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.waiting_len).await?;
        let row = client.query_one(&stmt, &[&queue]).await?;
        Ok(to_usize(row.try_get::<_, i64>(0)?))
    }

    // ========================================================================
    // Delayed Queue
    // ========================================================================

    async fn delayed_push(&self, queue: &str, job_id: &str, run_at_ms: i64) -> Result<(), Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.delayed_push).await?;
        client
            .execute(&stmt, &[&queue, &job_id, &run_at_ms])
            .await?;
        Ok(())
    }

    async fn delayed_move_ready(&self, queue: &str, now_ms: i64) -> Result<usize, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.delayed_move_ready).await?;
        let n = client.execute(&stmt, &[&queue, &now_ms]).await?;
        Ok(n as usize)
    }

    async fn delayed_remove(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.delayed_remove).await?;
        client.execute(&stmt, &[&queue, &job_id]).await?;
        Ok(())
    }

    async fn delayed_len(&self, queue: &str) -> Result<usize, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.delayed_len).await?;
        let row = client.query_one(&stmt, &[&queue]).await?;
        Ok(to_usize(row.try_get::<_, i64>(0)?))
    }

    // ========================================================================
    // Active Queue
    // ========================================================================

    async fn active_push(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.active_push).await?;
        client
            .execute(&stmt, &[&queue, &job_id, &get_now_as_ms()])
            .await?;
        Ok(())
    }

    async fn active_remove(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.active_remove).await?;
        client.execute(&stmt, &[&queue, &job_id]).await?;
        Ok(())
    }

    async fn active_len(&self, queue: &str) -> Result<usize, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.active_len).await?;
        let row = client.query_one(&stmt, &[&queue]).await?;
        Ok(to_usize(row.try_get::<_, i64>(0)?))
    }

    async fn active_list(&self, queue: &str) -> Result<Vec<String>, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.active_list).await?;
        let rows = client.query(&stmt, &[&queue]).await?;
        rows.iter()
            .map(|r| r.try_get(0))
            .collect::<Result<Vec<String>, _>>()
            .map_err(Into::into)
    }

    // ========================================================================
    // Job Storage
    // ========================================================================

    async fn job_save(&self, queue: &str, job_id: &str, data: &str) -> Result<(), Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.job_save).await?;
        client.execute(&stmt, &[&queue, &job_id, &data]).await?;
        Ok(())
    }

    async fn job_get(&self, queue: &str, job_id: &str) -> Result<Option<String>, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.job_get).await?;
        let row = client.query_opt(&stmt, &[&queue, &job_id]).await?;
        // Outer None: no row. Inner None: row exists with no payload. `payload` is
        // nullable, so this must be read as an Option or it errors at runtime.
        match row {
            Some(r) => Ok(r.try_get::<_, Option<String>>(0)?),
            None => Ok(None),
        }
    }

    async fn job_delete(&self, queue: &str, job_id: &str) -> Result<(), Error> {
        let mut client = self.client().await?;
        let txn = client.transaction().await?;
        let clear = txn.prepare_cached(&self.sql.job_clear_payload).await?;
        txn.execute(&clear, &[&queue, &job_id]).await?;
        let delete = txn.prepare_cached(&self.sql.job_delete_unqueued).await?;
        txn.execute(&delete, &[&queue, &job_id]).await?;
        txn.commit().await?;
        Ok(())
    }

    // ========================================================================
    // Distributed Locking
    // ========================================================================

    async fn lock_acquire(
        &self,
        job_id: &str,
        worker_id: &str,
        ttl_ms: u64,
    ) -> Result<bool, Error> {
        let (now_ms, expires_at_ms) = lock_window(ttl_ms);
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.lock_acquire).await?;
        let n = client
            .execute(&stmt, &[&job_id, &worker_id, &expires_at_ms, &now_ms])
            .await?;
        Ok(n == 1)
    }

    async fn lock_release(&self, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.lock_release).await?;
        let n = client.execute(&stmt, &[&job_id, &worker_id]).await?;
        Ok(n == 1)
    }

    async fn lock_extend(&self, job_id: &str, worker_id: &str, ttl_ms: u64) -> Result<bool, Error> {
        let (now_ms, expires_at_ms) = lock_window(ttl_ms);
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.lock_extend).await?;
        let n = client
            .execute(&stmt, &[&job_id, &worker_id, &expires_at_ms, &now_ms])
            .await?;
        Ok(n == 1)
    }

    // ========================================================================
    // Atomic Operations
    // ========================================================================

    async fn claim_job(
        &self,
        queue: &str,
        worker_id: &str,
        lock_ttl_ms: u64,
    ) -> Result<Option<String>, Error> {
        let (now_ms, expires_at_ms) = lock_window(lock_ttl_ms);
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.claim_job).await?;
        let row = client
            .query_opt(&stmt, &[&queue, &worker_id, &expires_at_ms, &now_ms])
            .await?;
        row.map(|r| r.try_get(0)).transpose().map_err(Into::into)
    }

    async fn complete_job(
        &self,
        queue: &str,
        job_id: &str,
        worker_id: &str,
    ) -> Result<bool, Error> {
        self.finish_job(queue, job_id, worker_id).await
    }

    async fn fail_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        self.finish_job(queue, job_id, worker_id).await
    }

    async fn requeue_orphaned(&self, queue: &str) -> Result<Vec<String>, Error> {
        let client = self.client().await?;
        let stmt = client.prepare_cached(&self.sql.requeue_orphaned).await?;
        let rows = client.query(&stmt, &[&queue, &get_now_as_ms()]).await?;
        rows.iter()
            .map(|r| r.try_get(0))
            .collect::<Result<Vec<String>, _>>()
            .map_err(Into::into)
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
    async fn finish_job(&self, queue: &str, job_id: &str, worker_id: &str) -> Result<bool, Error> {
        let mut client = self.client().await?;
        let txn = client.transaction().await?;
        let remove = txn.prepare_cached(&self.sql.active_remove).await?;
        txn.execute(&remove, &[&queue, &job_id]).await?;
        let release = txn.prepare_cached(&self.sql.lock_release).await?;
        txn.execute(&release, &[&job_id, &worker_id]).await?;
        txn.commit().await?;
        Ok(true)
    }

    /// Who holds the lock on `job_id`, if anyone. Test assertions only.
    #[cfg(test)]
    async fn lock_owner(&self, job_id: &str) -> Result<Option<String>, Error> {
        let client = self.client().await?;
        let sql = format!(
            "SELECT worker_id FROM {}job_lock WHERE job_id = $1::text",
            self.sql.prefix
        );
        let row = client.query_opt(&sql, &[&job_id]).await?;
        row.map(|r| r.try_get(0)).transpose().map_err(Into::into)
    }

    /// Delete everything a test may have touched. Test teardown only.
    #[cfg(test)]
    async fn purge_for_test(&self, queue: &str, job_ids: &[String]) -> Result<(), Error> {
        let client = self.client().await?;
        client
            .execute(
                &format!(
                    "DELETE FROM {}job_queue WHERE queue = $1::text",
                    self.sql.prefix
                ),
                &[&queue],
            )
            .await?;
        client
            .execute(
                &format!(
                    "DELETE FROM {}job_lock WHERE job_id = ANY($1)",
                    self.sql.prefix
                ),
                &[&job_ids],
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use uuid::Uuid;

    use super::*;

    const DEFAULT_TEST_URL: &str = "postgres://postgres:postgres@localhost:5432/aj_test";

    /// A fresh backend per test, deliberately not a shared `static`.
    ///
    /// deadpool spawns a task per connection to drive `tokio_postgres::Connection`, and that
    /// task belongs to whichever runtime created it. `#[tokio::test]` builds a new runtime
    /// per test, so a pool shared across tests loses its connections as soon as the first
    /// test's runtime is dropped, and every later test fails with "connection closed".
    /// Building a pool is cheap and opens no sockets, so per-test is the right granularity.
    /// The repeated schema bootstrap is idempotent and advisory-locked.
    fn test_postgres() -> Postgres {
        let url = std::env::var("AJ_TEST_POSTGRES_URL").unwrap_or_else(|_| DEFAULT_TEST_URL.into());
        Postgres::new(&url)
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
    async fn cleanup(pg: &Postgres, queue: &str, job_ids: &[&str]) {
        let ids: Vec<String> = job_ids.iter().map(|s| s.to_string()).collect();
        pg.purge_for_test(queue, &ids).await.unwrap();
    }

    // ========================================================================
    // Parity with the Redis backend's suite
    // ========================================================================

    #[tokio::test]
    async fn test_waiting_queue() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        pg.waiting_push(&queue, &job1).await.unwrap();
        pg.waiting_push(&queue, &job2).await.unwrap();

        assert_eq!(pg.waiting_len(&queue).await.unwrap(), 2);
        assert_eq!(pg.waiting_pop(&queue).await.unwrap(), Some(job1.clone()));
        assert_eq!(pg.waiting_pop(&queue).await.unwrap(), Some(job2.clone()));
        assert_eq!(pg.waiting_pop(&queue).await.unwrap(), None);

        cleanup(&pg, &queue, &[&job1, &job2]).await;
    }

    #[tokio::test]
    async fn test_delayed_queue() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();
        let job3 = unique_job();

        pg.delayed_push(&queue, &job1, 1000).await.unwrap();
        pg.delayed_push(&queue, &job2, 2000).await.unwrap();
        pg.delayed_push(&queue, &job3, 3000).await.unwrap();

        assert_eq!(pg.delayed_len(&queue).await.unwrap(), 3);

        // Move ready jobs (job1 and job2)
        let moved = pg.delayed_move_ready(&queue, 2500).await.unwrap();
        assert_eq!(moved, 2);

        assert_eq!(pg.delayed_len(&queue).await.unwrap(), 1);
        assert_eq!(pg.waiting_len(&queue).await.unwrap(), 2);

        // Order is preserved: run_at is the sort key.
        assert_eq!(pg.waiting_pop(&queue).await.unwrap(), Some(job1.clone()));
        assert_eq!(pg.waiting_pop(&queue).await.unwrap(), Some(job2.clone()));

        cleanup(&pg, &queue, &[&job1, &job2, &job3]).await;
    }

    #[tokio::test]
    async fn test_delayed_remove() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();

        pg.delayed_push(&queue, &job1, 1000).await.unwrap();
        assert_eq!(pg.delayed_len(&queue).await.unwrap(), 1);

        pg.delayed_remove(&queue, &job1).await.unwrap();
        assert_eq!(pg.delayed_len(&queue).await.unwrap(), 0);
        // Removal must not resurrect it into waiting.
        assert_eq!(pg.waiting_len(&queue).await.unwrap(), 0);

        cleanup(&pg, &queue, &[&job1]).await;
    }

    #[tokio::test]
    async fn test_claim_job() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        pg.waiting_push(&queue, &job1).await.unwrap();
        pg.waiting_push(&queue, &job2).await.unwrap();

        let job = pg.claim_job(&queue, "worker1", 30000).await.unwrap();
        assert_eq!(job, Some(job1.clone()));
        assert_eq!(pg.waiting_len(&queue).await.unwrap(), 1);
        assert_eq!(pg.active_len(&queue).await.unwrap(), 1);
        assert_eq!(pg.active_list(&queue).await.unwrap(), vec![job1.clone()]);
        assert_eq!(
            pg.lock_owner(&job1).await.unwrap().as_deref(),
            Some("worker1")
        );

        cleanup(&pg, &queue, &[&job1, &job2]).await;
    }

    #[tokio::test]
    async fn test_lock_operations() {
        let pg = test_postgres();
        // Note: no queue row exists for this job id at all. Locks are independent of the
        // job table, exactly as Redis' `aj:lock:{job_id}` is independent of the queues.
        let job_id = unique_job();

        assert!(pg.lock_acquire(&job_id, "worker1", 30000).await.unwrap());
        // Already held.
        assert!(!pg.lock_acquire(&job_id, "worker2", 30000).await.unwrap());
        // Extend by owner.
        assert!(pg.lock_extend(&job_id, "worker1", 60000).await.unwrap());
        // Extend by non-owner.
        assert!(!pg.lock_extend(&job_id, "worker2", 60000).await.unwrap());
        // Release by non-owner.
        assert!(!pg.lock_release(&job_id, "worker2").await.unwrap());
        // Release by owner.
        assert!(pg.lock_release(&job_id, "worker1").await.unwrap());
        // Now free.
        assert!(pg.lock_acquire(&job_id, "worker2", 30000).await.unwrap());
        pg.lock_release(&job_id, "worker2").await.unwrap();
    }

    #[tokio::test]
    async fn test_requeue_orphaned() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        // Active but unlocked is what "orphaned" means.
        pg.active_push(&queue, &job1).await.unwrap();
        pg.active_push(&queue, &job2).await.unwrap();
        assert!(pg.lock_acquire(&job1, "worker1", 30000).await.unwrap());

        let orphaned = pg.requeue_orphaned(&queue).await.unwrap();
        assert_eq!(orphaned, vec![job2.clone()]);

        assert_eq!(pg.active_len(&queue).await.unwrap(), 1);
        assert_eq!(pg.waiting_len(&queue).await.unwrap(), 1);
        assert_eq!(pg.waiting_pop(&queue).await.unwrap(), Some(job2.clone()));

        cleanup(&pg, &queue, &[&job1, &job2]).await;
    }

    #[tokio::test]
    async fn test_job_storage() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();

        pg.job_save(&queue, &job1, r#"{"data": 1}"#).await.unwrap();

        let data = pg.job_get(&queue, &job1).await.unwrap();
        assert_eq!(data, Some(r#"{"data": 1}"#.to_string()));

        pg.job_delete(&queue, &job1).await.unwrap();
        assert_eq!(pg.job_get(&queue, &job1).await.unwrap(), None);

        cleanup(&pg, &queue, &[&job1]).await;
    }

    #[tokio::test]
    async fn test_complete_and_fail_job() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        pg.waiting_push(&queue, &job1).await.unwrap();
        pg.waiting_push(&queue, &job2).await.unwrap();

        let claimed = pg
            .claim_job(&queue, "worker1", 30000)
            .await
            .unwrap()
            .unwrap();
        assert!(pg.complete_job(&queue, &claimed, "worker1").await.unwrap());
        assert_eq!(pg.active_len(&queue).await.unwrap(), 0);
        // The lock must be gone.
        assert!(pg.lock_acquire(&claimed, "worker2", 30000).await.unwrap());
        pg.lock_release(&claimed, "worker2").await.unwrap();

        let claimed2 = pg
            .claim_job(&queue, "worker1", 30000)
            .await
            .unwrap()
            .unwrap();
        assert!(pg.fail_job(&queue, &claimed2, "worker1").await.unwrap());
        assert_eq!(pg.active_len(&queue).await.unwrap(), 0);

        cleanup(&pg, &queue, &[&job1, &job2]).await;
    }

    #[tokio::test]
    async fn test_full_flow() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();

        pg.job_save(&queue, &job1, r#"{"id":"1"}"#).await.unwrap();
        pg.delayed_push(&queue, &job1, 1000).await.unwrap();
        assert_eq!(pg.delayed_move_ready(&queue, 2000).await.unwrap(), 1);

        let claimed = pg.claim_job(&queue, "worker1", 30000).await.unwrap();
        assert_eq!(claimed, Some(job1.clone()));
        // Claiming must not disturb the stored payload.
        assert_eq!(
            pg.job_get(&queue, &job1).await.unwrap(),
            Some(r#"{"id":"1"}"#.to_string())
        );

        assert!(pg.complete_job(&queue, &job1, "worker1").await.unwrap());
        assert_eq!(pg.active_len(&queue).await.unwrap(), 0);
        assert_eq!(pg.waiting_len(&queue).await.unwrap(), 0);
        assert_eq!(pg.delayed_len(&queue).await.unwrap(), 0);

        cleanup(&pg, &queue, &[&job1]).await;
    }

    // ========================================================================
    // Postgres-specific behaviour, with no Redis counterpart
    // ========================================================================

    /// The core guarantee of `FOR UPDATE SKIP LOCKED`: under concurrency every job is
    /// handed to exactly one claimer. Multi-thread on purpose, so the claims genuinely
    /// race rather than merely interleaving at await points.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_claim_is_exclusive() {
        const JOBS: usize = 40;
        const WORKERS: usize = 8;

        let pg = Arc::new(test_postgres());
        let queue = unique_queue();
        let jobs: Vec<String> = (0..JOBS).map(|_| unique_job()).collect();
        for job in &jobs {
            pg.waiting_push(&queue, job).await.unwrap();
        }

        let claimed_count = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for w in 0..WORKERS {
            let queue = queue.clone();
            let claimed_count = Arc::clone(&claimed_count);
            let pg = Arc::clone(&pg);
            handles.push(tokio::spawn(async move {
                let worker = format!("worker{w}");
                let mut mine = Vec::new();
                // Bounded so a bug cannot hang the suite. SKIP LOCKED means a claimer can
                // get None while work remains, so keep trying until all are accounted for.
                for _ in 0..JOBS * 20 {
                    if claimed_count.load(Ordering::SeqCst) >= JOBS {
                        break;
                    }
                    if let Some(id) = pg.claim_job(&queue, &worker, 30_000).await.unwrap() {
                        claimed_count.fetch_add(1, Ordering::SeqCst);
                        mine.push(id);
                    }
                }
                mine
            }));
        }

        let mut all = Vec::new();
        for h in handles {
            all.extend(h.await.unwrap());
        }

        assert_eq!(all.len(), JOBS, "every job should be claimed exactly once");
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), JOBS, "a job was claimed by two workers");
        assert_eq!(pg.waiting_len(&queue).await.unwrap(), 0);
        assert_eq!(pg.active_len(&queue).await.unwrap(), JOBS);

        let refs: Vec<&str> = jobs.iter().map(|s| s.as_str()).collect();
        cleanup(&pg, &queue, &refs).await;
    }

    /// Redis gets lock expiry for free from `PX`. Here it is a predicate on
    /// `expires_at_ms`, so it needs explicit coverage.
    #[tokio::test]
    async fn test_expired_lock_can_be_stolen() {
        let pg = test_postgres();
        let job_id = unique_job();

        assert!(pg.lock_acquire(&job_id, "worker1", 1).await.unwrap());
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Expired, so another worker may take it...
        assert!(pg.lock_acquire(&job_id, "worker2", 30000).await.unwrap());
        // ...and the original holder can no longer extend it.
        assert!(!pg.lock_extend(&job_id, "worker1", 30000).await.unwrap());

        pg.lock_release(&job_id, "worker2").await.unwrap();
    }

    /// An expired lock leaves a row behind, since Postgres has no TTL eviction.
    #[tokio::test]
    async fn test_purge_expired_locks() {
        let pg = test_postgres();
        let job_id = unique_job();

        assert!(pg.lock_acquire(&job_id, "worker1", 1).await.unwrap());
        tokio::time::sleep(Duration::from_millis(20)).await;

        // grace 0: purge anything already expired.
        assert!(pg.purge_expired_locks(0).await.unwrap() >= 1);
        assert!(pg.lock_owner(&job_id).await.unwrap().is_none());
    }

    /// Re-pushing a queued job moves it to the back, matching Redis' RPUSH.
    #[tokio::test]
    async fn test_waiting_repush_moves_to_back() {
        let pg = test_postgres();
        let queue = unique_queue();
        let job1 = unique_job();
        let job2 = unique_job();

        pg.waiting_push(&queue, &job1).await.unwrap();
        pg.waiting_push(&queue, &job2).await.unwrap();
        pg.waiting_push(&queue, &job1).await.unwrap();

        assert_eq!(
            pg.waiting_len(&queue).await.unwrap(),
            2,
            "re-push must not duplicate"
        );
        assert_eq!(pg.waiting_pop(&queue).await.unwrap(), Some(job2.clone()));
        assert_eq!(pg.waiting_pop(&queue).await.unwrap(), Some(job1.clone()));

        cleanup(&pg, &queue, &[&job1, &job2]).await;
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

    /// A malformed URL is rejected by the constructor, not deferred to the first query.
    #[test]
    fn test_invalid_url_is_rejected() {
        assert!(Postgres::try_new("not-a-url").is_err());
        assert!(Postgres::try_new("postgres://localhost/db").is_ok());
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
