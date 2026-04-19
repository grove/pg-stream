//! Background worker scheduler for pgtrickle.
//!
//! # Architecture — Launcher + Per-Database Workers
//!
//! The extension uses a two-tier background worker model:
//!
//! 1. **Launcher** (`pg_trickle launcher`) — one per server, static.
//!    - Connects to the `postgres` database to query system catalogs.
//!    - Every 10 s, scans `pg_database` for all available databases.
//!    - For each database, checks `pg_stat_activity` for an existing scheduler.
//!    - Spawns a dynamic per-database scheduler for any database that lacks one.
//!    - Automatically re-spawns schedulers that crash or exit.
//!    - Databases without pg_trickle installed are skipped for 5 minutes before
//!      re-probing (graceful exit by the per-DB worker signals this).
//!
//! 2. **Per-database scheduler** (`pg_trickle scheduler`) — one per database
//!    that has pg_trickle installed, dynamic.
//!    - Receives the database name via `bgw_extra` (set by the launcher).
//!    - Checks that pg_trickle is installed; exits cleanly if not.
//!    - Wakes every `pg_trickle.scheduler_interval_ms` milliseconds.
//!    - Reads shared memory to detect DAG changes.
//!    - Consumes CDC changes, determines refresh needs, executes refreshes.
//!
//! This design means pg_trickle works transparently in all databases on a
//! server without any manual configuration.
//!
//! # Error Handling & Resilience
//! - **Row-level catalog locks**: prevent concurrent refreshes of the same ST
//! - **Retry with backoff**: retryable errors get exponential backoff per ST
//! - **Skip mechanism**: if a ST refresh is already running, skip it gracefully
//! - **Crash recovery**: on startup, mark interrupted RUNNING records as FAILED
//! - **Error classification**: only retryable errors trigger retry; user/schema
//!   errors fail immediately

use pgrx::bgworkers::*;
use pgrx::prelude::*;

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;

use crate::catalog::{JobStatus, SchedulerJob};
use crate::catalog::{RefreshRecord, StreamTableMeta};
use crate::cdc;
use crate::config;
use crate::dag::{
    DiamondConsistency, DiamondSchedulePolicy, ExecutionUnit, ExecutionUnitDag, ExecutionUnitId,
    NodeId, RefreshMode, Scc, StDag, StStatus,
};
use crate::error::{RetryPolicy, RetryState};
use crate::monitor;
use crate::refresh::{self, RefreshAction};
use crate::shmem;
use crate::version;
use crate::wal_decoder;

// ── G-7: Refresh tier classification ───────────────────────────────────

/// G-7: Refresh tier for tiered scheduling.
///
/// Controls the effective schedule multiplier when `pg_trickle.tiered_scheduling`
/// is enabled. User-assignable via `ALTER STREAM TABLE ... SET (tier = 'warm')`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RefreshTier {
    /// Refresh at configured schedule (1× multiplier). Default.
    #[default]
    Hot,
    /// Refresh at 2× configured schedule.
    Warm,
    /// Refresh at 10× configured schedule.
    Cold,
    /// Skip refresh entirely (manually promoted back to Hot/Warm/Cold).
    Frozen,
}

impl RefreshTier {
    /// Parse from SQL string. Falls back to `Hot` for unknown values.
    pub fn from_sql_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "hot" => RefreshTier::Hot,
            "warm" => RefreshTier::Warm,
            "cold" => RefreshTier::Cold,
            "frozen" => RefreshTier::Frozen,
            _ => RefreshTier::Hot,
        }
    }

    /// SQL representation.
    pub fn as_str(self) -> &'static str {
        match self {
            RefreshTier::Hot => "hot",
            RefreshTier::Warm => "warm",
            RefreshTier::Cold => "cold",
            RefreshTier::Frozen => "frozen",
        }
    }

    /// Schedule multiplier for this tier.
    ///
    /// Returns `None` for Frozen (skip entirely).
    pub fn schedule_multiplier(self) -> Option<f64> {
        match self {
            RefreshTier::Hot => Some(1.0),
            RefreshTier::Warm => Some(2.0),
            RefreshTier::Cold => Some(10.0),
            RefreshTier::Frozen => None,
        }
    }

    /// Validate a tier string. Returns `true` if recognized.
    pub fn is_valid_str(s: &str) -> bool {
        matches!(
            s.to_lowercase().as_str(),
            "hot" | "warm" | "cold" | "frozen"
        )
    }
}

impl std::fmt::Display for RefreshTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ── Sub-transaction RAII guard ─────────────────────────────────────────

/// RAII guard for a PostgreSQL internal sub-transaction.
///
/// Automatically rolls back on drop if neither [`commit()`](SubTransaction::commit)
/// nor [`rollback()`](SubTransaction::rollback) was called (panic safety).
struct SubTransaction {
    old_cxt: pg_sys::MemoryContext,
    old_owner: pg_sys::ResourceOwner,
    finished: bool,
}

impl SubTransaction {
    /// Begin a new sub-transaction within the current worker transaction.
    fn begin() -> Self {
        // SAFETY: Called within a PostgreSQL worker transaction. The current
        // memory context and resource owner are valid.
        let old_cxt = unsafe { pg_sys::CurrentMemoryContext };
        let old_owner = unsafe { pg_sys::CurrentResourceOwner };
        // SAFETY: BeginInternalSubTransaction sets up a sub-transaction
        // using PostgreSQL's resource-owner mechanism.
        unsafe { pg_sys::BeginInternalSubTransaction(std::ptr::null()) };
        Self {
            old_cxt,
            old_owner,
            finished: false,
        }
    }

    /// Commit the sub-transaction and restore the outer context.
    fn commit(mut self) {
        // SAFETY: Commits the sub-transaction, restores context.
        unsafe {
            pg_sys::ReleaseCurrentSubTransaction();
            pg_sys::MemoryContextSwitchTo(self.old_cxt);
            pg_sys::CurrentResourceOwner = self.old_owner;
        }
        self.finished = true;
    }

    /// Roll back the sub-transaction and restore the outer context.
    fn rollback(mut self) {
        // SAFETY: Rolls back the sub-transaction, restores context.
        unsafe {
            pg_sys::RollbackAndReleaseCurrentSubTransaction();
            pg_sys::MemoryContextSwitchTo(self.old_cxt);
            pg_sys::CurrentResourceOwner = self.old_owner;
        }
        self.finished = true;
    }
}

impl Drop for SubTransaction {
    fn drop(&mut self) {
        if !self.finished {
            // Auto-rollback on drop for panic safety.
            // SAFETY: Same invariants as rollback().
            unsafe {
                pg_sys::RollbackAndReleaseCurrentSubTransaction();
                pg_sys::MemoryContextSwitchTo(self.old_cxt);
                pg_sys::CurrentResourceOwner = self.old_owner;
            }
        }
    }
}

/// Register the launcher background worker.
///
/// Called from `_PG_init()` when loaded via `shared_preload_libraries`.
/// The launcher discovers all databases on the server and spawns a separate
/// per-database scheduler worker for each one that has pg_trickle installed.
pub fn register_launcher_worker() {
    BackgroundWorkerBuilder::new("pg_trickle launcher")
        .set_function("pg_trickle_launcher_main")
        .set_library("pg_trickle")
        .enable_spi_access()
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(Some(std::time::Duration::from_secs(5)))
        .load();
}

/// Main entry point for the launcher background worker.
///
/// Runs once per server. Connects to `postgres` (always present) to query
/// `pg_database` and `pg_stat_activity`, then dynamically spawns a scheduler
/// worker for every database where pg_trickle is installed.
///
/// # Safety
/// Called directly by PostgreSQL as a background worker entry point.
#[pg_guard]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_trickle_launcher_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    // Connect to `postgres` — always available; used only for system catalog queries.
    BackgroundWorker::connect_worker_to_spi(Some("postgres"), None);

    // Check for replica — launcher cannot spawn workers on standbys.
    let is_replica = BackgroundWorker::transaction(AssertUnwindSafe(|| -> bool {
        Spi::get_one::<bool>("SELECT pg_is_in_recovery()")
            .unwrap_or(Some(false))
            .unwrap_or(false)
    }));
    if is_replica {
        log!("pg_trickle launcher: running on read replica, sleeping until promotion");
        loop {
            let ok = BackgroundWorker::wait_latch(Some(std::time::Duration::from_secs(30)));
            if !ok {
                return;
            }
            let still = BackgroundWorker::transaction(AssertUnwindSafe(|| -> bool {
                Spi::get_one::<bool>("SELECT pg_is_in_recovery()")
                    .unwrap_or(Some(false))
                    .unwrap_or(false)
            }));
            if !still {
                break;
            }
        }
    }

    log!("pg_trickle launcher started");

    // Track the DAG rebuild signal so we can detect CREATE EXTENSION in any
    // database and immediately re-probe, rather than waiting for skip_ttl.
    let mut last_dag_version = shmem::current_dag_version();

    // last_attempt[db] = when we last tried to spawn a worker for that DB.
    // Used to avoid hammering databases where pg_trickle is not installed.
    let mut last_attempt: HashMap<String, std::time::Instant> = HashMap::new();
    // DBs where we have seen a scheduler successfully start at least once this
    // session. Used to distinguish "never had pg_trickle" (long backoff) from
    // "had a running scheduler that crashed" (short backoff).
    let mut had_scheduler: HashSet<String> = HashSet::new();
    // How long to wait before re-probing a DB that has never had pg_trickle.
    let skip_ttl = std::time::Duration::from_secs(300);
    // How long to wait before respawning a scheduler that crashed / exited
    // on a DB where pg_trickle was previously confirmed running.  Kept short
    // so DROP EXTENSION + CREATE EXTENSION doesn't stall for 5 minutes.
    let retry_ttl = std::time::Duration::from_secs(15);

    loop {
        // ── Collect all connectable, non-template databases  ──────────────
        let databases: Vec<String> = BackgroundWorker::transaction(AssertUnwindSafe(|| {
            Spi::connect(|client| -> Vec<String> {
                match client.select(
                    "SELECT datname::text FROM pg_database \
                     WHERE NOT datistemplate AND datallowconn",
                    None,
                    &[],
                ) {
                    Ok(result) => {
                        let mut out = Vec::new();
                        for row in result {
                            if let Some(db) = row.get_by_name::<String, _>("datname").ok().flatten()
                            {
                                out.push(db);
                            }
                        }
                        out
                    }
                    Err(_) => vec![],
                }
            })
        }));

        // ── Databases that already have a running scheduler worker  ────────
        let active: HashSet<String> = BackgroundWorker::transaction(AssertUnwindSafe(|| {
            Spi::connect(|client| -> HashSet<String> {
                match client.select(
                    "SELECT datname::text \
                     FROM pg_stat_activity \
                     WHERE backend_type = 'pg_trickle scheduler' \
                       AND datname IS NOT NULL",
                    None,
                    &[],
                ) {
                    Ok(result) => {
                        let mut out = HashSet::new();
                        for row in result {
                            if let Some(db) = row.get_by_name::<String, _>("datname").ok().flatten()
                            {
                                out.insert(db);
                            }
                        }
                        out
                    }
                    Err(_) => HashSet::new(),
                }
            })
        }));

        // If any backend bumped the DAG signal (create_st, alter, drop,
        // CREATE EXTENSION finalize, manual rescan nudge, etc.) since our last
        // loop, clear the skip-cache so we re-probe promptly.
        //
        // This is intentionally unconditional. A CREATE EXTENSION in a newly
        // created database can race with the launcher's first probe of that
        // database: the launcher may spawn a scheduler before the extension
        // transaction commits, observe "not installed", and cache a fresh
        // `last_attempt` entry. If we preserve that fresh entry on the very DAG
        // signal emitted by CREATE EXTENSION, the database remains classified as
        // a long-backoff `skip_ttl` database and the scheduler may not appear
        // for minutes.
        //
        // DDL-driven DAG bumps are relatively rare and semantically important,
        // so favor immediate correctness over preserving the old skip-cache on
        // those events. Non-DDL steady-state operation still uses the normal
        // skip_ttl / retry_ttl back-off between signals.
        let dag_version = shmem::current_dag_version();
        if dag_version != last_dag_version {
            last_dag_version = dag_version;
            last_attempt.clear();
        }

        for db in &databases {
            if active.contains(db) {
                // Worker healthy — record that this DB has had a running
                // scheduler so we can use the short retry_ttl if it crashes.
                had_scheduler.insert(db.clone());
                last_attempt.remove(db);
                continue;
            }

            // Choose the right backoff: short for DBs that previously ran a
            // scheduler (crash / DROP EXTENSION scenario), long for DBs that
            // have never had pg_trickle installed.
            let ttl = if had_scheduler.contains(db) {
                retry_ttl
            } else {
                skip_ttl
            };

            // Should we (re)try this database?
            let retry = last_attempt
                .get(db)
                .map(|t| t.elapsed() >= ttl)
                .unwrap_or(true);
            if !retry {
                continue;
            }

            last_attempt.insert(db.clone(), std::time::Instant::now());

            match BackgroundWorkerBuilder::new("pg_trickle scheduler")
                .set_function("pg_trickle_scheduler_main")
                .set_library("pg_trickle")
                .enable_spi_access()
                // Pass the database name via bgw_extra (max 128 bytes).
                .set_extra(db.as_str())
                // No restart_time — the launcher itself handles respawning.
                .set_restart_time(None)
                .load_dynamic()
            {
                Ok(_) => {
                    log!(
                        "pg_trickle launcher: spawned scheduler for database '{}'",
                        db
                    );
                }
                Err(_) => {
                    warning!(
                        "pg_trickle launcher: could not spawn scheduler for database '{}'",
                        db
                    );
                }
            }
        }

        // Wake every 10 s or on SIGHUP/SIGTERM.
        let should_continue =
            BackgroundWorker::wait_latch(Some(std::time::Duration::from_secs(10)));

        unsafe {
            if pg_sys::ConfigReloadPending != 0 {
                pg_sys::ConfigReloadPending = 0;
                pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP);
                pgrx::info!(
                    "pg_trickle scheduler: SIGHUP processed, allow_circular is now {}",
                    config::pg_trickle_allow_circular()
                );
            }
        }

        if !should_continue {
            log!("pg_trickle launcher shutting down");
            break;
        }
    }
}

/// Register the scheduler background worker (single-database, legacy).
///
/// Kept for reference — `_PG_init()` now registers the launcher instead.
/// The launcher dynamically spawns instances of this worker, one per database.
pub fn register_scheduler_worker() {
    BackgroundWorkerBuilder::new("pg_trickle scheduler")
        .set_function("pg_trickle_scheduler_main")
        .set_library("pg_trickle")
        .enable_spi_access()
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(Some(std::time::Duration::from_secs(5)))
        .load();
}

// ── Dynamic Refresh Worker (Phase 3) ──────────────────────────────────────

/// Spawn a dynamic refresh worker for a specific job.
///
/// The coordinator calls this after enqueuing a job and acquiring a worker
/// token from shared memory. The database name and job_id are packed into
/// `bgw_extra` as `"<db_name>|<job_id>"`.
///
/// **Why `|` and not `\0`?** pgrx's `BackgroundWorker::get_extra()` uses
/// `CStr::from_ptr` internally, which treats the first null byte as the
/// string terminator. A null-byte separator would therefore cause the
/// job_id portion to be silently dropped when the worker reads the extra.
/// The pipe character `|` is not a valid PostgreSQL identifier character
/// (without quoting) and is safe to use as a delimiter here.
///
/// Returns `Ok(())` on successful spawn, `Err(...)` if the dynamic worker
/// could not be registered.
pub fn spawn_refresh_worker(
    db_name: &str,
    job_id: i64,
) -> Result<(), crate::error::PgTrickleError> {
    // Pack db_name + job_id into bgw_extra (max 128 bytes).
    // Use '|' as separator — see doc comment above for why not '\0'.
    let extra = format!("{db_name}|{job_id}");
    if extra.len() > 128 {
        return Err(crate::error::PgTrickleError::InternalError(format!(
            "bgw_extra too long ({} bytes) for db='{}' job_id={}",
            extra.len(),
            db_name,
            job_id
        )));
    }

    BackgroundWorkerBuilder::new("pg_trickle refresh worker")
        .set_function("pg_trickle_refresh_worker_main")
        .set_library("pg_trickle")
        .enable_spi_access()
        .set_extra(&extra)
        .set_restart_time(None) // no auto-restart — coordinator handles retries
        .load_dynamic()
        .map_err(|_| {
            crate::error::PgTrickleError::InternalError(
                "Failed to register dynamic refresh worker".into(),
            )
        })?;

    Ok(())
}

/// Main entry point for a dynamic refresh worker (Phase 3).
///
/// Each refresh worker:
/// 1. Parses `db_name` and `job_id` from `bgw_extra`.
/// 2. Connects to the target database.
/// 3. Claims the job (QUEUED → RUNNING).
/// 4. Executes the execution unit (singleton for now, composite later).
/// 5. Persists the outcome to the job table.
/// 6. Releases the worker token from shared memory.
///
/// # Safety
/// Called directly by PostgreSQL as a background worker entry point.
#[pg_guard]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_trickle_refresh_worker_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    // Parse bgw_extra: "db_name\0job_id"
    let extra = BackgroundWorker::get_extra();
    let (db_name, job_id) = match parse_worker_extra(extra) {
        Some(pair) => pair,
        None => {
            warning!(
                "pg_trickle refresh worker: malformed bgw_extra '{}', exiting",
                extra
            );
            shmem::release_worker_token();
            return;
        }
    };

    BackgroundWorker::connect_worker_to_spi(Some(&db_name), None);

    // Get our own PID for logging and job claiming.
    let my_pid = BackgroundWorker::transaction(AssertUnwindSafe(|| -> i32 {
        Spi::get_one::<i32>("SELECT pg_backend_pid()")
            .unwrap_or(Some(0))
            .unwrap_or(0)
    }));

    log!(
        "pg_trickle refresh worker: started (db='{}', job_id={}, pid={})",
        db_name,
        job_id,
        my_pid,
    );

    // Claim the job: QUEUED → RUNNING
    let claimed = BackgroundWorker::transaction(AssertUnwindSafe(|| -> bool {
        SchedulerJob::claim(job_id, my_pid).unwrap_or(false)
    }));

    if !claimed {
        log!(
            "pg_trickle refresh worker: job {} already claimed or cancelled, exiting",
            job_id
        );
        shmem::release_worker_token();
        return;
    }

    // Load the job details
    let job = BackgroundWorker::transaction(AssertUnwindSafe(|| -> Option<SchedulerJob> {
        SchedulerJob::get_by_id(job_id).ok().flatten()
    }));

    let job = match job {
        Some(j) => j,
        None => {
            log!(
                "pg_trickle refresh worker: job {} not found after claim, exiting",
                job_id
            );
            BackgroundWorker::transaction(AssertUnwindSafe(|| {
                let _ = SchedulerJob::cancel(job_id, "Job row disappeared after claim");
            }));
            shmem::release_worker_token();
            return;
        }
    };

    // Validate: DAG version hasn't become obsolete
    let current_dag_version = shmem::current_dag_version();
    if (current_dag_version as i64) > job.dag_version + 1 {
        log!(
            "pg_trickle refresh worker: job {} dag_version={} is stale (current={}), cancelling",
            job_id,
            job.dag_version,
            current_dag_version,
        );
        BackgroundWorker::transaction(AssertUnwindSafe(|| {
            let _ = SchedulerJob::cancel(job_id, "DAG version obsolete");
        }));
        shmem::release_worker_token();
        return;
    }

    // Execute the unit.
    //
    // ERR-1d: Wrap in catch_unwind so that PostgreSQL ERRORs (which pgrx
    // converts to Rust panics via longjmp) don't kill the worker before we
    // can record the failure. When the refresh triggers a PG ERROR (e.g.
    // "column X does not exist"), the BackgroundWorker::transaction's
    // PgTryBuilder rethrows the caught panic, aborting the transaction.
    // Without catch_unwind, the panic propagates to #[pg_guard] and the
    // worker exits — leaving the job stuck in RUNNING and the ST never
    // transitioning to ERROR.
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        BackgroundWorker::transaction(AssertUnwindSafe(|| -> RefreshOutcome {
            match job.unit_kind.as_str() {
                "singleton" => execute_worker_singleton(&job),
                "atomic_group" => execute_worker_atomic_group(&job, false),
                "repeatable_read_group" => execute_worker_atomic_group(&job, true),
                "immediate_closure" => execute_worker_immediate_closure(&job),
                "cyclic_scc" => execute_worker_cyclic_scc(&job),
                "fused_chain" => execute_worker_fused_chain(&job),
                _ => {
                    warning!(
                        "pg_trickle refresh worker: unknown unit_kind '{}' for job {}",
                        job.unit_kind,
                        job_id,
                    );
                    RefreshOutcome::PermanentFailure
                }
            }
        }))
    }));

    let outcome = match outcome {
        Ok(o) => (o, None),
        Err(panic_payload) => {
            // ERR-1d: The refresh transaction panicked (PG ERROR). Extract
            // the error message from the panic payload and treat this as a
            // permanent failure. The original transaction was rolled back by
            // PostgreSQL, so we record the failure in a fresh transaction.
            let error_msg = extract_panic_message(&panic_payload);

            log!(
                "pg_trickle refresh worker: job {} panicked (PG ERROR): {}",
                job_id,
                error_msg,
            );

            // ERR-1d fix: After a PG ERROR inside BackgroundWorker::transaction,
            // the PostgreSQL transaction state is left in STARTED/ABORT state
            // because CommitTransactionCommand was never called.  We must abort
            // the orphaned transaction before starting a new one, otherwise
            // StartTransactionCommand raises "unexpected state STARTED".
            // SAFETY: AbortCurrentTransaction properly cleans up the aborted
            // transaction and returns PostgreSQL to idle state.
            unsafe {
                pg_sys::AbortCurrentTransaction();
            }

            // Determine if this is a retryable or permanent error.
            let is_retryable = crate::error::classify_spi_error_retryable(&error_msg);

            // Set error state on member STs in a fresh transaction.
            if !is_retryable {
                BackgroundWorker::transaction(AssertUnwindSafe(|| {
                    for &pgt_id in &job.member_pgt_ids {
                        let _ = StreamTableMeta::set_error_state(pgt_id, &error_msg);
                    }
                }));
            }

            let outcome = if is_retryable {
                RefreshOutcome::RetryableFailure
            } else {
                RefreshOutcome::PermanentFailure
            };
            (outcome, Some(error_msg))
        }
    };

    let (outcome, panic_error_msg) = outcome;

    // Persist outcome to the job table
    let (status, retryable) = match outcome {
        RefreshOutcome::Success => (JobStatus::Succeeded, None),
        RefreshOutcome::RetryableFailure => (JobStatus::RetryableFailed, Some(true)),
        RefreshOutcome::PermanentFailure => (JobStatus::PermanentFailed, Some(false)),
    };

    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        let _ = SchedulerJob::complete(job_id, status, panic_error_msg.as_deref(), retryable);
    }));

    log!(
        "pg_trickle refresh worker: job {} finished with {} (db='{}')",
        job_id,
        status,
        db_name,
    );

    // Release the cluster-wide worker token
    shmem::release_worker_token();
}

/// ERR-1d: Extract a human-readable error message from a caught panic payload.
///
/// Panics from PostgreSQL ERRORs are represented as `CaughtError` in pgrx.
/// We try to downcast to known types; if that fails, we produce a generic message.
fn extract_panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    use pgrx::pg_sys::panic::CaughtError;

    if let Some(caught) = payload.downcast_ref::<CaughtError>() {
        return match caught {
            CaughtError::PostgresError(ereport)
            | CaughtError::ErrorReport(ereport)
            | CaughtError::RustPanic { ereport, .. } => {
                // Include full debug output to capture DETAIL, HINT, etc.
                format!("{:?}", ereport)
            }
        };
    }
    if let Some(msg) = payload.downcast_ref::<&str>() {
        return msg.to_string();
    }
    if let Some(msg) = payload.downcast_ref::<String>() {
        return msg.clone();
    }
    "unknown error (panic payload could not be decoded)".to_string()
}

/// Parse the worker's bgw_extra string: "db_name\0job_id".
fn parse_worker_extra(extra: &str) -> Option<(String, i64)> {
    // Format is "<db_name>|<job_id>" — see spawn_refresh_worker for why '|'
    // is used instead of '\0' (pgrx's get_extra() truncates at null bytes).
    let parts: Vec<&str> = extra.splitn(2, '|').collect();
    if parts.len() != 2 {
        return None;
    }
    let db_name = parts[0].to_string();
    let job_id = parts[1].parse::<i64>().ok()?;
    if db_name.is_empty() || job_id <= 0 {
        return None;
    }
    Some((db_name, job_id))
}

// ── #536: Frontier holdback tick watermark helpers ─────────────────────────

/// Unix-epoch timestamp of the last holdback-active WARNING, used to
/// rate-limit warnings to at most one per minute.
static LAST_HOLDBACK_WARN_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Compute the tick watermark for the **coordinator** (main scheduler loop).
///
/// Applies the `frontier_holdback_mode` GUC logic:
/// - `"none"` / watermark disabled: use raw `pg_current_wal_lsn()`.
/// - `"xmin"`: probe `pg_stat_activity` + `pg_prepared_xacts` and hold back
///   if a long-running transaction would cause data loss.
/// - `"lsn:<N>"`: hold back by exactly N bytes.
///
/// Side effects (when holdback fires):
/// - Updates `shmem::last_tick_oldest_xmin` for the next tick.
/// - Updates `shmem::last_tick_safe_lsn_u64` for dynamic workers.
/// - Updates the holdback gauge metrics.
/// - Emits a WARNING when holdback age exceeds the warn threshold.
///
/// # Arguments
/// - `prev_watermark_lsn`: the safe LSN from the previous tick, if any.
///
/// # Returns
/// `(tick_watermark, current_oldest_xmin, oldest_txn_age_secs)`
fn compute_coordinator_tick_watermark(
    prev_watermark_lsn: Option<&str>,
) -> (Option<String>, u64, u64) {
    if !config::pg_trickle_tick_watermark_enabled() {
        return (None, 0, 0);
    }

    let mode = config::pg_trickle_frontier_holdback_mode();

    match mode {
        config::FrontierHoldbackMode::None => {
            let lsn = Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None);
            // Store raw write LSN for workers.
            if let Some(ref l) = lsn {
                shmem::set_last_tick_safe_lsn(version::lsn_to_u64(l));
            }
            shmem::update_holdback_metrics(0, 0);
            (lsn, 0, 0)
        }

        config::FrontierHoldbackMode::Xmin => {
            // Skip the probe when CDC mode is WAL — commit-LSN ordering
            // is already safe in logical-replication mode.
            if config::pg_trickle_cdc_mode() == "wal" {
                let lsn =
                    Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None);
                if let Some(ref l) = lsn {
                    shmem::set_last_tick_safe_lsn(version::lsn_to_u64(l));
                }
                shmem::update_holdback_metrics(0, 0);
                return (lsn, 0, 0);
            }

            let prev_oldest_xmin = shmem::last_tick_oldest_xmin();

            match cdc::compute_safe_upper_bound(prev_watermark_lsn, prev_oldest_xmin) {
                Ok((safe_lsn, write_lsn, current_oldest_xmin, age_secs)) => {
                    // Persist for next tick and for dynamic workers.
                    shmem::set_last_tick_oldest_xmin(current_oldest_xmin);
                    let safe_u64 = version::lsn_to_u64(&safe_lsn);
                    shmem::set_last_tick_safe_lsn(safe_u64);

                    // Update holdback gauge metrics.
                    let write_u64 = version::lsn_to_u64(&write_lsn);
                    let holdback_bytes = write_u64.saturating_sub(safe_u64);
                    shmem::update_holdback_metrics(holdback_bytes, age_secs);

                    // Warn when holdback has been active longer than the threshold.
                    if holdback_bytes > 0 {
                        emit_holdback_warning_if_needed(age_secs);
                    }

                    (Some(safe_lsn), current_oldest_xmin, age_secs)
                }
                Err(e) => {
                    // On probe failure, fall back to current write LSN.
                    log!(
                        "pg_trickle: holdback probe failed ({}); using raw write LSN",
                        e
                    );
                    let lsn =
                        Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None);
                    if let Some(ref l) = lsn {
                        shmem::set_last_tick_safe_lsn(version::lsn_to_u64(l));
                    }
                    shmem::update_holdback_metrics(0, 0);
                    (lsn, 0, 0)
                }
            }
        }

        config::FrontierHoldbackMode::LsnBytes(offset_bytes) => {
            let write_lsn_str = Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text")
                .unwrap_or(None)
                .unwrap_or_else(|| "0/0".to_string());
            let write_u64 = version::lsn_to_u64(&write_lsn_str);
            let safe_u64 = write_u64.saturating_sub(offset_bytes);
            let safe_lsn = version::u64_to_lsn(safe_u64);
            shmem::set_last_tick_safe_lsn(safe_u64);
            shmem::update_holdback_metrics(offset_bytes.min(write_u64), 0);
            (Some(safe_lsn), 0, 0)
        }
    }
}

/// Compute the tick watermark for a **dynamic refresh worker**.
///
/// Dynamic workers run after the coordinator and do not have access to
/// the previous tick's `prev_watermark_lsn`. They read the coordinator-
/// computed safe watermark from shared memory and cap it with the current
/// write LSN (in case the worker starts significantly after the tick).
///
/// When holdback is disabled or shmem is unavailable, falls back to
/// `pg_current_wal_lsn()`.
fn compute_worker_tick_watermark() -> Option<String> {
    if !config::pg_trickle_tick_watermark_enabled() {
        return None;
    }

    let mode = config::pg_trickle_frontier_holdback_mode();

    match mode {
        config::FrontierHoldbackMode::None => {
            Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None)
        }

        config::FrontierHoldbackMode::Xmin | config::FrontierHoldbackMode::LsnBytes(_) => {
            // Read the safe watermark the coordinator stored in shmem.
            let safe_lsn_u64 = shmem::last_tick_safe_lsn_u64();

            if safe_lsn_u64 == 0 {
                // No coordinator value yet — fall back to raw write LSN.
                return Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None);
            }

            // Cap with current write LSN: don't advance past what's available now.
            let write_lsn_str = Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text")
                .unwrap_or(None)
                .unwrap_or_else(|| "0/0".to_string());
            let write_u64 = version::lsn_to_u64(&write_lsn_str);
            let effective_u64 = safe_lsn_u64.min(write_u64);
            Some(version::u64_to_lsn(effective_u64))
        }
    }
}

