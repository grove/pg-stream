//! Shared memory structures for scheduler↔backend coordination.
//!
//! The scheduler background worker and user sessions communicate via shared
//! memory using lightweight locks (`PgLwLock`) and atomic variables (`PgAtomic`).

use pgrx::prelude::*;
use pgrx::{PGRXSharedMemory, PgAtomic, PgLwLock, pg_shmem_init};
use std::sync::atomic::{AtomicU32, AtomicU64};

/// Shared state visible to both the scheduler and user backends.
///
/// Protected by `PGS_STATE` lightweight lock for concurrent access.
#[derive(Copy, Clone, Default)]
pub struct PgTrickleSharedState {
    /// Incremented when the DAG changes (create/alter/drop ST).
    pub dag_version: u64,
    /// PID of the scheduler background worker (0 if not running).
    pub scheduler_pid: i32,
    /// Whether the scheduler is currently running.
    pub scheduler_running: bool,
    /// Unix timestamp (seconds) of the scheduler's last wake cycle.
    pub last_scheduler_wake: i64,
}

// SAFETY: PgTrickleSharedState is Copy + Clone + Default and contains only
// primitive types, making it safe for shared memory access under PgLwLock.
unsafe impl PGRXSharedMemory for PgTrickleSharedState {}

/// Lightweight-lock–protected shared state.
// SAFETY: PgLwLock::new requires a static CStr name for the lock.
pub static PGS_STATE: PgLwLock<PgTrickleSharedState> =
    unsafe { PgLwLock::new(c"pg_trickle_state") };

/// Atomic signal for DAG rebuild. Backends increment this when creating,
/// altering, or dropping stream tables. The scheduler compares its local
/// version to detect changes.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static DAG_REBUILD_SIGNAL: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_dag_signal") };

/// Atomic generation counter for cross-session cache invalidation (G8.1).
///
/// Incremented by any backend that modifies stream table metadata in a way
/// that would invalidate delta/MERGE template caches (e.g., DDL on source
/// tables that triggers reinitialize). Each backend tracks its last-seen
/// generation and flushes its thread-local caches when the shared value
/// advances.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static CACHE_GENERATION: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_cache_gen") };

/// Cluster-wide counter of active dynamic refresh workers.
///
/// Coordinators increment this before spawning a worker and workers decrement
/// on exit. Used together with the `max_dynamic_refresh_workers` GUC to
/// enforce a cluster-wide worker budget.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static ACTIVE_REFRESH_WORKERS: PgAtomic<AtomicU32> =
    unsafe { PgAtomic::new(c"pg_trickle_active_workers") };

/// Epoch counter incremented each time worker tokens are reconciled after a
/// crash or abnormal exit. Allows coordinators to detect that reconciliation
/// happened and re-check their in-flight job state.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static RECONCILE_EPOCH: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_reconcile_epoch") };

/// Register shared memory allocations. Called from `_PG_init()`.
pub fn init_shared_memory() {
    pg_shmem_init!(PGS_STATE);
    pg_shmem_init!(DAG_REBUILD_SIGNAL);
    pg_shmem_init!(CACHE_GENERATION);
    pg_shmem_init!(ACTIVE_REFRESH_WORKERS);
    pg_shmem_init!(RECONCILE_EPOCH);
    SHMEM_INITIALIZED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Signal the scheduler to rebuild the dependency DAG.
///
/// Called by API functions (create/alter/drop) after modifying catalog entries.
/// No-op if shared memory is not initialized (i.e., extension not in
/// `shared_preload_libraries`).
pub fn signal_dag_rebuild() {
    // Guard: PgAtomic is only initialized when loaded via shared_preload_libraries.
    // When loaded dynamically (CREATE EXTENSION without shared_preload), the
    // scheduler and shared memory are unavailable. Just skip the signal.
    if !is_shmem_available() {
        return;
    }
    DAG_REBUILD_SIGNAL
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Read the current DAG rebuild signal value.
pub fn current_dag_version() -> u64 {
    if !is_shmem_available() {
        return 0;
    }
    DAG_REBUILD_SIGNAL
        .get()
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Bump the shared cache generation counter.
///
/// Called by DDL paths that invalidate delta/MERGE template caches:
/// - Source table DDL causing stream table reinitialize
/// - Stream table drop
///
/// No-op if shared memory is not initialized.
pub fn bump_cache_generation() {
    if !is_shmem_available() {
        return;
    }
    CACHE_GENERATION
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Read the current shared cache generation value.
///
/// Backends compare this against their local generation to detect if
/// caches need flushing.
pub fn current_cache_generation() -> u64 {
    if !is_shmem_available() {
        return 0;
    }
    CACHE_GENERATION
        .get()
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Check if shared memory has been initialized.
///
/// Returns `false` when the extension was loaded via `CREATE EXTENSION`
/// without being listed in `shared_preload_libraries`.
fn is_shmem_available() -> bool {
    // Use a simple flag set during init_shared_memory()
    SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed)
}

// ── Worker token management (Phase 2: parallel refresh) ───────────────────

/// Try to acquire a cluster-wide refresh worker token.
///
/// Returns `true` if a token was acquired (active count incremented),
/// `false` if the budget is exhausted.
///
/// The caller **must** call [`release_worker_token`] when the worker
/// finishes, regardless of success or failure.
pub fn try_acquire_worker_token(max_workers: u32) -> bool {
    if !is_shmem_available() {
        return false;
    }
    // CAS loop: increment only if below the cap.
    let atomic = ACTIVE_REFRESH_WORKERS.get();
    loop {
        let current = atomic.load(std::sync::atomic::Ordering::Acquire);
        if current >= max_workers {
            return false;
        }
        match atomic.compare_exchange_weak(
            current,
            current + 1,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(_) => continue, // spurious failure — retry
        }
    }
}

/// Release a cluster-wide refresh worker token (decrement active count).
///
/// Called by workers on exit and by reconciliation logic for leaked tokens.
pub fn release_worker_token() {
    if !is_shmem_available() {
        return;
    }
    // Saturating decrement — never go below 0.
    let atomic = ACTIVE_REFRESH_WORKERS.get();
    loop {
        let current = atomic.load(std::sync::atomic::Ordering::Acquire);
        if current == 0 {
            return;
        }
        match atomic.compare_exchange_weak(
            current,
            current - 1,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(_) => continue,
        }
    }
}

/// Read the current number of active dynamic refresh workers.
pub fn active_worker_count() -> u32 {
    if !is_shmem_available() {
        return 0;
    }
    ACTIVE_REFRESH_WORKERS
        .get()
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Force-set the active worker count to a specific value.
///
/// Used by reconciliation logic to correct the count after a crash.
pub fn set_active_worker_count(count: u32) {
    if !is_shmem_available() {
        return;
    }
    ACTIVE_REFRESH_WORKERS
        .get()
        .store(count, std::sync::atomic::Ordering::Release);
}

/// Bump the reconcile epoch (signals that token count was corrected).
pub fn bump_reconcile_epoch() {
    if !is_shmem_available() {
        return;
    }
    RECONCILE_EPOCH
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Read the current reconcile epoch.
pub fn current_reconcile_epoch() -> u64 {
    if !is_shmem_available() {
        return 0;
    }
    RECONCILE_EPOCH
        .get()
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Flag indicating whether shared memory was initialized via _PG_init.
static SHMEM_INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
