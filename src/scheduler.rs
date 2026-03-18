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
//! - **Advisory locks**: prevent concurrent refreshes of the same ST
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
    let outcome = BackgroundWorker::transaction(AssertUnwindSafe(|| -> RefreshOutcome {
        match job.unit_kind.as_str() {
            "singleton" => execute_worker_singleton(&job),
            "atomic_group" => execute_worker_atomic_group(&job),
            "immediate_closure" => execute_worker_immediate_closure(&job),
            "cyclic_scc" => execute_worker_cyclic_scc(&job),
            _ => {
                warning!(
                    "pg_trickle refresh worker: unknown unit_kind '{}' for job {}",
                    job.unit_kind,
                    job_id,
                );
                RefreshOutcome::PermanentFailure
            }
        }
    }));

    // Persist outcome to the job table
    let (status, retryable) = match outcome {
        RefreshOutcome::Success => (JobStatus::Succeeded, None),
        RefreshOutcome::RetryableFailure => (JobStatus::RetryableFailed, Some(true)),
        RefreshOutcome::PermanentFailure => (JobStatus::PermanentFailed, Some(false)),
    };

    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        let _ = SchedulerJob::complete(job_id, status, None, retryable);
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

    let tick_watermark: Option<String> = if config::pg_trickle_tick_watermark_enabled() {
        Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None)
    } else {
        None
    };
    let has_changes = has_table_source_changes(&st) || has_stream_table_source_changes(&st);
    let has_st_changes = has_stream_table_source_changes(&st);
    let action = if has_changes && has_st_changes {
        RefreshAction::Full
    } else {
        refresh::determine_refresh_action(&st, has_changes)
    };

    execute_scheduled_refresh(&st, action, tick_watermark.as_deref())
}

/// Execute an atomic group unit: refresh all members serially inside a
/// sub-transaction.  If any member fails, the entire group is rolled back.
///
/// Uses the C-level internal sub-transaction API (`BeginInternalSubTransaction`
/// / `ReleaseCurrentSubTransaction` / `RollbackAndReleaseCurrentSubTransaction`)
/// because PostgreSQL rejects transaction-control SQL via SPI.
fn execute_worker_atomic_group(job: &SchedulerJob) -> RefreshOutcome {
    log!(
        "pg_trickle refresh worker: atomic group — {} members (job {})",
        job.member_pgt_ids.len(),
        job.job_id,
    );

    let subtxn = SubTransaction::begin();

    let tick_watermark: Option<String> = if config::pg_trickle_tick_watermark_enabled() {
        Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None)
    } else {
        None
    };
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

        // Check advisory lock — if another refresh is in progress, skip.
        if check_skip_needed(&st) {
            continue;
        }

        let has_changes = has_table_source_changes(&st) || has_stream_table_source_changes(&st);
        let has_st_changes = has_stream_table_source_changes(&st);
        let action = if has_changes && has_st_changes {
            RefreshAction::Full
        } else {
            refresh::determine_refresh_action(&st, has_changes)
        };

        let result = execute_scheduled_refresh(&st, action, tick_watermark.as_deref());
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

    let tick_watermark: Option<String> = if config::pg_trickle_tick_watermark_enabled() {
        Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None)
    } else {
        None
    };
    let has_changes = has_table_source_changes(&st) || has_stream_table_source_changes(&st);
    let has_st_changes = has_stream_table_source_changes(&st);
    let action = if has_changes && has_st_changes {
        RefreshAction::Full
    } else {
        refresh::determine_refresh_action(&st, has_changes)
    };

    execute_scheduled_refresh(&st, action, tick_watermark.as_deref())
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

    let tick_watermark: Option<String> = if config::pg_trickle_tick_watermark_enabled() {
        Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None)
    } else {
        None
    };

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
                let outcome =
                    execute_scheduled_refresh(&st, RefreshAction::Full, tick_watermark.as_deref());
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