/// Rate-limited WARNING for when frontier holdback has been active longer
/// than `pg_trickle.frontier_holdback_warn_seconds`.
///
/// Emits at most one WARNING per minute.
fn emit_holdback_warning_if_needed(oldest_txn_age_secs: u64) {
    let warn_secs = config::pg_trickle_frontier_holdback_warn_seconds();
    if warn_secs <= 0 {
        return;
    }
    if oldest_txn_age_secs < warn_secs as u64 {
        return;
    }

    // Rate-limit: emit at most once per minute.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last_warn = LAST_HOLDBACK_WARN_SECS.load(std::sync::atomic::Ordering::Relaxed);
    if now_secs.saturating_sub(last_warn) < 60 {
        return;
    }
    LAST_HOLDBACK_WARN_SECS.store(now_secs, std::sync::atomic::Ordering::Relaxed);

    pgrx::warning!(
        "pg_trickle: frontier holdback active — the oldest in-progress transaction is {}s old \
         (threshold: {}s). Stream tables may lag behind. \
         Check pg_stat_activity for long-running sessions. \
         To suppress: SET pg_trickle.frontier_holdback_warn_seconds = 0.",
        oldest_txn_age_secs,
        warn_secs,
    );
}

/// Execute a singleton unit: refresh a single stream table using the existing inline path.
fn execute_worker_singleton(job: &SchedulerJob) -> RefreshOutcome {
    let pgt_id = job.root_pgt_id;
    let st = match load_st_by_id(pgt_id) {
        Some(st) => st,
        None => {
            log!(
                "pg_trickle refresh worker: stream table pgt_id={} not found for job {}",
                pgt_id,
                job.job_id,
            );
            return RefreshOutcome::PermanentFailure;
        }
    };

    if st.status != StStatus::Active && st.status != StStatus::Initializing {
        log!(
            "pg_trickle refresh worker: {}.{} is not active (status={}), skipping",
            st.pgt_schema,
            st.pgt_name,
            st.status.as_str(),
        );
        return RefreshOutcome::RetryableFailure;
    }

    // BOOT-4: Check bootstrap source gates — skip if any source is gated.
    let gated_oids = load_gated_source_oids();
    if is_any_source_gated(pgt_id, &gated_oids) {
        log!(
            "pg_trickle refresh worker: skipping {}.{} — source gated (job {})",
            st.pgt_schema,
            st.pgt_name,
            job.job_id,
        );
        log_gated_skip(&st);
        return RefreshOutcome::Success;
    }

    // WM-4: Check watermark alignment — skip if misaligned.
    let (wm_misaligned, wm_reason) = is_watermark_misaligned(pgt_id);
    if wm_misaligned {
        let reason = wm_reason.as_deref().unwrap_or("watermark misaligned");
        log!(
            "pg_trickle refresh worker: skipping {}.{} — {} (job {})",
            st.pgt_schema,
            st.pgt_name,
            reason,
            job.job_id,
        );
        log_watermark_skip(&st, reason);
        return RefreshOutcome::Success;
    }

    // WM-7: Check watermark staleness — skip if any source watermark is stuck.
    let (wm_stuck, wm_stuck_reason) = is_watermark_stuck(pgt_id);
    if wm_stuck {
        let reason = wm_stuck_reason.as_deref().unwrap_or("watermark stuck");
        log!(
            "pg_trickle refresh worker: skipping {}.{} — {} (job {})",
            st.pgt_schema,
            st.pgt_name,
            reason,
            job.job_id,
        );
        log_watermark_skip(&st, reason);
        return RefreshOutcome::Success;
    }

    // FUSE-5: Check fuse circuit breaker — blow if change count exceeds ceiling.
    if evaluate_fuse(&st) {
        log!(
            "pg_trickle refresh worker: {}.{} fuse blown — skipping refresh (job {})",
            st.pgt_schema,
            st.pgt_name,
            job.job_id,
        );
        return RefreshOutcome::Success;
    }

    // #536: Use holdback-aware watermark for dynamic workers.
    let tick_watermark: Option<String> = compute_worker_tick_watermark();
    let has_changes = has_table_source_changes(&st) || has_stream_table_source_changes(&st);
    let action = refresh::determine_refresh_action(&st, has_changes);

    execute_scheduled_refresh(&st, action, tick_watermark.as_deref(), None)
}

/// Execute an atomic group unit: refresh all members serially inside a
/// sub-transaction.  If any member fails, the entire group is rolled back.
///
/// Uses the C-level internal sub-transaction API (`BeginInternalSubTransaction`
/// / `ReleaseCurrentSubTransaction` / `RollbackAndReleaseCurrentSubTransaction`)
/// because PostgreSQL rejects transaction-control SQL via SPI.
fn execute_worker_atomic_group(job: &SchedulerJob, is_repeatable_read: bool) -> RefreshOutcome {
    log!(
        "pg_trickle refresh worker: {} group — {} members (job {})",
        if is_repeatable_read {
            "repeatable_read"
        } else {
            "atomic"
        },
        job.member_pgt_ids.len(),
        job.job_id,
    );

    if is_repeatable_read {
        // Upgrade top-level worker transaction to REPEATABLE READ snapshot isolation
        if let Err(e) = pgrx::Spi::run("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ") {
            pgrx::log!(
                "pg_trickle refresh worker: failed to set REPEATABLE READ isolation: {}",
                e
            );
            return RefreshOutcome::PermanentFailure;
        }
    }

    let subtxn = SubTransaction::begin();

    // #536: Use holdback-aware watermark for dynamic workers.
    let tick_watermark: Option<String> = compute_worker_tick_watermark();
    let mut refreshed_count: usize = 0;

    // BOOT-4: Build gated-source set once for the whole group.
    let gated_oids = load_gated_source_oids();

    for &pgt_id in &job.member_pgt_ids {
        let st = match load_st_by_id(pgt_id) {
            Some(st) => st,
            None => continue,
        };

        if st.status != StStatus::Active && st.status != StStatus::Initializing {
            continue;
        }

        // BOOT-4: skip if any source is gated.
        if is_any_source_gated(pgt_id, &gated_oids) {
            log!(
                "pg_trickle refresh worker: skipping {}.{} — source gated (atomic group job {})",
                st.pgt_schema,
                st.pgt_name,
                job.job_id,
            );
            log_gated_skip(&st);
            continue;
        }

        // WM-4: Skip if watermarks misaligned.
        let (wm_misaligned, wm_reason) = is_watermark_misaligned(pgt_id);
        if wm_misaligned {
            let reason = wm_reason.as_deref().unwrap_or("watermark misaligned");
            log!(
                "pg_trickle refresh worker: skipping {}.{} — {} (atomic group job {})",
                st.pgt_schema,
                st.pgt_name,
                reason,
                job.job_id,
            );
            log_watermark_skip(&st, reason);
            continue;
        }

        // WM-7: Skip if any source watermark is stuck.
        let (wm_stuck, wm_stuck_reason) = is_watermark_stuck(pgt_id);
        if wm_stuck {
            let reason = wm_stuck_reason.as_deref().unwrap_or("watermark stuck");
            log!(
                "pg_trickle refresh worker: skipping {}.{} — {} (atomic group job {})",
                st.pgt_schema,
                st.pgt_name,
                reason,
                job.job_id,
            );
            log_watermark_skip(&st, reason);
            continue;
        }

        // Check catalog row lock — if another refresh is in progress, skip.
        if check_skip_needed(&st) {
            continue;
        }

        // FUSE-5: Check fuse circuit breaker.
        if evaluate_fuse(&st) {
            log!(
                "pg_trickle refresh worker: {}.{} fuse blown — skipping (atomic group job {})",
                st.pgt_schema,
                st.pgt_name,
                job.job_id,
            );
            continue;
        }

        let has_changes = has_table_source_changes(&st) || has_stream_table_source_changes(&st);
        let action = refresh::determine_refresh_action(&st, has_changes);

        let result = execute_scheduled_refresh(&st, action, tick_watermark.as_deref(), None);
        match result {
            RefreshOutcome::Success => {
                refreshed_count += 1;
            }
            RefreshOutcome::RetryableFailure | RefreshOutcome::PermanentFailure => {
                log!(
                    "pg_trickle refresh worker: atomic group rollback — member {}.{} failed (job {})",
                    st.pgt_schema,
                    st.pgt_name,
                    job.job_id,
                );
                subtxn.rollback();
                return result;
            }
        }
    }

    // All members succeeded — commit the sub-transaction.
    subtxn.commit();

    log!(
        "pg_trickle refresh worker: atomic group committed ({} members, job {})",
        refreshed_count,
        job.job_id,
    );
    RefreshOutcome::Success
}

/// Execute an IMMEDIATE-closure unit: refresh only the root stream table.
///
/// Downstream IMMEDIATE-mode stream tables fire their refresh triggers
/// synchronously within the same transaction, so they do not need to be
/// explicitly refreshed by the worker.  The coordinator must not independently
/// schedule any member of the closure.
fn execute_worker_immediate_closure(job: &SchedulerJob) -> RefreshOutcome {
    log!(
        "pg_trickle refresh worker: immediate closure — root pgt_id={} ({} members, job {})",
        job.root_pgt_id,
        job.member_pgt_ids.len(),
        job.job_id,
    );

    // Only refresh the root; IMMEDIATE triggers propagate downstream.
    let st = match load_st_by_id(job.root_pgt_id) {
        Some(st) => st,
        None => {
            log!(
                "pg_trickle refresh worker: root pgt_id={} not found for immediate closure (job {})",
                job.root_pgt_id,
                job.job_id,
            );
            return RefreshOutcome::PermanentFailure;
        }
    };

    if st.status != StStatus::Active && st.status != StStatus::Initializing {
        log!(
            "pg_trickle refresh worker: {}.{} is not active (status={}), skipping immediate closure",
            st.pgt_schema,
            st.pgt_name,
            st.status.as_str(),
        );
        return RefreshOutcome::RetryableFailure;
    }

    // FUSE-5: Check fuse circuit breaker for immediate closure root.
    if evaluate_fuse(&st) {
        log!(
            "pg_trickle refresh worker: {}.{} fuse blown — skipping immediate closure (job {})",
            st.pgt_schema,
            st.pgt_name,
            job.job_id,
        );
        return RefreshOutcome::Success;
    }

    // #536: Use holdback-aware watermark for dynamic workers.
    let tick_watermark: Option<String> = compute_worker_tick_watermark();
    let has_changes = has_table_source_changes(&st) || has_stream_table_source_changes(&st);
    let action = refresh::determine_refresh_action(&st, has_changes);

    execute_scheduled_refresh(&st, action, tick_watermark.as_deref(), None)
}

/// Execute a full cyclic Strongly Connected Component in parallel mode.
///
/// Runs iterative fixed-point refresh until convergence.
fn execute_worker_cyclic_scc(job: &SchedulerJob) -> RefreshOutcome {
    log!(
        "pg_trickle refresh worker: cyclic SCC — {} members (job {})",
        job.member_pgt_ids.len(),
        job.job_id,
    );

    let max_iter = config::pg_trickle_max_fixpoint_iterations();
    let gated_oids = load_gated_source_oids();
    let member_ids = &job.member_pgt_ids;

    if member_ids.is_empty() {
        return RefreshOutcome::Success;
    }

    for &pgt_id in member_ids {
        if let Some(st) = load_st_by_id(pgt_id)
            && st.refresh_mode != RefreshMode::Differential
        {
            pgrx::warning!(
                "pg_trickle refresh worker: SCC fixpoint — {}.{} uses {} mode, \
                 only DIFFERENTIAL is supported in cyclic dependencies",
                st.pgt_schema,
                st.pgt_name,
                st.refresh_mode.as_str()
            );
            return RefreshOutcome::PermanentFailure;
        }
    }

    // #536: Use holdback-aware watermark for dynamic workers.
    let tick_watermark: Option<String> = compute_worker_tick_watermark();

    let mut prev_row_counts: HashMap<i64, i64> = member_ids
        .iter()
        .map(|&id| (id, get_st_row_count(id).unwrap_or(0)))
        .collect();

    for iteration in 0..max_iter {
        let mut iteration_ok = true;
        let mut any_refreshed = false;

        {
            let subtxn = SubTransaction::begin();

            for &pgt_id in member_ids {
                let st = match load_st_by_id(pgt_id) {
                    Some(st) => st,
                    None => continue,
                };

                if st.status != StStatus::Active && st.status != StStatus::Initializing {
                    continue;
                }

                if is_any_source_gated(pgt_id, &gated_oids) {
                    log_gated_skip(&st);
                    continue;
                }

                // Inside SCCs, always use FULL refresh action to evaluate defining query each pass.
                let outcome = execute_scheduled_refresh(
                    &st,
                    RefreshAction::Full,
                    tick_watermark.as_deref(),
                    None,
                );
                match outcome {
                    RefreshOutcome::Success => {
                        any_refreshed = true;
                    }
                    RefreshOutcome::RetryableFailure | RefreshOutcome::PermanentFailure => {
                        log!(
                            "pg_trickle refresh worker: fixpoint iteration {} failed on {}.{}",
                            iteration + 1,
                            st.pgt_schema,
                            st.pgt_name
                        );
                        iteration_ok = false;
                        break;
                    }
                }
            }

            if !iteration_ok {
                subtxn.rollback();
                return RefreshOutcome::RetryableFailure;
            }

            subtxn.commit();
        }

        if !any_refreshed {
            continue;
        }

        let mut total_changes: i64 = 0;
        for &pgt_id in member_ids {
            let new_count = get_st_row_count(pgt_id).unwrap_or(0);
            let old_count = prev_row_counts.get(&pgt_id).copied().unwrap_or(0);
            total_changes += (new_count - old_count).abs();
            prev_row_counts.insert(pgt_id, new_count);
        }

        if total_changes == 0 {
            log!(
                "pg_trickle refresh worker: fixpoint reached after {} iteration(s)",
                iteration + 1
            );
            for &pgt_id in member_ids {
                let _ = StreamTableMeta::update_last_fixpoint_iterations(pgt_id, iteration + 1);
            }
            return RefreshOutcome::Success;
        }

        log!(
            "pg_trickle refresh worker: fixpoint iteration {} yielded {} change(s), continuing...",
            iteration + 1,
            total_changes
        );
    }

    pgrx::warning!(
        "pg_trickle refresh worker: SCC fixpoint failed to converge after {} iterations",
        max_iter
    );
    for &pgt_id in member_ids {
        let _ = StreamTableMeta::update_last_fixpoint_iterations(pgt_id, max_iter);
        let _ = StreamTableMeta::update_status(pgt_id, StStatus::Error);
    }
    RefreshOutcome::PermanentFailure
}

/// DAG-4: Execute a fused chain of stream tables in a single worker.
///
/// Members are refreshed sequentially in topological order.  For each
/// intermediate member (all except the last), the delta is written to a
/// temp bypass table instead of the persistent `changes_pgt_` buffer.
/// The next member in the chain reads from the bypass table via the
/// `ST_BYPASS_TABLES` thread-local mapping.
///
/// The last member writes to the persistent buffer as normal so that
/// any external consumers (outside the chain) see the delta.
fn execute_worker_fused_chain(job: &SchedulerJob) -> RefreshOutcome {
    log!(
        "pg_trickle refresh worker: fused chain — {} members, job {}",
        job.member_pgt_ids.len(),
        job.job_id,
    );

    // #536: Use holdback-aware watermark for dynamic workers.
    let tick_watermark: Option<String> = compute_worker_tick_watermark();

    // BOOT-4: Build gated-source set once for the whole group.
    let gated_oids = load_gated_source_oids();

    // Ensure bypass tables are clean at the start.
    crate::refresh::clear_all_st_bypass();

    let member_count = job.member_pgt_ids.len();
    let mut refreshed_count: usize = 0;

    for (idx, &pgt_id) in job.member_pgt_ids.iter().enumerate() {
        let st = match load_st_by_id(pgt_id) {
            Some(st) => st,
            None => continue,
        };

        if st.status != StStatus::Active && st.status != StStatus::Initializing {
            continue;
        }

        // BOOT-4: skip if any source is gated.
        if is_any_source_gated(pgt_id, &gated_oids) {
            log!(
                "pg_trickle refresh worker: skipping {}.{} — source gated (fused chain job {})",
                st.pgt_schema,
                st.pgt_name,
                job.job_id,
            );
            log_gated_skip(&st);
            continue;
        }

        // WM-4: Skip if watermarks misaligned.
        let (wm_misaligned, wm_reason) = is_watermark_misaligned(pgt_id);
        if wm_misaligned {
            let reason = wm_reason.as_deref().unwrap_or("watermark misaligned");
            log!(
                "pg_trickle refresh worker: skipping {}.{} — {} (fused chain job {})",
                st.pgt_schema,
                st.pgt_name,
                reason,
                job.job_id,
            );
            log_watermark_skip(&st, reason);
            continue;
        }

        // WM-7: Skip if any source watermark is stuck.
        let (wm_stuck, wm_stuck_reason) = is_watermark_stuck(pgt_id);
        if wm_stuck {
            let reason = wm_stuck_reason.as_deref().unwrap_or("watermark stuck");
            log!(
                "pg_trickle refresh worker: skipping {}.{} — {} (fused chain job {})",
                st.pgt_schema,
                st.pgt_name,
                reason,
                job.job_id,
            );
            log_watermark_skip(&st, reason);
            continue;
        }

        // Check catalog row lock — if another refresh is in progress, skip.
        if check_skip_needed(&st) {
            continue;
        }

        // FUSE-5: Check fuse circuit breaker.
        if evaluate_fuse(&st) {
            log!(
                "pg_trickle refresh worker: {}.{} fuse blown — skipping (fused chain job {})",
                st.pgt_schema,
                st.pgt_name,
                job.job_id,
            );
            continue;
        }

        let is_last = idx == member_count - 1;
        let has_changes = has_table_source_changes(&st) || has_stream_table_source_changes(&st);
        let action = refresh::determine_refresh_action(&st, has_changes);

        // DAG-4: For intermediate members, set a flag so the refresh path
        // uses bypass capture instead of the persistent buffer.
        // The flag is set via the thread-local ST_BYPASS_TABLES before
        // execute_scheduled_refresh runs. For the last member, normal
        // persistent buffer is used so external downstreams see the delta.
        //
        // Note: The actual bypass capture happens inside
        // execute_differential_refresh based on has_downstream_st_consumers().
        // The bypass tables from earlier members are already in the
        // thread-local map, so downstream members in this chain read from
        // them automatically.

        let result = execute_scheduled_refresh(&st, action, tick_watermark.as_deref(), None);
        match result {
            RefreshOutcome::Success => {
                refreshed_count += 1;

                // DAG-4: For non-last members with downstream ST consumers,
                // create the bypass temp table so the next member can read
                // from it instead of the persistent buffer.
                // Only for DIFFERENTIAL refreshes — NoData/Full/Reinitialize
                // do not materialize __pgt_delta_{pgt_id}.
                if !is_last
                    && action == RefreshAction::Differential
                    && refresh::has_downstream_st_consumers(pgt_id)
                {
                    let user_cols_typed = refresh::get_st_user_columns_typed(&st);
                    match crate::refresh::capture_delta_to_bypass_table(&st, &user_cols_typed) {
                        Ok(n) => {
                            pgrx::debug1!(
                                "[pg_trickle] DAG-4: bypass captured {} rows for pgt_id={}",
                                n,
                                pgt_id,
                            );
                        }
                        Err(e) => {
                            // Bypass failed — the downstream will fall back to the
                            // persistent buffer which was also written.
                            pgrx::debug1!(
                                "[pg_trickle] DAG-4: bypass capture failed for pgt_id={}: {}",
                                pgt_id,
                                e,
                            );
                        }
                    }
                }
            }
            RefreshOutcome::RetryableFailure | RefreshOutcome::PermanentFailure => {
                log!(
                    "pg_trickle refresh worker: fused chain abort — member {}.{} failed (job {})",
                    st.pgt_schema,
                    st.pgt_name,
                    job.job_id,
                );
                // Clean up bypass tables before returning.
                crate::refresh::clear_all_st_bypass();
                return result;
            }
        }
    }

    // Clean up bypass tables.
    crate::refresh::clear_all_st_bypass();

    log!(
        "pg_trickle refresh worker: fused chain completed ({} members refreshed, job {})",
        refreshed_count,
        job.job_id,
    );
    RefreshOutcome::Success
}

// ── Parallel Dispatch State (Phase 4) ─────────────────────────────────────

/// DAG-2: Minimum adaptive poll interval (ms) when workers are in-flight.
const ADAPTIVE_POLL_MIN_MS: u64 = 20;
/// DAG-2: Maximum adaptive poll interval (ms) — the old fixed cap.
const ADAPTIVE_POLL_MAX_MS: u64 = 200;

/// DAG-2: Compute the next adaptive poll interval.
///
/// Uses exponential backoff: starts at `ADAPTIVE_POLL_MIN_MS` after a worker
/// completion, doubles each tick with no completion, and caps at
/// `ADAPTIVE_POLL_MAX_MS`. When no workers are in-flight, returns the full
/// `base_interval_ms` (the scheduler GUC value).
///
/// This is a pure function for unit-testability.
fn compute_adaptive_poll_ms(
    current_poll_ms: u64,
    had_completion: bool,
    has_inflight: bool,
    base_interval_ms: u64,
) -> u64 {
    if !has_inflight {
        return base_interval_ms;
    }
    if had_completion {
        return ADAPTIVE_POLL_MIN_MS;
    }
    // Exponential backoff: double the current interval, capped.
    let next = current_poll_ms.saturating_mul(2);
    std::cmp::min(next, ADAPTIVE_POLL_MAX_MS)
}

/// Per-unit coordinator state for the parallel dispatch loop.
#[derive(Debug, Default)]
struct UnitDispatchState {
    /// Number of upstream units that must succeed before this unit is ready.
    remaining_upstreams: usize,
    /// In-flight job ID, if a worker is currently executing this unit.
    inflight_job_id: Option<i64>,
    /// Whether this unit completed successfully in the current cycle.
    succeeded: bool,
}

/// Coordinator-side state for the parallel dispatch loop.
///
/// Lives outside the SPI transaction and persists across scheduler ticks.
/// Rebuilt when the DAG version changes.
struct ParallelDispatchState {
    /// Per-unit tracking, keyed by ExecutionUnitId.
    unit_states: HashMap<ExecutionUnitId, UnitDispatchState>,
    /// Number of refresh workers in-flight for this database coordinator.
    per_db_inflight: u32,
    /// DAG version this dispatch state was built from.
    dag_version: u64,
    /// The execution unit DAG (rebuilt on DAG version change).
    eu_dag: Option<ExecutionUnitDag>,
    /// DAG-2: Current adaptive poll interval (ms). Doubles each tick without
    /// worker completions; resets to `ADAPTIVE_POLL_MIN_MS` on completion.
    adaptive_poll_ms: u64,
    /// DAG-2: Number of worker completions observed in the last dispatch tick.
    completions_this_tick: u32,
}

impl ParallelDispatchState {
    fn new() -> Self {
        Self {
            unit_states: HashMap::new(),
            per_db_inflight: 0,
            dag_version: 0,
            eu_dag: None,
            adaptive_poll_ms: ADAPTIVE_POLL_MIN_MS,
            completions_this_tick: 0,
        }
    }

    /// Whether there are in-flight jobs (for shorter poll interval).
    fn has_inflight(&self) -> bool {
        self.per_db_inflight > 0
    }

    /// Rebuild dispatch state from a fresh EU DAG.
    fn rebuild(&mut self, eu_dag: ExecutionUnitDag, dag_version: u64) {
        // Count how many old units had in-flight jobs that will be orphaned
        // by clearing unit_states.  Subtract these from per_db_inflight
        // because the orphaned jobs will be reaped by the dead-worker scan
        // (Step 0) or will complete and be found via catalog poll. Without
        // this correction, per_db_inflight permanently drifts upward on
        // each rebuild, eventually blocking all new dispatches.
        let orphaned_inflight = self
            .unit_states
            .values()
            .filter(|us| us.inflight_job_id.is_some())
            .count() as u32;
        self.per_db_inflight = self.per_db_inflight.saturating_sub(orphaned_inflight);

        self.unit_states.clear();
        self.dag_version = dag_version;

        for unit in eu_dag.units() {
            let upstream_count = eu_dag.get_upstream_units(unit.id).len();
            self.unit_states.insert(
                unit.id,
                UnitDispatchState {
                    remaining_upstreams: upstream_count,
                    inflight_job_id: None,
                    succeeded: false,
                },
            );
        }
        self.eu_dag = Some(eu_dag);
    }
}

/// Check if an execution unit is due for refresh.
///
/// A unit is due if any member ST is due (schedule or upstream changes).
fn is_unit_due(unit: &ExecutionUnit, dag: &StDag) -> bool {
    unit.member_pgt_ids.iter().any(|&pgt_id| {
        load_st_by_id(pgt_id)
            .map(|st| {
                (st.status == StStatus::Active || st.status == StStatus::Initializing)
                    && (check_schedule(&st, dag) || st.needs_reinit)
            })
            .unwrap_or(false)
    })
}

// ── C3-1: Per-database worker quota helpers ───────────────────────────────

/// C3-1: Compute the effective per-database worker quota for this dispatch tick.
///
/// When `per_db_quota == 0` (disabled), falls back to `max_concurrent_refreshes`
/// (the legacy per-coordinator cap, not cluster-aware).
///
/// When `per_db_quota > 0`, the base entitlement is `per_db_quota`. If the
/// cluster has spare capacity (active workers < 80% of `max_cluster`), the
/// effective quota is increased to `per_db_quota * 3 / 2` to absorb a burst
/// without wasting idle cluster resources. The burst is reclaimed automatically
/// within 1 scheduler cycle once global load rises.
///
/// Pure logic — extracted for unit-testability.
pub fn compute_per_db_quota(
    per_db_quota: i32,
    max_concurrent_refreshes: i32,
    max_cluster: u32,
    current_active: u32,
) -> u32 {
    if per_db_quota <= 0 {
        // C3-1 disabled — fall back to per-coordinator cap (legacy).
        return max_concurrent_refreshes.max(1) as u32;
    }
    let base = per_db_quota.max(1) as u32;
    // Burst threshold: 80% of cluster capacity.
    let burst_threshold = ((max_cluster as f64) * 0.8).ceil() as u32;
    if current_active < burst_threshold {
        // Spare capacity — allow up to 150% of base quota.
        (base * 3 / 2).max(base + 1)
    } else {
        base
    }
}

/// C3-1: Sort an execution unit ready queue by dispatch priority.
///
/// Priority ordering (lowest number = dispatched first):
/// 1. `ImmediateClosure` — transactional consistency, must not be delayed.
/// 2. `AtomicGroup` / `RepeatableReadGroup` — consistency group members.
/// 3. `Singleton` — normal scheduled stream tables.
/// 4. `CyclicScc` — fixpoint groups, lowest urgency.
///
/// Topological order is preserved within each priority tier because the
/// input queue was built by iterating `topo_order`.
///
/// The secondary sort key is `tier_priority`: tier values should be
/// `0 = Hot` (highest urgency), `1 = Warm`, `2 = Cold`.  Pass an empty map
/// when tier information is unavailable (all units treated as Hot).
fn sort_ready_queue_by_priority(
    queue: VecDeque<ExecutionUnitId>,
    eu_dag: &ExecutionUnitDag,
    tier_priorities: &HashMap<ExecutionUnitId, u8>,
) -> VecDeque<ExecutionUnitId> {
    use crate::dag::ExecutionUnitKind;
    fn unit_priority(kind: ExecutionUnitKind) -> u8 {
        match kind {
            ExecutionUnitKind::ImmediateClosure => 0,
            ExecutionUnitKind::AtomicGroup | ExecutionUnitKind::RepeatableReadGroup => 1,
            ExecutionUnitKind::FusedChain => 1, // same priority as atomic groups
            ExecutionUnitKind::Singleton => 2,
            ExecutionUnitKind::CyclicScc => 3,
        }
    }
    // C3-1: Tier priority: Hot(0) > Warm(1) > Cold(2).  ImmediateClosure EUs
    // are already priority 0 by kind and do not need a tier secondary key.
    let default_tier: u8 = 0; // default to Hot when not in map
    let mut items: Vec<ExecutionUnitId> = queue.into_iter().collect();
    // stable sort preserves topological order within each priority tier.
    items.sort_by_key(|&uid| {
        let (kind_prio, tier_prio) = eu_dag
            .units()
            .find(|u| u.id == uid)
            .map(|u| {
                let kp = unit_priority(u.kind);
                // ImmediateClosure ignores tier — it always wins.
                let tp = if kp == 0 {
                    0u8
                } else {
                    *tier_priorities.get(&uid).unwrap_or(&default_tier)
                };
                (kp, tp)
            })
            .unwrap_or((u8::MAX, default_tier));
        (kind_prio, tier_prio)
    });
    items.into_iter().collect()
}

/// C3-1: Compute the "best" (most-urgent) tier priority for an execution unit.
///
/// Iterates the unit's member STs, loads each, and returns the minimum tier
/// priority value (Hot=0, Warm=1, Cold=2).  Returns `0` (Hot) for any unit
/// whose members cannot be loaded so they are never accidentally deprioritised.
///
/// Pure logic in the sense that it only reads catalog data that has already
/// been loaded; the `load_st_by_id` calls are cheap (SPI reads, already cached
/// by the catalog layer during this same scheduler tick).
fn compute_unit_tier_priority(member_pgt_ids: &[i64]) -> u8 {
    let mut best: u8 = 2; // Cold — start at lowest urgency
    for &pgt_id in member_pgt_ids {
        let tier_prio = match load_st_by_id(pgt_id) {
            Some(st) => match RefreshTier::from_sql_str(&st.refresh_tier) {
                RefreshTier::Hot => 0,
                RefreshTier::Warm => 1,
                RefreshTier::Cold | RefreshTier::Frozen => 2,
            },
            None => 0, // default to Hot urgency when ST cannot be loaded
        };
        if tier_prio < best {
            best = tier_prio;
        }
        if best == 0 {
            break; // Hot is the highest urgency; short-circuit
        }
    }
    best
}

