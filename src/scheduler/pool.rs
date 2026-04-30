//! Pool module: persistent worker pool management for the scheduler.
//!
//! Contains helpers for registering, launching, and polling the persistent
//! background worker pool introduced in SCAL-5 (v0.25.0).

use pgrx::bgworkers::*;
use pgrx::prelude::*;
use std::panic::AssertUnwindSafe;

use crate::catalog::SchedulerJob;
use crate::config;
use crate::shmem;

use super::RefreshOutcome;
use super::{
    execute_worker_atomic_group, execute_worker_cyclic_scc, execute_worker_fused_chain,
    execute_worker_immediate_closure, execute_worker_singleton,
};

/// SCAL-5: Return the configured persistent pool size.
///
/// Returns 0 (the default) when the persistent worker pool is disabled.
pub(crate) fn configured_worker_pool_size() -> usize {
    config::pg_trickle_worker_pool_size().max(0) as usize
}

/// SCAL-5: Persistent worker pool coordination.
///
/// Returns the number of currently active pool workers from shmem.
pub(crate) fn pool_worker_count() -> u32 {
    shmem::active_worker_count()
}

/// Check if the persistent worker pool is enabled.
pub(crate) fn is_pool_enabled() -> bool {
    configured_worker_pool_size() > 0
}

/// SCAL-5 (v0.25.0): Spawn persistent pool workers for a database.
///
/// Called by the per-database scheduler after connecting to a target DB.
/// When `pg_trickle.worker_pool_size > 0`, registers N auto-restart BGWs
/// that loop on the job queue instead of being spawned and de-registered
/// each tick.  Workers use a 100 ms poll loop and handle `SIGTERM` cleanly.
///
/// If `worker_pool_size = 0` (default), this function is a no-op.
pub(crate) fn spawn_persistent_pool_workers(db_name: &str) {
    let pool_size = config::pg_trickle_worker_pool_size();
    if pool_size <= 0 {
        return;
    }

    for i in 0..pool_size as u32 {
        // Pack "db_name\0worker_index" into bgw_extra.
        let extra = format!("{db_name}\0{i}");
        match BackgroundWorkerBuilder::new("pg_trickle pool worker")
            .set_function("pg_trickle_pool_worker_main")
            .set_library("pg_trickle")
            .enable_spi_access()
            .set_extra(&extra)
            .set_restart_time(Some(std::time::Duration::from_secs(5)))
            .load_dynamic()
        {
            Ok(_) => {
                log!("pg_trickle: spawned pool worker {} for db '{}'", i, db_name);
            }
            Err(_) => {
                warning!("pg_trickle: failed to register pool worker {i} for db '{db_name}'");
            }
        }
    }
}

/// SCAL-5: Entry point for a persistent pool worker.
///
/// Loops until `SIGTERM`, polling the job queue and executing QUEUED jobs.
///
/// # Safety
/// Called directly by PostgreSQL as a background worker entry point.
#[pg_guard]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_trickle_pool_worker_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    let extra = BackgroundWorker::get_extra();
    // extra = "db_name\0worker_index"
    let db_name = extra.split('\0').next().unwrap_or("postgres").to_string();
    let worker_idx: u32 = extra
        .split('\0')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    BackgroundWorker::connect_worker_to_spi(Some(&db_name), None);

    log!(
        "pg_trickle pool worker {}: started (db='{}')",
        worker_idx,
        db_name,
    );

    // Acquire a worker token (shared with dynamic workers for the cluster budget).
    let max_workers = config::pg_trickle_max_dynamic_refresh_workers().max(1) as u32;
    if !shmem::try_acquire_worker_token(max_workers) {
        log!(
            "pg_trickle pool worker {}: could not acquire token — cluster budget exhausted, exiting",
            worker_idx,
        );
        return;
    }

    // Main poll loop: pick up and execute QUEUED jobs.
    loop {
        // Check for SIGTERM signal (returns false when signal received).
        if !BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(0))) {
            break;
        }

        // A41-4: Process any pending SIGHUP config reload so that GUC
        // changes (e.g. pg_trickle.enabled toggled back to on) take effect
        // without requiring a process restart.
        unsafe {
            if pg_sys::ConfigReloadPending != 0 {
                pg_sys::ConfigReloadPending = 0;
                pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP);
            }
        }

        // A41-4: Check pg_trickle.enabled before claiming any job.
        // When the extension is disabled, defer all work and sleep until
        // re-enabled to avoid processing jobs while maintenance is in progress.
        if !config::pg_trickle_enabled() {
            let ok = BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(500)));
            if !ok {
                // SIGTERM received during sleep.
                break;
            }
            continue;
        }

        // Try to claim one QUEUED job.
        let claimed = execute_pool_worker_tick(&db_name, worker_idx);

        if !claimed {
            // No work available — sleep 100 ms.
            let ok = BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(100)));
            if !ok {
                // SIGTERM received during sleep.
                break;
            }
        }
    }

    shmem::release_worker_token();
    log!(
        "pg_trickle pool worker {}: exiting (db='{}')",
        worker_idx,
        db_name,
    );
}

/// SCAL-5: Execute one pool worker tick: claim and run one QUEUED job.
///
/// Returns `true` if a job was claimed and executed, `false` if the queue
/// is empty.
fn execute_pool_worker_tick(db_name: &str, worker_idx: u32) -> bool {
    // Claim one QUEUED job for this db.
    let job_id = match BackgroundWorker::transaction(AssertUnwindSafe(|| {
        Spi::get_one::<i64>(&format!(
            "UPDATE pgtrickle.pgt_scheduler_jobs \
             SET status = 'RUNNING', \
                 started_at = now(), \
                 worker_pid = pg_backend_pid() \
             WHERE job_id = ( \
               SELECT job_id FROM pgtrickle.pgt_scheduler_jobs \
               WHERE status = 'QUEUED' \
                 AND db_name = '{db}' \
               ORDER BY enqueued_at \
               LIMIT 1 \
               FOR UPDATE SKIP LOCKED \
             ) \
             RETURNING job_id",
            db = db_name.replace('\'', "''"),
        ))
    })) {
        Ok(Some(id)) => id,
        _ => return false,
    };

    log!(
        "pg_trickle pool worker {}: executing job_id={}",
        worker_idx,
        job_id,
    );

    // Load the job.
    let job =
        match BackgroundWorker::transaction(AssertUnwindSafe(|| SchedulerJob::get_by_id(job_id))) {
            Ok(Some(j)) => j,
            _ => return true, // job was claimed, but couldn't load it — skip
        };

    // Execute the job using the same dispatch as the dynamic refresh worker.
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
                        "pg_trickle pool worker {}: unknown unit_kind '{}' for job {}",
                        worker_idx,
                        job.unit_kind,
                        job_id,
                    );
                    RefreshOutcome::PermanentFailure
                }
            }
        }))
    }));

    let status = match outcome {
        Ok(RefreshOutcome::Success) => "COMPLETED",
        _ => "FAILED",
    };

    // Mark job complete.
    let _ = BackgroundWorker::transaction(AssertUnwindSafe(|| {
        Spi::run(&format!(
            "UPDATE pgtrickle.pgt_scheduler_jobs \
             SET status = '{status}', \
                 completed_at = now() \
             WHERE job_id = {job_id}",
        ))
    }));

    true
}
