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

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;

use crate::catalog::{RefreshRecord, StreamTableMeta};
use crate::cdc;
use crate::config;
use crate::dag::{DiamondConsistency, DiamondSchedulePolicy, NodeId, StDag, StStatus};
use crate::error::{RetryPolicy, RetryState};
use crate::monitor;
use crate::refresh::{self, RefreshAction};
use crate::shmem;
use crate::version;
use crate::wal_decoder;

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
                     WHERE application_name = 'pg_trickle scheduler' \
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

        // If any backend bumped the DAG signal (CREATE EXTENSION, create_st,
        // etc.) since our last loop, clear the skip cache so we re-probe all
        // databases immediately.
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
        if !BackgroundWorker::wait_latch(Some(std::time::Duration::from_secs(10))) {
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

    // Phase 10: Crash recovery — mark any interrupted RUNNING records
    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        recover_from_crash();
    }));

    // EC-20: Post-restart CDC TRANSITIONING health check.
    // If the scheduler was restarted during an active TRIGGER→WAL transition,
    // verify the transition is still valid. Invalid transitions (stale slot,
    // missing publication) are rolled back to TRIGGER mode.
    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        check_cdc_transition_health();
    }));

    loop {
        // Wait for the configured interval or a signal.
        let should_continue = BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(
            config::pg_trickle_scheduler_interval_ms() as u64,
        )));

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

        // Run the scheduler tick inside a transaction
        BackgroundWorker::transaction(AssertUnwindSafe(|| {
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
            if let Err(e) = dag_ref.topological_order() {
                log!("pg_trickle: DAG has cycles: {}", e);
                return;
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
                    refresh_single_st(
                        pgt_id,
                        dag_ref,
                        now_ms,
                        &mut retry_states,
                        &retry_policy,
                        initial_table_changes.get(&pgt_id).copied(),
                    );
                    continue;
                }

                // Multi-member group: check if all members have diamond_consistency = 'atomic'.
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
                let old_cxt = unsafe { pg_sys::CurrentMemoryContext };
                let old_owner = unsafe { pg_sys::CurrentResourceOwner };
                unsafe { pg_sys::BeginInternalSubTransaction(std::ptr::null()) };

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
                    let result = execute_scheduled_refresh(&st, action);

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
                    // SAFETY: Commits the sub-transaction and restores context.
                    unsafe {
                        pg_sys::ReleaseCurrentSubTransaction();
                        pg_sys::MemoryContextSwitchTo(old_cxt);
                        pg_sys::CurrentResourceOwner = old_owner;
                    }
                    // Reset retry states for all refreshed members
                    for id in &refreshed_ids {
                        retry_states.entry(*id).or_default().reset();
                    }
                } else {
                    // SAFETY: Rolls back the sub-transaction and restores context.
                    unsafe {
                        pg_sys::RollbackAndReleaseCurrentSubTransaction();
                        pg_sys::MemoryContextSwitchTo(old_cxt);
                        pg_sys::CurrentResourceOwner = old_owner;
                    }
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

/// Returns `true` if any TABLE-type upstream source has rows in its CDC change
/// buffer that have not yet been consumed by a differential refresh.
fn has_table_source_changes(st: &StreamTableMeta) -> bool {
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
) {
    let st = match load_st_by_id(pgt_id) {
        Some(st) => st,
        None => return,
    };

    if st.status != StStatus::Active && st.status != StStatus::Initializing {
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
    let result = execute_scheduled_refresh(&st, action);

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
fn execute_scheduled_refresh(st: &StreamTableMeta, action: RefreshAction) -> RefreshOutcome {
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
    let slot_positions = match cdc::get_slot_positions(&source_oids) {
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

/// Get the source OIDs (base table OIDs) for a given ST.
fn get_source_oids_for_st(pgt_id: i64) -> Vec<pg_sys::Oid> {
    use crate::catalog::StDependency;

    StDependency::get_for_st(pgt_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|dep| dep.source_type == "TABLE")
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
}