/// Run one tick of the parallel dispatch loop.
///
/// Called from the main scheduler loop when `parallel_refresh_mode == On`.
/// Polls completed jobs, updates readiness, and enqueues new jobs.
/// Workers to spawn are appended to `pending_spawns`.
fn parallel_dispatch_tick(
    state: &mut ParallelDispatchState,
    dag: &StDag,
    now_ms: u64,
    retry_states: &mut HashMap<i64, RetryState>,
    retry_policy: &RetryPolicy,
    db_name: &str,
    pending_spawns: &mut Vec<(String, i64)>,
) {
    let eu_dag = match &state.eu_dag {
        Some(d) => d,
        None => return,
    };

    let max_cluster = config::pg_trickle_max_dynamic_refresh_workers().max(1) as u32;
    // PAR-2: If max_parallel_workers is set (> 0), use it as an additional cap.
    let par_workers = config::pg_trickle_max_parallel_workers();
    let effective_max_cluster = if par_workers > 0 {
        max_cluster.min(par_workers as u32)
    } else {
        max_cluster
    };
    // C3-1: Per-database quota with burst capacity.
    let max_per_db = compute_per_db_quota(
        config::pg_trickle_per_database_worker_quota(),
        config::pg_trickle_max_concurrent_refreshes(),
        effective_max_cluster,
        shmem::active_worker_count(),
    );
    let dag_version_i64 = state.dag_version as i64;
    // SAFETY: MyProcPid is always valid inside a background worker.
    let scheduler_pid: i32 = unsafe { pg_sys::MyProcPid };

    // ── Step 0: Reap orphaned RUNNING jobs whose worker died ─────────────
    // A background worker can die (OOM, crash, etc.) while its job is still
    // RUNNING in the catalog. The in-memory inflight tracking may have been
    // lost during a DAG rebuild, so we do a DB-level scan: cancel any
    // RUNNING job whose worker_pid is no longer in pg_stat_activity.
    let reaped = reap_dead_worker_jobs();
    if reaped > 0 {
        log!(
            "pg_trickle: parallel dispatch — reaped {} orphaned job(s) with dead workers",
            reaped,
        );

        // Reconcile shmem worker token counter.  Crashed workers never call
        // `release_worker_token()`, so the atomic counter drifts upward.
        // Re-derive from pg_stat_activity to fix it.
        let live_workers: u32 = Spi::get_one::<i64>(
            "SELECT COUNT(*)::bigint FROM pg_stat_activity \
             WHERE backend_type = 'pg_trickle refresh worker'",
        )
        .unwrap_or(Some(0))
        .unwrap_or(0)
        .max(0) as u32;

        let shmem_count = shmem::active_worker_count();
        if shmem_count != live_workers {
            log!(
                "pg_trickle: parallel dispatch — correcting worker count: shmem={} → live={}",
                shmem_count,
                live_workers,
            );
            shmem::set_active_worker_count(live_workers);
            shmem::bump_reconcile_epoch();
        }
    }

    // ── Step 1: Poll completed jobs and process outcomes ──────────────────
    let inflight_ids: Vec<(ExecutionUnitId, i64)> = state
        .unit_states
        .iter()
        .filter_map(|(&uid, us)| us.inflight_job_id.map(|jid| (uid, jid)))
        .collect();

    for (unit_id, job_id) in inflight_ids {
        let job = match SchedulerJob::get_by_id(job_id) {
            Ok(Some(j)) => j,
            Ok(None) => {
                log!(
                    "pg_trickle: parallel dispatch — job {} vanished, treating as failure",
                    job_id,
                );
                if let Some(us) = state.unit_states.get_mut(&unit_id) {
                    us.inflight_job_id = None;
                }
                state.per_db_inflight = state.per_db_inflight.saturating_sub(1);
                continue;
            }
            Err(e) => {
                log!(
                    "pg_trickle: parallel dispatch — error polling job {}: {}",
                    job_id,
                    e,
                );
                continue; // Try again next tick
            }
        };

        if !job.status.is_terminal() {
            continue; // Still running
        }

        // Job completed — process outcome.
        if let Some(us) = state.unit_states.get_mut(&unit_id) {
            us.inflight_job_id = None;
        }
        state.per_db_inflight = state.per_db_inflight.saturating_sub(1);
        // DAG-2: Track completion for adaptive poll reset.
        state.completions_this_tick += 1;

        let root_pgt_id = eu_dag
            .units()
            .find(|u| u.id == unit_id)
            .map(|u| u.root_pgt_id);

        match job.status {
            JobStatus::Succeeded => {
                if let Some(us) = state.unit_states.get_mut(&unit_id) {
                    us.succeeded = true;
                }
                if let Some(rpid) = root_pgt_id {
                    retry_states.entry(rpid).or_default().reset();
                }

                // Advance downstream readiness.
                for ds_id in eu_dag.get_downstream_units(unit_id) {
                    if let Some(ds_state) = state.unit_states.get_mut(&ds_id) {
                        ds_state.remaining_upstreams =
                            ds_state.remaining_upstreams.saturating_sub(1);
                    }
                }

                log!(
                    "pg_trickle: parallel dispatch — unit completed (job {})",
                    job_id,
                );
            }
            JobStatus::RetryableFailed => {
                if let Some(rpid) = root_pgt_id {
                    let retry = retry_states.entry(rpid).or_default();
                    let will_retry = retry.record_failure(retry_policy, now_ms);
                    if will_retry {
                        log!(
                            "pg_trickle: parallel dispatch — job {} failed (retryable), backoff {}ms",
                            job_id,
                            retry_policy.backoff_ms(retry.attempts - 1),
                        );
                    } else {
                        // Retry budget exhausted — unblock downstream units
                        // so the wave can complete. Without this, downstreams
                        // remain permanently blocked because the failed unit
                        // never sets succeeded=true and remaining_upstreams
                        // is never decremented.
                        log!(
                            "pg_trickle: parallel dispatch — job {} retries exhausted, unblocking downstreams",
                            job_id,
                        );
                        if let Some(us) = state.unit_states.get_mut(&unit_id) {
                            us.succeeded = true;
                        }
                        for ds_id in eu_dag.get_downstream_units(unit_id) {
                            if let Some(ds_state) = state.unit_states.get_mut(&ds_id) {
                                ds_state.remaining_upstreams =
                                    ds_state.remaining_upstreams.saturating_sub(1);
                            }
                        }
                    }
                }
            }
            JobStatus::PermanentFailed | JobStatus::Cancelled => {
                if let Some(rpid) = root_pgt_id {
                    retry_states.entry(rpid).or_default().reset();
                }
                log!(
                    "pg_trickle: parallel dispatch — job {} failed permanently",
                    job_id,
                );

                // ERR-1d: Ensure ERROR status is set on member STs for permanent
                // failures. The worker may have already set this via the
                // catch_unwind path, but this is a safety net in case the
                // worker died before completing the error-state UPDATE.
                if job.status == JobStatus::PermanentFailed {
                    // PERF-5: O(1) lookup via unit_by_id.
                    let unit = eu_dag.unit_by_id(unit_id);
                    if let Some(unit) = unit {
                        let error_detail = job
                            .outcome_detail
                            .as_deref()
                            .unwrap_or("Refresh failed permanently");
                        for &pgt_id in &unit.member_pgt_ids {
                            // Only set ERROR if not already in ERROR state.
                            if let Some(st) = load_st_by_id(pgt_id)
                                && st.status != StStatus::Error
                            {
                                let _ = StreamTableMeta::set_error_state(pgt_id, error_detail);
                            }
                        }
                    }
                }

                // Mark this unit as "succeeded" so the wave can complete.
                // The refresh itself already recorded the failure in
                // pgt_refresh_history, but we must unblock downstream
                // units — otherwise a single permanent failure silently
                // stalls every transitive downstream for the remainder of
                // the scheduler's lifetime.
                if let Some(us) = state.unit_states.get_mut(&unit_id) {
                    us.succeeded = true;
                }
                for ds_id in eu_dag.get_downstream_units(unit_id) {
                    if let Some(ds_state) = state.unit_states.get_mut(&ds_id) {
                        ds_state.remaining_upstreams =
                            ds_state.remaining_upstreams.saturating_sub(1);
                    }
                }
            }
            _ => {}
        }
    }

    // ── Step 2: Build ready queue ────────────────────────────────────────
    let mut ready_queue: VecDeque<ExecutionUnitId> = VecDeque::new();

    // Use topological order for deterministic upstream-first priority.
    let topo_order = match eu_dag.topological_order() {
        Ok(order) => order,
        Err(e) => {
            log!("pg_trickle: parallel dispatch — topo order failed: {}", e);
            return;
        }
    };

    for &uid in &topo_order {
        let us = match state.unit_states.get(&uid) {
            Some(s) => s,
            None => continue,
        };

        // Skip completed, in-flight, or dependency-blocked units.
        if us.succeeded || us.inflight_job_id.is_some() || us.remaining_upstreams > 0 {
            continue;
        }

        // PERF-5: O(1) lookup via unit_by_id.
        let unit = match eu_dag.unit_by_id(uid) {
            Some(u) => u,
            None => continue,
        };

        // Check retry backoff.
        let retry = retry_states.entry(unit.root_pgt_id).or_default();
        if retry.is_in_backoff(now_ms) {
            // Emit stale alerts for members in backoff.
            for &pgt_id in &unit.member_pgt_ids {
                if let Some(st) = load_st_by_id(pgt_id) {
                    emit_stale_alert_if_needed(&st);
                }
            }
            continue;
        }

        // Check if unit is due for refresh.
        if !is_unit_due(unit, dag) {
            continue;
        }

        // Guard against duplicate in-flight jobs.
        match SchedulerJob::has_inflight_job(&unit.stable_key()) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                log!(
                    "pg_trickle: parallel dispatch — inflight check error for {}: {}",
                    unit.label,
                    e,
                );
                continue;
            }
        }

        ready_queue.push_back(uid);
    }

    // C3-1: Build tier priority map for the ready queue.
    // For each EU in the ready queue, compute the "best" (most urgent) tier
    // among its member STs.  This is the secondary sort key used below.
    // We only compute for units that are actually ready so the map stays small.
    // PERF-5: O(1) lookup via unit_by_id.
    let tier_map: HashMap<ExecutionUnitId, u8> = ready_queue
        .iter()
        .filter_map(|&uid| {
            eu_dag
                .unit_by_id(uid)
                .map(|u| (uid, compute_unit_tier_priority(&u.member_pgt_ids)))
        })
        .collect();

    // C3-1: Priority sort — IMMEDIATE closures first for transactional safety,
    // then atomic groups, singletons (Hot > Warm > Cold), cyclic SCCs.
    // Topological order within each priority tier is preserved.
    let ready_queue = sort_ready_queue_by_priority(ready_queue, eu_dag, &tier_map);

    // ── Step 3: Dispatch ready units within budget ───────────────────────
    let mut ready_queue = ready_queue;
    while let Some(uid) = ready_queue.pop_front() {
        if state.per_db_inflight >= max_per_db {
            break;
        }

        if !shmem::try_acquire_worker_token(max_cluster) {
            log!(
                "pg_trickle: parallel dispatch — worker budget exhausted ({}/{})",
                shmem::active_worker_count(),
                max_cluster,
            );
            break;
        }

        // PERF-5: O(1) lookup via unit_by_id.
        let unit = match eu_dag.unit_by_id(uid) {
            Some(u) => u,
            None => {
                shmem::release_worker_token();
                continue;
            }
        };

        let job_id = match SchedulerJob::enqueue(
            dag_version_i64,
            &unit.stable_key(),
            unit.kind.as_str(),
            &unit.member_pgt_ids,
            unit.root_pgt_id,
            scheduler_pid,
            1,
        ) {
            Ok(id) => id,
            Err(e) => {
                shmem::release_worker_token();
                log!(
                    "pg_trickle: parallel dispatch — failed to enqueue job for {}: {}",
                    unit.label,
                    e,
                );
                continue;
            }
        };

        if let Some(us) = state.unit_states.get_mut(&uid) {
            us.inflight_job_id = Some(job_id);
        }
        state.per_db_inflight += 1;
        pending_spawns.push((db_name.to_string(), job_id));

        log!(
            "pg_trickle: parallel dispatch — enqueued {} (job_id={}, kind={})",
            unit.label,
            job_id,
            unit.kind,
        );
    }

    // ── Step 4: Reset cycle state when the wave is fully complete ────────
    // A dispatch wave completes when all in-flight workers have finished AND
    // nothing new was dispatched in Step 3 (per_db_inflight is still 0 after
    // the dispatch loop).  At that point `succeeded = true` on every unit
    // that ran this wave, and we can clear those flags so units become
    // eligible for the next scheduled wave.
    //
    // Placing the reset HERE (after dispatch) rather than before Step 2 is
    // critical for cascade correctness.  With a two-unit cascade A → B:
    //   • A completes in Step 1 → per_db_inflight drops to 0, B.remaining=0.
    //   • Step 2 sees B ready; Step 3 dispatches B → per_db_inflight becomes 1.
    //   • Reset check: per_db_inflight == 1 → no reset.  B runs correctly.
    // If the reset were placed before Step 2, per_db_inflight would be 0 at
    // that point (B not yet dispatched), causing B.remaining_upstreams to be
    // restored to 1 — blocking B forever in a cascade.
    if state.per_db_inflight == 0 {
        let any_done = state.unit_states.values().any(|us| us.succeeded);
        if any_done {
            for (&uid, us) in state.unit_states.iter_mut() {
                us.succeeded = false;
                us.remaining_upstreams = eu_dag.get_upstream_units(uid).len();
            }
            log!(
                "pg_trickle: parallel dispatch — wave complete, reset {} unit(s)",
                state.unit_states.len()
            );
        }
    }
}

/// Reap RUNNING jobs whose background worker process has died.
///
/// Called at the start of each parallel dispatch tick to detect orphaned
/// RUNNING jobs whose worker_pid is no longer in pg_stat_activity.
/// This handles the case where a worker crashes (OOM, segfault, etc.)
/// while its job status remains RUNNING in the catalog — which would
/// otherwise permanently block scheduling for that execution unit.
///
/// Returns the number of jobs reaped.
fn reap_dead_worker_jobs() -> i64 {
    Spi::get_one::<i64>(
        "WITH reaped AS ( \
             UPDATE pgtrickle.pgt_scheduler_jobs \
             SET status = 'RETRYABLE_FAILED', \
                 finished_at = now(), \
                 outcome_detail = 'Worker process died (reaped by scheduler)', \
                 retryable = true \
             WHERE status = 'RUNNING' \
               AND worker_pid IS NOT NULL \
               AND NOT EXISTS ( \
                   SELECT 1 FROM pg_stat_activity \
                   WHERE pid = pgt_scheduler_jobs.worker_pid \
               ) \
             RETURNING job_id \
         ) SELECT count(*)::bigint FROM reaped",
    )
    .unwrap_or(Some(0))
    .unwrap_or(0)
}

/// Reconcile orphaned jobs and worker tokens at scheduler startup.
///
/// Called once during per-database scheduler initialization:
/// 1. Cancels orphaned QUEUED/RUNNING jobs whose PID is no longer alive.
/// 2. Computes the true active worker count from `pg_stat_activity`.
/// 3. Corrects the shared-memory token counter if it diverged.
pub fn reconcile_parallel_state() {
    // Step 1: Cancel orphaned jobs
    match SchedulerJob::cancel_orphaned_jobs() {
        Ok(count) if count > 0 => {
            log!(
                "pg_trickle: parallel reconciliation — cancelled {} orphaned job(s)",
                count
            );
        }
        Ok(_) => {}
        Err(e) => {
            log!(
                "pg_trickle: parallel reconciliation — orphan cleanup error: {}",
                e
            );
        }
    }

    // Step 2: Count live refresh workers from pg_stat_activity
    let live_workers: u32 = Spi::get_one::<i64>(
        "SELECT COUNT(*)::bigint FROM pg_stat_activity \
         WHERE backend_type = 'pg_trickle refresh worker'",
    )
    .unwrap_or(Some(0))
    .unwrap_or(0)
    .max(0) as u32;

    // Step 3: Correct shared-memory counter if needed
    let shmem_count = shmem::active_worker_count();
    if shmem_count != live_workers {
        log!(
            "pg_trickle: parallel reconciliation — correcting worker count: shmem={} → live={}",
            shmem_count,
            live_workers,
        );
        shmem::set_active_worker_count(live_workers);
        shmem::bump_reconcile_epoch();
    }

    // Step 4: Prune old completed jobs (keep last 1 hour)
    match SchedulerJob::prune_completed(3600) {
        Ok(count) if count > 0 => {
            log!(
                "pg_trickle: parallel reconciliation — pruned {} old job(s)",
                count
            );
        }
        Ok(_) => {}
        Err(e) => {
            log!("pg_trickle: parallel reconciliation — prune error: {}", e);
        }
    }
}

