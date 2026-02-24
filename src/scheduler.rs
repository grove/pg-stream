//! Background worker scheduler for pgstream.
//!
//! The scheduler runs as a PostgreSQL background worker, periodically checking
//! for stream tables that need refreshing based on their schedule.
//!
//! # Architecture
//! - Registered in `_PG_init()` via `BackgroundWorkerBuilder`
//! - Wakes every `pg_stream.scheduler_interval_ms` milliseconds
//! - Reads shared memory to detect DAG changes
//! - Consumes CDC changes, determines refresh needs, executes refreshes
//!
//! # Error Handling & Resilience (Phase 10)
//! - **Advisory locks**: prevent concurrent refreshes of the same ST
//! - **Retry with backoff**: retryable errors get exponential backoff per ST
//! - **Skip mechanism**: if a ST refresh is already running, skip it gracefully
//! - **Crash recovery**: on startup, mark interrupted RUNNING records as FAILED
//! - **Error classification**: only retryable errors trigger retry; user/schema
//!   errors fail immediately

use pgrx::bgworkers::*;
use pgrx::prelude::*;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;

use crate::catalog::{RefreshRecord, StreamTableMeta};
use crate::cdc;
use crate::config;
use crate::dag::{DtDag, DtStatus, NodeId};
use crate::error::{RetryPolicy, RetryState};
use crate::monitor;
use crate::refresh::{self, RefreshAction};
use crate::shmem;
use crate::version;
use crate::wal_decoder;

/// Register the scheduler background worker.
///
/// Called from `_PG_init()` when loaded via `shared_preload_libraries`.
pub fn register_scheduler_worker() {
    BackgroundWorkerBuilder::new("pg_stream scheduler")
        .set_function("pg_stream_scheduler_main")
        .set_library("pg_stream")
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
pub extern "C-unwind" fn pg_stream_scheduler_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    BackgroundWorker::connect_worker_to_spi(Some("postgres"), None);

    log!(
        "pg_stream scheduler started (interval={}ms)",
        config::pg_stream_scheduler_interval_ms()
    );

    let mut dag_version: u64 = 0;
    let mut dag: Option<DtDag> = None;

    // Per-ST retry state (in-memory only, reset on scheduler restart)
    let mut retry_states: HashMap<i64, RetryState> = HashMap::new();
    let retry_policy = RetryPolicy::default();

    // Phase 10: Crash recovery — mark any interrupted RUNNING records
    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        recover_from_crash();
    }));

    loop {
        // Wait for the configured interval or a signal.
        let should_continue = BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(
            config::pg_stream_scheduler_interval_ms() as u64,
        )));

        if !should_continue {
            // SIGTERM received — shut down gracefully.
            log!("pg_stream scheduler shutting down");
            break;
        }

        if !config::pg_stream_enabled() {
            continue;
        }

        let now_ms = current_epoch_ms();

        // Run the scheduler tick inside a transaction
        BackgroundWorker::transaction(AssertUnwindSafe(|| {
            // Step A: Check if DAG needs rebuild
            let current_version = shmem::current_dag_version();
            if current_version != dag_version || dag.is_none() {
                match DtDag::build_from_catalog(config::pg_stream_min_schedule_seconds()) {
                    Ok(new_dag) => {
                        dag = Some(new_dag);
                        dag_version = current_version;
                        log!("pg_stream: DAG rebuilt (version={})", dag_version);
                    }
                    Err(e) => {
                        log!("pg_stream: failed to rebuild DAG: {}", e);
                        return;
                    }
                }
            }

            let dag_ref = match &dag {
                Some(d) => d,
                None => return,
            };

            // Step B: Get topological refresh order
            let ordered = match dag_ref.topological_order() {
                Ok(order) => order,
                Err(e) => {
                    log!("pg_stream: DAG has cycles: {}", e);
                    return;
                }
            };

            // Step C: Check each ST for schedule and refresh if needed
            for node_id in &ordered {
                let pgs_id = match node_id {
                    NodeId::StreamTable(id) => *id,
                    _ => continue,
                };

                // Load fresh ST metadata
                let dt = match load_dt_by_id(pgs_id) {
                    Some(dt) => dt,
                    None => continue,
                };

                // Skip non-active STs
                if dt.status != DtStatus::Active && dt.status != DtStatus::Initializing {
                    continue;
                }

                // Phase 10: Check retry backoff — skip if in cooldown
                let retry = retry_states.entry(pgs_id).or_default();
                if retry.is_in_backoff(now_ms) {
                    continue;
                }

                // Check if ST exceeds its effective schedule
                let needs_refresh = check_schedule(&dt, dag_ref);
                if !needs_refresh && !dt.needs_reinit {
                    continue;
                }

                // Phase 10: Skip mechanism — check advisory lock
                if check_skip_needed(&dt) {
                    log!(
                        "pg_stream: skipping {}.{} — previous refresh still running",
                        dt.pgs_schema,
                        dt.pgs_name,
                    );
                    continue;
                }

                // Determine the refresh action
                let has_changes = check_upstream_changes(&dt);
                let action = refresh::determine_refresh_action(&dt, has_changes);

                // Execute refresh with retry-aware error handling
                let result = execute_scheduled_refresh(&dt, action);

                // Update retry state based on result
                let retry = retry_states.entry(pgs_id).or_default();
                match result {
                    RefreshOutcome::Success => {
                        retry.reset();
                    }
                    RefreshOutcome::RetryableFailure => {
                        let will_retry = retry.record_failure(&retry_policy, now_ms);
                        if will_retry {
                            log!(
                                "pg_stream: {}.{} will retry in {}ms (attempt {}/{})",
                                dt.pgs_schema,
                                dt.pgs_name,
                                retry_policy.backoff_ms(retry.attempts - 1),
                                retry.attempts,
                                retry_policy.max_attempts,
                            );
                        }
                        // If max attempts exhausted, the error has already been
                        // counted toward consecutive_errors and may trigger suspension
                    }
                    RefreshOutcome::PermanentFailure => {
                        // Non-retryable: don't use backoff, let consecutive_errors
                        // handle suspension
                        retry.reset();
                    }
                }
            }

            // Step D: Advance WAL transitions (trigger → WAL migration)
            // Check and progress any CDC mode transitions for source tables.
            // This polls WAL changes for TRANSITIONING/WAL sources and checks
            // transition completion or timeout.
            {
                let change_schema = config::pg_stream_change_buffer_schema();
                if let Err(e) = wal_decoder::advance_wal_transitions(&change_schema) {
                    log!("pg_stream: WAL transition advancement error: {}", e);
                }
            }

            // Step E: Check replication slot health and emit alerts
            monitor::check_slot_health_and_alert();

            // Step F: Prune retry states for STs that no longer exist
            // (avoid accumulating stale state)
            let active_ids: std::collections::HashSet<i64> = ordered
                .iter()
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
            "UPDATE pgstream.pgs_refresh_history \
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
                log!("pg_stream: crash recovery query failed: {}", e);
                0
            }
        }
    });

    if updated > 0 {
        log!(
            "pg_stream: crash recovery — marked {} interrupted refresh(es) as FAILED",
            updated
        );
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
fn check_skip_needed(dt: &StreamTableMeta) -> bool {
    // Use pgs_id as the advisory lock key (guaranteed unique)
    let lock_key = dt.pgs_id;

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

/// Load a stream table by its pgs_id, or return None if not found.
fn load_dt_by_id(pgs_id: i64) -> Option<StreamTableMeta> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT pgs_id, pgs_relid, pgs_name, pgs_schema, defining_query, \
                 schedule, refresh_mode, status, is_populated, data_timestamp, \
                 consecutive_errors, needs_reinit, frontier \
                 FROM pgstream.pgs_stream_tables WHERE pgs_id = $1",
                None,
                &[pgs_id.into()],
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
            .find(|dt| dt.pgs_id == pgs_id)
    })
}