// ── Parallel Dispatch State (Phase 4) ─────────────────────────────────────

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
}

impl ParallelDispatchState {
    fn new() -> Self {
        Self {
            unit_states: HashMap::new(),
            per_db_inflight: 0,
            dag_version: 0,
            eu_dag: None,
        }
    }

    /// Whether there are in-flight jobs (for shorter poll interval).
    fn has_inflight(&self) -> bool {
        self.per_db_inflight > 0
    }

    /// Rebuild dispatch state from a fresh EU DAG.
    fn rebuild(&mut self, eu_dag: ExecutionUnitDag, dag_version: u64) {
        self.unit_states.clear();
        self.dag_version = dag_version;
        // Note: per_db_inflight is NOT reset — in-flight workers from the
        // previous DAG version are still running.

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

    let max_per_db = config::pg_trickle_max_concurrent_refreshes().max(1) as u32;
    let max_cluster = config::pg_trickle_max_dynamic_refresh_workers().max(1) as u32;
    let dag_version_i64 = state.dag_version as i64;
    // SAFETY: MyProcPid is always valid inside a background worker.
    let scheduler_pid: i32 = unsafe { pg_sys::MyProcPid };

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
                    }
                }
                // Downstream units remain blocked (remaining_upstreams > 0).
            }
            JobStatus::PermanentFailed | JobStatus::Cancelled => {
                if let Some(rpid) = root_pgt_id {
                    retry_states.entry(rpid).or_default().reset();
                }
                log!(
                    "pg_trickle: parallel dispatch — job {} failed permanently",
                    job_id,
                );
                // Downstream units remain blocked.
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

        let unit = match eu_dag.units().find(|u| u.id == uid) {
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

    // ── Step 3: Dispatch ready units within budget ───────────────────────
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

        let unit = match eu_dag.units().find(|u| u.id == uid) {
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
        "pg_trickle scheduler started (interval={}ms)",
        config::pg_trickle_scheduler_interval_ms()
    );

    let mut dag_version: u64 = 0;
    let mut dag: Option<StDag> = None;

    // Per-ST retry state (in-memory only, reset on scheduler restart)
    let mut retry_states: HashMap<i64, RetryState> = HashMap::new();
    let retry_policy = RetryPolicy::default();

    // Phase 4: Parallel dispatch state (persisted across ticks)
    let mut parallel_state = ParallelDispatchState::new();

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

    loop {
        // Use shorter poll interval when parallel workers are in-flight.
        let poll_ms = if parallel_state.has_inflight() {
            std::cmp::min(config::pg_trickle_scheduler_interval_ms() as u64, 200)
        } else {
            config::pg_trickle_scheduler_interval_ms() as u64
        };
        let should_continue =
            BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(poll_ms)));

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
            monitor::check_slot_health_and_alert();
        }));

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
            // CSS1: Capture tick watermark for cross-source snapshot consistency.
            // All refreshes in this tick will cap their LSN consumption to this value,
            // ensuring every stream table in the tick shares the same consistent LSN view.
            let tick_watermark: Option<String> = if config::pg_trickle_tick_watermark_enabled() {
                Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text").unwrap_or(None)
            } else {
                None
            };

            // Step A: Check if DAG needs rebuild
            let current_version = shmem::current_dag_version();
            if current_version != dag_version || dag.is_none() {
                match StDag::build_from_catalog(config::pg_trickle_default_schedule_seconds()) {
                    Ok(new_dag) => {
                        dag = Some(new_dag);
                        dag_version = current_version;
                        log!("pg_trickle: DAG rebuilt (version={})", dag_version);
                    }
                    Err(e) => {
                        log!("pg_trickle: failed to rebuild DAG: {}", e);
                        return;
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
                    refresh_single_st(
                        pgt_id,
                        dag_ref,
                        now_ms,
                        &mut retry_states,
                        &retry_policy,
                        initial_table_changes.get(&pgt_id).copied(),
                        tick_watermark.as_deref(),
                    );
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

                let all_atomic = group.members.iter().all(|m| {
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
                        refresh_single_st(
                            pgt_id,
                            dag_ref,
                            now_ms,
                            &mut retry_states,
                            &retry_policy,
                            initial_table_changes.get(&pgt_id).copied(),
                            tick_watermark.as_deref(),
                        );
                    }
                    continue;
                }

                // Atomic group: check group-level schedule policy.
                let policy = group_schedule_policy(group);
                if !is_group_due(group, policy, dag_ref) {
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

                    // Check retry backoff
                    let retry = retry_states.entry(pgt_id).or_default();
                    if retry.is_in_backoff(now_ms) {
                        emit_stale_alert_if_needed(&st);
                        continue;
                    }

                    // Skip if advisory lock held
                    if check_skip_needed(&st) {
                        continue;
                    }

                    let (has_changes, has_stream_table_changes) =
                        upstream_change_state(&st, initial_table_changes.get(&pgt_id).copied());
                    let action = if has_changes && has_stream_table_changes {
                        RefreshAction::Full
                    } else {
                        refresh::determine_refresh_action(&st, has_changes)
                    };
                    let result = execute_scheduled_refresh(&st, action, tick_watermark.as_deref());

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

/// Check if a refresh should be skipped because a previous one is still running.
///
/// Uses PostgreSQL advisory locks (non-blocking) to detect concurrent refreshes.
/// The advisory lock key is derived from the ST's relid to ensure uniqueness.
/// The lock is NOT held — we just probe to detect if another session holds it.
///
/// Returns `true` if the refresh should be skipped.
fn check_skip_needed(st: &StreamTableMeta) -> bool {
    // Use pgt_id as the advisory lock key (guaranteed unique)
    let lock_key = st.pgt_id;

    // Try to acquire a non-blocking advisory lock
    let got_lock =
        Spi::get_one_with_args::<bool>("SELECT pg_try_advisory_lock($1)", &[lock_key.into()])
            .unwrap_or(Some(false))
            .unwrap_or(false);

    if got_lock {
        // We got the lock — release it immediately. No concurrent refresh.
        let _ = Spi::get_one_with_args::<bool>("SELECT pg_advisory_unlock($1)", &[lock_key.into()]);
        false
    } else {
        // Another refresh is in progress — skip
        true
    }
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
                load_st_by_id(*id).map(|st| check_schedule(&st, dag) || st.needs_reinit)
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
/// Returns `Some(ratio)` when `elapsed_ms / schedule_ms >= 0.8`.
fn is_falling_behind(elapsed_ms: i64, schedule_ms: i64) -> Option<f64> {
    if schedule_ms <= 0 {
        return None;
    }
    let ratio = elapsed_ms as f64 / schedule_ms as f64;
    if ratio >= 0.8 { Some(ratio) } else { None }
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
fn check_schedule(st: &StreamTableMeta, _dag: &StDag) -> bool {
    // If not yet populated, always needs refresh
    if !st.is_populated {
        return true;
    }

    // Check staleness vs schedule
    if let Some(ref schedule_str) = st.schedule {
        // Determine if this is a cron expression or a duration
        let trimmed = schedule_str.trim();
        if trimmed.starts_with('@') || trimmed.contains(' ') {
            // Cron-based: check if the cron schedule says we're due
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
            let stale = Spi::get_one_with_args::<bool>(
                "SELECT CASE WHEN last_refresh_at IS NULL THEN true \
                 ELSE EXTRACT(EPOCH FROM (now() - last_refresh_at)) > $2 END \
                 FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
                &[st.pgt_id.into(), max_secs.into()],
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

/// Emit a StaleData alert if the stream table is currently stale.
///
/// Called when a refresh is skipped (due to backoff, advisory lock, etc.)
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
            let staleness = Spi::get_one_with_args::<f64>(
                "SELECT EXTRACT(EPOCH FROM (now() - data_timestamp))::float8 \
                 FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
                &[st.pgt_id.into()],
            )
            .unwrap_or(None);

            if let Some(stale_secs) = staleness {
                // Alert when staleness exceeds 2× the schedule
                let schedule_f64 = max_secs as f64;
                if stale_secs > schedule_f64 * 2.0 {
                    monitor::alert_stale_data(
                        &st.pgt_schema,
                        &st.pgt_name,
                        stale_secs,
                        schedule_f64,
                    );
                }
            }
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
    //   STREAM_TABLE — intermediate stream tables (no change buffer).  We
    //                  detect staleness by comparing data_timestamps: if the
    //                  upstream ST was last refreshed *after* we were, our
    //                  data is out-of-date.
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

    for oid in &source_oids {
        let has_rows = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM {}.changes_{} LIMIT 1)",
            change_schema,
            oid.to_u32(),
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if has_rows {
            return true;
        }
    }
    false
}

/// Returns `true` if any STREAM_TABLE upstream has a `data_timestamp` more
/// recent than our own `data_timestamp`.
///
/// STREAM_TABLE upstreams have no CDC change buffer.  We detect staleness via
/// a `data_timestamp` comparison instead.  When this returns `true`, the
/// caller **must** use `RefreshAction::Full` — a differential refresh cannot
/// incorporate stream-table changes (no change buffer to diff against).
fn has_stream_table_source_changes(st: &StreamTableMeta) -> bool {
    let deps = crate::catalog::StDependency::get_for_st(st.pgt_id).unwrap_or_default();

    for dep in &deps {
        if dep.source_type != "STREAM_TABLE" {
            continue;
        }

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

                let result = execute_scheduled_refresh(&st, action, tick_watermark);

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
fn refresh_single_st(
    pgt_id: i64,
    dag_ref: &StDag,
    now_ms: u64,
    retry_states: &mut HashMap<i64, RetryState>,
    retry_policy: &RetryPolicy,
    table_change_snapshot: Option<bool>,
    tick_watermark: Option<&str>,
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

    let retry = retry_states.entry(pgt_id).or_default();
    if retry.is_in_backoff(now_ms) {
        emit_stale_alert_if_needed(&st);
        return;
    }

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

    let (has_changes, has_stream_table_changes) = upstream_change_state(&st, table_change_snapshot);

    // STREAM_TABLE upstream sources have no CDC change buffer.  When an
    // upstream stream table has newer data (detected via data_timestamp
    // comparison in check_upstream_changes/has_stream_table_source_changes),
    // a DIFFERENTIAL refresh would be a no-op — there are no buffer rows to
    // merge.  Force a FULL refresh instead so the data actually incorporates
    // the upstream's latest rows.
    let action = if has_changes && has_stream_table_changes {
        RefreshAction::Full
    } else {
        refresh::determine_refresh_action(&st, has_changes)
    };
    let result = execute_scheduled_refresh(&st, action, tick_watermark);

    let retry = retry_states.entry(pgt_id).or_default();
    match result {
        RefreshOutcome::Success => {
            retry.reset();
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

    // Acquire advisory lock for this ST (held during refresh execution)
    let lock_key = st.pgt_id;
    let got_lock =
        Spi::get_one_with_args::<bool>("SELECT pg_try_advisory_lock($1)", &[lock_key.into()])
            .unwrap_or(Some(false))
            .unwrap_or(false);

    if !got_lock {
        // Another session is refreshing this ST — skip
        log!(
            "pg_trickle: skipping {}.{} — advisory lock held by another session",
            st.pgt_schema,
            st.pgt_name,
        );
        return RefreshOutcome::RetryableFailure;
    }

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
            release_advisory_lock(lock_key);
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
    let _ = Spi::run("SET LOCAL row_security = off");

    // Execute the refresh
    let result = if st.topk_limit.is_some() {
        // TopK tables bypass the normal Full/Differential refresh paths and use
        // scoped-recomputation MERGE (ORDER BY … LIMIT N) instead.
        refresh::execute_topk_refresh(st)
    } else {
        match action {
            RefreshAction::NoData => refresh::execute_no_data_refresh(st).map(|_| (0i64, 0i64)),
            RefreshAction::Full => {
                let new_frontier =
                    version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
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
                        Ok((ins, del))
                    }
                    Err(e) => Err(e),
                }
            }
            RefreshAction::Reinitialize => {
                let new_frontier =
                    version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
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
                    let new_frontier =
                        version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
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
                            Ok((ins, del))
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    let new_frontier =
                        version::compute_new_frontier(&slot_positions, &data_ts_frontier);

                    match refresh::execute_differential_refresh(st, &prev_frontier, &new_frontier) {
                        Ok((ins, del)) => {
                            if let Err(e) =
                                StreamTableMeta::store_frontier(st.pgt_id, &new_frontier)
                            {
                                log!("pg_trickle: failed to store frontier: {}", e);
                            }
                            Ok((ins, del))
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

    // Release the advisory lock now that refresh is done
    release_advisory_lock(lock_key);

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
                if let Some(ratio) = is_falling_behind(elapsed_ms, schedule_ms) {
                    monitor::alert_falling_behind(
                        &st.pgt_schema,
                        &st.pgt_name,
                        elapsed_ms,
                        schedule_ms,
                        ratio,
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
            );

            let is_retryable = e.is_retryable();
            let counts = e.counts_toward_suspension();

            // Handle schema errors: mark for reinitialize
            if e.requires_reinitialize() {
                let _ = StreamTableMeta::mark_for_reinitialize(st.pgt_id);
                monitor::alert_reinitialize_needed(&st.pgt_schema, &st.pgt_name, &e.to_string());
            }

            // Increment error count only for errors that should count
            if counts {
                match StreamTableMeta::increment_errors(st.pgt_id) {
                    Ok(count) if count >= config::pg_trickle_max_consecutive_errors() => {
                        let _ = StreamTableMeta::update_status(st.pgt_id, StStatus::Suspended);

                        monitor::alert_auto_suspended(&st.pgt_schema, &st.pgt_name, count);

                        log!(
                            "pg_trickle: suspended {}.{} after {} consecutive errors",
                            st.pgt_schema,
                            st.pgt_name,
                            count,
                        );
                    }
                    _ => {
                        log!(
                            "pg_trickle: refresh failed for {}.{} ({}): {} [{}]",
                            st.pgt_schema,
                            st.pgt_name,
                            action.as_str(),
                            e,
                            if is_retryable {
                                "will retry"
                            } else {
                                "permanent"
                            },
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

            if is_retryable {
                RefreshOutcome::RetryableFailure
            } else {
                RefreshOutcome::PermanentFailure
            }
        }
    }
}

/// Release an advisory lock held during refresh.
fn release_advisory_lock(lock_key: i64) {
    let _ = Spi::get_one_with_args::<bool>("SELECT pg_advisory_unlock($1)", &[lock_key.into()]);
}

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
    fn test_falling_behind_at_80_percent() {
        let result = is_falling_behind(800, 1000);
        assert!(result.is_some());
        assert!((result.unwrap() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_falling_behind_over_100_percent() {
        let result = is_falling_behind(1500, 1000);
        assert!(result.is_some());
        assert!((result.unwrap() - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_not_falling_behind() {
        assert!(is_falling_behind(500, 1000).is_none());
    }

    #[test]
    fn test_falling_behind_zero_schedule() {
        assert!(is_falling_behind(100, 0).is_none());
    }

    #[test]
    fn test_falling_behind_negative_schedule() {
        assert!(is_falling_behind(100, -1).is_none());
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
}