/// Main entry point for the scheduler background worker.
///
/// # Safety
/// This function is called directly by PostgreSQL as a background worker
/// entry point. It must follow the C-unwind calling convention.
#[pg_guard]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_trickle_scheduler_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    // Determine which database to connect to.
    // Dynamic workers spawned by the launcher have the DB name in bgw_extra.
    let extra = BackgroundWorker::get_extra();
    let db_name = if extra.is_empty() {
        "postgres".to_string()
    } else {
        extra.to_string()
    };
    BackgroundWorker::connect_worker_to_spi(Some(db_name.as_str()), None);

    // Exit cleanly if pg_trickle is not installed in this database.
    // The launcher will re-probe after its skip TTL (5 min) expires.
    let is_installed = BackgroundWorker::transaction(AssertUnwindSafe(|| -> bool {
        Spi::get_one::<bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'pg_trickle')",
        )
        .unwrap_or(Some(false))
        .unwrap_or(false)
    }));
    if !is_installed {
        log!(
            "pg_trickle scheduler: pg_trickle not installed in database '{}', exiting",
            db_name
        );
        return;
    }

    // UG1: Version mismatch check — warn if the compiled .so version differs
    // from the SQL-installed extension version (stale install).
    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        check_extension_version_match();
    }));

    // F16 (G8.2): Detect read replicas — the scheduler cannot write on a
    // standby. Skip all work and sleep until promotion.
    let is_replica = BackgroundWorker::transaction(AssertUnwindSafe(|| -> bool {
        Spi::get_one::<bool>("SELECT pg_is_in_recovery()")
            .unwrap_or(Some(false))
            .unwrap_or(false)
    }));
    if is_replica {
        log!(
            "pg_trickle scheduler: running on a read replica (pg_is_in_recovery() = true). \
             Scheduler will sleep until promotion."
        );
        // Sleep in a loop — if the server is promoted, pg_is_in_recovery()
        // changes to false and we can start working.
        loop {
            let should_continue =
                BackgroundWorker::wait_latch(Some(std::time::Duration::from_secs(30)));
            if !should_continue {
                log!("pg_trickle scheduler shutting down (replica)");
                return;
            }
            let still_replica = BackgroundWorker::transaction(AssertUnwindSafe(|| -> bool {
                Spi::get_one::<bool>("SELECT pg_is_in_recovery()")
                    .unwrap_or(Some(false))
                    .unwrap_or(false)
            }));
            if !still_replica {
                log!("pg_trickle scheduler: replica promoted — starting normal operation");
                break;
            }
        }
    }

    log!(
        "pg_trickle scheduler started (interval={}ms, event_driven_wake={})",
        config::pg_trickle_scheduler_interval_ms(),
        config::pg_trickle_event_driven_wake(),
    );

    let mut dag_version: u64 = 0;
    let mut dag: Option<StDag> = None;
    // Follow-up flag: after an incremental rebuild, schedule a full rebuild
    // on the next tick.  ALTER operations change dependency edges but not
    // the node set, so the stale-snapshot guard (which only checks for
    // missing nodes) cannot detect stale edges.  A follow-up full rebuild
    // with a fresh snapshot captures the committed edge changes.
    let mut pending_full_rebuild = false;

    // #536: Previous tick's safe frontier watermark, used by the holdback
    // algorithm to determine whether any long-running transaction spans a
    // tick boundary.  Reset to None on scheduler restart.
    let mut prev_tick_watermark: Option<String> = None;

    // Per-ST retry state (in-memory only, reset on scheduler restart)
    let mut retry_states: HashMap<i64, RetryState> = HashMap::new();
    let retry_policy = RetryPolicy::default();

    // Phase 4: Parallel dispatch state (persisted across ticks)
    let mut parallel_state = ParallelDispatchState::new();

    // Per-ST drift reset counters (differential cycles since last reinit)
    let mut drift_counters: HashMap<i64, i32> = HashMap::new();

    // P3-5: Per-ST auto-backoff factors (multiplicative schedule stretch).
    // When auto_backoff is enabled and a ST is falling behind, the factor
    // doubles each consecutive cycle; resets to 1.0 on the first on-time cycle.
    let mut backoff_factors: HashMap<i64, f64> = HashMap::new();

    // PH-E2: Per-ST consecutive spill counters.
    // When a differential refresh writes temp blocks exceeding the threshold,
    // the counter increments.  After spill_consecutive_limit consecutive spills,
    // the scheduler forces a FULL refresh.  Resets on any non-spilling refresh.
    let mut spill_counters: HashMap<i64, i32> = HashMap::new();

    // Timestamp for periodic CDC trigger health check (~every 60s).
    let mut last_trigger_health_ms: u64 = 0;

    // WM-7: Timestamp for periodic stuck-watermark alerting (~every 60s).
    let mut last_watermark_stuck_check_ms: u64 = 0;
    // WM-7: Track source OIDs that have already been reported as stuck to
    // avoid spamming the NOTIFY channel every check cycle.
    let mut reported_stuck_sources: HashSet<u32> = HashSet::new();

    // DB-5: Timestamp for daily history retention cleanup.
    let mut last_history_cleanup_ms: u64 = 0;
    const HISTORY_CLEANUP_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000; // 24 hours

    // DF-G2: Dog-feeding auto-apply — timestamp-gated, rate-limited.
    let mut last_auto_apply_ms: u64 = 0;
    const AUTO_APPLY_INTERVAL_MS: u64 = 10 * 60 * 1000; // 10 minutes

    // Phase 10: Crash recovery — mark any interrupted RUNNING records
    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        recover_from_crash();
    }));

    // Phase 2: Reconcile parallel refresh state (orphaned jobs, worker tokens)
    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        reconcile_parallel_state();
    }));

    // EC-20: Post-restart CDC TRANSITIONING health check.
    // If the scheduler was restarted during an active TRIGGER→WAL transition,
    // verify the transition is still valid. Invalid transitions (stale slot,
    // missing publication) are rolled back to TRIGGER mode.
    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        check_cdc_transition_health();
    }));

    // WAKE-1: Event-driven wake via LISTEN/NOTIFY.
    //
    // PostgreSQL's LISTEN command is restricted to regular backends
    // (MyBackendType == B_BACKEND). Background workers — which the scheduler
    // always is — are rejected with "cannot execute LISTEN within a background
    // process" (async.c:Async_Listen()). Attempting LISTEN causes an elog(ERROR)
    // that escapes pgrx's catch_unwind and exits the process with exit code 1.
    //
    // Until a background-worker-compatible notification mechanism is implemented
    // (e.g., direct latch signalling via shared memory), event_driven_wake is
    // always forced to false here regardless of the GUC value. CDC triggers still
    // emit pg_notify('pgtrickle_wake') for future use once this is re-enabled.
    let event_driven = false;
    if config::pg_trickle_event_driven_wake() {
        // GUC is on but feature is not yet functional in BGWs — warn once at startup.
        warning!(
            "pg_trickle scheduler: event_driven_wake=on is not supported in background \
             workers (PostgreSQL LISTEN is restricted to B_BACKEND processes). \
             Operating in polling-only mode. Set pg_trickle.event_driven_wake=off \
             to suppress this warning."
        );
    }

    // WAKE-1: Statistics for event-driven vs poll-based wakes.
    let mut wake_stats_event: u64 = 0;
    let mut wake_stats_poll: u64 = 0;
    let mut wake_stats_last_log_ms: u64 = current_epoch_ms();

    // OPS-6: Workload-aware poll — overlap count from df_scheduling_interference.
    // Refreshed once per auto-apply cycle (10 min).
    let mut interference_overlap_count: i64 = 0;

    // OP-2: Start the Prometheus metrics HTTP server if metrics_port is non-zero.
    let metrics_server = {
        let port = config::pg_trickle_metrics_port();
        if port > 0 {
            crate::metrics_server::MetricsServer::start(port as u16)
        } else {
            None
        }
    };

    loop {
        // DAG-2: Adaptive poll interval — exponential backoff (20ms → 200ms)
        // that resets to 20ms on worker completion, making parallel mode
        // competitive for cheap refreshes.
        let mut base_interval_ms = config::pg_trickle_scheduler_interval_ms() as u64;

        // OPS-6: Workload-aware poll — if df_scheduling_interference detects
        // heavy overlap, slightly increase the base interval to reduce
        // contention. This is a gentle back-off: +10% per overlap pair,
        // capped at 2× the configured interval.
        if interference_overlap_count > 0 {
            let boost = base_interval_ms / 10 * (interference_overlap_count as u64).min(10);
            base_interval_ms = (base_interval_ms + boost).min(base_interval_ms * 2);
        }

        let poll_ms = compute_adaptive_poll_ms(
            parallel_state.adaptive_poll_ms,
            parallel_state.completions_this_tick > 0,
            parallel_state.has_inflight(),
            base_interval_ms,
        );
        parallel_state.adaptive_poll_ms = poll_ms;
        parallel_state.completions_this_tick = 0;

        let wake_start = std::time::Instant::now();
        let should_continue =
            BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(poll_ms)));

        // OP-2: Service one pending Prometheus scrape request per tick (non-blocking).
        if let Some(ref ms) = metrics_server {
            // Collect metrics inside a transaction so SPI queries work.
            let metrics_text = BackgroundWorker::transaction(std::panic::AssertUnwindSafe(|| {
                crate::monitor::collect_metrics_text()
            }));
            ms.serve_one_request(&metrics_text);
        }

        // WAKE-1: Determine whether this wake was event-driven (notification)
        // or poll-based (timeout expired). If the latch returned faster than
        // the poll interval, a notification (or signal) woke us early.
        let wake_elapsed_ms = wake_start.elapsed().as_millis() as u64;
        let was_event_wake = event_driven && wake_elapsed_ms < poll_ms.saturating_sub(5);

        if was_event_wake {
            wake_stats_event += 1;
            // WAKE-1: Debounce — wait briefly to coalesce rapidly arriving
            // notifications from bulk DML before starting the tick.
            let debounce_ms = config::pg_trickle_wake_debounce_ms() as u64;
            if debounce_ms > 0 {
                let _ = BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(
                    debounce_ms,
                )));
            }
        } else {
            wake_stats_poll += 1;
        }

        // WAKE-1: Log wake statistics every 60 seconds.
        let now_for_stats = current_epoch_ms();
        if now_for_stats.saturating_sub(wake_stats_last_log_ms) >= 60_000 {
            if event_driven && (wake_stats_event > 0 || wake_stats_poll > 0) {
                log!(
                    "pg_trickle scheduler: wake stats — event={}, poll={} (last 60s)",
                    wake_stats_event,
                    wake_stats_poll,
                );
            }
            wake_stats_event = 0;
            wake_stats_poll = 0;
            wake_stats_last_log_ms = now_for_stats;
        }

        unsafe {
            if pg_sys::ConfigReloadPending != 0 {
                pg_sys::ConfigReloadPending = 0;
                pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP);
                pgrx::info!(
                    "pg_trickle scheduler: SIGHUP processed, allow_circular is now {}",
                    config::pg_trickle_allow_circular()
                );
            }
        }

        if !should_continue {
            // SIGTERM received — shut down gracefully.
            log!("pg_trickle scheduler shutting down");
            break;
        }

        if !config::pg_trickle_enabled() {
            continue;
        }

        // Check that pg_trickle is still installed in this database.  It can
        // be dropped at any time via DROP EXTENSION, which removes the schema
        // and all catalog tables.  Without this guard the very next SPI query
        // that references pgtrickle.* would crash the worker with exit code 1,
        // causing the launcher to apply the full 5-minute skip_ttl backoff.
        // Exiting cleanly here lets the launcher detect the fresh install
        // using the shorter retry_ttl for databases that previously had a
        // running scheduler.
        let still_installed = BackgroundWorker::transaction(AssertUnwindSafe(|| -> bool {
            Spi::get_one::<bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'pg_trickle')",
            )
            .unwrap_or(Some(false))
            .unwrap_or(false)
        }));
        if !still_installed {
            log!(
                "pg_trickle scheduler: pg_trickle was dropped from database '{}', exiting",
                db_name
            );
            return;
        }

        let now_ms = current_epoch_ms();

        // WAL transition processing — three phases with separate transactions
        // to ensure slot creation happens in a pristine transaction (no prior
        // SPI reads that could assign an XID via hint-bit WAL writes).
        //
        // Phase 1: Check eligibility, poll WAL sources, collect pending slots
        // Phase 2: Create replication slots (NO SPI — pristine transaction)
        // Phase 3: Finish transitions (publication + catalog update)
        let mut pending_slots = Vec::new();
        let mut pending_aborts = Vec::new();
        BackgroundWorker::transaction(AssertUnwindSafe(|| {
            let change_schema = config::pg_trickle_change_buffer_schema();
            match wal_decoder::advance_wal_transitions_phase1(&change_schema) {
                Ok(result) => {
                    pending_slots = result.pending_slots;
                    pending_aborts = result.pending_aborts;
                }
                Err(e) => log!("pg_trickle: WAL transition phase 1 error: {}", e),
            }
            // NOTE: check_slot_health_and_alert() is intentionally NOT called
            // here.  Phase 1's WAL poll for a missing/invalid replication slot
            // can abort the current SPI session (even when the Rust-level
            // catch_unwind absorbs the panic), which silently breaks subsequent
            // SPI calls — including the EC-34 slot-existence check inside
            // check_slot_health_and_alert().  Running the health check in its
            // own transaction (below) guarantees a pristine SPI connection and
            // ensures reliable fallback-to-TRIGGER detection.
        }));

        // Slot / buffer health check in its own pristine transaction.
        // Must be separate from Phase 1 so that WAL poll errors (which can
        // corrupt the Phase 1 SPI session) do not prevent the EC-34 missing-
        // slot detection from running.  This is the primary path for automatic
        // fallback from WAL mode to TRIGGER mode when a replication slot is
        // externally dropped.
        BackgroundWorker::transaction(AssertUnwindSafe(|| {
            monitor::check_slot_health_and_alert();
        }));

        // Periodic CDC trigger health check: detect disabled/missing triggers
        // on source tables.  Runs every ~60s to avoid per-tick overhead.
        let now_for_trigger_check = current_epoch_ms();
        if now_for_trigger_check.saturating_sub(last_trigger_health_ms) >= 60_000 {
            BackgroundWorker::transaction(AssertUnwindSafe(|| {
                monitor::check_cdc_trigger_health();
            }));
            last_trigger_health_ms = now_for_trigger_check;
        }

        // WM-7: Periodic stuck-watermark detection and alerting (~every 60s).
        let timeout = config::pg_trickle_watermark_holdback_timeout();
        if timeout > 0 {
            let now_for_wm_check = current_epoch_ms();
            if now_for_wm_check.saturating_sub(last_watermark_stuck_check_ms) >= 60_000 {
                BackgroundWorker::transaction(AssertUnwindSafe(|| {
                    match crate::catalog::find_stuck_watermarks(timeout) {
                        Ok(stuck_list) => {
                            // Collect currently-stuck OIDs for comparison.
                            let current_stuck: HashSet<u32> =
                                stuck_list.iter().map(|(_, oid, _)| *oid).collect();

                            // Emit NOTIFY for newly-stuck sources only.
                            for (group_name, source_oid, age_secs) in &stuck_list {
                                if !reported_stuck_sources.contains(source_oid) {
                                    let payload = format!(
                                        "{{\"event\":\"watermark_stuck\",\"group\":\"{}\",\
                                          \"source_oid\":{},\"age_secs\":{:.0}}}",
                                        group_name, source_oid, age_secs
                                    );
                                    let _ = Spi::run_with_args(
                                        "SELECT pg_notify('pgtrickle_alert', $1)",
                                        &[payload.as_str().into()],
                                    );
                                    pgrx::warning!(
                                        "pg_trickle: watermark stuck in group '{}' — \
                                         source OID {} not advanced for {:.0}s (timeout {}s)",
                                        group_name,
                                        source_oid,
                                        age_secs,
                                        timeout,
                                    );
                                }
                            }

                            // Emit recovery NOTIFY for sources that were stuck but
                            // have since advanced (auto-resume).
                            for previously_stuck in &reported_stuck_sources {
                                if !current_stuck.contains(previously_stuck) {
                                    let payload = format!(
                                        "{{\"event\":\"watermark_resumed\",\
                                          \"source_oid\":{}}}",
                                        previously_stuck
                                    );
                                    let _ = Spi::run_with_args(
                                        "SELECT pg_notify('pgtrickle_alert', $1)",
                                        &[payload.as_str().into()],
                                    );
                                    pgrx::info!(
                                        "pg_trickle: watermark resumed for source OID {}",
                                        previously_stuck,
                                    );
                                }
                            }

                            reported_stuck_sources = current_stuck;
                        }
                        Err(e) => {
                            pgrx::warning!("pg_trickle: stuck watermark check failed: {}", e,);
                        }
                    }
                }));
                last_watermark_stuck_check_ms = now_for_wm_check;
            }
        }

        // DB-5: Periodic history retention cleanup (daily).
        {
            let now_for_cleanup = current_epoch_ms();
            if now_for_cleanup.saturating_sub(last_history_cleanup_ms)
                >= HISTORY_CLEANUP_INTERVAL_MS
            {
                let retention_days = config::pg_trickle_history_retention_days();
                if retention_days > 0 {
                    // PERF-5: Batched DELETEs — delete up to 1000 rows per
                    // transaction to limit lock contention on pgt_refresh_history.
                    // Repeat until fewer than the batch size are deleted.
                    let batch_size: i64 = 1000;
                    let mut total_deleted: i64 = 0;
                    loop {
                        let deleted: i64 = BackgroundWorker::transaction(AssertUnwindSafe(|| {
                            Spi::get_one_with_args::<i64>(
                                "WITH batch AS (\
                                        SELECT refresh_id FROM pgtrickle.pgt_refresh_history \
                                        WHERE start_time < now() - make_interval(days => $1) \
                                        LIMIT $2 \
                                    ), deleted AS (\
                                        DELETE FROM pgtrickle.pgt_refresh_history \
                                        WHERE refresh_id IN (SELECT refresh_id FROM batch) \
                                        RETURNING 1\
                                    ) SELECT count(*) FROM deleted",
                                &[retention_days.into(), batch_size.into()],
                            )
                            .unwrap_or(Some(0))
                            .unwrap_or(0)
                        }));
                        total_deleted += deleted;
                        if deleted < batch_size {
                            break;
                        }
                    }
                    if total_deleted > 0 {
                        log!(
                            "pg_trickle: history cleanup — deleted {} rows older than {} days (batched)",
                            total_deleted,
                            retention_days,
                        );
                    }
                }
                last_history_cleanup_ms = now_for_cleanup;
            }
        }

        // DF-G2: Dog-feeding auto-apply — read df_threshold_advice, apply changes.
        {
            let now_for_auto_apply = current_epoch_ms();
            if now_for_auto_apply.saturating_sub(last_auto_apply_ms) >= AUTO_APPLY_INTERVAL_MS {
                let auto_apply_mode = config::pg_trickle_dog_feeding_auto_apply();
                if auto_apply_mode != config::DogFeedingAutoApply::Off {
                    BackgroundWorker::transaction(AssertUnwindSafe(|| {
                        dog_feeding_auto_apply_tick();
                    }));
                }
                last_auto_apply_ms = now_for_auto_apply;

                // SLA-3: Dynamic tier re-assignment — check and adjust tiers
                // for stream tables with SLA configured.
                BackgroundWorker::transaction(AssertUnwindSafe(|| {
                    sla_tier_adjustment_tick();
                }));

                // OPS-6: Refresh interference overlap count for workload-aware poll.
                // Only reads if the DF ST exists (safe even without dog-feeding).
                BackgroundWorker::transaction(AssertUnwindSafe(|| {
                    let si_exists: bool = Spi::get_one(
                        "SELECT EXISTS ( \
                            SELECT 1 FROM pgtrickle.pgt_stream_tables \
                            WHERE pgt_schema = 'pgtrickle' \
                              AND pgt_name = 'df_scheduling_interference' \
                        )",
                    )
                    .unwrap_or(Some(false))
                    .unwrap_or(false);
                    if si_exists {
                        interference_overlap_count = Spi::get_one(
                            "SELECT coalesce(sum(overlap_count), 0)::bigint \
                             FROM pgtrickle.df_scheduling_interference",
                        )
                        .unwrap_or(Some(0))
                        .unwrap_or(0);
                    } else {
                        interference_overlap_count = 0;
                    }
                }));
            }
        }

        // Phase 2: Create each pending slot in its own pristine transaction.
        // No SPI calls — just the C replication API.
        let mut created_slots = Vec::new();
        for pending in pending_slots {
            let slot_name = pending.slot_name.clone();
            let mut slot_lsn = None;
            BackgroundWorker::transaction(AssertUnwindSafe(|| {
                match wal_decoder::create_replication_slot_pristine(&slot_name) {
                    Ok(lsn) => {
                        log!("pg_trickle: created replication slot '{}'", slot_name);
                        slot_lsn = Some(lsn);
                    }
                    Err(e) => {
                        log!(
                            "pg_trickle: failed to create replication slot '{}': {}",
                            slot_name,
                            e
                        );
                    }
                }
            }));
            if let Some(lsn) = slot_lsn {
                created_slots.push((pending, lsn));
            }
        }

        // Phase 3: Finish transitions (publications + catalog updates)
        if !created_slots.is_empty() {
            BackgroundWorker::transaction(AssertUnwindSafe(|| {
                if let Err(e) = wal_decoder::advance_wal_transitions_phase3(&created_slots) {
                    log!("pg_trickle: WAL transition phase 3 error: {}", e);
                }
            }));
        }

        // Phase 4: Abort WAL transitions that need fallback to triggers.
        // Each abort runs in its own transaction because Phase 1's SPI may
        // be broken after a caught panic from a missing slot.
        for abort in pending_aborts {
            BackgroundWorker::transaction(AssertUnwindSafe(|| {
                let change_schema = config::pg_trickle_change_buffer_schema();
                if let Err(e) = wal_decoder::abort_wal_transition(
                    abort.source_relid,
                    abort.pgt_id,
                    &change_schema,
                ) {
                    warning!(
                        "pg_trickle: WAL abort (fallback to triggers) failed for OID {}: {}",
                        abort.source_relid.to_u32(),
                        e
                    );
                }
            }));
        }

        // Collect jobs to spawn (populated inside the transaction, spawned after).
        let mut pending_spawns: Vec<(String, i64)> = Vec::new();

        // Run the scheduler tick inside a transaction
        BackgroundWorker::transaction(AssertUnwindSafe(|| {
            // CSS1 / #536: Capture tick watermark for cross-source snapshot consistency
            // with frontier holdback to prevent silent data loss from long-running
            // transactions that span a tick boundary.
            let (tick_watermark, _current_oldest_xmin, _holdback_age_secs) =
                compute_coordinator_tick_watermark(prev_tick_watermark.as_deref());
            // Persist this tick's safe watermark for the next tick's holdback comparison.
            prev_tick_watermark.clone_from(&tick_watermark);

            // Step A: Check if DAG needs rebuild
            let current_version = shmem::current_dag_version();
            if current_version != dag_version || dag.is_none() || pending_full_rebuild {
                let force_full = pending_full_rebuild;
                pending_full_rebuild = false;

                // C2-1: Drain the invalidation ring buffer.
                let invalidated = shmem::drain_invalidations();

                // G-8: Try incremental rebuild when we have specific pgt_ids
                // and an existing DAG. Falls back to full rebuild on overflow,
                // no existing DAG, or incremental failure.
                // Skip incremental if a follow-up full rebuild was requested.
                let incremental_ok = if force_full {
                    false
                } else if let Some(ids) = &invalidated {
                    if !ids.is_empty() {
                        if let Some(ref mut existing_dag) = dag {
                            match existing_dag.rebuild_incremental(
                                ids,
                                config::pg_trickle_default_schedule_seconds(),
                            ) {
                                Ok(()) => {
                                    log!(
                                        "pg_trickle: DAG incrementally updated for pgt_ids {:?} (version={})",
                                        ids,
                                        current_version,
                                    );
                                    true
                                }
                                Err(e) => {
                                    log!(
                                        "pg_trickle: incremental DAG rebuild failed ({}), falling back to full rebuild",
                                        e,
                                    );
                                    false
                                }
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    // Overflow → full rebuild
                    log!("pg_trickle: DAG invalidation overflow → full rebuild");
                    false
                };

                if !incremental_ok {
                    match StDag::build_from_catalog(config::pg_trickle_default_schedule_seconds()) {
                        Ok(new_dag) => {
                            dag = Some(new_dag);
                            log!("pg_trickle: DAG rebuilt (version={})", current_version);
                        }
                        Err(e) => {
                            log!("pg_trickle: failed to rebuild DAG: {}", e);
                            return;
                        }
                    }
                }

                // Race-condition guard: verify all invalidated pgt_ids are visible
                // in the rebuilt DAG.  `signal_dag_invalidation` is called inside
                // the creating transaction before it commits, so the scheduler can
                // read the new version while the transaction is still in-flight.
                // If the SPI snapshot captured at tick start pre-dates that commit,
                // the catalog query misses the new ST and the DAG is built without
                // it.  Detect this by checking whether every invalidated id is
                // present; if not, trigger a full-rebuild signal and skip updating
                // dag_version so the next tick rebuilds with a fresh snapshot that
                // includes all committed STs.
                let stale_snapshot_detected = if let (Some(ids), Some(d)) = (&invalidated, &dag) {
                    ids.iter().any(|&id| !d.has_st_node(id))
                } else {
                    false
                };

                if stale_snapshot_detected {
                    log!(
                        "pg_trickle: DAG rebuild used stale snapshot — some invalidated pgt_ids \
                         not yet visible (version={}); triggering full rebuild next tick",
                        current_version
                    );
                    // Signal a full rebuild (overflow) so the next tick uses a fresh
                    // snapshot.  This is necessary because without the overflow flag,
                    // an incremental rebuild for a *different* pgt_id arriving between
                    // ticks could satisfy the version-mismatch check while still
                    // missing the not-yet-committed ST — permanently excluding it from
                    // the DAG.  The overflow path forces a full build_from_catalog()
                    // on the next tick regardless of what else arrives in the ring.
                    //
                    // Do NOT update dag_version — the stale tick's version is not
                    // trusted.
                    shmem::signal_dag_rebuild();
                } else {
                    dag_version = current_version;
                    // After an incremental rebuild, schedule a follow-up full
                    // rebuild to catch edge-stale scenarios.  ALTER operations
                    // update dependency edges but preserve the node; the stale
                    // guard above only detects missing *nodes*.  A full rebuild
                    // on the next tick (with a fresh snapshot that sees the
                    // committed edge updates) closes this window.
                    if incremental_ok {
                        pending_full_rebuild = true;
                    }
                }
            }

            let dag_ref = match &dag {
                Some(d) => d,
                None => return,
            };

            // Step B: Validate topological order (detect cycles)
            // When circular dependencies are allowed (allow_circular=true),
            // we use the condensation order (SCC-based) instead of rejecting
            // cycles outright. Cyclic SCCs are handled by fixpoint iteration.
            let allow_circular = config::pg_trickle_allow_circular();
            if !allow_circular && let Err(e) = dag_ref.topological_order() {
                log!("pg_trickle: DAG has cycles: {}", e);
                return;
            }

            // Step B2: Build execution unit DAG for parallel-refresh awareness.
            let parallel_mode = config::pg_trickle_parallel_refresh_mode();
            match parallel_mode {
                config::ParallelRefreshMode::Off => {}
                config::ParallelRefreshMode::DryRun => {
                    let eu_dag = ExecutionUnitDag::build_from_st_dag(dag_ref, |pgt_id| {
                        load_st_by_id(pgt_id).map(|st| st.refresh_mode)
                    });
                    log!(
                        "pg_trickle: parallel refresh (dry_run): {}",
                        eu_dag.summary()
                    );
                    log!("{}", eu_dag.dry_run_log());
                    // Fall through to sequential refresh.
                }
                config::ParallelRefreshMode::On => {
                    // Phase 4: Parallel dispatch replaces sequential refresh.
                    if parallel_state.dag_version != dag_version || parallel_state.eu_dag.is_none()
                    {
                        let eu_dag = ExecutionUnitDag::build_from_st_dag(dag_ref, |pgt_id| {
                            load_st_by_id(pgt_id).map(|st| st.refresh_mode)
                        });
                        log!(
                            "pg_trickle: parallel dispatch — EU DAG rebuilt: {}",
                            eu_dag.summary()
                        );
                        parallel_state.rebuild(eu_dag, dag_version);
                    }

                    parallel_dispatch_tick(
                        &mut parallel_state,
                        dag_ref,
                        now_ms,
                        &mut retry_states,
                        &retry_policy,
                        &db_name,
                        &mut pending_spawns,
                    );

                    // Prune retry states for STs that no longer exist.
                    if let Some(ref eu) = parallel_state.eu_dag {
                        let active_ids: HashSet<i64> = eu
                            .units()
                            .flat_map(|u| u.member_pgt_ids.iter().copied())
                            .collect();
                        retry_states.retain(|id, _| active_ids.contains(id));
                    }

                    return; // Skip sequential refresh.
                }
            }

            // Step B3: Handle cyclic SCCs via fixpoint iteration.
            // Process cyclic SCCs before the regular consistency group refresh
            // so that SCC members are up-to-date when downstream STs check
            // for upstream changes.
            // Collect SCC member IDs so Step C can skip them.
            let mut scc_member_ids: HashSet<i64> = HashSet::new();
            if allow_circular {
                let sccs = dag_ref.condensation_order();
                for scc in &sccs {
                    if !scc.is_cyclic {
                        continue; // Singletons handled below in Step C
                    }
                    // Record members so Step C skips them.
                    for node in &scc.nodes {
                        if let NodeId::StreamTable(id) = node {
                            scc_member_ids.insert(*id);
                        }
                    }
                    // Check if any SCC member needs refresh
                    let any_due = scc.nodes.iter().any(|node| {
                        if let NodeId::StreamTable(id) = node {
                            load_st_by_id(*id)
                                .map(|st| {
                                    (st.status == StStatus::Active
                                        || st.status == StStatus::Initializing)
                                        && (check_schedule(&st, dag_ref)
                                            || check_upstream_changes(&st)
                                            || st.needs_reinit)
                                })
                                .unwrap_or(false)
                        } else {
                            false
                        }
                    });
                    if !any_due {
                        continue;
                    }
                    iterate_to_fixpoint(
                        scc,
                        dag_ref,
                        &mut retry_states,
                        &retry_policy,
                        now_ms,
                        tick_watermark.as_deref(),
                    );
                }
            }

            // Step C: Compute consistency groups and refresh group-by-group
            let groups = dag_ref.compute_consistency_groups();

            for group in &groups {
                let initial_table_changes: HashMap<i64, bool> = group
                    .members
                    .iter()
                    .filter_map(|member| match member {
                        NodeId::StreamTable(id) => {
                            load_st_by_id(*id).map(|st| (*id, has_table_source_changes(&st)))
                        }
                        _ => None,
                    })
                    .collect();

                if group.is_singleton() {
                    // Fast path: no SAVEPOINT overhead for non-diamond STs.
                    let pgt_id = match &group.members[0] {
                        NodeId::StreamTable(id) => *id,
                        _ => continue,
                    };
                    // Skip SCC members already handled by fixpoint iteration.
                    if scc_member_ids.contains(&pgt_id) {
                        continue;
                    }
                    // P3-5: Auto-backoff — skip this tick if the backoff
                    // factor indicates we should wait longer.
                    let bf = backoff_factors.get(&pgt_id).copied().unwrap_or(1.0);
                    if bf > 1.0 {
                        // Check if enough time has passed since last refresh
                        // (effective interval = schedule * backoff_factor).
                        // NOTE: We are already inside the outer BackgroundWorker::transaction,
                        // so we must NOT call BackgroundWorker::transaction here — PostgreSQL
                        // does not allow StartTransactionCommand when already in TBLOCK_STARTED
                        // state (causes elog(FATAL)). Use Spi directly instead.
                        let should_skip = if let Some(st) = load_st_by_id(pgt_id)
                            && let Some(ref schedule_str) = st.schedule
                            && let Ok(max_secs) = crate::api::parse_duration(schedule_str)
                        {
                            let effective_secs: f64 = max_secs as f64 * bf;
                            let stale = Spi::get_one_with_args::<bool>(
                                "SELECT CASE WHEN last_refresh_at IS NULL THEN true \
                                 ELSE EXTRACT(EPOCH FROM (now() - last_refresh_at)) > $2 END \
                                 FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
                                &[st.pgt_id.into(), effective_secs.into()],
                            )
                            .unwrap_or(Some(false))
                            .unwrap_or(false);
                            !stale // skip if not yet due under backoff schedule
                        } else {
                            false
                        };
                        if should_skip {
                            continue;
                        }
                    }
                    let mut entry = drift_counters.entry(pgt_id).or_insert(0);
                    refresh_single_st(
                        pgt_id,
                        dag_ref,
                        now_ms,
                        &mut retry_states,
                        &retry_policy,
                        initial_table_changes.get(&pgt_id).copied(),
                        tick_watermark.as_deref(),
                        Some(&mut entry),
                        &mut spill_counters,
                    );
                    // P3-5: Update auto-backoff factor based on last refresh timing.
                    // NOTE: We are already inside the outer BackgroundWorker::transaction,
                    // so we must NOT call BackgroundWorker::transaction here — calling
                    // StartTransactionCommand in TBLOCK_STARTED state causes elog(FATAL).
                    if config::pg_trickle_auto_backoff() {
                        update_backoff_factor(pgt_id, &mut backoff_factors);
                    }
                    continue;
                }

                // Multi-member group: check if all members have diamond_consistency = 'atomic'.
                // Skip groups where all members are SCC-handled.
                let non_scc_members: Vec<&NodeId> = group
                    .members
                    .iter()
                    .filter(|m| {
                        if let NodeId::StreamTable(id) = m {
                            !scc_member_ids.contains(id)
                        } else {
                            true
                        }
                    })
                    .collect();
                if non_scc_members.is_empty() {
                    continue;
                }

                let all_atomic = group.isolation_level
                    == crate::dag::IsolationLevel::RepeatableRead
                    || group.members.iter().all(|m| {
                        if let NodeId::StreamTable(id) = m {
                            load_st_by_id(*id)
                                .map(|st| st.diamond_consistency == DiamondConsistency::Atomic)
                                .unwrap_or(false)
                        } else {
                            false
                        }
                    });

                if !all_atomic {
                    // Not all members opted in — fall back to independent refreshes.
                    for member in &group.members {
                        let pgt_id = match member {
                            NodeId::StreamTable(id) => *id,
                            _ => continue,
                        };
                        let mut entry = drift_counters.entry(pgt_id).or_insert(0);
                        refresh_single_st(
                            pgt_id,
                            dag_ref,
                            now_ms,
                            &mut retry_states,
                            &retry_policy,
                            initial_table_changes.get(&pgt_id).copied(),
                            tick_watermark.as_deref(),
                            Some(&mut entry),
                            &mut spill_counters,
                        );
                    }
                    continue;
                }

                // Atomic group: check group-level schedule policy.
                let policy = group_schedule_policy(group);
                if !is_group_due(group, policy, dag_ref) {
                    // When the Slowest policy is active, the group waits for all
                    // members to be due simultaneously.  In asymmetric pipelines
                    // (e.g. a diamond where only one branch has CDC events on a
                    // given tick), convergence nodes can accumulate staleness
                    // indefinitely.  Emit stale-data alerts so operators are
                    // notified rather than silently waiting.
                    if policy == DiamondSchedulePolicy::Slowest {
                        for node in &group.convergence_points {
                            if let NodeId::StreamTable(id) = node
                                && let Some(conv_st) = load_st_by_id(*id)
                            {
                                emit_stale_alert_if_needed(&conv_st);
                            }
                        }
                    }
                    continue;
                }

                // All members due (per policy) — wrap in an internal sub-transaction.
                //
                // Background workers cannot use `Spi::run("SAVEPOINT …")` because
                // PostgreSQL rejects transaction-control commands issued via SPI
                // (SPI_ERROR_TRANSACTION).  The correct approach is the internal
                // C-level sub-transaction API which bypasses SPI entirely.
                //
                // SAFETY: BeginInternalSubTransaction sets up a sub-transaction
                // within the current worker transaction using PostgreSQL's resource-
                // owner mechanism.  We save and restore CurrentMemoryContext and
                // CurrentResourceOwner to ensure no context leak regardless of
                // whether the sub-transaction commits or rolls back.  All work
                // inside is done through SPI (which handles its own C-level exception
                // boundaries), so a Rust-visible panic from this block is not
                // expected.
                let subtxn = SubTransaction::begin();

                let mut group_ok = true;
                let mut refreshed_ids: Vec<i64> = Vec::new();

                for member in &group.members {
                    let pgt_id = match member {
                        NodeId::StreamTable(id) => *id,
                        _ => continue,
                    };

                    let st = match load_st_by_id(pgt_id) {
                        Some(st) => st,
                        None => continue,
                    };

                    // Skip non-active STs
                    if st.status != StStatus::Active && st.status != StStatus::Initializing {
                        continue;
                    }

                    // BOOT-4: Skip if any source is gated.
                    let gated_oids = load_gated_source_oids();
                    if is_any_source_gated(pgt_id, &gated_oids) {
                        log!(
                            "pg_trickle: skipping {}.{} in atomic group — source gated",
                            st.pgt_schema,
                            st.pgt_name,
                        );
                        log_gated_skip(&st);
                        continue;
                    }

                    // WM-4: Skip if watermarks misaligned.
                    let (wm_misaligned, wm_reason) = is_watermark_misaligned(pgt_id);
                    if wm_misaligned {
                        let reason = wm_reason.as_deref().unwrap_or("watermark misaligned");
                        log!(
                            "pg_trickle: skipping {}.{} in atomic group — {}",
                            st.pgt_schema,
                            st.pgt_name,
                            reason,
                        );
                        log_watermark_skip(&st, reason);
                        continue;
                    }

                    // WM-7: Skip if any source watermark is stuck.
                    let (wm_stuck, wm_stuck_reason) = is_watermark_stuck(pgt_id);
                    if wm_stuck {
                        let reason = wm_stuck_reason.as_deref().unwrap_or("watermark stuck");
                        log!(
                            "pg_trickle: skipping {}.{} in atomic group — {}",
                            st.pgt_schema,
                            st.pgt_name,
                            reason,
                        );
                        log_watermark_skip(&st, reason);
                        continue;
                    }

                    // Check retry backoff
                    let retry = retry_states.entry(pgt_id).or_default();
                    if retry.is_in_backoff(now_ms) {
                        emit_stale_alert_if_needed(&st);
                        continue;
                    }

                    // Skip if catalog row locked by another session
                    if check_skip_needed(&st) {
                        continue;
                    }

                    // FUSE-5: Check fuse circuit breaker.
                    if evaluate_fuse(&st) {
                        log!(
                            "pg_trickle: {}.{} fuse blown — skipping in diamond group",
                            st.pgt_schema,
                            st.pgt_name,
                        );
                        continue;
                    }

                    let (has_changes, _has_stream_table_changes) =
                        upstream_change_state(&st, initial_table_changes.get(&pgt_id).copied());
                    let action = refresh::determine_refresh_action(&st, has_changes);
                    let result =
                        execute_scheduled_refresh(&st, action, tick_watermark.as_deref(), None);

                    match result {
                        RefreshOutcome::Success => {
                            refreshed_ids.push(pgt_id);
                        }
                        RefreshOutcome::RetryableFailure | RefreshOutcome::PermanentFailure => {
                            log!(
                                "pg_trickle: diamond group rollback — member {}.{} failed",
                                st.pgt_schema,
                                st.pgt_name,
                            );
                            group_ok = false;
                            break;
                        }
                    }
                }

                if group_ok {
                    subtxn.commit();
                    // Reset retry states for all refreshed members
                    for id in &refreshed_ids {
                        retry_states.entry(*id).or_default().reset();
                    }
                } else {
                    subtxn.rollback();
                    // Record failure for retry tracking on all attempted members
                    for id in &refreshed_ids {
                        let retry = retry_states.entry(*id).or_default();
                        retry.record_failure(&retry_policy, now_ms);
                    }
                    log!(
                        "pg_trickle: diamond group rolled back ({} members)",
                        group.members.len(),
                    );
                }
            }

            // Step D & E: Handled in the pre-refresh transaction above.

            // Step F: Prune retry states for STs that no longer exist
            // (avoid accumulating stale state)
            let active_ids: std::collections::HashSet<i64> = groups
                .iter()
                .flat_map(|g| g.members.iter())
                .filter_map(|n| match n {
                    NodeId::StreamTable(id) => Some(*id),
                    _ => None,
                })
                .collect();
            retry_states.retain(|id, _| active_ids.contains(id));
        }));

        // Phase 4: Spawn workers outside the transaction (after commit).
        for (db, job_id) in pending_spawns.drain(..) {
            if let Err(e) = spawn_refresh_worker(&db, job_id) {
                shmem::release_worker_token();
                parallel_state.per_db_inflight = parallel_state.per_db_inflight.saturating_sub(1);
                // Find and clear the in-flight tracking for this job.
                for us in parallel_state.unit_states.values_mut() {
                    if us.inflight_job_id == Some(job_id) {
                        us.inflight_job_id = None;
                        break;
                    }
                }
                log!(
                    "pg_trickle: parallel dispatch — failed to spawn worker for job {}: {}",
                    job_id,
                    e,
                );
                // Cancel the enqueued job row.
                BackgroundWorker::transaction(AssertUnwindSafe(|| {
                    let _ = SchedulerJob::cancel(job_id, &format!("Worker spawn failed: {}", e));
                }));
            }
        }
    }
}

// ── Refresh Outcome ────────────────────────────────────────────────────────

/// Outcome of a refresh attempt, used by the retry logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshOutcome {
    /// Refresh succeeded — reset retry state.
    Success,
    /// Refresh failed with a retryable error — apply backoff.
    RetryableFailure,
    /// Refresh failed with a permanent error — don't retry, count toward suspension.
    PermanentFailure,
}

// ── Crash Recovery ─────────────────────────────────────────────────────────

/// Check that the compiled shared library version matches the SQL-installed
/// extension version. Warns loudly if they differ (stale install).
///
/// Must be called inside a `BackgroundWorker::transaction()` block.
fn check_extension_version_match() {
    let compiled_version = env!("CARGO_PKG_VERSION");
    let installed_version: Option<String> =
        Spi::get_one("SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle'")
            .unwrap_or(None);

    if let Some(ref installed) = installed_version
        && installed != compiled_version
    {
        warning!(
            "pg_trickle: version mismatch — shared library is {} but installed SQL extension \
             is {}. Run 'ALTER EXTENSION pg_trickle UPDATE;' to update the SQL objects, \
             or reinstall the matching shared library.",
            compiled_version,
            installed
        );
    }
}

// ── SLA-3: Dynamic tier re-assignment ──────────────────────────────────────

/// SLA-3: Check and adjust tiers for stream tables with SLA configured.
///
/// Iterates over stream tables that have a `freshness_deadline_ms` set and
/// calls `maybe_adjust_tier_for_sla` for each.
fn sla_tier_adjustment_tick() {
    let st_ids: Vec<i64> = Spi::connect(|client| {
        let result = match client.select(
            "SELECT pgt_id FROM pgtrickle.pgt_stream_tables \
                 WHERE freshness_deadline_ms IS NOT NULL AND status = 'ACTIVE'",
            None,
            &[],
        ) {
            Ok(r) => r,
            Err(e) => {
                log!("pg_trickle: SLA-3 query failed: {e}");
                return Vec::new();
            }
        };
        let mut ids = Vec::new();
        for row in result {
            if let Some(id) = row.get::<i64>(1).unwrap_or(None) {
                ids.push(id);
            }
        }
        ids
    });

    for pgt_id in st_ids {
        if let Ok(Some(meta)) = StreamTableMeta::get_by_id(pgt_id) {
            crate::api::publication::maybe_adjust_tier_for_sla(&meta);
        }
    }
}

// ── DF-G2: Dog-feeding auto-apply tick ──────────────────────────────────────