/// Check if a ST is stale (staleness exceeds effective schedule or cron is due).
fn check_schedule(dt: &StreamTableMeta, _dag: &DtDag) -> bool {
    // If not yet populated, always needs refresh
    if !dt.is_populated {
        return true;
    }

    // Check staleness vs schedule
    if let Some(ref schedule_str) = dt.schedule {
        // Determine if this is a cron expression or a duration
        let trimmed = schedule_str.trim();
        if trimmed.starts_with('@') || trimmed.contains(' ') {
            // Cron-based: check if the cron schedule says we're due
            let last_refresh_epoch = Spi::get_one_with_args::<f64>(
                "SELECT EXTRACT(EPOCH FROM last_refresh_at) FROM pgstream.pgs_stream_tables WHERE pgs_id = $1",
                &[dt.pgs_id.into()],
            )
            .unwrap_or(None)
            .map(|e| e as i64);

            return crate::api::cron_is_due(trimmed, last_refresh_epoch);
        }

        // Duration-based: compare staleness against parsed seconds
        if let Ok(max_secs) = crate::api::parse_duration(trimmed) {
            let stale = Spi::get_one_with_args::<bool>(
                "SELECT CASE WHEN data_timestamp IS NULL THEN true \
                 ELSE EXTRACT(EPOCH FROM (now() - data_timestamp)) > $2 END \
                 FROM pgstream.pgs_stream_tables WHERE pgs_id = $1",
                &[dt.pgs_id.into(), max_secs.into()],
            )
            .unwrap_or(Some(false))
            .unwrap_or(false);

            return stale;
        }

        // Unparseable schedule — log a warning and skip
        pgrx::warning!(
            "pg_stream: could not parse schedule '{}' for pgs_id={}; skipping schedule check",
            schedule_str,
            dt.pgs_id
        );
        return false;
    }

    // CALCULATED STs: refresh when upstream refreshed
    false
}