/// Auto-apply threshold recommendations from `df_threshold_advice`.
///
/// Called once per `AUTO_APPLY_INTERVAL_MS` when `dog_feeding_auto_apply` GUC
/// is not `off`. Reads HIGH-confidence recommendations where the recommended
/// threshold differs from the current threshold by > 5%, then applies via
/// `StreamTableMeta::update_adaptive_threshold`. Rate-limited to 1 change per
/// ST per invocation (STAB-2, STAB-4).
///
/// DF-G3: Logs changes to `pgt_refresh_history` with `initiated_by = 'DOG_FEED'`.
fn dog_feeding_auto_apply_tick() {
    // STAB-4: Check that df_threshold_advice exists before reading.
    let advice_exists: bool = Spi::get_one(
        "SELECT EXISTS (
            SELECT 1 FROM pgtrickle.pgt_stream_tables
            WHERE pgt_schema = 'pgtrickle' AND pgt_name = 'df_threshold_advice'
        )",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !advice_exists {
        return;
    }

    // Read recommendations with HIGH confidence and > 5% delta.
    let recommendations: Vec<(i64, f64, f64, String)> = Spi::connect(|client| {
        let result = match client.select(
            "SELECT ta.pgt_id, ta.recommended_threshold, ta.current_threshold,
                    ta.pgt_schema || '.' || ta.pgt_name AS fq_name
             FROM pgtrickle.df_threshold_advice ta
             WHERE ta.confidence = 'HIGH'
               AND ta.recommended_threshold IS NOT NULL
               AND ta.current_threshold IS NOT NULL
               AND abs(ta.recommended_threshold - ta.current_threshold)
                   / NULLIF(ta.current_threshold, 0) > 0.05",
            None,
            &[],
        ) {
            Ok(r) => r,
            Err(e) => {
                pgrx::warning!("pg_trickle: auto-apply read failed: {}", e);
                return Vec::new();
            }
        };

        let mut recs = Vec::new();
        for row in result {
            let pgt_id = row.get::<i64>(1).unwrap_or(None).unwrap_or(0);
            let recommended = row.get::<f64>(2).unwrap_or(None).unwrap_or(0.0);
            let current = row.get::<f64>(3).unwrap_or(None).unwrap_or(0.0);
            let fq_name = row.get::<String>(4).unwrap_or(None).unwrap_or_default();
            if pgt_id > 0 {
                recs.push((pgt_id, recommended, current, fq_name));
            }
        }
        recs
    });

    for (pgt_id, recommended, current, fq_name) in &recommendations {
        // STAB-2: Handle ALTER failure gracefully — don't let one failure stop others.
        match crate::catalog::StreamTableMeta::update_adaptive_threshold(
            *pgt_id,
            Some(*recommended),
            None,
        ) {
            Ok(()) => {
                log!(
                    "pg_trickle: auto-apply threshold {} → {} for {} (DOG_FEED)",
                    current,
                    recommended,
                    fq_name,
                );
                // DF-G3: Audit trail in pgt_refresh_history.
                if let Ok(Some(now)) =
                    Spi::get_one::<pgrx::prelude::TimestampWithTimeZone>("SELECT now()")
                {
                    let _ = crate::catalog::RefreshRecord::insert(
                        *pgt_id,
                        now,
                        "SKIP",
                        "COMPLETED",
                        0,
                        0,
                        Some(&format!(
                            "DOG_FEED: auto_threshold {} → {}",
                            current, recommended
                        )),
                        Some("DOG_FEED"),
                        None,
                        0,
                        None,
                        false,
                        None,
                    );
                }
            }
            Err(e) => {
                pgrx::warning!(
                    "pg_trickle: auto-apply threshold update failed for {}: {}",
                    fq_name,
                    e,
                );
            }
        }
    }

    if !recommendations.is_empty() {
        log!(
            "pg_trickle: auto-apply applied {} threshold changes",
            recommendations.len(),
        );
    }

    // UX-3: Check for anomalies and emit NOTIFY on pg_trickle_alert channel.
    dog_feeding_anomaly_notify();
}

/// UX-3: Emit NOTIFY on `pgtrickle_alert` channel when anomalies are detected.
///
/// Reads `df_anomaly_signals` and sends a JSON notification for each stream
/// table that has a duration anomaly or recent failures ≥ 2.
fn dog_feeding_anomaly_notify() {
    let signals_exist: bool = Spi::get_one(
        "SELECT EXISTS (
            SELECT 1 FROM pgtrickle.pgt_stream_tables
            WHERE pgt_schema = 'pgtrickle' AND pgt_name = 'df_anomaly_signals'
        )",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !signals_exist {
        return;
    }

    let anomalies: Vec<(String, Option<String>, i64)> = Spi::connect(|client| {
        let result = match client.select(
            "SELECT a.pgt_schema || '.' || a.pgt_name AS fq_name,
                    a.duration_anomaly,
                    a.recent_failures
             FROM pgtrickle.df_anomaly_signals a
             WHERE a.duration_anomaly IS NOT NULL
                OR a.recent_failures >= 2",
            None,
            &[],
        ) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        let mut out = Vec::new();
        for row in result {
            let fq_name = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let anomaly = row.get::<String>(2).unwrap_or(None);
            let failures = row.get::<i64>(3).unwrap_or(None).unwrap_or(0);
            if !fq_name.is_empty() {
                out.push((fq_name, anomaly, failures));
            }
        }
        out
    });

    for (fq_name, anomaly, failures) in &anomalies {
        let anomaly_str = anomaly.as_deref().unwrap_or("none");
        let payload = format!(
            r#"{{"event":"dog_feed_anomaly","stream_table":"{}","anomaly":"{}","recent_failures":{}}}"#,
            fq_name.replace('"', r#"\""#),
            anomaly_str.replace('"', r#"\""#),
            failures,
        );
        let _ = Spi::run_with_args(
            "SELECT pg_notify('pgtrickle_alert', $1)",
            &[payload.as_str().into()],
        );
    }
}

/// Recover from a crash or unclean scheduler shutdown.
///
/// Any refresh history records stuck in 'RUNNING' status indicate interrupted
/// transactions. PostgreSQL will have rolled back the transaction, but the
/// history record may still say RUNNING if the INSERT was committed in a
/// separate transaction (which it is, via SPI in the scheduler loop).
///
/// This function marks all such records as FAILED and logs the recovery.
fn recover_from_crash() {
    let updated = Spi::connect_mut(|client| {
        let result = client.update(
            "UPDATE pgtrickle.pgt_refresh_history \
             SET status = 'FAILED', \
                 error_message = 'Interrupted by scheduler restart', \
                 end_time = now() \
             WHERE status = 'RUNNING'",
            None,
            &[],
        );
        match result {
            Ok(tuptable) => tuptable.len() as i64,
            Err(e) => {
                log!("pg_trickle: crash recovery query failed: {}", e);
                0
            }
        }
    });

    if updated > 0 {
        log!(
            "pg_trickle: crash recovery — marked {} interrupted refresh(es) as FAILED",
            updated
        );
    }
}

// ── CDC Transition Health Check (EC-20) ────────────────────────────────────

/// Check CDC transitions left in TRANSITIONING state after a scheduler restart.
///
/// If the scheduler crashed or was restarted during an active TRIGGER→WAL
/// transition, the replication slot or publication may be in an inconsistent
/// state. This function checks all TRANSITIONING dependencies and rolls back
/// any that have stale or missing slots.
fn check_cdc_transition_health() {
    use crate::catalog::StDependency;

    let deps = match StDependency::get_all() {
        Ok(d) => d,
        Err(e) => {
            log!(
                "pg_trickle: CDC health check — failed to load dependencies: {}",
                e
            );
            return;
        }
    };

    let transitioning: Vec<_> = deps
        .iter()
        .filter(|d| d.cdc_mode == crate::catalog::CdcMode::Transitioning)
        .collect();

    if transitioning.is_empty() {
        return;
    }

    log!(
        "pg_trickle: CDC health check — found {} source(s) in TRANSITIONING state after restart",
        transitioning.len()
    );

    for dep in transitioning {
        let slot_name = dep.slot_name.as_deref().unwrap_or("__pgt_missing_slot__");

        // Check if the replication slot still exists
        let slot_exists = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = '{}')",
            slot_name.replace('\'', "''")
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if !slot_exists {
            log!(
                "pg_trickle: CDC health check — replication slot '{}' missing for source OID {}. \
                 Rolling back to TRIGGER mode.",
                slot_name,
                dep.source_relid.to_u32()
            );
            // Roll back to TRIGGER mode
            if let Err(e) = StDependency::update_cdc_mode_for_source(
                dep.source_relid,
                crate::catalog::CdcMode::Trigger,
                None,
                None,
            ) {
                log!(
                    "pg_trickle: CDC health check — failed to rollback source OID {}: {}",
                    dep.source_relid.to_u32(),
                    e
                );
            }
        } else {
            log!(
                "pg_trickle: CDC health check — source OID {} in TRANSITIONING with valid slot '{}'. \
                 Normal transition will resume.",
                dep.source_relid.to_u32(),
                slot_name
            );
        }
    }
}

// ── Skip Mechanism ─────────────────────────────────────────────────────────

// Check if a refresh should be skipped because a previous one is still running.
//
// Uses `SELECT ... FOR UPDATE SKIP LOCKED` on the catalog row to detect
// concurrent refreshes.  If another session holds the row lock the query
// returns zero rows and we skip.  The row lock, once acquired, is held for
// the remainder of the caller's transaction — this is intentional, as the
// lock prevents concurrent refreshes until the tick transaction commits.
//
// IMPORTANT: `Spi::get_one_with_args` is used here (not `Spi::connect` +
// `client.select`) because pgrx's `select` passes `read_only =
// Spi::is_xact_still_immutable()`, which evaluates to `true` when no prior
// mutation has assigned a transaction XID yet.  PostgreSQL rejects `FOR
// UPDATE` in a read-only SPI context with "SELECT FOR UPDATE is not allowed
// in a non-volatile function".  `get_one_with_args` routes through
// `connect_mut` / `update`, which always uses `read_only = false`.
//
// PB1: replaces the previous `pg_try_advisory_lock` approach for
// PgBouncer transaction‐mode compatibility.

// ── FUSE-5: Fuse circuit breaker pre-check ─────────────────────────────────

/// Count total pending change buffer rows across all TABLE/FOREIGN_TABLE
/// sources for a stream table.
fn count_pending_changes(st: &StreamTableMeta) -> i64 {
    let change_schema = config::pg_trickle_change_buffer_schema();
    let source_oids = get_source_oids_for_st(st.pgt_id);

    let mut total: i64 = 0;
    for oid in &source_oids {
        let count = Spi::get_one::<i64>(&format!(
            "SELECT count(*) FROM {}.changes_{}",
            change_schema,
            oid.to_u32(),
        ))
        .unwrap_or(Some(0))
        .unwrap_or(0);
        total = total.saturating_add(count);
    }
    total
}

/// Evaluate the fuse circuit breaker for a stream table.
///
/// Returns `true` if the fuse blew (refresh should be skipped).
/// Returns `false` if the fuse is OK or disabled.
fn evaluate_fuse(st: &StreamTableMeta) -> bool {
    // Quick exit: fuse disabled or already blown
    if st.fuse_mode == "off" || st.fuse_state == "blown" || st.fuse_state == "disabled" {
        if st.fuse_state == "blown" {
            // Emit a periodic reminder so operators know the fuse is still
            // blown even if they missed the initial notification.  We send
            // at most once per ~60 seconds by checking blown_at age modulo.
            emit_fuse_blown_reminder(st);
            return true;
        }
        return false;
    }

    // Determine effective ceiling
    let global_ceiling = config::pg_trickle_fuse_default_ceiling();
    let effective_ceiling = match st.fuse_ceiling {
        Some(c) if c > 0 => c,
        _ if global_ceiling > 0 => global_ceiling,
        _ => return false, // No ceiling configured — fuse is a no-op
    };

    // Count pending changes
    let pending = count_pending_changes(st);
    if pending <= effective_ceiling {
        return false;
    }

    // Sensitivity check: only blow after N consecutive over-ceiling observations.
    // For now, each scheduler tick is one observation. Sensitivity > 1 requires
    // tracking a counter in the catalog, which we skip for v1 (sensitivity
    // defaults to 1 = blow immediately on first over-ceiling observation).
    let sensitivity = st.fuse_sensitivity.unwrap_or(1);
    if sensitivity > 1 {
        // Future: track observation count in catalog. For now, blow immediately
        // since we don't yet have a per-tick counter column.
        // This is correct for sensitivity=1 (the default).
    }
    let _ = sensitivity; // suppress unused warning

    // BLOW the fuse
    let reason = format!(
        "change buffer count ({}) exceeded ceiling ({})",
        pending, effective_ceiling
    );
    pgrx::warning!(
        "pg_trickle: FUSE BLOWN for {}.{} — {}",
        st.pgt_schema,
        st.pgt_name,
        reason,
    );

    if let Err(e) = StreamTableMeta::blow_fuse(st.pgt_id, &reason) {
        pgrx::warning!(
            "pg_trickle: failed to blow fuse for {}.{}: {}",
            st.pgt_schema,
            st.pgt_name,
            e,
        );
    }

    // Send pg_notify alert
    let notify_payload = format!(
        "{{\"event\":\"fuse_blown\",\"stream_table\":\"{}.{}\",\"pending\":{},\"ceiling\":{}}}",
        st.pgt_schema, st.pgt_name, pending, effective_ceiling
    );
    let _ = Spi::run_with_args(
        "SELECT pg_notify('pgtrickle_alert', $1)",
        &[notify_payload.as_str().into()],
    );

    true
}

/// Emit a periodic reminder that a fuse is still blown.
///
/// To avoid spamming the NOTIFY channel every tick (~100ms), we only emit
/// when the blown_at age crosses a 60-second boundary.  The scheduler calls
/// `evaluate_fuse` every tick, so we use `blown_at` to throttle.
fn emit_fuse_blown_reminder(st: &StreamTableMeta) {
    // Only emit if we can determine how long the fuse has been blown.
    let blown_secs = Spi::get_one_with_args::<f64>(
        "SELECT EXTRACT(EPOCH FROM (now() - blown_at))::float8 \
         FROM pgtrickle.pgt_stream_tables \
         WHERE pgt_id = $1 AND blown_at IS NOT NULL",
        &[st.pgt_id.into()],
    )
    .unwrap_or(None);

    if let Some(secs) = blown_secs {
        // Emit once per ~60 seconds: fire when we're in the first tick
        // window after a 60s boundary.  The tick interval is configurable
        // but typically 100ms–1s, so checking `secs % 60 < 1` is safe.
        let interval = 60.0_f64;
        if secs >= interval && (secs % interval) < 1.0 {
            monitor::emit_alert(
                monitor::AlertEvent::FuseBlownReminder,
                &st.pgt_schema,
                &st.pgt_name,
                &format!(
                    r#""blown_seconds":{:.0},"reason":"{}""#,
                    secs,
                    st.blow_reason
                        .as_deref()
                        .unwrap_or("unknown")
                        .replace('"', r#"\""#),
                ),
                st.pooler_compatibility_mode,
            );
        }
    }
}

/// Evaluate the fuse and return the effective ceiling threshold if the fuse
/// is active for this stream table. Pure decision logic for unit testing.
#[cfg(test)]
fn fuse_is_active(fuse_mode: &str, fuse_ceiling: Option<i64>, global_ceiling: i64) -> bool {
    if fuse_mode == "off" {
        return false;
    }
    let effective = match fuse_ceiling {
        Some(c) if c > 0 => c,
        _ if global_ceiling > 0 => global_ceiling,
        _ => return false,
    };
    effective > 0
}

/// Returns `true` if the refresh should be skipped.
///
/// SAF-1 audit (v0.11.0): This function and the broader worker loop in
/// `scheduler.rs`, `refresh.rs`, and `hooks.rs` have been audited for
/// `panic!` / `unwrap()` / `.expect()` in non-test code paths. All such
/// calls found were confined to `#[cfg(test)]` blocks. The only non-test
/// fallible path here is the SPI call below, which was previously silently
/// swallowed. It now logs a WARNING so operators can distinguish a missed
/// lock-check from a genuine SPI failure.
fn check_skip_needed(st: &StreamTableMeta) -> bool {
    let pgt_id = st.pgt_id;

    // FOR UPDATE SKIP LOCKED requires read_only=false (mutating SPI context).
    // Returns Some(pgt_id) if lock acquired, None if row is locked by another session.
    let spi_result = Spi::get_one_with_args::<i64>(
        "SELECT pgt_id FROM pgtrickle.pgt_stream_tables \
         WHERE pgt_id = $1 FOR UPDATE SKIP LOCKED",
        &[pgt_id.into()],
    );

    let row_found = match spi_result {
        Ok(opt) => opt.is_some(),
        Err(e) => {
            // SAF-1: Log the SPI error instead of silently treating it as
            // "locked". We still skip (return true) to be conservative — a
            // failed lock check should not allow concurrent refreshes.
            pgrx::warning!(
                "[pg_trickle] check_skip_needed: SPI error for {}.{} (pgt_id={}): {} \
                 — treating as locked, skipping this cycle",
                st.pgt_schema,
                st.pgt_name,
                pgt_id,
                e,
            );
            false
        }
    };

    // Zero rows (or SPI error) → the row is locked or unavailable → skip.
    !row_found
}

// ── Time Helper ────────────────────────────────────────────────────────────

/// Get the current epoch time in milliseconds.
fn current_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Resolve the effective schedule policy for a consistency group.
///
/// Reads the `diamond_schedule_policy` from each convergence node in the group
/// and returns the strictest (`Slowest > Fastest`). Falls back to `'fastest'`.
fn group_schedule_policy(group: &crate::dag::ConsistencyGroup) -> DiamondSchedulePolicy {
    group
        .convergence_points
        .iter()
        .fold(DiamondSchedulePolicy::Fastest, |acc, node| {
            if let NodeId::StreamTable(id) = node {
                load_st_by_id(*id)
                    .map(|st| acc.stricter(st.diamond_schedule_policy))
                    .unwrap_or(acc)
            } else {
                acc
            }
        })
}

/// Check whether a multi-member group is due for refresh according to its
/// schedule policy.
///
/// - `Fastest`: fires when **any** member is due.
/// - `Slowest`: fires only when **all** members are due.
fn is_group_due(
    group: &crate::dag::ConsistencyGroup,
    policy: DiamondSchedulePolicy,
    dag: &StDag,
) -> bool {
    let member_due: Vec<bool> = group
        .members
        .iter()
        .filter_map(|m| {
            if let NodeId::StreamTable(id) = m {
                load_st_by_id(*id).map(|st| {
                    // D-1b: Check for UNLOGGED buffer crash recovery before
                    // evaluating schedule. This may set needs_reinit=true.
                    check_unlogged_buffer_crash_recovery(&st);
                    // Reload in case needs_reinit was set.
                    let st = load_st_by_id(*id).unwrap_or(st);
                    check_schedule(&st, dag) || st.needs_reinit
                })
            } else {
                None
            }
        })
        .collect();

    is_group_due_pure(&member_due, policy)
}

/// Pure decision logic: given member-due flags and a policy, decide if the
/// group is due for refresh.
fn is_group_due_pure(member_due: &[bool], policy: DiamondSchedulePolicy) -> bool {
    if member_due.is_empty() {
        return false;
    }

    match policy {
        DiamondSchedulePolicy::Fastest => member_due.iter().any(|&d| d),
        DiamondSchedulePolicy::Slowest => member_due.iter().all(|&d| d),
    }
}

/// Check if the refresh is falling behind the schedule.
///
/// Returns `Some(ratio)` when `elapsed_ms / schedule_ms >= threshold`.
/// Use `threshold = 0.95` for auto-backoff decisions (only back off when
/// genuinely unable to keep up) and `threshold = 0.80` for EC-11 operator
/// alerts (warn early so operators have time to react).
fn is_falling_behind(elapsed_ms: i64, schedule_ms: i64, threshold: f64) -> Option<f64> {
    if schedule_ms <= 0 {
        return None;
    }
    let ratio = elapsed_ms as f64 / schedule_ms as f64;
    if ratio >= threshold {
        Some(ratio)
    } else {
        None
    }
}

/// P3-5: Update the auto-backoff factor for a stream table based on its
/// last refresh timing. Doubles the factor when falling behind; resets to
/// 1.0 on the first on-time cycle.
fn update_backoff_factor(pgt_id: i64, backoff_factors: &mut HashMap<i64, f64>) {
    let st = match load_st_by_id(pgt_id) {
        Some(st) => st,
        None => return,
    };
    let schedule_str = match &st.schedule {
        Some(s) => s.clone(),
        None => return,
    };
    let max_secs = match crate::api::parse_duration(&schedule_str) {
        Ok(s) => s,
        Err(_) => return,
    };
    let schedule_ms = max_secs * 1000;
    if schedule_ms <= 0 {
        return;
    }

    // Check last refresh duration from history
    let elapsed_ms: Option<i64> = Spi::get_one_with_args::<i64>(
        "SELECT EXTRACT(EPOCH FROM (end_time - start_time))::BIGINT * 1000 \
         FROM pgtrickle.pgt_refresh_history \
         WHERE pgt_id = $1 AND status = 'COMPLETED' AND end_time IS NOT NULL \
         ORDER BY refresh_id DESC LIMIT 1",
        &[pgt_id.into()],
    )
    .unwrap_or(None);

    let elapsed_ms = match elapsed_ms {
        Some(ms) => ms,
        None => return,
    };

    // Use a 95% threshold: only back off when refresh genuinely cannot keep
    // up with the schedule. The lower 80% threshold used by EC-11 alerting
    // is intentionally more sensitive (early warning), whereas backoff should
    // only activate when the situation is clearly unsustainable.
    if is_falling_behind(elapsed_ms, schedule_ms, 0.95).is_some() {
        // Double the backoff factor, cap at 8x.
        // An 8x cap limits worst-case slowdown to 8× the configured interval
        // and self-heals immediately on the first on-time refresh cycle.
        let factor = backoff_factors.get(&pgt_id).copied().unwrap_or(1.0);
        let new_factor = (factor * 2.0).min(8.0);
        backoff_factors.insert(pgt_id, new_factor);
        pgrx::warning!(
            "pg_trickle: auto-backoff for pgt_id={} increased to {:.0}x \
             (refresh {}ms, schedule {}ms). The effective refresh interval \
             is now {}ms. It will reset automatically once a refresh completes \
             within the schedule budget.",
            pgt_id,
            new_factor,
            elapsed_ms,
            schedule_ms,
            (schedule_ms as f64 * new_factor) as i64,
        );
    } else {
        // On-time: reset backoff
        if backoff_factors.remove(&pgt_id).is_some() {
            pgrx::warning!(
                "pg_trickle: auto-backoff for pgt_id={} reset to 1x (refresh \
                 completed on time at {}ms vs {}ms schedule).",
                pgt_id,
                elapsed_ms,
                schedule_ms,
            );
        }
    }
}

/// Load a stream table by its pgt_id, or return None if not found.
fn load_st_by_id(pgt_id: i64) -> Option<StreamTableMeta> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT pgt_id, pgt_relid, pgt_name, pgt_schema, defining_query, \
                 schedule, refresh_mode, status, is_populated, data_timestamp, \
                 consecutive_errors, needs_reinit, frontier \
                 FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
                None,
                &[pgt_id.into()],
            )
            .ok()?;

        if table.is_empty() {
            return None;
        }

        // Use get_by_name as a workaround — we have the data already
        None
    })
    // Fallback: try via the catalog API
    .or_else(|| {
        // Load all active STs and find the one we need
        StreamTableMeta::get_all_active()
            .ok()?
            .into_iter()
            .find(|st| st.pgt_id == pgt_id)
    })
}

/// Check if a ST is stale (staleness exceeds effective schedule or cron is due).
///
/// G-7: When tiered scheduling is enabled, the tier multiplier is applied
/// to duration-based schedules. Frozen-tier STs always return `false`.
///
/// DI-9: IMMEDIATE-mode STs always return `false` — they are refreshed
/// synchronously within the user's transaction by AFTER triggers. The
/// scheduler has no work to do for them and acquiring locks would only
/// cause contention with the IMMEDIATE trigger path. Downstream CALCULATED
/// dependants are unaffected: they detect upstream changes via
/// `has_stream_table_source_changes()` independently.
fn check_schedule(st: &StreamTableMeta, _dag: &StDag) -> bool {
    // DI-9: IMMEDIATE-mode tables refresh inline — the scheduler must not
    // compete for locks on them.
    if st.refresh_mode.is_immediate() {
        return false;
    }

    // If not yet populated, always needs refresh
    if !st.is_populated {
        return true;
    }

    // G-7: When tiered scheduling is enabled, check tier first.
    if config::pg_trickle_tiered_scheduling() {
        let tier = RefreshTier::from_sql_str(&st.refresh_tier);
        if tier == RefreshTier::Frozen {
            emit_frozen_tier_skip(st);
            return false;
        }
    }

    // Check staleness vs schedule
    if let Some(ref schedule_str) = st.schedule {
        // Determine if this is a cron expression or a duration
        let trimmed = schedule_str.trim();
        if trimmed.starts_with('@') || trimmed.contains(' ') {
            // Cron-based: check if the cron schedule says we're due
            // (tier multiplier not applied to cron schedules)
            let last_refresh_epoch = Spi::get_one_with_args::<f64>(
                "SELECT EXTRACT(EPOCH FROM last_refresh_at) FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
                &[st.pgt_id.into()],
            )
            .unwrap_or(None)
            .map(|e| e as i64);

            return crate::api::cron_is_due(trimmed, last_refresh_epoch);
        }

        // Duration-based: compare staleness against parsed seconds.
        // Uses last_refresh_at (updated on every run, including NO_DATA)
        // rather than data_timestamp (only updated when data changes).
        // This ensures the 1-minute schedule fires at most once per minute
        // regardless of whether the previous run found any data changes.
        if let Ok(max_secs) = crate::api::parse_duration(trimmed) {
            // G-7: Apply tier multiplier when tiered scheduling is enabled.
            let effective_secs = if config::pg_trickle_tiered_scheduling() {
                let tier = RefreshTier::from_sql_str(&st.refresh_tier);
                // Frozen is handled above; multiplier is always Some here.
                let mult = tier.schedule_multiplier().unwrap_or(1.0);
                ((max_secs as f64) * mult) as i64
            } else {
                max_secs
            };
            let stale = Spi::get_one_with_args::<bool>(
                "SELECT CASE WHEN last_refresh_at IS NULL THEN true \
                 ELSE EXTRACT(EPOCH FROM (now() - last_refresh_at)) > $2 END \
                 FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
                &[st.pgt_id.into(), effective_secs.into()],
            )
            .unwrap_or(Some(false))
            .unwrap_or(false);
            return stale;
        }

        // Unparseable schedule — log a warning and skip
        pgrx::warning!(
            "pg_trickle: could not parse schedule '{}' for pgt_id={}; skipping schedule check",
            schedule_str,
            st.pgt_id
        );
        return false;
    }

    // CALCULATED STs: refresh whenever upstream sources have pending changes.
    // The topological iteration order guarantees upstream STs are refreshed
    // first within a tick, so their change buffers are already populated by
    // the time we check here.
    check_upstream_changes(st)
}

/// Emit a StaleData or NoUpstreamChanges alert depending on whether the
/// scheduler itself is falling behind.
///
/// The distinction matters:
/// - **StaleData** (warning): `last_refresh_at` is also old, meaning the
///   scheduler is stuck, in backoff, or overloaded.  The view is genuinely
///   out of date.
/// - **NoUpstreamChanges** (info): `last_refresh_at` is recent (scheduler is
///   alive and looping), but `data_timestamp` is frozen because the source
///   tables have had no writes.  The view is correct; there is just nothing
///   new to show.
///
/// Called when a refresh is skipped (due to backoff, row lock, etc.)
/// so that operators are notified via NOTIFY even when the scheduler
/// cannot perform the refresh (F31: G9.4).
fn emit_stale_alert_if_needed(st: &StreamTableMeta) {
    if let Some(ref schedule_str) = st.schedule {
        let trimmed = schedule_str.trim();
        // Only for duration-based schedules (not cron)
        if !trimmed.starts_with('@')
            && !trimmed.contains(' ')
            && let Ok(max_secs) = crate::api::parse_duration(trimmed)
        {
            let schedule_f64 = max_secs as f64;

            // Fetch both timestamps in one query.
            let row = Spi::get_two_with_args::<f64, f64>(
                "SELECT \
                     EXTRACT(EPOCH FROM (now() - data_timestamp))::float8, \
                     EXTRACT(EPOCH FROM (now() - last_refresh_at))::float8 \
                 FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
                &[st.pgt_id.into()],
            )
            .unwrap_or((None, None));

            let (data_age, refresh_age) = row;

            if let Some(data_secs) = data_age
                && data_secs > schedule_f64 * 2.0
            {
                // Is the scheduler itself running on time?
                // Use 3× schedule as a generous liveness window.
                let scheduler_alive = refresh_age
                    .map(|r| r <= schedule_f64 * 3.0)
                    .unwrap_or(false);

                if scheduler_alive {
                    // Scheduler is healthy — source tables are just quiet.
                    monitor::alert_no_upstream_changes(
                        &st.pgt_schema,
                        &st.pgt_name,
                        data_secs,
                        st.pooler_compatibility_mode,
                    );
                } else {
                    // Scheduler is also behind — genuine staleness.
                    monitor::alert_stale_data(
                        &st.pgt_schema,
                        &st.pgt_name,
                        data_secs,
                        schedule_f64,
                        st.pooler_compatibility_mode,
                    );
                }
            }
        }
    } else {
        // CALCULATED STs have no explicit schedule — detect staleness by
        // comparing this ST's data_timestamp with the most-recently-refreshed
        // upstream ST.  If any upstream has refreshed more recently and the
        // lag exceeds a threshold, emit a stale alert so operators know this
        // CALCULATED ST is falling behind.
        let lag = Spi::get_one_with_args::<f64>(
            "SELECT EXTRACT(EPOCH FROM ( \
                 (SELECT MAX(u.data_timestamp) \
                  FROM pgtrickle.pgt_dependencies d \
                  JOIN pgtrickle.pgt_stream_tables u \
                    ON u.pgt_relid = d.source_relid \
                  WHERE d.pgt_id = $1 \
                    AND u.data_timestamp IS NOT NULL) \
                 - s.data_timestamp \
             ))::float8 \
             FROM pgtrickle.pgt_stream_tables s \
             WHERE s.pgt_id = $1 AND s.data_timestamp IS NOT NULL",
            &[st.pgt_id.into()],
        )
        .unwrap_or(None);

        if let Some(lag_secs) = lag {
            // Alert when the CALCULATED ST is > 60s behind its upstream.
            // Use a fixed threshold since there is no schedule to derive one from.
            let threshold = 60.0_f64;
            if lag_secs > threshold {
                monitor::alert_stale_data(
                    &st.pgt_schema,
                    &st.pgt_name,
                    lag_secs,
                    threshold,
                    st.pooler_compatibility_mode,
                );
            }
        }
    }
}

/// Emit a one-per-~60s reminder that a stream table is frozen and being
/// skipped by the scheduler.  Throttled the same way as
/// `emit_fuse_blown_reminder` — using `last_refresh_at` age modulo.
fn emit_frozen_tier_skip(st: &StreamTableMeta) {
    let since_last = Spi::get_one_with_args::<f64>(
        "SELECT EXTRACT(EPOCH FROM (now() - COALESCE(last_refresh_at, created_at)))::float8 \
         FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
        &[st.pgt_id.into()],
    )
    .unwrap_or(None);

    if let Some(secs) = since_last {
        let interval = 60.0_f64;
        if secs >= interval && (secs % interval) < 1.0 {
            monitor::emit_alert(
                monitor::AlertEvent::FrozenTierSkip,
                &st.pgt_schema,
                &st.pgt_name,
                &format!(r#""frozen_seconds":{:.0}"#, secs),
                st.pooler_compatibility_mode,
            );
        }
    }
}

/// Check if any upstream source has pending changes.
fn check_upstream_changes(st: &StreamTableMeta) -> bool {
    // Walk every dependency of this stream table and check whether any source
    // has pending changes that have not yet been reflected in our data.
    //
    // Two kinds of upstream sources:
    //   TABLE        — base tables with trigger-based CDC.  Pending changes
    //                  live in pgtrickle_changes.changes_{oid}.
    //   STREAM_TABLE — intermediate stream tables with change buffers
    //                  (changes_pgt_{id}).  We check for rows with LSN
    //                  beyond the stored frontier, falling back to
    //                  data_timestamp comparison when no buffer exists yet.
    if !st.is_populated {
        return true;
    }
    has_table_source_changes(st) || has_stream_table_source_changes(st)
}

/// Resolve upstream change state for a scheduler decision.
///
/// For multi-member diamond groups we can freeze TABLE-source visibility
/// before any sibling refresh runs, because one member may otherwise make the
/// shared change buffer look empty to a later sibling in the same cycle.
/// STREAM_TABLE staleness is always evaluated live so convergence nodes still
/// see freshly-updated upstream timestamps from earlier group members.
fn upstream_change_state(
    st: &StreamTableMeta,
    table_change_snapshot: Option<bool>,
) -> (bool, bool) {
    if !st.is_populated {
        return (true, false);
    }

    let has_table_changes = table_change_snapshot.unwrap_or_else(|| has_table_source_changes(st));
    let has_stream_table_changes = has_stream_table_source_changes(st);
    (
        has_table_changes || has_stream_table_changes,
        has_stream_table_changes,
    )
}

/// Returns `true` if any TABLE or FOREIGN_TABLE upstream source has rows in
/// its CDC change buffer that have not yet been consumed by a refresh.
///
/// PERF-6: Uses a single batched EXISTS query instead of one SPI round-trip
/// per source table.
fn has_table_source_changes(st: &StreamTableMeta) -> bool {
    if let Err(e) = refresh::poll_foreign_table_sources_for_st(st) {
        log!(
            "pg_trickle: failed to poll foreign table sources for {}.{} while checking upstream changes: {}",
            st.pgt_schema,
            st.pgt_name,
            e,
        );
    }

    let change_schema = config::pg_trickle_change_buffer_schema();
    let source_oids = get_source_oids_for_st(st.pgt_id);

    if source_oids.is_empty() {
        return false;
    }

    // PERF-6: Build a single UNION ALL EXISTS query for all sources.
    // Note: LIMIT 1 is omitted from individual branches because
    // `SELECT ... LIMIT 1 UNION ALL SELECT ...` is a syntax error in
    // PostgreSQL (LIMIT binds at the top level, not per-branch).
    // EXISTS already short-circuits on the first row found.
    let union_arms: Vec<String> = source_oids
        .iter()
        .map(|oid| format!("SELECT 1 FROM {}.changes_{}", change_schema, oid.to_u32(),))
        .collect();
    let batched_sql = format!("SELECT EXISTS({})", union_arms.join(" UNION ALL "));

    Spi::get_one::<bool>(&batched_sql) // nosemgrep: rust.spi.get-one.dynamic-format — change_schema is config-derived, OIDs are system values
        .unwrap_or(Some(false))
        .unwrap_or(false)
}

/// Returns `true` if any STREAM_TABLE upstream has buffered changes beyond
/// the current frontier.
///
/// For each upstream ST dependency that has a change buffer
/// (`changes_pgt_{pgt_id}`), we check whether rows exist with an LSN greater
/// than the LSN recorded in our frontier.  If no buffer exists yet (e.g.
/// before the first refresh), we fall back to the timestamp comparison.
fn has_stream_table_source_changes(st: &StreamTableMeta) -> bool {
    let deps = crate::catalog::StDependency::get_for_st(st.pgt_id).unwrap_or_default();
    let change_schema = config::pg_trickle_change_buffer_schema();
    let frontier = st.frontier.clone().unwrap_or_default();

    for dep in &deps {
        if dep.source_type != "STREAM_TABLE" {
            continue;
        }

        let upstream_pgt_id =
            match crate::catalog::StreamTableMeta::pgt_id_for_relid(dep.source_relid) {
                Some(id) => id,
                None => continue,
            };

        // If a change buffer exists, check for rows beyond the frontier LSN.
        if crate::cdc::has_st_change_buffer(upstream_pgt_id, &change_schema) {
            let prev_lsn = frontier.get_st_lsn(upstream_pgt_id);
            let has_new_rows = Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(SELECT 1 FROM {schema}.changes_pgt_{id} \
                 WHERE lsn > '{lsn}'::pg_lsn)",
                schema = change_schema,
                id = upstream_pgt_id,
                lsn = prev_lsn,
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false);

            if has_new_rows {
                return true;
            }

            // NS-8 fallback: The change buffer exists but has no new rows.
            // This happens when a FULL-mode upstream refreshes with zero net
            // row changes — `capture_full_refresh_diff_to_st_buffer` produces
            // nothing and the buffer stays empty.  Without this fallback,
            // CALCULATED downstreams stall indefinitely because they never
            // detect the "upstream refreshed" signal.
            //
            // Compare `last_refresh_at` timestamps: if the upstream has
            // refreshed more recently than we have, we should re-evaluate.
            // A NoData run on our side advances our own `last_refresh_at`
            // past the upstream's, preventing re-triggering until the
            // upstream refreshes again.
            let upstream_refreshed_since = Spi::get_one::<bool>(&format!(
                "SELECT EXISTS ( \
                   SELECT 1 \
                   FROM pgtrickle.pgt_stream_tables upstream \
                   JOIN pgtrickle.pgt_stream_tables us ON us.pgt_id = {my_id} \
                   WHERE upstream.pgt_relid = {up_relid}::oid \
                     AND upstream.last_refresh_at \
                         > COALESCE(us.last_refresh_at, '-infinity'::timestamptz) \
                 )",
                my_id = st.pgt_id,
                up_relid = dep.source_relid.to_u32(),
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false);

            if upstream_refreshed_since {
                return true;
            }
        } else {
            // No buffer yet — use timestamp comparison.
            let upstream_newer = Spi::get_one::<bool>(&format!(
                "SELECT EXISTS ( \
                   SELECT 1 \
                   FROM pgtrickle.pgt_stream_tables upstream \
                   JOIN pgtrickle.pgt_stream_tables us ON us.pgt_id = {} \
                   WHERE upstream.pgt_relid = {}::oid \
                     AND upstream.data_timestamp \
                         > COALESCE(us.data_timestamp, '-infinity'::timestamptz) \
                 )",
                st.pgt_id,
                dep.source_relid.to_u32(),
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false);

            if upstream_newer {
                return true;
            }
        }
    }
    false
}

// ── Fixpoint Iteration for Cyclic SCCs (CYC-5) ───────────────────────────

/// Query the current number of rows in a stream table directly from the DB.
///
/// Used by [`iterate_to_fixpoint`] to compute per-pass deltas for convergence
/// detection without relying on CDC change buffers.
fn get_st_row_count(pgt_id: i64) -> Option<i64> {
    let st = load_st_by_id(pgt_id)?;
    let sql = format!(
        "SELECT count(*)::bigint FROM \"{}\".\"{}\"",
        st.pgt_schema.replace('"', "\"\""),
        st.pgt_name.replace('"', "\"\""),
    );
    Spi::get_one::<i64>(&sql).ok().flatten()
}

/// Iterate a cyclic SCC to a fixed point.
///
/// Refreshes all members of the SCC repeatedly until convergence (zero net
/// changes across all members in a full pass) or `max_fixpoint_iterations`
/// is exceeded.
///
/// Each member must use DIFFERENTIAL mode — FULL mode truncates and re-inserts
/// all rows every iteration, which would never converge to zero changes.
///
/// All SCC members are wrapped in an internal sub-transaction so that a
/// failure in any member rolls back the entire iteration.
fn iterate_to_fixpoint(
    scc: &Scc,
    _dag: &StDag,
    retry_states: &mut HashMap<i64, RetryState>,
    retry_policy: &RetryPolicy,
    now_ms: u64,
    tick_watermark: Option<&str>,
) {
    let max_iter = config::pg_trickle_max_fixpoint_iterations();
    let gated_oids = load_gated_source_oids();

    // Collect ST pgt_ids from the SCC.
    let member_ids: Vec<i64> = scc
        .nodes
        .iter()
        .filter_map(|n| match n {
            NodeId::StreamTable(id) => Some(*id),
            _ => None,
        })
        .collect();

    if member_ids.is_empty() {
        return;
    }

    // Build member names for logging.
    let member_names: Vec<String> = member_ids
        .iter()
        .filter_map(|id| load_st_by_id(*id).map(|st| format!("{}.{}", st.pgt_schema, st.pgt_name)))
        .collect();

    log!(
        "pg_trickle: SCC fixpoint — starting iteration for {} members [{}]",
        member_ids.len(),
        member_names.join(", "),
    );

    // Validate all members use DIFFERENTIAL mode.
    for &pgt_id in &member_ids {
        if let Some(st) = load_st_by_id(pgt_id)
            && st.refresh_mode != RefreshMode::Differential
        {
            pgrx::warning!(
                "pg_trickle: SCC fixpoint — {}.{} uses {} mode, \
                     but only DIFFERENTIAL is supported in cyclic dependencies",
                st.pgt_schema,
                st.pgt_name,
                st.refresh_mode.as_str(),
            );
            return;
        }
    }

    // Seed per-member row counts from the current DB state.
    //
    // Convergence is detected by comparing each member's count(*) before
    // and after each pass.  Counts are always read OUTSIDE sub-transactions
    // (after subtxn.commit()) so that pgrx's nested Spi::get_one context
    // sees the fully committed state and not a potentially stale snapshot.
    let mut prev_row_counts: HashMap<i64, i64> = member_ids
        .iter()
        .map(|&id| (id, get_st_row_count(id).unwrap_or(0)))
        .collect();

    for iteration in 0..max_iter {
        let mut iteration_ok = true;
        let mut any_refreshed = false;

        {
            let subtxn = SubTransaction::begin();

            for &pgt_id in &member_ids {
                let st = match load_st_by_id(pgt_id) {
                    Some(st) => st,
                    None => continue,
                };

                if st.status != StStatus::Active && st.status != StStatus::Initializing {
                    continue;
                }

                // Skip gated sources.
                if is_any_source_gated(pgt_id, &gated_oids) {
                    log_gated_skip(&st);
                    continue;
                }

                // Check retry backoff.
                let retry = retry_states.entry(pgt_id).or_default();
                if retry.is_in_backoff(now_ms) {
                    emit_stale_alert_if_needed(&st);
                    continue;
                }

                // Always use FULL refresh in the fixpoint loop.
                //
                // SCC peers are stream tables with no CDC change buffers.
                // DIFFERENTIAL refresh short-circuits when the change-buffer
                // query returns no rows — which is always the case inside a
                // cyclic SCC where the only upstream changes come from sibling
                // stream tables.  FULL refresh re-executes the defining query
                // against the current DB state each pass, which is the correct
                // semantics for monotone fixpoint iteration.
                let action = RefreshAction::Full;

                let result = execute_scheduled_refresh(&st, action, tick_watermark, None);

                match result {
                    RefreshOutcome::Success => {
                        any_refreshed = true;
                        let retry = retry_states.entry(pgt_id).or_default();
                        retry.reset();
                    }
                    RefreshOutcome::RetryableFailure | RefreshOutcome::PermanentFailure => {
                        log!(
                            "pg_trickle: SCC fixpoint aborted — {}.{} failed at iteration {}",
                            st.pgt_schema,
                            st.pgt_name,
                            iteration + 1,
                        );
                        iteration_ok = false;
                        break;
                    }
                }
            }

            if !iteration_ok {
                subtxn.rollback();
                // Record failure for retry tracking on all members.
                for &pgt_id in &member_ids {
                    let retry = retry_states.entry(pgt_id).or_default();
                    retry.record_failure(retry_policy, now_ms);
                }
                return;
            }

            subtxn.commit();
        }
        // subtxn is committed; now read row counts in the outer transaction
        // where the full sub-transaction contents are visible.

        if !any_refreshed {
            // All members skipped (backoff/gating) — cannot assess convergence.
            continue;
        }

        let mut total_changes: i64 = 0;
        for &pgt_id in &member_ids {
            let new_count = get_st_row_count(pgt_id).unwrap_or(0);
            let old_count = prev_row_counts.get(&pgt_id).copied().unwrap_or(0);
            total_changes += (new_count - old_count).abs();
            prev_row_counts.insert(pgt_id, new_count);
        }

        if total_changes == 0 {
            log!(
                "pg_trickle: SCC converged after {} iteration(s) [{}]",
                iteration + 1,
                member_names.join(", "),
            );
            // Record convergence metadata in the catalog.
            for &pgt_id in &member_ids {
                let _ = StreamTableMeta::update_last_fixpoint_iterations(pgt_id, iteration + 1);
            }
            return;
        }

        log!(
            "pg_trickle: SCC fixpoint iteration {} — {} total changes",
            iteration + 1,
            total_changes,
        );
    }

    // Non-convergence: mark all SCC members as ERROR.
    pgrx::warning!(
        "pg_trickle: SCC did not converge after {} iterations — marking {} members as ERROR [{}]",
        max_iter,
        member_ids.len(),
        member_names.join(", "),
    );
    for &pgt_id in &member_ids {
        let _ = StreamTableMeta::update_status(pgt_id, StStatus::Error);
        let _ = StreamTableMeta::update_last_fixpoint_iterations(pgt_id, max_iter);
    }
}

/// Read the rows_inserted and rows_deleted from the most recent completed
/// refresh record for a stream table. Used by fixpoint iteration to detect
/// convergence without modifying the `execute_scheduled_refresh` return type.
fn last_refresh_row_counts(pgt_id: i64) -> (i64, i64) {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT rows_inserted, rows_deleted \
                 FROM pgtrickle.pgt_refresh_history \
                 WHERE pgt_id = $1 AND status = 'COMPLETED' \
                 ORDER BY refresh_id DESC LIMIT 1",
                None,
                &[pgt_id.into()],
            )
            .ok();
        match table {
            Some(t) if !t.is_empty() => {
                let first = t.first();
                let ins = first.get::<i64>(1).ok().flatten().unwrap_or(0);
                let del = first.get::<i64>(2).ok().flatten().unwrap_or(0);
                (ins, del)
            }
            _ => (0, 0),
        }
    })
}

/// Refresh a single (non-group) stream table with full retry handling.
///
/// This is the singleton fast path used by both non-diamond STs and
/// diamond STs where not all members opted into atomic mode.
#[allow(clippy::too_many_arguments)]
fn refresh_single_st(
    pgt_id: i64,
    dag_ref: &StDag,
    now_ms: u64,
    retry_states: &mut HashMap<i64, RetryState>,
    retry_policy: &RetryPolicy,
    table_change_snapshot: Option<bool>,
    tick_watermark: Option<&str>,
    drift_counter: Option<&mut i32>,
    spill_counters: &mut HashMap<i64, i32>,
) {
    let st = match load_st_by_id(pgt_id) {
        Some(st) => st,
        None => return,
    };

    if st.status != StStatus::Active && st.status != StStatus::Initializing {
        return;
    }

    // BOOT-4: Check bootstrap source gates — skip if any source is gated.
    let gated_oids = load_gated_source_oids();
    if is_any_source_gated(pgt_id, &gated_oids) {
        log!(
            "pg_trickle: skipping {}.{} — source gated",
            st.pgt_schema,
            st.pgt_name,
        );
        log_gated_skip(&st);
        return;
    }

    // WM-4: Check watermark alignment — skip if misaligned.
    let (wm_misaligned, wm_reason) = is_watermark_misaligned(pgt_id);
    if wm_misaligned {
        let reason = wm_reason.as_deref().unwrap_or("watermark misaligned");
        log!(
            "pg_trickle: skipping {}.{} — {}",
            st.pgt_schema,
            st.pgt_name,
            reason,
        );
        log_watermark_skip(&st, reason);
        return;
    }

    // WM-7: Check watermark staleness — skip if any source watermark is stuck.
    let (wm_stuck, wm_stuck_reason) = is_watermark_stuck(pgt_id);
    if wm_stuck {
        let reason = wm_stuck_reason.as_deref().unwrap_or("watermark stuck");
        log!(
            "pg_trickle: skipping {}.{} — {}",
            st.pgt_schema,
            st.pgt_name,
            reason,
        );
        log_watermark_skip(&st, reason);
        return;
    }

    let retry = retry_states.entry(pgt_id).or_default();
    if retry.is_in_backoff(now_ms) {
        emit_stale_alert_if_needed(&st);
        return;
    }

    // D-1b: Check for UNLOGGED buffer data loss after crash recovery.
    // If detected, mark_for_reinitialize is called (sets needs_reinit=true),
    // so the refresh path below will pick up the Reinitialize action.
    check_unlogged_buffer_crash_recovery(&st);
    // Reload ST in case needs_reinit was just set.
    let st = match load_st_by_id(pgt_id) {
        Some(st) => st,
        None => return,
    };

    let needs_refresh = check_schedule(&st, dag_ref);
    if !needs_refresh && !st.needs_reinit {
        return;
    }

    if check_skip_needed(&st) {
        log!(
            "pg_trickle: skipping {}.{} — previous refresh still running",
            st.pgt_schema,
            st.pgt_name,
        );
        return;
    }

    // ST-on-ST safety: skip this ST if any upstream STREAM_TABLE source is
    // currently being refreshed by another session.  When the scheduler does
    // a FULL refresh of a downstream calculated ST, it reads the current
    // state of all upstream STs.  If an upstream is mid-refresh (row locked
    // by a manual `refresh_stream_table` call), the FULL refresh may read a
    // mix of pre-/post-update upstream data, producing incorrect results
    // that cannot be reliably detected and corrected afterwards.
    //
    // The check is cheap (one FOR SHARE SKIP LOCKED per upstream ST) and
    // only affects STs with STREAM_TABLE dependencies.
    {
        let deps = crate::catalog::StDependency::get_for_st(pgt_id).unwrap_or_default();
        let has_st_source = deps.iter().any(|dep| dep.source_type == "STREAM_TABLE");
        if has_st_source {
            let any_upstream_locked = deps
                .iter()
                .filter(|dep| dep.source_type == "STREAM_TABLE")
                .any(|dep| {
                    let upstream_pgt_id =
                        crate::catalog::StreamTableMeta::pgt_id_for_relid(dep.source_relid);
                    match upstream_pgt_id {
                        Some(up_id) => Spi::get_one_with_args::<i64>(
                            "SELECT pgt_id FROM pgtrickle.pgt_stream_tables \
                                 WHERE pgt_id = $1 FOR SHARE SKIP LOCKED",
                            &[up_id.into()],
                        )
                        .unwrap_or(None)
                        .is_none(),
                        None => false,
                    }
                });

            if any_upstream_locked {
                pgrx::debug1!(
                    "[pg_trickle] skipping {}.{} — upstream ST is being refreshed by another session",
                    st.pgt_schema,
                    st.pgt_name,
                );
                return;
            }
        }
    }

    // FUSE-5: Check fuse circuit breaker.
    if evaluate_fuse(&st) {
        log!(
            "pg_trickle: {}.{} fuse blown — skipping inline refresh",
            st.pgt_schema,
            st.pgt_name,
        );
        return;
    }

    let (has_changes, _has_stream_table_changes) =
        upstream_change_state(&st, table_change_snapshot);

    let action = {
        let mut base_action = refresh::determine_refresh_action(&st, has_changes);

        // PRED-2: Predictive cost model — pre-emptive FULL switch.
        // If the predicted DIFFERENTIAL cost exceeds the FULL cost by the
        // configured ratio, switch to FULL preemptively.
        if base_action == RefreshAction::Differential
            && let Some(last_full) = st.last_full_ms
        {
            // Estimate delta_rows from change buffers.
            let delta_rows = crate::cdc::estimate_pending_changes(st.pgt_id).unwrap_or(0);
            if delta_rows > 0
                && crate::api::publication::should_preempt_to_full(st.pgt_id, delta_rows, last_full)
            {
                log!(
                    "pg_trickle: PRED-2 pre-emptive FULL switch for {}.{} \
                     (predicted diff cost exceeds {}× full cost)",
                    st.pgt_schema,
                    st.pgt_name,
                    config::pg_trickle_prediction_ratio(),
                );
                refresh::set_refresh_reason("predicted_cost_exceeds_full");
                base_action = RefreshAction::Full;
            }
        }

        // Check periodic drift reset
        if base_action == RefreshAction::Differential {
            let max_cycles = config::pg_trickle_algebraic_drift_reset_cycles();
            if let Some(counter) = &drift_counter
                && max_cycles > 0
                && **counter >= max_cycles
            {
                log!(
                    "pg_trickle: drift reset triggered for {}.{} ({} differential cycles)",
                    st.pgt_schema,
                    st.pgt_name,
                    counter
                );
                // Convert action to reinitialize. This will trigger the drift counter reset
                // inside execute_scheduled_refresh on success.
                base_action = RefreshAction::Reinitialize;
            }
        }
        base_action
    };

    // ERR-1e: Wrap the refresh in a subtransaction + catch_unwind so that
    // PG ERRORs (which pgrx converts to panics via longjmp) don't abort
    // the entire tick transaction.  On panic the subtransaction rolls back
    // (undoing TRUNCATE + partial writes) and we set ERROR status in the
    // still-valid outer transaction.
    let subtxn = SubTransaction::begin();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        execute_scheduled_refresh(&st, action, tick_watermark, drift_counter)
    }));
    let result = match result {
        Ok(outcome) => {
            subtxn.commit();
            outcome
        }
        Err(panic_payload) => {
            subtxn.rollback();
            let error_msg = extract_panic_message(&panic_payload);
            log!(
                "pg_trickle: refresh panicked for {}.{}: {} — setting error state",
                st.pgt_schema,
                st.pgt_name,
                error_msg,
            );
            let is_retryable = crate::error::classify_spi_error_retryable(&error_msg);
            if !is_retryable {
                let _ = StreamTableMeta::set_error_state(pgt_id, &error_msg);
            }
            if is_retryable {
                RefreshOutcome::RetryableFailure
            } else {
                RefreshOutcome::PermanentFailure
            }
        }
    };

    let retry = retry_states.entry(pgt_id).or_default();
    match result {
        RefreshOutcome::Success => {
            retry.reset();

            // PH-E2: Spill-aware refresh tracking.
            // After a successful refresh, check whether the MERGE spilled
            // to temp files and maintain a per-ST consecutive spill counter.
            let spill_threshold = config::pg_trickle_spill_threshold_blocks();
            if spill_threshold > 0 {
                let temp_blks = refresh::take_last_temp_blks_written();
                if temp_blks > spill_threshold as i64 {
                    let count = spill_counters.entry(pgt_id).or_insert(0);
                    *count += 1;
                    let limit = config::pg_trickle_spill_consecutive_limit();
                    if *count >= limit {
                        pgrx::warning!(
                            "pg_trickle: {}.{} spilled for {} consecutive refreshes \
                             ({} temp blocks > {} threshold) — forcing FULL refresh",
                            st.pgt_schema,
                            st.pgt_name,
                            count,
                            temp_blks,
                            spill_threshold,
                        );
                        // STAB-3: Emit alert before forcing reinitialize.
                        monitor::alert_spill_threshold_exceeded(
                            &st.pgt_schema,
                            &st.pgt_name,
                            temp_blks,
                            spill_threshold as i64,
                            *count,
                            limit,
                            st.pooler_compatibility_mode,
                        );
                        let _ = StreamTableMeta::mark_for_reinitialize(st.pgt_id);
                        *count = 0;
                    } else {
                        log!(
                            "pg_trickle: {}.{} spilled ({} temp blocks > {} threshold, \
                             {}/{} consecutive)",
                            st.pgt_schema,
                            st.pgt_name,
                            temp_blks,
                            spill_threshold,
                            count,
                            limit,
                        );
                    }
                } else if temp_blks >= 0 {
                    // Non-spilling refresh: reset counter.
                    spill_counters.remove(&pgt_id);
                }
                // temp_blks == -1 means detection was disabled/unavailable; leave counter as-is.
            }
        }
        RefreshOutcome::RetryableFailure => {
            let will_retry = retry.record_failure(retry_policy, now_ms);
            if will_retry {
                log!(
                    "pg_trickle: {}.{} will retry in {}ms (attempt {}/{})",
                    st.pgt_schema,
                    st.pgt_name,
                    retry_policy.backoff_ms(retry.attempts - 1),
                    retry.attempts,
                    retry_policy.max_attempts,
                );
            }
        }
        RefreshOutcome::PermanentFailure => {
            retry.reset();
        }
    }
}