/// Check if any upstream source has pending changes.
fn check_upstream_changes(dt: &StreamTableMeta) -> bool {
    // With trigger-based CDC, changes are written directly to buffer tables.
    // Check if any buffer table for this ST's sources has pending rows.
    let change_schema = config::pg_stream_change_buffer_schema();

    // Get source OIDs for this ST
    let source_oids = get_source_oids_for_dt(dt.pgs_id);

    for oid in &source_oids {
        // Check if the buffer table has any rows
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

    // If no CDC tracking yet, assume changes exist (conservative)
    if !dt.is_populated {
        return true;
    }

    false
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
fn execute_scheduled_refresh(dt: &StreamTableMeta, action: RefreshAction) -> RefreshOutcome {
    let start_instant = std::time::Instant::now();

    let now = Spi::get_one::<TimestampWithTimeZone>("SELECT now()")
        .unwrap_or(None)
        .unwrap_or_else(|| {
            pgrx::warning!("now() returned NULL in scheduler");
            TimestampWithTimeZone::try_from(0i64).unwrap()
        });

    // Acquire advisory lock for this ST (held during refresh execution)
    let lock_key = dt.pgs_id;
    let got_lock =
        Spi::get_one_with_args::<bool>("SELECT pg_try_advisory_lock($1)", &[lock_key.into()])
            .unwrap_or(Some(false))
            .unwrap_or(false);

    if !got_lock {
        // Another session is refreshing this ST — skip
        log!(
            "pg_stream: skipping {}.{} — advisory lock held by another session",
            dt.pgs_schema,
            dt.pgs_name,
        );
        return RefreshOutcome::RetryableFailure;
    }

    // Record refresh start
    // Compute freshness_deadline for duration-based schedules:
    // deadline = data_timestamp + schedule_seconds (when data becomes stale)
    let freshness_deadline = compute_freshness_deadline(dt);
    let refresh_id = RefreshRecord::insert(
        dt.pgs_id,
        now,
        action.as_str(),
        "RUNNING",
        0,
        0,
        None,
        Some("SCHEDULER"),
        freshness_deadline,
    );

    let refresh_id = match refresh_id {
        Ok(id) => id,
        Err(e) => {
            log!(
                "pg_stream: failed to record refresh start for {}.{}: {}",
                dt.pgs_schema,
                dt.pgs_name,
                e
            );
            release_advisory_lock(lock_key);
            return RefreshOutcome::RetryableFailure;
        }
    };

    // Compute frontier information for this refresh
    let source_oids = get_source_oids_for_dt(dt.pgs_id);
    let slot_positions = match cdc::get_slot_positions(&source_oids) {
        Ok(pos) => pos,
        Err(e) => {
            log!(
                "pg_stream: failed to get slot positions for {}.{}: {}",
                dt.pgs_schema,
                dt.pgs_name,
                e
            );
            std::collections::HashMap::new()
        }
    };

    // Select target data timestamp
    let schedule_secs = dt
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
        .unwrap()
        .as_secs();
    let data_ts_frontier = format!("{}Z", now_secs);

    // Execute the refresh
    let result = match action {
        RefreshAction::NoData => refresh::execute_no_data_refresh(dt).map(|_| (0i64, 0i64)),
        RefreshAction::Full => {
            let new_frontier =
                version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
            match refresh::execute_full_refresh(dt) {
                Ok((ins, del)) => {
                    if let Err(e) = StreamTableMeta::store_frontier(dt.pgs_id, &new_frontier) {
                        log!(
                            "pg_stream: failed to store frontier for {}.{}: {}",
                            dt.pgs_schema,
                            dt.pgs_name,
                            e
                        );
                    }
                    Ok((ins, del))
                }
                Err(e) => Err(e),
            }
        }
        RefreshAction::Reinitialize => {
            let new_frontier =
                version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
            match refresh::execute_reinitialize_refresh(dt) {
                Ok((ins, del)) => {
                    if let Err(e) = StreamTableMeta::store_frontier(dt.pgs_id, &new_frontier) {
                        log!(
                            "pg_stream: failed to store frontier for {}.{}: {}",
                            dt.pgs_schema,
                            dt.pgs_name,
                            e
                        );
                    }
                    Ok((ins, del))
                }
                Err(e) => Err(e),
            }
        }
        RefreshAction::Differential => {
            let prev_frontier = dt.frontier.clone().unwrap_or_default();

            if prev_frontier.is_empty() {
                log!(
                    "pg_stream: no previous frontier for {}.{}, doing FULL refresh",
                    dt.pgs_schema,
                    dt.pgs_name
                );
                let new_frontier =
                    version::compute_initial_frontier(&slot_positions, &data_ts_frontier);
                match refresh::execute_full_refresh(dt) {
                    Ok((ins, del)) => {
                        if let Err(e) = StreamTableMeta::store_frontier(dt.pgs_id, &new_frontier) {
                            log!("pg_stream: failed to store frontier: {}", e);
                        }
                        Ok((ins, del))
                    }
                    Err(e) => Err(e),
                }
            } else {
                let new_frontier =
                    version::compute_new_frontier(&slot_positions, &data_ts_frontier);

                match refresh::execute_differential_refresh(dt, &prev_frontier, &new_frontier) {
                    Ok((ins, del)) => {
                        if let Err(e) = StreamTableMeta::store_frontier(dt.pgs_id, &new_frontier) {
                            log!("pg_stream: failed to store frontier: {}", e);
                        }
                        Ok((ins, del))
                    }
                    Err(e) => {
                        log!(
                            "pg_stream: differential refresh failed for {}.{}: {}, will reinitialize on next cycle",
                            dt.pgs_schema,
                            dt.pgs_name,
                            e
                        );
                        let _ = StreamTableMeta::mark_for_reinitialize(dt.pgs_id);
                        Err(e)
                    }
                }
            }
        }
    };

    // Release the advisory lock now that refresh is done
    release_advisory_lock(lock_key);

    let elapsed_ms = start_instant.elapsed().as_millis() as i64;

    // Record refresh completion and determine outcome
    match result {
        Ok((rows_inserted, rows_deleted)) => {
            let _ =
                RefreshRecord::complete(refresh_id, "COMPLETED", rows_inserted, rows_deleted, None);

            let _ = StreamTableMeta::update_after_refresh(dt.pgs_id, now, rows_inserted);

            monitor::alert_refresh_completed(
                &dt.pgs_schema,
                &dt.pgs_name,
                action.as_str(),
                rows_inserted,
                rows_deleted,
                elapsed_ms,
            );

            log!(
                "pg_stream: refreshed {}.{} ({}, +{} -{} rows, {}ms)",
                dt.pgs_schema,
                dt.pgs_name,
                action.as_str(),
                rows_inserted,
                rows_deleted,
                elapsed_ms,
            );

            RefreshOutcome::Success
        }
        Err(e) => {
            let _ = RefreshRecord::complete(refresh_id, "FAILED", 0, 0, Some(&e.to_string()));

            monitor::alert_refresh_failed(
                &dt.pgs_schema,
                &dt.pgs_name,
                action.as_str(),
                &e.to_string(),
            );

            let is_retryable = e.is_retryable();
            let counts = e.counts_toward_suspension();

            // Handle schema errors: mark for reinitialize
            if e.requires_reinitialize() {
                let _ = StreamTableMeta::mark_for_reinitialize(dt.pgs_id);
                monitor::alert_reinitialize_needed(&dt.pgs_schema, &dt.pgs_name, &e.to_string());
            }

            // Increment error count only for errors that should count
            if counts {
                match StreamTableMeta::increment_errors(dt.pgs_id) {
                    Ok(count) if count >= config::pg_stream_max_consecutive_errors() => {
                        let _ = StreamTableMeta::update_status(dt.pgs_id, DtStatus::Suspended);

                        monitor::alert_auto_suspended(&dt.pgs_schema, &dt.pgs_name, count);

                        log!(
                            "pg_stream: suspended {}.{} after {} consecutive errors",
                            dt.pgs_schema,
                            dt.pgs_name,
                            count,
                        );
                    }
                    _ => {
                        log!(
                            "pg_stream: refresh failed for {}.{} ({}): {} [{}]",
                            dt.pgs_schema,
                            dt.pgs_name,
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
                    "pg_stream: refresh skipped for {}.{}: {}",
                    dt.pgs_schema,
                    dt.pgs_name,
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
fn get_source_oids_for_dt(pgs_id: i64) -> Vec<pg_sys::Oid> {
    use crate::catalog::DtDependency;

    DtDependency::get_for_dt(pgs_id)
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
fn compute_freshness_deadline(dt: &StreamTableMeta) -> Option<TimestampWithTimeZone> {
    let schedule_str = dt.schedule.as_deref()?;

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
         FROM pgstream.pgs_stream_tables WHERE pgs_id = $1"
    );
    Spi::get_one_with_args::<TimestampWithTimeZone>(&deadline_sql, &[dt.pgs_id.into()])
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
}