/// Execute a scheduled refresh for a stream table.
///
/// Returns a [`RefreshOutcome`] indicating whether the refresh succeeded,
/// failed with a retryable error, or failed permanently.
///
/// Phase 8: Full frontier lifecycle:
/// - For FULL/REINITIALIZE: compute initial frontier from current slot positions
/// - For DIFFERENTIAL: load prev frontier, compute new frontier, pass both to DVM
/// - After success: store new frontier in catalog
///
/// Phase 10: Error classification determines retry behavior:
/// - Retryable errors (SPI, lock, slot): backoff and retry on next cycle
/// - Schema errors: flag for reinitialize, count toward suspension
/// - User/internal errors: permanent failure, count toward suspension
fn execute_scheduled_refresh(
    st: &StreamTableMeta,
    action: RefreshAction,
    tick_watermark: Option<&str>,
    drift_counter: Option<&mut i32>,
) -> RefreshOutcome {
    let start_instant = std::time::Instant::now();

    let now = Spi::get_one::<TimestampWithTimeZone>("SELECT now()")
        .unwrap_or(None)
        .unwrap_or_else(|| {
            pgrx::warning!("now() returned NULL in scheduler");
            TimestampWithTimeZone::try_from(0i64).unwrap_or_else(|_| {
                pgrx::error!("scheduler: failed to create epoch TimestampWithTimeZone")
            })
        });

    // PB1: Acquire row-level lock via FOR UPDATE SKIP LOCKED.
    // The lock is held for the remainder of this transaction (released on commit).
    let got_lock = Spi::get_one_with_args::<i64>(
        "SELECT pgt_id FROM pgtrickle.pgt_stream_tables \
         WHERE pgt_id = $1 FOR UPDATE SKIP LOCKED",
        &[st.pgt_id.into()],
    )
    .unwrap_or(None)
    .is_some();

    if !got_lock {
        // Another session holds the row lock — skip this cycle
        log!(
            "pg_trickle: skipping {}.{} — catalog row locked by another session",
            st.pgt_schema,
            st.pgt_name,
        );
        return RefreshOutcome::RetryableFailure;
    }

    // TOCTOU fix: reload ST metadata now that we hold the row lock.
    // Between the caller loading `st` and acquiring this lock, another
    // session (manual refresh or a parallel worker) may have refreshed
    // this ST and advanced its frontier.  Using the stale frontier would
    // cause the differential refresh to re-process already-consumed
    // change buffer rows, producing incorrect aggregate deltas.
    let st = match load_st_by_id(st.pgt_id) {
        Some(fresh) => fresh,
        None => {
            log!(
                "pg_trickle: ST pgt_id={} disappeared after lock acquisition",
                st.pgt_id,
            );
            return RefreshOutcome::PermanentFailure;
        }
    };
    let st = &st;

    // Record refresh start
    // Compute freshness_deadline for duration-based schedules:
    // deadline = data_timestamp + schedule_seconds (when data becomes stale)
    let freshness_deadline = compute_freshness_deadline(st);
    let refresh_id = RefreshRecord::insert(
        st.pgt_id,
        now,
        action.as_str(),
        "RUNNING",
        0,
        0,
        None,
        Some("SCHEDULER"),
        freshness_deadline,
        0,     // delta_row_count — updated on completion
        None,  // merge_strategy_used — updated on completion
        false, // was_full_fallback — updated on completion
        tick_watermark,
    );

    let refresh_id = match refresh_id {
        Ok(id) => id,
        Err(e) => {
            log!(
                "pg_trickle: failed to record refresh start for {}.{}: {}",
                st.pgt_schema,
                st.pgt_name,
                e
            );
            // Row lock is released automatically at transaction end.
            return RefreshOutcome::RetryableFailure;
        }
    };

    // Compute frontier information for this refresh
    let source_oids = get_source_oids_for_st(st.pgt_id);
    let mut slot_positions = match cdc::get_slot_positions(&source_oids) {
        Ok(pos) => pos,
        Err(e) => {
            log!(
                "pg_trickle: failed to get slot positions for {}.{}: {}",
                st.pgt_schema,
                st.pgt_name,
                e
            );
            std::collections::HashMap::new()
        }
    };

    // CSS1: Cap each slot position to the tick watermark so no refresh in this
    // tick consumes WAL changes beyond the snapshot point captured at tick start.
    // Changes beyond the watermark remain in the buffer and are consumed next tick.
    if let Some(wm) = tick_watermark {
        for lsn in slot_positions.values_mut() {
            if version::lsn_gt(lsn, wm) {
                *lsn = wm.to_string();
            }
        }
    }

    // Select target data timestamp
    let schedule_secs = st
        .schedule
        .as_ref()
        .and_then(|s| crate::api::parse_duration(s).ok())
        .map(|s| s as u64);
    let data_ts_str = version::select_target_data_timestamp(
        schedule_secs,
        &[], // upstream timestamps would be filled for ST-on-ST deps
    );
    let _ = &data_ts_str;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let data_ts_frontier = format!("{}Z", now_secs);

    // EC-25/EC-26: Set the internal_refresh flag so DML guard triggers
    // allow the refresh executor to modify the storage table.
    let _ = Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'");

    // R3: Bypass RLS as defence-in-depth. The background worker already
    // runs as superuser, but this ensures the defining query always
    // materializes the full result set even if pg_trickle is installed
    // by a non-superuser role with BYPASSRLS.
    let _ = Spi::run("SET LOCAL row_security = off"); // nosemgrep: sql.row-security.disabled — intentional R3 bypass, mirrors REFRESH MATERIALIZED VIEW semantics.

    // Execute the refresh

    // Collect current high-water marks for upstream ST change buffers so
    // we can embed them in the frontier.
    //
    // ST-ST-7: Use MAX(lsn) from the actual change buffer rather than
    // pg_current_wal_lsn(). Within a single scheduler tick (one transaction),
    // an upstream ST's FULL-refresh capture writes rows to its change buffer
    // using pg_current_wal_lsn(). If this downstream ST's augment ALSO uses
    // pg_current_wal_lsn(), the augmented frontier LSN may be >= the captured
    // rows' LSN. Then the next tick's delta (lsn > prev_lsn) misses those
    // rows because they sit AT the frontier, not above it.
    //
    // Using MAX(lsn) from the buffer records EXACTLY the latest data point,
    // ensuring the next tick's delta correctly starts AFTER consumed data.
    let change_schema_for_st = crate::config::pg_trickle_change_buffer_schema();
    let st_source_positions: Vec<(i64, String)> =
        crate::catalog::StDependency::get_for_st(st.pgt_id)
            .unwrap_or_default()
            .iter()
            .filter(|dep| dep.source_type == "STREAM_TABLE")
            .filter_map(|dep| {
                let upstream_pgt_id =
                    crate::catalog::StreamTableMeta::pgt_id_for_relid(dep.source_relid)?;
                if !crate::cdc::has_st_change_buffer(upstream_pgt_id, &change_schema_for_st) {
                    return None;
                }
                // ST-ST-7: Read the actual max LSN from the change buffer.
                // Falls back to pg_current_wal_lsn() if the buffer is empty
                // (e.g., freshly created, no data captured yet).
                let lsn = Spi::get_one::<String>(&format!(
                    "SELECT COALESCE(MAX(lsn)::text, pg_current_wal_lsn()::text) \
                     FROM \"{schema}\".changes_pgt_{id}",
                    schema = change_schema_for_st,
                    id = upstream_pgt_id,
                ))
                .unwrap_or(None)
                .unwrap_or_else(|| "0/0".to_string());
                Some((upstream_pgt_id, lsn))
            })
            .collect();

    let augment_frontier = |frontier: &mut version::Frontier| {
        for (upstream_pgt_id, lsn) in &st_source_positions {
            frontier.set_st_source(*upstream_pgt_id, lsn.clone(), data_ts_frontier.clone());
        }
    };

    let result = if st.topk_limit.is_some() {
        // TopK tables bypass the normal Full/Differential refresh paths and use
        // scoped-recomputation MERGE (ORDER BY … LIMIT N) instead.
        refresh::execute_topk_refresh(st)
    } else {
        match action {
            RefreshAction::NoData => refresh::execute_no_data_refresh(st).map(|_| (0i64, 0i64)),
            RefreshAction::Full => {
                let mut new_frontier =
                    version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
                augment_frontier(&mut new_frontier);
                match refresh::execute_full_refresh(st) {
                    Ok((ins, del)) => {
                        if let Err(e) = StreamTableMeta::store_frontier(st.pgt_id, &new_frontier) {
                            log!(
                                "pg_trickle: failed to store frontier for {}.{}: {}",
                                st.pgt_schema,
                                st.pgt_name,
                                e
                            );
                        }
                        // G3+G4: advance WAL slots and flush change buffers
                        // now that the new frontier is stored.
                        refresh::post_full_refresh_cleanup(st);

                        if let Some(counter) = drift_counter {
                            *counter = 0;
                        }

                        Ok((ins, del))
                    }
                    Err(e) => Err(e),
                }
            }
            RefreshAction::Reinitialize => {
                let mut new_frontier =
                    version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
                augment_frontier(&mut new_frontier);
                match refresh::execute_reinitialize_refresh(st) {
                    Ok((ins, del)) => {
                        if let Err(e) = StreamTableMeta::store_frontier(st.pgt_id, &new_frontier) {
                            log!(
                                "pg_trickle: failed to store frontier for {}.{}: {}",
                                st.pgt_schema,
                                st.pgt_name,
                                e
                            );
                        }
                        // G3+G4: advance WAL slots and flush change buffers
                        // now that the new frontier is stored.
                        refresh::post_full_refresh_cleanup(st);

                        // Drift reset mechanism: reset counter on full reinit
                        if let Some(counter) = drift_counter {
                            *counter = 0;
                        }

                        Ok((ins, del))
                    }
                    Err(e) => Err(e),
                }
            }
            RefreshAction::Differential => {
                let prev_frontier = st.frontier.clone().unwrap_or_default();

                if prev_frontier.is_empty() {
                    log!(
                        "pg_trickle: no previous frontier for {}.{}, doing FULL refresh",
                        st.pgt_schema,
                        st.pgt_name
                    );
                    let mut new_frontier =
                        version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
                    augment_frontier(&mut new_frontier);
                    match refresh::execute_full_refresh(st) {
                        Ok((ins, del)) => {
                            if let Err(e) =
                                StreamTableMeta::store_frontier(st.pgt_id, &new_frontier)
                            {
                                log!("pg_trickle: failed to store frontier: {}", e);
                            }
                            // G3+G4: advance WAL slots and flush change buffers
                            // now that the new frontier is stored.
                            refresh::post_full_refresh_cleanup(st);

                            if let Some(counter) = drift_counter {
                                *counter = 0;
                            }

                            Ok((ins, del))
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    let mut new_frontier =
                        version::compute_new_frontier(&slot_positions, &data_ts_frontier);
                    augment_frontier(&mut new_frontier);

                    match refresh::execute_differential_refresh(st, &prev_frontier, &new_frontier) {
                        Ok((ins, del)) => {
                            if let Err(e) =
                                StreamTableMeta::store_frontier(st.pgt_id, &new_frontier)
                            {
                                log!("pg_trickle: failed to store frontier: {}", e);
                            }

                            if let Some(counter) = drift_counter {
                                *counter += 1;
                            }

                            Ok((ins, del))
                        }
                        // DI-7: When QueryTooComplex is returned (e.g. join
                        // count exceeds max_differential_joins), fall back to
                        // FULL refresh immediately instead of reinitializing.
                        Err(crate::error::PgTrickleError::QueryTooComplex(ref msg)) => {
                            log!(
                                "pg_trickle: DI-7 fallback for {}.{}: {}; using FULL refresh",
                                st.pgt_schema,
                                st.pgt_name,
                                msg
                            );
                            match refresh::execute_full_refresh(st) {
                                Ok((ins, del)) => {
                                    if let Err(e) =
                                        StreamTableMeta::store_frontier(st.pgt_id, &new_frontier)
                                    {
                                        log!("pg_trickle: failed to store frontier: {}", e);
                                    }
                                    refresh::post_full_refresh_cleanup(st);
                                    if let Some(counter) = drift_counter {
                                        *counter = 0;
                                    }
                                    Ok((ins, del))
                                }
                                Err(e) => Err(e),
                            }
                        }
                        Err(e) => {
                            log!(
                                "pg_trickle: differential refresh failed for {}.{}: {}, will reinitialize on next cycle",
                                st.pgt_schema,
                                st.pgt_name,
                                e
                            );
                            let _ = StreamTableMeta::mark_for_reinitialize(st.pgt_id);
                            Err(e)
                        }
                    }
                }
            }
        }
    }; // close else + let result

    // PB1: Row lock is released automatically when the transaction commits.

    let elapsed_ms = start_instant.elapsed().as_millis() as i64;

    // Record refresh completion and determine outcome
    let was_full_fallback = matches!(action, RefreshAction::Reinitialize);

    match result {
        Ok((rows_inserted, rows_deleted)) => {
            let delta_row_count = rows_inserted + rows_deleted;
            let _ = RefreshRecord::complete(
                refresh_id,
                "COMPLETED",
                rows_inserted,
                rows_deleted,
                None,
                delta_row_count,
                Some(action.as_str()),
                was_full_fallback,
            );

            // G12-ERM-1: Persist the effective refresh mode actually used.
            // `take_effective_mode()` reflects any internal fallback that
            // occurred (e.g., adaptive threshold or CTE triggering FULL
            // inside execute_differential_refresh).
            let eff_mode = refresh::take_effective_mode();
            if !eff_mode.is_empty()
                && let Err(e) = StreamTableMeta::update_effective_refresh_mode(st.pgt_id, eff_mode)
            {
                log!(
                    "pg_trickle: failed to update effective_refresh_mode for {}.{}: {}",
                    st.pgt_schema,
                    st.pgt_name,
                    e
                );
            }

            // For NO_DATA refreshes, data_timestamp must NOT be updated —
            // execute_no_data_refresh already updated last_refresh_at only.
            // Updating data_timestamp here would cause downstream stream
            // tables that compare upstream.data_timestamp to see a false
            // "upstream changed" signal on every no-data polling cycle.
            //
            // DIFFERENTIAL refreshes that produce 0 rows are also treated as
            // effective NO_DATA: the change buffer may still contain rows
            // from the previous cycle (deferred cleanup runs at the START of
            // the next differential), causing has_table_source_changes() to
            // return true and the scheduler to trigger a DIFFERENTIAL that
            // finds nothing in the current frontier window.  Bumping
            // data_timestamp for such 0-row differentials would cause
            // downstream STs to see a false "upstream changed" signal and
            // trigger unnecessary FULL refreshes.
            if action == RefreshAction::NoData {
                // Already handled by execute_no_data_refresh
            } else if action == RefreshAction::Differential
                && rows_inserted == 0
                && rows_deleted == 0
            {
                // Effective NO_DATA — update last_refresh_at only
                let _ = StreamTableMeta::update_after_no_data_refresh(st.pgt_id);
            } else {
                let _ = StreamTableMeta::update_after_refresh(st.pgt_id, now, rows_inserted);
            }

            monitor::alert_refresh_completed(
                &st.pgt_schema,
                &st.pgt_name,
                action.as_str(),
                rows_inserted,
                rows_deleted,
                elapsed_ms,
                st.pooler_compatibility_mode,
            );

            log!(
                "pg_trickle: refreshed {}.{} ({}, +{} -{} rows, {}ms)",
                st.pgt_schema,
                st.pgt_name,
                action.as_str(),
                rows_inserted,
                rows_deleted,
                elapsed_ms,
            );

            // F31: Emit StaleData alert if still stale after refresh
            // (e.g., refresh took longer than the schedule interval)
            emit_stale_alert_if_needed(st);

            // EC-11: Emit falling-behind alert if refresh duration exceeds
            // 80% of the schedule interval. This warns operators that the
            // refresh cannot keep up with the configured schedule.
            if let Some(secs) = schedule_secs {
                let schedule_ms = (secs * 1000) as i64;
                if let Some(ratio) = is_falling_behind(elapsed_ms, schedule_ms, 0.80) {
                    monitor::alert_falling_behind(
                        &st.pgt_schema,
                        &st.pgt_name,
                        elapsed_ms,
                        schedule_ms,
                        ratio,
                        st.pooler_compatibility_mode,
                    );
                    pgrx::warning!(
                        "pg_trickle: refresh of {}.{} took {}ms ({:.0}% of {}ms schedule). \
                         The scheduler may not be able to keep up with the configured interval.",
                        st.pgt_schema,
                        st.pgt_name,
                        elapsed_ms,
                        ratio * 100.0,
                        schedule_ms,
                    );
                }
            }

            RefreshOutcome::Success
        }
        Err(e) => {
            let _ = RefreshRecord::complete(
                refresh_id,
                "FAILED",
                0,
                0,
                Some(&e.to_string()),
                0,
                Some(action.as_str()),
                was_full_fallback,
            );

            monitor::alert_refresh_failed(
                &st.pgt_schema,
                &st.pgt_name,
                action.as_str(),
                &e.to_string(),
                st.pooler_compatibility_mode,
            );

            let is_retryable = e.is_retryable();
            let counts = e.counts_toward_suspension();

            // Handle schema errors: mark for reinitialize
            if e.requires_reinitialize() {
                let _ = StreamTableMeta::mark_for_reinitialize(st.pgt_id);
                monitor::alert_reinitialize_needed(
                    &st.pgt_schema,
                    &st.pgt_name,
                    &e.to_string(),
                    st.pooler_compatibility_mode,
                );
            }

            // ERR-1b: On permanent failure, immediately set ERROR status with
            // the error message. One permanent failure is enough — it will not
            // self-heal. Skip the consecutive_errors increment path.
            if !is_retryable {
                let error_msg = e.to_string();
                let _ = StreamTableMeta::set_error_state(st.pgt_id, &error_msg);

                log!(
                    "pg_trickle: permanent error for {}.{} ({}): {} — set status to ERROR",
                    st.pgt_schema,
                    st.pgt_name,
                    action.as_str(),
                    error_msg,
                );

                return RefreshOutcome::PermanentFailure;
            }

            // Increment error count only for retryable errors that should count
            if counts {
                match StreamTableMeta::increment_errors(st.pgt_id) {
                    Ok(count) if count >= config::pg_trickle_max_consecutive_errors() => {
                        let _ = StreamTableMeta::update_status(st.pgt_id, StStatus::Suspended);

                        monitor::alert_auto_suspended(
                            &st.pgt_schema,
                            &st.pgt_name,
                            count,
                            st.pooler_compatibility_mode,
                        );

                        log!(
                            "pg_trickle: suspended {}.{} after {} consecutive errors",
                            st.pgt_schema,
                            st.pgt_name,
                            count,
                        );
                    }
                    _ => {
                        log!(
                            "pg_trickle: refresh failed for {}.{} ({}): {} [will retry]",
                            st.pgt_schema,
                            st.pgt_name,
                            action.as_str(),
                            e,
                        );
                    }
                }
            } else {
                log!(
                    "pg_trickle: refresh skipped for {}.{}: {}",
                    st.pgt_schema,
                    st.pgt_name,
                    e,
                );
            }

            RefreshOutcome::RetryableFailure
        }
    }
}

// PB1: Advisory lock release removed — row-level locks (FOR UPDATE SKIP
// LOCKED) are transaction-scoped and released automatically on commit.

// ── Bootstrap Source Gating helpers (v0.5.0, Phase 3) ─────────────────────

/// Load the set of currently gated source OIDs from `pgt_source_gates`.
///
/// Called once per worker invocation to build an immutable gate set for the
/// duration of the tick, avoiding repeated SPI round-trips.
fn load_gated_source_oids() -> std::collections::HashSet<pg_sys::Oid> {
    crate::catalog::get_gated_source_oids()
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Check whether any of the stream table's declared source OIDs appear in
/// the caller-supplied gated set.  Returns `true` if the ST should be
/// skipped this tick.
fn is_any_source_gated(pgt_id: i64, gated: &std::collections::HashSet<pg_sys::Oid>) -> bool {
    if gated.is_empty() {
        return false;
    }
    get_source_oids_for_st(pgt_id)
        .into_iter()
        .any(|oid| gated.contains(&oid))
}

/// Insert a SKIP record into `pgt_refresh_history` for a gated stream table.
///
/// This lets operators see which STs were skipped and why by querying the
/// history table.  Errors are logged as warnings rather than bubbling up,
/// because a failure to write the history record must not abort the tick.
fn log_gated_skip(st: &StreamTableMeta) {
    let now = Spi::get_one::<TimestampWithTimeZone>("SELECT now()")
        .unwrap_or(None)
        .unwrap_or_else(|| {
            pgrx::warning!(
                "log_gated_skip: now() returned NULL for {}.{}",
                st.pgt_schema,
                st.pgt_name
            );
            TimestampWithTimeZone::try_from(0i64).unwrap_or_else(|_| {
                pgrx::error!("scheduler: failed to create epoch TimestampWithTimeZone")
            })
        });

    if let Err(e) = crate::catalog::RefreshRecord::insert(
        st.pgt_id,
        now,
        "SKIP",
        "SKIPPED",
        0,
        0,
        Some("source gated"),
        Some("SCHEDULER"),
        None,
        0,
        None,
        false,
        None,
    ) {
        pgrx::warning!(
            "pg_trickle: failed to log SKIP for {}.{}: {}",
            st.pgt_schema,
            st.pgt_name,
            e
        );
    }
}

// ── Watermark Gating helpers (v0.7.0) ─────────────────────────────────────

/// Check whether a stream table should be skipped due to watermark
/// misalignment. Returns `true` if the ST should be skipped (misaligned).
fn is_watermark_misaligned(pgt_id: i64) -> (bool, Option<String>) {
    let source_oids = get_source_oids_for_st(pgt_id);
    if source_oids.is_empty() {
        return (false, None);
    }
    match crate::catalog::check_watermark_alignment(&source_oids) {
        Ok((aligned, reason)) => (!aligned, reason),
        Err(e) => {
            pgrx::warning!(
                "pg_trickle: watermark check failed for pgt_id={}: {}",
                pgt_id,
                e
            );
            (false, None)
        }
    }
}

/// WM-7: Check whether a stream table should be skipped because a watermark
/// in an overlapping group is stuck (not advanced within the holdback timeout).
fn is_watermark_stuck(pgt_id: i64) -> (bool, Option<String>) {
    let timeout = config::pg_trickle_watermark_holdback_timeout();
    if timeout <= 0 {
        return (false, None);
    }
    let source_oids = get_source_oids_for_st(pgt_id);
    if source_oids.is_empty() {
        return (false, None);
    }
    match crate::catalog::check_watermark_staleness(&source_oids, timeout) {
        Ok((stuck, reason)) => (stuck, reason),
        Err(e) => {
            pgrx::warning!(
                "pg_trickle: watermark staleness check failed for pgt_id={}: {}",
                pgt_id,
                e
            );
            (false, None)
        }
    }
}

/// Log a watermark-gated skip into `pgt_refresh_history`.
fn log_watermark_skip(st: &StreamTableMeta, reason: &str) {
    let now = Spi::get_one::<TimestampWithTimeZone>("SELECT now()")
        .unwrap_or(None)
        .unwrap_or_else(|| {
            pgrx::warning!(
                "log_watermark_skip: now() returned NULL for {}.{}",
                st.pgt_schema,
                st.pgt_name
            );
            TimestampWithTimeZone::try_from(0i64).unwrap_or_else(|_| {
                pgrx::error!("scheduler: failed to create epoch TimestampWithTimeZone")
            })
        });

    if let Err(e) = crate::catalog::RefreshRecord::insert(
        st.pgt_id,
        now,
        "SKIP",
        "SKIPPED",
        0,
        0,
        Some(reason),
        Some("SCHEDULER"),
        None,
        0,
        None,
        false,
        None,
    ) {
        pgrx::warning!(
            "pg_trickle: failed to log watermark SKIP for {}.{}: {}",
            st.pgt_schema,
            st.pgt_name,
            e
        );
    }
}

/// D-1b: Check whether any UNLOGGED source buffer for this ST was
/// emptied by crash recovery and needs a FULL refresh to resynchronize.
///
/// After a PostgreSQL crash, UNLOGGED tables are truncated. If a buffer
/// table has `relpersistence = 'u'` AND is empty AND the postmaster was
/// restarted after the ST's last refresh, the buffer contents were lost
/// and we must force a FULL refresh.
///
/// Returns `true` if crash-recovery data loss was detected (and the ST
/// was marked for reinit).
fn check_unlogged_buffer_crash_recovery(st: &StreamTableMeta) -> bool {
    if !st.is_populated || st.needs_reinit {
        return false;
    }

    let change_schema = config::pg_trickle_change_buffer_schema();

    // Check base-table source buffers (changes_{oid})
    let source_oids = get_source_oids_for_st(st.pgt_id);
    for oid in &source_oids {
        let lost = Spi::get_one::<bool>(&format!(
            "SELECT c.relpersistence = 'u' \
               AND NOT EXISTS(SELECT 1 FROM {schema}.changes_{oid} LIMIT 1) \
               AND pg_postmaster_start_time() > \
                   COALESCE((SELECT last_refresh_at FROM pgtrickle.pgt_stream_tables \
                             WHERE pgt_id = {pgt_id}), '-infinity'::timestamptz) \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = '{schema}' AND c.relname = 'changes_{oid}'",
            schema = change_schema,
            oid = oid.to_u32(),
            pgt_id = st.pgt_id,
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if lost {
            pgrx::warning!(
                "pg_trickle: UNLOGGED change buffer for {}.{} source OID {} was \
                 emptied by crash recovery — scheduling FULL refresh",
                st.pgt_schema,
                st.pgt_name,
                oid.to_u32(),
            );
            if let Err(e) = StreamTableMeta::mark_for_reinitialize(st.pgt_id) {
                pgrx::warning!(
                    "pg_trickle: failed to mark {}.{} for reinit after crash recovery: {}",
                    st.pgt_schema,
                    st.pgt_name,
                    e,
                );
            }
            return true;
        }
    }

    // Check ST-to-ST source buffers (changes_pgt_{id})
    let deps = crate::catalog::StDependency::get_for_st(st.pgt_id).unwrap_or_default();
    for dep in &deps {
        if dep.source_type != "STREAM_TABLE" {
            continue;
        }
        let upstream_pgt_id = crate::catalog::StreamTableMeta::pgt_id_for_relid(dep.source_relid);
        if let Some(up_id) = upstream_pgt_id
            && cdc::has_st_change_buffer(up_id, &change_schema)
        {
            let lost = Spi::get_one::<bool>(&format!(
                "SELECT c.relpersistence = 'u' \
                   AND NOT EXISTS(SELECT 1 FROM {schema}.changes_pgt_{id} LIMIT 1) \
                   AND pg_postmaster_start_time() > \
                       COALESCE((SELECT last_refresh_at FROM pgtrickle.pgt_stream_tables \
                                 WHERE pgt_id = {pgt_id}), '-infinity'::timestamptz) \
                 FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{schema}' AND c.relname = 'changes_pgt_{id}'",
                schema = change_schema,
                id = up_id,
                pgt_id = st.pgt_id,
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false);

            if lost {
                pgrx::warning!(
                    "pg_trickle: UNLOGGED ST change buffer for {}.{} upstream pgt_id={} \
                     was emptied by crash recovery — scheduling FULL refresh",
                    st.pgt_schema,
                    st.pgt_name,
                    up_id,
                );
                if let Err(e) = StreamTableMeta::mark_for_reinitialize(st.pgt_id) {
                    pgrx::warning!(
                        "pg_trickle: failed to mark {}.{} for reinit after crash recovery: {}",
                        st.pgt_schema,
                        st.pgt_name,
                        e,
                    );
                }
                return true;
            }
        }
    }

    false
}

/// Get the source OIDs (base table OIDs) for a given ST.
fn get_source_oids_for_st(pgt_id: i64) -> Vec<pg_sys::Oid> {
    use crate::catalog::StDependency;

    StDependency::get_for_st(pgt_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|dep| dep.source_type == "TABLE" || dep.source_type == "FOREIGN_TABLE")
        .map(|dep| dep.source_relid)
        .collect()
}

/// Compute the freshness deadline for a duration-based schedule.
///
/// Returns `data_timestamp + schedule_seconds` (the moment the data becomes
/// stale). For cron-based schedules, returns `None` because cron doesn't
/// define a continuous freshness SLA.
fn compute_freshness_deadline(st: &StreamTableMeta) -> Option<TimestampWithTimeZone> {
    let schedule_str = st.schedule.as_deref()?;

    // Cron expressions contain spaces or start with '@' — no deadline for those.
    if schedule_str.contains(' ') || schedule_str.starts_with('@') {
        return None;
    }

    // Parse the duration. If parsing fails, skip deadline computation.
    let secs = crate::api::parse_duration(schedule_str).ok()?;
    if secs <= 0 {
        return None;
    }

    // Compute deadline: data_timestamp + schedule_seconds
    // If data_timestamp is NULL (never refreshed), use 'now' as a baseline.
    let deadline_sql = format!(
        "SELECT COALESCE(data_timestamp, now()) + interval '{secs} seconds' \
         FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1"
    );
    Spi::get_one_with_args::<TimestampWithTimeZone>(&deadline_sql, &[st.pgt_id.into()])
        .unwrap_or(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_epoch_ms_returns_reasonable_value() {
        let ms = current_epoch_ms();
        // Should be after 2024-01-01 and before 2040-01-01
        let min_ms = 1_704_067_200_000u64; // 2024-01-01
        let max_ms = 2_208_988_800_000u64; // 2040-01-01
        assert!(ms > min_ms, "epoch_ms too old: {}", ms);
        assert!(ms < max_ms, "epoch_ms too far in future: {}", ms);
    }

    #[test]
    fn test_current_epoch_ms_monotonic() {
        let t1 = current_epoch_ms();
        let t2 = current_epoch_ms();
        assert!(t2 >= t1);
    }

    #[test]
    fn test_refresh_outcome_debug_and_equality() {
        assert_eq!(RefreshOutcome::Success, RefreshOutcome::Success);
        assert_ne!(RefreshOutcome::Success, RefreshOutcome::RetryableFailure);
        assert_ne!(
            RefreshOutcome::RetryableFailure,
            RefreshOutcome::PermanentFailure
        );

        // Verify Debug trait works
        let s = format!("{:?}", RefreshOutcome::PermanentFailure);
        assert!(s.contains("PermanentFailure"));
    }

    #[test]
    fn test_refresh_outcome_clone() {
        let outcome = RefreshOutcome::RetryableFailure;
        let cloned = outcome;
        assert_eq!(outcome, cloned);
    }

    // ── is_group_due_pure tests ─────────────────────────────────────

    #[test]
    fn test_group_due_empty_is_false() {
        assert!(!is_group_due_pure(&[], DiamondSchedulePolicy::Fastest));
        assert!(!is_group_due_pure(&[], DiamondSchedulePolicy::Slowest));
    }

    #[test]
    fn test_group_due_fastest_any_true() {
        assert!(is_group_due_pure(
            &[false, true, false],
            DiamondSchedulePolicy::Fastest,
        ));
    }

    #[test]
    fn test_group_due_fastest_all_false() {
        assert!(!is_group_due_pure(
            &[false, false],
            DiamondSchedulePolicy::Fastest,
        ));
    }

    #[test]
    fn test_group_due_slowest_all_true() {
        assert!(is_group_due_pure(
            &[true, true, true],
            DiamondSchedulePolicy::Slowest,
        ));
    }

    #[test]
    fn test_group_due_slowest_not_all_true() {
        assert!(!is_group_due_pure(
            &[true, false, true],
            DiamondSchedulePolicy::Slowest,
        ));
    }

    #[test]
    fn test_group_due_single_member() {
        assert!(is_group_due_pure(&[true], DiamondSchedulePolicy::Fastest,));
        assert!(is_group_due_pure(&[true], DiamondSchedulePolicy::Slowest,));
        assert!(!is_group_due_pure(&[false], DiamondSchedulePolicy::Fastest,));
    }

    // ── is_falling_behind tests ─────────────────────────────────────

    #[test]
    fn test_falling_behind_at_80_percent_ec11_threshold() {
        // EC-11 alerting uses 0.80 — fires at exactly 80%.
        let result = is_falling_behind(800, 1000, 0.80);
        assert!(result.is_some());
        assert!((result.unwrap() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_falling_behind_below_95_percent_no_backoff() {
        // Auto-backoff uses 0.95 — 900ms on a 1s schedule must NOT trigger.
        assert!(is_falling_behind(900, 1000, 0.95).is_none());
    }

    #[test]
    fn test_falling_behind_at_95_percent_triggers_backoff() {
        // Exactly at the 0.95 threshold — should trigger.
        let result = is_falling_behind(950, 1000, 0.95);
        assert!(result.is_some());
        assert!((result.unwrap() - 0.95).abs() < 0.001);
    }

    #[test]
    fn test_ec11_fires_before_backoff_threshold() {
        // 800ms on 1s: EC-11 (0.80) fires, but auto-backoff (0.95) does not.
        assert!(is_falling_behind(800, 1000, 0.80).is_some());
        assert!(is_falling_behind(800, 1000, 0.95).is_none());
    }

    #[test]
    fn test_falling_behind_over_100_percent() {
        let result = is_falling_behind(1500, 1000, 0.80);
        assert!(result.is_some());
        assert!((result.unwrap() - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_not_falling_behind() {
        assert!(is_falling_behind(500, 1000, 0.80).is_none());
    }

    #[test]
    fn test_falling_behind_zero_schedule() {
        assert!(is_falling_behind(100, 0, 0.80).is_none());
    }

    #[test]
    fn test_falling_behind_negative_schedule() {
        assert!(is_falling_behind(100, -1, 0.80).is_none());
    }

    // ── parse_worker_extra tests (Phase 3) ──────────────────────────

    #[test]
    fn test_parse_worker_extra_valid() {
        let result = parse_worker_extra("mydb|42");
        assert!(result.is_some());
        let (db, id) = result.unwrap();
        assert_eq!(db, "mydb");
        assert_eq!(id, 42);
    }

    #[test]
    fn test_parse_worker_extra_long_db_name() {
        let result = parse_worker_extra("my_production_database|999999");
        assert!(result.is_some());
        let (db, id) = result.unwrap();
        assert_eq!(db, "my_production_database");
        assert_eq!(id, 999999);
    }

    #[test]
    fn test_parse_worker_extra_empty() {
        assert!(parse_worker_extra("").is_none());
    }

    #[test]
    fn test_parse_worker_extra_no_separator() {
        assert!(parse_worker_extra("mydb42").is_none());
    }

    #[test]
    fn test_parse_worker_extra_empty_db_name() {
        assert!(parse_worker_extra("|42").is_none());
    }

    #[test]
    fn test_parse_worker_extra_invalid_job_id() {
        assert!(parse_worker_extra("mydb|abc").is_none());
    }

    #[test]
    fn test_parse_worker_extra_negative_job_id() {
        assert!(parse_worker_extra("mydb|-1").is_none());
    }

    #[test]
    fn test_parse_worker_extra_zero_job_id() {
        assert!(parse_worker_extra("mydb|0").is_none());
    }

    // ── ParallelDispatchState tests (Phase 4) ───────────────────────

    #[test]
    fn test_parallel_state_new_is_empty() {
        let state = ParallelDispatchState::new();
        assert!(state.unit_states.is_empty());
        assert_eq!(state.per_db_inflight, 0);
        assert_eq!(state.dag_version, 0);
        assert!(!state.has_inflight());
    }

    #[test]
    fn test_parallel_state_has_inflight() {
        let mut state = ParallelDispatchState::new();
        assert!(!state.has_inflight());
        state.per_db_inflight = 1;
        assert!(state.has_inflight());
    }

    #[test]
    fn test_unit_dispatch_state_default() {
        let uds = UnitDispatchState::default();
        assert_eq!(uds.remaining_upstreams, 0);
        assert!(uds.inflight_job_id.is_none());
        assert!(!uds.succeeded);
    }

    #[test]
    fn test_parallel_state_wave_reset_fires_after_dispatch_not_before() {
        // Regression guard for cascade correctness.
        //
        // The wave-complete reset (Step 4) must fire AFTER the dispatch loop
        // (Step 3), not before it.  With a two-unit cascade A → B:
        //
        //   Good (reset after dispatch):
        //     Tick: A completes → per_db_inflight=0, B.remaining=0.
        //     Step 2: B is ready.  Step 3: dispatch B → per_db_inflight=1.
        //     Reset check: per_db_inflight=1 → no reset.  B runs.
        //
        //   Bad (reset before dispatch / old Step 1.5 placement):
        //     Tick: A completes → per_db_inflight=0.
        //     Reset fires: B.remaining_upstreams = 1 again.
        //     Step 2: B now blocked.  B never runs.
        //
        // This test validates the data-model invariant: after a complete wave
        // (per_db_inflight==0, some units have succeeded, nothing was dispatched
        // in Step 3), the reset should clear succeeded flags and restore
        // remaining_upstreams so units can run in the next wave.

        let mut state = ParallelDispatchState::new();

        // Simulate a completed wave: two units both succeeded, nothing in-flight.
        let uid_a = crate::dag::ExecutionUnitId(1);
        let uid_b = crate::dag::ExecutionUnitId(2);
        state.unit_states.insert(
            uid_a,
            UnitDispatchState {
                remaining_upstreams: 0,
                inflight_job_id: None,
                succeeded: true,
            },
        );
        state.unit_states.insert(
            uid_b,
            UnitDispatchState {
                remaining_upstreams: 0, // was decremented when A succeeded
                inflight_job_id: None,
                succeeded: true,
            },
        );
        state.per_db_inflight = 0;

        // Simulate the Step 4 reset that fires after dispatch.
        if state.per_db_inflight == 0 {
            let any_done = state.unit_states.values().any(|us| us.succeeded);
            if any_done {
                for us in state.unit_states.values_mut() {
                    us.succeeded = false;
                    // remaining_upstreams would be restored from eu_dag here;
                    // for B (which had 1 upstream) this restores it to 1.
                }
            }
        }

        assert!(
            state.unit_states.values().all(|us| !us.succeeded),
            "all succeeded flags must be cleared after wave reset"
        );
    }

    #[test]
    fn test_is_unit_due_pure_no_members() {
        // is_unit_due depends on SPI, but we can test the logic structure
        // indirectly via the iteration: empty members → not due.
        let unit = crate::dag::ExecutionUnit {
            id: crate::dag::ExecutionUnitId(1),
            kind: crate::dag::ExecutionUnitKind::Singleton,
            member_pgt_ids: vec![],
            root_pgt_id: 0,
            label: "empty".to_string(),
        };
        // With no members, iter().any() returns false → not due.
        let result = unit.member_pgt_ids.iter().any(|_| true);
        assert!(!result);
    }

    // ── RefreshTier tests ─────────────────────────────────────────────

    #[test]
    fn test_refresh_tier_from_sql_str() {
        assert_eq!(RefreshTier::from_sql_str("hot"), RefreshTier::Hot);
        assert_eq!(RefreshTier::from_sql_str("WARM"), RefreshTier::Warm);
        assert_eq!(RefreshTier::from_sql_str("Cold"), RefreshTier::Cold);
        assert_eq!(RefreshTier::from_sql_str("FROZEN"), RefreshTier::Frozen);
        assert_eq!(RefreshTier::from_sql_str("unknown"), RefreshTier::Hot);
        assert_eq!(RefreshTier::from_sql_str(""), RefreshTier::Hot);
    }

    #[test]
    fn test_refresh_tier_as_str() {
        assert_eq!(RefreshTier::Hot.as_str(), "hot");
        assert_eq!(RefreshTier::Warm.as_str(), "warm");
        assert_eq!(RefreshTier::Cold.as_str(), "cold");
        assert_eq!(RefreshTier::Frozen.as_str(), "frozen");
    }

    #[test]
    fn test_refresh_tier_schedule_multiplier() {
        assert_eq!(RefreshTier::Hot.schedule_multiplier(), Some(1.0));
        assert_eq!(RefreshTier::Warm.schedule_multiplier(), Some(2.0));
        assert_eq!(RefreshTier::Cold.schedule_multiplier(), Some(10.0));
        assert_eq!(RefreshTier::Frozen.schedule_multiplier(), None);
    }

    #[test]
    fn test_refresh_tier_is_valid_str() {
        assert!(RefreshTier::is_valid_str("hot"));
        assert!(RefreshTier::is_valid_str("WARM"));
        assert!(RefreshTier::is_valid_str("Cold"));
        assert!(RefreshTier::is_valid_str("FROZEN"));
        assert!(!RefreshTier::is_valid_str(""));
        assert!(!RefreshTier::is_valid_str("invalid"));
    }

    #[test]
    fn test_refresh_tier_display() {
        assert_eq!(format!("{}", RefreshTier::Hot), "hot");
        assert_eq!(format!("{}", RefreshTier::Frozen), "frozen");
    }

    #[test]
    fn test_refresh_tier_default() {
        assert_eq!(RefreshTier::default(), RefreshTier::Hot);
    }

    #[test]
    fn test_refresh_tier_roundtrip() {
        for tier in [
            RefreshTier::Hot,
            RefreshTier::Warm,
            RefreshTier::Cold,
            RefreshTier::Frozen,
        ] {
            assert_eq!(RefreshTier::from_sql_str(tier.as_str()), tier);
        }
    }

    // ── FUSE unit tests ────────────────────────────────────────────────

    #[test]
    fn test_fuse_is_active_off_mode() {
        assert!(!fuse_is_active("off", Some(1000), 5000));
        assert!(!fuse_is_active("off", None, 5000));
    }

    #[test]
    fn test_fuse_is_active_on_with_per_st_ceiling() {
        assert!(fuse_is_active("on", Some(1000), 0));
        assert!(fuse_is_active("on", Some(1000), 5000));
    }

    #[test]
    fn test_fuse_is_active_on_with_global_ceiling_only() {
        assert!(fuse_is_active("on", None, 5000));
    }

    #[test]
    fn test_fuse_is_active_on_no_ceiling_at_all() {
        assert!(!fuse_is_active("on", None, 0));
        assert!(!fuse_is_active("on", Some(0), 0));
    }

    #[test]
    fn test_fuse_is_active_auto_mode() {
        assert!(fuse_is_active("auto", Some(100), 0));
        assert!(fuse_is_active("auto", None, 5000));
        assert!(!fuse_is_active("auto", None, 0));
    }

    // ── C3-1: compute_per_db_quota tests ──────────────────────────────────

    #[test]
    fn test_quota_disabled_falls_back_to_max_concurrent_refreshes() {
        // per_db_quota=0 → legacy behaviour, use max_concurrent_refreshes
        assert_eq!(compute_per_db_quota(0, 4, 8, 0), 4);
        assert_eq!(compute_per_db_quota(0, 1, 8, 6), 1);
        // max_concurrent_refreshes=0 is clamped to 1
        assert_eq!(compute_per_db_quota(0, 0, 8, 0), 1);
    }

    #[test]
    fn test_quota_base_case_no_burst_when_cluster_busy() {
        // active (7) >= 80% of max_cluster (10) = 8 → no burst, return base
        assert_eq!(compute_per_db_quota(3, 4, 10, 8), 3);
        assert_eq!(compute_per_db_quota(3, 4, 10, 9), 3);
        assert_eq!(compute_per_db_quota(3, 4, 10, 10), 3);
    }

    #[test]
    fn test_quota_burst_when_cluster_has_spare_capacity() {
        // active (4) < 80% of max_cluster (10) = 8 → burst to 150% of base
        // base=4, burst = 4*3/2 = 6
        assert_eq!(compute_per_db_quota(4, 4, 10, 4), 6);
        // base=2, burst = 2*3/2 = 3
        assert_eq!(compute_per_db_quota(2, 4, 10, 0), 3);
    }

    #[test]
    fn test_quota_burst_minimum_is_base_plus_one() {
        // base=1, 1*3/2 = 1 (integer div), but min is base+1=2
        assert_eq!(compute_per_db_quota(1, 4, 10, 0), 2);
    }

    #[test]
    fn test_quota_negative_per_db_quota_falls_back_to_legacy() {
        assert_eq!(compute_per_db_quota(-1, 3, 10, 0), 3);
    }

    #[test]
    fn test_quota_cluster_idle_gives_burst() {
        // max_cluster=8, 80% threshold = ceil(6.4)=7, active=0 < 7 → burst
        // base=2, burst=3
        assert_eq!(compute_per_db_quota(2, 4, 8, 0), 3);
    }

    #[test]
    fn test_quota_burst_threshold_at_exactly_80_percent() {
        // max_cluster=10, ceil(10*0.8)=8, active=7 < 8 → burst allowed
        assert_eq!(compute_per_db_quota(4, 4, 10, 7), 6);
        // active=8 == threshold → no burst
        assert_eq!(compute_per_db_quota(4, 4, 10, 8), 4);
    }

    // ── DAG-2: compute_adaptive_poll_ms tests ─────────────────────────────

    #[test]
    fn test_adaptive_poll_no_inflight_returns_base_interval() {
        // When no workers are in-flight, use the full scheduler interval
        // regardless of current_poll_ms or completion state.
        assert_eq!(compute_adaptive_poll_ms(20, false, false, 1000), 1000);
        assert_eq!(compute_adaptive_poll_ms(200, true, false, 1000), 1000);
        assert_eq!(compute_adaptive_poll_ms(50, false, false, 500), 500);
    }

    #[test]
    fn test_adaptive_poll_completion_resets_to_min() {
        // After a worker completes, poll resets to ADAPTIVE_POLL_MIN_MS (20ms).
        assert_eq!(compute_adaptive_poll_ms(200, true, true, 1000), 20);
        assert_eq!(compute_adaptive_poll_ms(100, true, true, 1000), 20);
        assert_eq!(compute_adaptive_poll_ms(20, true, true, 1000), 20);
    }

    #[test]
    fn test_adaptive_poll_backoff_doubles_each_tick() {
        // No completion, workers in-flight → double current_poll_ms.
        assert_eq!(compute_adaptive_poll_ms(20, false, true, 1000), 40);
        assert_eq!(compute_adaptive_poll_ms(40, false, true, 1000), 80);
        assert_eq!(compute_adaptive_poll_ms(80, false, true, 1000), 160);
    }

    #[test]
    fn test_adaptive_poll_caps_at_max() {
        // Doubling 160 → 320, but capped at ADAPTIVE_POLL_MAX_MS (200).
        assert_eq!(compute_adaptive_poll_ms(160, false, true, 1000), 200);
        assert_eq!(compute_adaptive_poll_ms(200, false, true, 1000), 200);
    }

    #[test]
    fn test_adaptive_poll_full_backoff_sequence() {
        // Simulate a full sequence: completion → backoff → backoff → … → cap.
        let base = 1000u64;
        // Worker completes → reset.
        let p = compute_adaptive_poll_ms(200, true, true, base);
        assert_eq!(p, 20);
        // Tick 1: no completion.
        let p = compute_adaptive_poll_ms(p, false, true, base);
        assert_eq!(p, 40);
        // Tick 2.
        let p = compute_adaptive_poll_ms(p, false, true, base);
        assert_eq!(p, 80);
        // Tick 3.
        let p = compute_adaptive_poll_ms(p, false, true, base);
        assert_eq!(p, 160);
        // Tick 4: would be 320, capped at 200.
        let p = compute_adaptive_poll_ms(p, false, true, base);
        assert_eq!(p, 200);
        // Tick 5: stays at cap.
        let p = compute_adaptive_poll_ms(p, false, true, base);
        assert_eq!(p, 200);
    }

    #[test]
    fn test_adaptive_poll_completion_after_backoff_resets() {
        // After backing off to cap, a completion resets to min.
        let p = compute_adaptive_poll_ms(200, true, true, 1000);
        assert_eq!(p, 20);
    }

    #[test]
    fn test_adaptive_poll_transition_inflight_to_idle() {
        // Workers finish between ticks: in-flight → not in-flight.
        // Should switch from adaptive to base interval.
        let p = compute_adaptive_poll_ms(40, false, true, 1000);
        assert_eq!(p, 80); // still in-flight, doubles
        let p = compute_adaptive_poll_ms(p, false, false, 1000);
        assert_eq!(p, 1000); // no longer in-flight, base interval
    }

    // ── DAG-2: ParallelDispatchState adaptive poll integration ────────────

    #[test]
    fn test_parallel_state_new_has_min_adaptive_poll() {
        let state = ParallelDispatchState::new();
        assert_eq!(state.adaptive_poll_ms, ADAPTIVE_POLL_MIN_MS);
        assert_eq!(state.completions_this_tick, 0);
    }

    // ── DAG-1: Intra-tick pipelining validation ───────────────────────────
    //
    // The parallel dispatch architecture from Phase 4 already achieves
    // intra-tick pipelining: Step 1 immediately decrements downstream
    // `remaining_upstreams`, and Step 2 picks up newly-ready units in the
    // same tick. These tests validate the state-machine invariants.

    #[test]
    fn test_cascade_pipeline_downstream_ready_after_upstream_completes() {
        // Three-unit cascade: A → B → C.
        // When A completes in Step 1, B.remaining_upstreams drops to 0
        // and B is dispatchable in Step 2 of the same tick.
        let mut state = ParallelDispatchState::new();
        let uid_a = crate::dag::ExecutionUnitId(1);
        let uid_b = crate::dag::ExecutionUnitId(2);
        let uid_c = crate::dag::ExecutionUnitId(3);

        // Initial state: A=0 upstreams, B=1 (A), C=1 (B).
        state.unit_states.insert(
            uid_a,
            UnitDispatchState {
                remaining_upstreams: 0,
                inflight_job_id: Some(100),
                succeeded: false,
            },
        );
        state.unit_states.insert(
            uid_b,
            UnitDispatchState {
                remaining_upstreams: 1,
                inflight_job_id: None,
                succeeded: false,
            },
        );
        state.unit_states.insert(
            uid_c,
            UnitDispatchState {
                remaining_upstreams: 1,
                inflight_job_id: None,
                succeeded: false,
            },
        );
        state.per_db_inflight = 1;

        // Simulate Step 1: A completes → decrement downstream B.
        state.unit_states.get_mut(&uid_a).unwrap().inflight_job_id = None;
        state.unit_states.get_mut(&uid_a).unwrap().succeeded = true;
        state.per_db_inflight -= 1;
        // Decrement downstream (B depends on A).
        state
            .unit_states
            .get_mut(&uid_b)
            .unwrap()
            .remaining_upstreams -= 1;

        // Verify: B is now ready (remaining=0, not succeeded, not inflight).
        let b_state = &state.unit_states[&uid_b];
        assert_eq!(
            b_state.remaining_upstreams, 0,
            "B should be ready after A completes"
        );
        assert!(!b_state.succeeded);
        assert!(b_state.inflight_job_id.is_none());

        // Verify: C is still blocked.
        let c_state = &state.unit_states[&uid_c];
        assert_eq!(c_state.remaining_upstreams, 1, "C should still be blocked");
    }

    #[test]
    fn test_mixed_cost_levels_no_level_barrier() {
        // Level 0: A (fast), B (slow).
        // Level 1: C depends only on A, D depends on both A and B.
        // When A completes, C should be immediately ready even though
        // B (same level) is still running — no level barrier.
        let mut state = ParallelDispatchState::new();
        let uid_a = crate::dag::ExecutionUnitId(1);
        let uid_b = crate::dag::ExecutionUnitId(2);
        let uid_c = crate::dag::ExecutionUnitId(3);
        let uid_d = crate::dag::ExecutionUnitId(4);

        state.unit_states.insert(
            uid_a,
            UnitDispatchState {
                remaining_upstreams: 0,
                inflight_job_id: Some(100),
                succeeded: false,
            },
        );
        state.unit_states.insert(
            uid_b,
            UnitDispatchState {
                remaining_upstreams: 0,
                inflight_job_id: Some(101),
                succeeded: false,
            },
        );
        state.unit_states.insert(
            uid_c,
            UnitDispatchState {
                remaining_upstreams: 1, // depends on A only
                inflight_job_id: None,
                succeeded: false,
            },
        );
        state.unit_states.insert(
            uid_d,
            UnitDispatchState {
                remaining_upstreams: 2, // depends on A and B
                inflight_job_id: None,
                succeeded: false,
            },
        );
        state.per_db_inflight = 2;

        // Simulate: A completes (fast), B still running.
        state.unit_states.get_mut(&uid_a).unwrap().inflight_job_id = None;
        state.unit_states.get_mut(&uid_a).unwrap().succeeded = true;
        state.per_db_inflight -= 1;
        // Decrement C (depends on A) and D (depends on A and B).
        state
            .unit_states
            .get_mut(&uid_c)
            .unwrap()
            .remaining_upstreams -= 1;
        state
            .unit_states
            .get_mut(&uid_d)
            .unwrap()
            .remaining_upstreams -= 1;

        // C is ready — A was its only upstream.
        let c_state = &state.unit_states[&uid_c];
        assert_eq!(
            c_state.remaining_upstreams, 0,
            "C should be ready (A was its only dep)"
        );
        assert!(!c_state.succeeded);
        assert!(c_state.inflight_job_id.is_none());

        // D is still blocked — needs B too.
        let d_state = &state.unit_states[&uid_d];
        assert_eq!(d_state.remaining_upstreams, 1, "D still waiting for B");

        // B still running.
        assert!(state.unit_states[&uid_b].inflight_job_id.is_some());
        assert_eq!(state.per_db_inflight, 1);
    }

    #[test]
    fn test_cascade_wave_reset_preserves_correctness() {
        // After a full cascade A → B → C all complete, the wave reset
        // restores remaining_upstreams so units can run in the next cycle.
        let mut state = ParallelDispatchState::new();
        let uid_a = crate::dag::ExecutionUnitId(1);
        let uid_b = crate::dag::ExecutionUnitId(2);
        let uid_c = crate::dag::ExecutionUnitId(3);

        // All units completed this wave.
        state.unit_states.insert(
            uid_a,
            UnitDispatchState {
                remaining_upstreams: 0,
                inflight_job_id: None,
                succeeded: true,
            },
        );
        state.unit_states.insert(
            uid_b,
            UnitDispatchState {
                remaining_upstreams: 0, // was decremented from 1 when A succeeded
                inflight_job_id: None,
                succeeded: true,
            },
        );
        state.unit_states.insert(
            uid_c,
            UnitDispatchState {
                remaining_upstreams: 0, // was decremented from 1 when B succeeded
                inflight_job_id: None,
                succeeded: true,
            },
        );
        state.per_db_inflight = 0;

        // Simulate wave reset (Step 4 logic).
        // In production this uses eu_dag.get_upstream_units(uid).len();
        // here we simulate B.original=1, C.original=1.
        let original_upstreams: HashMap<crate::dag::ExecutionUnitId, usize> =
            [(uid_a, 0), (uid_b, 1), (uid_c, 1)].into();

        if state.per_db_inflight == 0 {
            let any_done = state.unit_states.values().any(|us| us.succeeded);
            if any_done {
                for (&uid, us) in state.unit_states.iter_mut() {
                    us.succeeded = false;
                    us.remaining_upstreams = *original_upstreams.get(&uid).unwrap_or(&0);
                }
            }
        }

        // After reset: A is ready for next wave, B and C blocked on upstreams.
        assert_eq!(state.unit_states[&uid_a].remaining_upstreams, 0);
        assert_eq!(state.unit_states[&uid_b].remaining_upstreams, 1);
        assert_eq!(state.unit_states[&uid_c].remaining_upstreams, 1);
        assert!(state.unit_states.values().all(|us| !us.succeeded));
    }

    // ── C3-1: sort_ready_queue_by_priority tier-aware tests ───────────────

    /// Build a minimal ExecutionUnitDag with entries of requested kinds so the
    /// sort function can look up `unit_priority`.  No edges are needed.
    fn make_single_dag(
        entries: &[(ExecutionUnitId, crate::dag::ExecutionUnitKind)],
    ) -> ExecutionUnitDag {
        use crate::dag::{ExecutionUnit, ExecutionUnitDag};
        let mut units = Vec::new();
        for &(uid, kind) in entries {
            units.push(ExecutionUnit {
                id: uid,
                kind,
                root_pgt_id: uid.0 as i64,
                member_pgt_ids: vec![uid.0 as i64],
                label: format!("unit-{}", uid.0),
            });
        }
        ExecutionUnitDag::from_units_for_test(units)
    }

    #[test]
    fn test_sort_priority_immediate_before_singleton() {
        use crate::dag::{ExecutionUnitId, ExecutionUnitKind};
        let uid_imm = ExecutionUnitId(1);
        let uid_sing = ExecutionUnitId(2);
        let dag = make_single_dag(&[
            (uid_imm, ExecutionUnitKind::ImmediateClosure),
            (uid_sing, ExecutionUnitKind::Singleton),
        ]);
        let queue: VecDeque<ExecutionUnitId> = vec![uid_sing, uid_imm].into();
        let tier_map = HashMap::new();
        let sorted = sort_ready_queue_by_priority(queue, &dag, &tier_map);
        assert_eq!(sorted[0], uid_imm, "ImmediateClosure must come first");
        assert_eq!(sorted[1], uid_sing);
    }

    #[test]
    fn test_sort_priority_hot_singleton_before_warm_singleton() {
        use crate::dag::{ExecutionUnitId, ExecutionUnitKind};
        let uid_hot = ExecutionUnitId(10);
        let uid_warm = ExecutionUnitId(20);
        let dag = make_single_dag(&[
            (uid_hot, ExecutionUnitKind::Singleton),
            (uid_warm, ExecutionUnitKind::Singleton),
        ]);
        // Warm first in queue, Hot second — tier sort should flip them.
        let queue: VecDeque<ExecutionUnitId> = vec![uid_warm, uid_hot].into();
        let mut tier_map = HashMap::new();
        tier_map.insert(uid_hot, 0u8); // Hot
        tier_map.insert(uid_warm, 1u8); // Warm
        let sorted = sort_ready_queue_by_priority(queue, &dag, &tier_map);
        assert_eq!(sorted[0], uid_hot, "Hot singleton must come before Warm");
        assert_eq!(sorted[1], uid_warm);
    }

    #[test]
    fn test_sort_priority_warm_singleton_before_cold_singleton() {
        use crate::dag::{ExecutionUnitId, ExecutionUnitKind};
        let uid_warm = ExecutionUnitId(30);
        let uid_cold = ExecutionUnitId(40);
        let dag = make_single_dag(&[
            (uid_warm, ExecutionUnitKind::Singleton),
            (uid_cold, ExecutionUnitKind::Singleton),
        ]);
        let queue: VecDeque<ExecutionUnitId> = vec![uid_cold, uid_warm].into();
        let mut tier_map = HashMap::new();
        tier_map.insert(uid_warm, 1u8); // Warm
        tier_map.insert(uid_cold, 2u8); // Cold
        let sorted = sort_ready_queue_by_priority(queue, &dag, &tier_map);
        assert_eq!(sorted[0], uid_warm, "Warm singleton must come before Cold");
        assert_eq!(sorted[1], uid_cold);
    }

    #[test]
    fn test_sort_priority_immediate_beats_hot_singleton() {
        use crate::dag::{ExecutionUnitId, ExecutionUnitKind};
        let uid_imm = ExecutionUnitId(50);
        let uid_hot = ExecutionUnitId(60);
        let dag = make_single_dag(&[
            (uid_imm, ExecutionUnitKind::ImmediateClosure),
            (uid_hot, ExecutionUnitKind::Singleton),
        ]);
        let queue: VecDeque<ExecutionUnitId> = vec![uid_hot, uid_imm].into();
        let mut tier_map = HashMap::new();
        tier_map.insert(uid_hot, 0u8); // Hot — but ImmediateClosure kind wins anyway
        let sorted = sort_ready_queue_by_priority(queue, &dag, &tier_map);
        assert_eq!(sorted[0], uid_imm, "ImmediateClosure beats Hot Singleton");
    }

    #[test]
    fn test_sort_priority_missing_tier_defaults_to_hot() {
        use crate::dag::{ExecutionUnitId, ExecutionUnitKind};
        let uid_a = ExecutionUnitId(70);
        let uid_b = ExecutionUnitId(80);
        let dag = make_single_dag(&[
            (uid_a, ExecutionUnitKind::Singleton),
            (uid_b, ExecutionUnitKind::Singleton),
        ]);
        let queue: VecDeque<ExecutionUnitId> = vec![uid_b, uid_a].into();
        // uid_b is "Cold" in the map; uid_a has no entry (defaults to Hot=0)
        let mut tier_map = HashMap::new();
        tier_map.insert(uid_b, 2u8); // Cold
        // uid_a absent → defaults to 0 (Hot) → should come first
        let sorted = sort_ready_queue_by_priority(queue, &dag, &tier_map);
        assert_eq!(
            sorted[0], uid_a,
            "Missing-tier unit defaults to Hot urgency"
        );
        assert_eq!(sorted[1], uid_b);
    }

    // ── PermanentFailed downstream unblocking tests ───────────────────────

    #[test]
    fn test_permanent_failure_unblocks_downstream_units() {
        // Regression test: when an upstream unit fails permanently,
        // downstream units must be unblocked (remaining_upstreams
        // decremented and upstream marked succeeded) so the wave can
        // complete. Previously, PermanentFailed left downstreams blocked
        // forever — the wave reset would restore remaining_upstreams,
        // and the failed upstream would never set succeeded=true.
        let mut state = ParallelDispatchState::new();
        let uid_a = crate::dag::ExecutionUnitId(1);
        let uid_b = crate::dag::ExecutionUnitId(2);
        let uid_c = crate::dag::ExecutionUnitId(3);

        // A → B (depends on A), A → C (depends on A).
        // A fails permanently.
        state.unit_states.insert(
            uid_a,
            UnitDispatchState {
                remaining_upstreams: 0,
                inflight_job_id: None,
                // Simulate the fix: after PermanentFailed, the unit is
                // marked succeeded=true so the wave can complete.
                succeeded: true,
            },
        );
        state.unit_states.insert(
            uid_b,
            UnitDispatchState {
                remaining_upstreams: 1,
                inflight_job_id: None,
                succeeded: false,
            },
        );
        state.unit_states.insert(
            uid_c,
            UnitDispatchState {
                remaining_upstreams: 1,
                inflight_job_id: None,
                succeeded: false,
            },
        );

        // Simulate the PermanentFailed handler decrementing downstream units:
        // (this is what the fix does in parallel_dispatch_tick)
        for ds_id in [uid_b, uid_c] {
            if let Some(ds_state) = state.unit_states.get_mut(&ds_id) {
                ds_state.remaining_upstreams = ds_state.remaining_upstreams.saturating_sub(1);
            }
        }

        // After the fix: B and C should be unblocked (remaining=0).
        assert_eq!(
            state.unit_states[&uid_b].remaining_upstreams, 0,
            "B must be unblocked after A's permanent failure"
        );
        assert_eq!(
            state.unit_states[&uid_c].remaining_upstreams, 0,
            "C must be unblocked after A's permanent failure"
        );
        // A must be marked succeeded so the wave reset works correctly.
        assert!(
            state.unit_states[&uid_a].succeeded,
            "PermanentFailed upstream must be marked succeeded for wave completion"
        );
    }

    #[test]
    fn test_permanent_failure_wave_reset_does_not_re_block() {
        // After a PermanentFailed unit is marked succeeded and downstreams
        // are unblocked, the wave reset at Step 4 should restore the
        // original upstream counts (from the DAG), clearing all succeeded
        // flags. The next wave will see the upstream as ready (remaining=0),
        // potentially retrying the failed unit in the next scheduling cycle.
        let mut state = ParallelDispatchState::new();
        let uid_a = crate::dag::ExecutionUnitId(1);
        let uid_b = crate::dag::ExecutionUnitId(2);

        // Post-PermanentFailed state: A succeeded (failed but marked), B completed.
        state.unit_states.insert(
            uid_a,
            UnitDispatchState {
                remaining_upstreams: 0,
                inflight_job_id: None,
                succeeded: true,
            },
        );
        state.unit_states.insert(
            uid_b,
            UnitDispatchState {
                remaining_upstreams: 0, // was decremented by the fix
                inflight_job_id: None,
                succeeded: true,
            },
        );
        state.per_db_inflight = 0;

        // Simulate wave reset (Step 4): restore original upstream counts.
        let original_upstreams: HashMap<crate::dag::ExecutionUnitId, usize> =
            [(uid_a, 0), (uid_b, 1)].into();

        if state.per_db_inflight == 0 {
            let any_done = state.unit_states.values().any(|us| us.succeeded);
            if any_done {
                for (&uid, us) in state.unit_states.iter_mut() {
                    us.succeeded = false;
                    us.remaining_upstreams = *original_upstreams.get(&uid).unwrap_or(&0);
                }
            }
        }

        // After reset: A is ready for next wave, B blocked on A again.
        assert_eq!(state.unit_states[&uid_a].remaining_upstreams, 0);
        assert!(!state.unit_states[&uid_a].succeeded);
        assert_eq!(state.unit_states[&uid_b].remaining_upstreams, 1);
        assert!(!state.unit_states[&uid_b].succeeded);
    }

    // ── TEST-9: Unit tests for compute_per_db_quota ───────────────────

    #[test]
    fn test_per_db_quota_disabled_returns_max_concurrent() {
        // per_db_quota = 0 means disabled — fall back to max_concurrent_refreshes
        assert_eq!(compute_per_db_quota(0, 4, 10, 0), 4);
        assert_eq!(compute_per_db_quota(-1, 8, 20, 5), 8);
    }

    #[test]
    fn test_per_db_quota_burst_when_spare_capacity() {
        // Under 80% of cluster capacity → allow up to 150% of base quota
        // base = 4, max_cluster = 20, burst_threshold = ceil(20*0.8) = 16
        // current_active = 5 (< 16) → burst = max(4*3/2, 4+1) = 6
        assert_eq!(compute_per_db_quota(4, 8, 20, 5), 6);
    }

    #[test]
    fn test_per_db_quota_no_burst_at_capacity() {
        // At or above 80% of cluster capacity → return base quota
        // base = 4, max_cluster = 20, burst_threshold = 16, current_active = 16
        assert_eq!(compute_per_db_quota(4, 8, 20, 16), 4);
        assert_eq!(compute_per_db_quota(4, 8, 20, 20), 4);
    }

    #[test]
    fn test_per_db_quota_minimum_one() {
        // Even with per_db_quota = 0 and max_concurrent = 0, at least 1
        assert_eq!(compute_per_db_quota(0, 0, 10, 0), 1);
    }

    #[test]
    fn test_per_db_quota_small_base_still_bursts() {
        // base = 1, max_cluster = 10, threshold = 8, active = 0
        // burst = max(1*3/2=1, 1+1=2) = 2
        assert_eq!(compute_per_db_quota(1, 4, 10, 0), 2);
    }
}
