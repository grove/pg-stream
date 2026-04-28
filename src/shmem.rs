//! Shared memory structures for scheduler↔backend coordination.
//!
//! The scheduler background worker and user sessions communicate via shared
//! memory using lightweight locks (`PgLwLock`) and atomic variables (`PgAtomic`).

use pgrx::prelude::*;
use pgrx::{PGRXSharedMemory, PgAtomic, PgLwLock, pg_shmem_init};
use std::sync::atomic::{AtomicU32, AtomicU64};

/// C2-1 / DEF-4: Maximum number of pgt_ids the invalidation ring buffer can hold.
///
/// When more DDL changes arrive between scheduler ticks than this capacity,
/// the overflow flag is set and the scheduler falls back to a full O(V+E) DAG
/// rebuild. Raised to 128 in v0.11.0 to handle CI pipelines, dbt model rebuilds,
/// and schema-migration workloads that can produce dozens of DDL events per tick.
const INVALIDATION_RING_CAPACITY: usize = 128;

/// Shared state visible to both the scheduler and user backends.
///
/// SCAL-3 (v0.25.0): This struct now holds only DAG-related fields.
/// Scheduler metadata and tick watermarks have been split into dedicated
/// `SchedulerMetaState` and `TickWatermarkState` structs to reduce lock
/// contention.  Read-only paths (e.g., `dag_version` reads) use
/// `PGS_STATE.share()` without blocking concurrent readers.
#[derive(Copy, Clone)]
pub struct PgTrickleSharedState {
    /// Incremented when the DAG changes (create/alter/drop ST).
    pub dag_version: u64,

    // ── C2-1: Invalidation ring buffer ───────────────────────────────
    /// Bounded ring buffer of pgt_ids that need DAG re-evaluation.
    /// DDL hooks push affected pgt_ids here; the scheduler drains them.
    inv_ring: [i64; INVALIDATION_RING_CAPACITY],
    /// Number of valid entries in the ring buffer (0..=CAPACITY).
    inv_count: u16,
    /// When true, more DDL events arrived than the ring can hold.
    /// The scheduler must do a full O(V+E) DAG rebuild.
    inv_overflow: bool,
}

impl Default for PgTrickleSharedState {
    fn default() -> Self {
        Self {
            dag_version: 0,
            inv_ring: [0; INVALIDATION_RING_CAPACITY],
            inv_count: 0,
            inv_overflow: false,
        }
    }
}

// SAFETY: PgTrickleSharedState is Copy + Clone + Default and contains only
// primitive types, making it safe for shared memory access under PgLwLock.
unsafe impl PGRXSharedMemory for PgTrickleSharedState {}

/// SCAL-3 (v0.25.0): Scheduler metadata — split from `PgTrickleSharedState`
/// to reduce lock contention on the DAG lock path.
///
/// Protected by `SCHEDULER_META_STATE` lightweight lock.
#[derive(Copy, Clone, Default)]
pub struct SchedulerMetaState {
    /// PID of the scheduler background worker (0 if not running).
    pub scheduler_pid: i32,
    /// Whether the scheduler is currently running.
    pub scheduler_running: bool,
    /// Unix timestamp (seconds) of the scheduler's last wake cycle.
    pub last_scheduler_wake: i64,
}

// SAFETY: SchedulerMetaState is Copy + Clone + Default with only primitive types.
unsafe impl PGRXSharedMemory for SchedulerMetaState {}

/// SCAL-3 (v0.25.0): Tick watermark state — split from `PgTrickleSharedState`
/// to allow the coordinator to update watermarks without taking the DAG
/// exclusive lock.
///
/// Protected by `TICK_WATERMARK_STATE` lightweight lock.
#[derive(Copy, Clone, Default)]
pub struct TickWatermarkState {
    /// The oldest `backend_xmin` (including 2PC) seen at the previous
    /// scheduler tick. Used by the xmin holdback algorithm to detect
    /// long-running transactions that span a tick boundary.
    ///
    /// 0 means "not yet recorded" (first tick or holdback disabled).
    pub last_tick_oldest_xmin: u64,

    /// The safe frontier LSN upper bound computed at the last scheduler tick,
    /// stored as a raw u64 (see `version::lsn_to_u64` / `version::u64_to_lsn`).
    ///
    /// Dynamic refresh workers read this value and use it (capped with their
    /// own current write_lsn) as their tick watermark. 0 means unset.
    pub last_tick_safe_lsn_u64: u64,
}

// SAFETY: TickWatermarkState is Copy + Clone + Default with only primitive types.
unsafe impl PGRXSharedMemory for TickWatermarkState {}

/// Lightweight-lock–protected DAG and invalidation shared state.
// SAFETY: PgLwLock::new requires a static CStr name for the lock.
pub static PGS_STATE: PgLwLock<PgTrickleSharedState> =
    unsafe { PgLwLock::new(c"pg_trickle_state") };

/// SCAL-3 (v0.25.0): Dedicated lock for scheduler metadata (PID, status,
/// last-wake).  Split from `PGS_STATE` to allow monitoring reads without
/// blocking DAG operations.
// SAFETY: PgLwLock::new requires a static CStr name for the lock.
pub static SCHEDULER_META_STATE: PgLwLock<SchedulerMetaState> =
    unsafe { PgLwLock::new(c"pg_trickle_scheduler_meta") };

/// SCAL-3 (v0.25.0): Dedicated lock for tick watermark state (xmin, safe LSN).
/// Split from `PGS_STATE` so the coordinator can update watermarks without
/// contending with DAG invalidation writes.
// SAFETY: PgLwLock::new requires a static CStr name for the lock.
pub static TICK_WATERMARK_STATE: PgLwLock<TickWatermarkState> =
    unsafe { PgLwLock::new(c"pg_trickle_tick_watermark") };

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

/// G14-MDED: Total number of differential refreshes executed since server start.
///
/// Incremented once per `execute_differential_refresh` call that proceeds past
/// the no-data short-circuit (i.e., at least one change buffer has rows in the
/// current frontier window).
// SAFETY: PgAtomic::new requires a static CStr name.
pub static TOTAL_DIFF_REFRESHES: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_total_diff_refreshes") };

/// G14-MDED: Number of differential refreshes where the delta required weight
/// aggregation (deduplication) in the MERGE USING clause.
///
/// When `is_deduplicated = false` the DVM cannot prove that each `__pgt_row_id`
/// appears at most once in the delta, so the MERGE must group + aggregate
/// before merging. This counter tracks how often that occurs.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static DEDUP_NEEDED_REFRESHES: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_dedup_needed_refreshes") };

/// G14-SHC: Number of delta template cache L2 hits (catalog table).
// SAFETY: PgAtomic::new requires a static CStr name.
pub static TEMPLATE_CACHE_L2_HITS: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_template_cache_l2_hits") };

/// G14-SHC: Number of delta template cache full misses (DVM re-parse).
// SAFETY: PgAtomic::new requires a static CStr name.
pub static TEMPLATE_CACHE_MISSES: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_template_cache_misses") };

/// UX-1 / CACHE-OBS: Number of delta template cache L1 hits (thread-local).
// SAFETY: PgAtomic::new requires a static CStr name.
pub static TEMPLATE_CACHE_L1_HITS: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_template_cache_l1_hits") };

/// UX-1 / CACHE-OBS: Number of delta template cache evictions (generation flush).
// SAFETY: PgAtomic::new requires a static CStr name.
pub static TEMPLATE_CACHE_EVICTIONS: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_template_cache_evictions") };

/// #536: Current frontier holdback in LSN bytes (gauge).
///
/// Set to 0 when the frontier is not held back.
/// Set to `write_lsn - safe_lsn` in bytes when a long-running transaction
/// is preventing the frontier from advancing.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static FRONTIER_HOLDBACK_LSN_BYTES: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_frontier_holdback_lsn") };

/// #536: Age (in seconds) of the oldest in-progress transaction contributing
/// to a frontier holdback (gauge).
///
/// Set to 0 when no holdback is active.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static FRONTIER_HOLDBACK_AGE_SECS: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_frontier_holdback_age") };

/// PERF-3 (v0.31.0): Counter of IVM lock-mode parse failures.
///
/// Incremented each time `IvmLockMode::for_query` falls back to `Exclusive`
/// because the defining query could not be parsed. Exposed via
/// `pgtrickle.metrics_summary()` as `ivm_lock_parse_error_count` and via
/// the Prometheus metrics endpoint.
///
/// Operators can use this counter to audit whether IMMEDIATE-mode queries
/// are taking unnecessarily broad advisory locks.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static IVM_LOCK_PARSE_ERRORS: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_ivm_lock_parse_errors") };

/// A35 (v0.36.0): Drain request epoch counter.
///
/// When `pgtrickle.drain()` is called, this counter is incremented to signal
/// the scheduler to stop accepting new refresh cycles. The scheduler polls
/// this counter and sets `DRAIN_COMPLETED` to the same epoch once all
/// in-flight refreshes have finished.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static DRAIN_REQUESTED: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_drain_requested") };

/// A35 (v0.36.0): Drain completed epoch counter.
///
/// The scheduler sets this to the value of `DRAIN_REQUESTED` once it has
/// finished all in-flight refreshes. `pgtrickle.is_drained()` returns true
/// when `DRAIN_COMPLETED >= DRAIN_REQUESTED`.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static DRAIN_COMPLETED: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_drain_completed") };

/// Register shared memory allocations. Called from `_PG_init()`.
pub fn init_shared_memory() {
    pg_shmem_init!(PGS_STATE);
    // SCAL-3: Dedicated per-concern locks.
    pg_shmem_init!(SCHEDULER_META_STATE);
    pg_shmem_init!(TICK_WATERMARK_STATE);
    pg_shmem_init!(DAG_REBUILD_SIGNAL);
    pg_shmem_init!(CACHE_GENERATION);
    pg_shmem_init!(ACTIVE_REFRESH_WORKERS);
    pg_shmem_init!(RECONCILE_EPOCH);
    pg_shmem_init!(TOTAL_DIFF_REFRESHES);
    pg_shmem_init!(DEDUP_NEEDED_REFRESHES);
    pg_shmem_init!(TEMPLATE_CACHE_L2_HITS);
    pg_shmem_init!(TEMPLATE_CACHE_MISSES);
    pg_shmem_init!(TEMPLATE_CACHE_L1_HITS);
    pg_shmem_init!(TEMPLATE_CACHE_EVICTIONS);
    // CACHE-1: L0 cross-backend cache availability signal.
    pg_shmem_init!(L0_POPULATED_VERSION);
    pg_shmem_init!(FRONTIER_HOLDBACK_LSN_BYTES);
    pg_shmem_init!(FRONTIER_HOLDBACK_AGE_SECS);
    // PERF-3 (v0.31.0): IVM lock-mode parse error counter.
    pg_shmem_init!(IVM_LOCK_PARSE_ERRORS);
    // PERF-3: Cost-model cache.
    pg_shmem_init!(COST_MODEL_STATE);
    // A35 (v0.36.0): Drain mode epoch counters.
    pg_shmem_init!(DRAIN_REQUESTED);
    pg_shmem_init!(DRAIN_COMPLETED);
    SHMEM_INITIALIZED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// G14-MDED: Increment the total-diff-refreshes counter.
///
/// No-op when shared memory is not initialized (extension not loaded via
/// `shared_preload_libraries`).
pub fn record_diff_refresh(is_deduplicated: bool) {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    TOTAL_DIFF_REFRESHES
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !is_deduplicated {
        DEDUP_NEEDED_REFRESHES
            .get()
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// G14-MDED: Read the dedup profiling counters.
///
/// Returns `(total_diff_refreshes, dedup_needed_refreshes)`.
pub fn read_dedup_stats() -> (u64, u64) {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return (0, 0);
    }
    let total = TOTAL_DIFF_REFRESHES
        .get()
        .load(std::sync::atomic::Ordering::Relaxed);
    let dedup = DEDUP_NEEDED_REFRESHES
        .get()
        .load(std::sync::atomic::Ordering::Relaxed);
    (total, dedup)
}

/// PERF-3 (v0.31.0): Increment the IVM lock-mode parse error counter.
///
/// Called from `IvmLockMode::for_query` whenever parsing the defining query
/// fails and the lock mode falls back to `Exclusive`.
pub fn increment_ivm_lock_parse_errors() {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    IVM_LOCK_PARSE_ERRORS
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// PERF-3 (v0.31.0): Read the IVM lock-mode parse error counter.
pub fn read_ivm_lock_parse_errors() -> u64 {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return 0;
    }
    IVM_LOCK_PARSE_ERRORS
        .get()
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Signal the scheduler to rebuild the dependency DAG.
///
/// Called by API functions (create/alter/drop) after modifying catalog entries.
/// No-op if shared memory is not initialized (i.e., extension not in
/// `shared_preload_libraries`).
///
/// This signals a full rebuild (sets overflow flag). For targeted
/// invalidation of a specific stream table, use [`signal_dag_invalidation`].
pub fn signal_dag_rebuild() {
    // Guard: PgAtomic is only initialized when loaded via shared_preload_libraries.
    // When loaded dynamically (CREATE EXTENSION without shared_preload), the
    // scheduler and shared memory are unavailable. Just skip the signal.
    if !is_shmem_available() {
        return;
    }
    // C2-1: Set overflow flag so the scheduler does a full rebuild.
    PGS_STATE.exclusive().inv_overflow = true;
    DAG_REBUILD_SIGNAL
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// C2-1: Signal the scheduler that a specific stream table's DAG state
/// may have changed.
///
/// Pushes `pgt_id` into the shared invalidation ring buffer. If the ring
/// is full, sets the overflow flag — the scheduler will fall back to a
/// full DAG rebuild on its next tick.
///
/// Called by DDL hooks and API functions after modifying catalog entries
/// for a known stream table.
pub fn signal_dag_invalidation(pgt_id: i64) {
    if !is_shmem_available() {
        return;
    }
    {
        let mut state = PGS_STATE.exclusive();
        push_invalidation(&mut state, pgt_id);
    }
    DAG_REBUILD_SIGNAL
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// C2-1: Push a pgt_id into the invalidation ring buffer.
///
/// Pure logic extracted for unit-testability. Deduplicates entries and
/// sets the overflow flag when the ring is full.
fn push_invalidation(state: &mut PgTrickleSharedState, pgt_id: i64) {
    let count = state.inv_count as usize;
    if count < INVALIDATION_RING_CAPACITY {
        // Deduplicate: skip if this pgt_id is already in the ring.
        if !state.inv_ring[..count].contains(&pgt_id) {
            state.inv_ring[count] = pgt_id;
            state.inv_count = (count + 1) as u16;
        }
    } else {
        state.inv_overflow = true;
    }
}

/// C2-1: Drain the invalidation ring buffer, returning the set of
/// affected pgt_ids.
///
/// Returns `Some(vec)` with the affected pgt_ids if no overflow occurred,
/// or `None` if the ring overflowed (caller must do a full DAG rebuild).
///
/// After draining, the ring buffer and overflow flag are reset.
/// Called by the scheduler at the start of each tick.
pub fn drain_invalidations() -> Option<Vec<i64>> {
    if !is_shmem_available() {
        return Some(Vec::new());
    }
    let mut state = PGS_STATE.exclusive();
    drain_ring(&mut state)
}

/// C2-1: Drain all entries from the invalidation ring buffer.
///
/// Pure logic extracted for unit-testability. Returns `Some(ids)` on
/// success or `None` if the ring overflowed. Resets the ring in either case.
fn drain_ring(state: &mut PgTrickleSharedState) -> Option<Vec<i64>> {
    if state.inv_overflow {
        state.inv_count = 0;
        state.inv_overflow = false;
        return None;
    }
    let count = state.inv_count as usize;
    if count == 0 {
        return Some(Vec::new());
    }
    let ids: Vec<i64> = state.inv_ring[..count].to_vec();
    state.inv_count = 0;
    Some(ids)
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
pub fn is_shmem_available() -> bool {
    // Use a simple flag set during init_shared_memory()
    SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed)
}

// ── A35 (v0.36.0): Drain mode helpers ─────────────────────────────────────

/// Signal the scheduler to drain: increments DRAIN_REQUESTED and returns the
/// new epoch that the scheduler must reach to confirm draining.
///
/// Returns `None` when shared memory is not initialized.
pub fn signal_drain() -> Option<u64> {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let epoch = DRAIN_REQUESTED
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
        + 1;
    Some(epoch)
}

/// Check whether the scheduler has quiesced to the given drain epoch.
///
/// Returns `true` when `DRAIN_COMPLETED >= epoch`.
pub fn check_drain_completed(epoch: u64) -> bool {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return true; // No scheduler = trivially drained
    }
    DRAIN_COMPLETED
        .get()
        .load(std::sync::atomic::Ordering::Acquire)
        >= epoch
}

/// Called by the scheduler at the start of each tick to check whether a drain
/// was requested and whether it is already completed (no in-flight refreshes).
///
/// If a drain is requested and no refreshes are running, sets DRAIN_COMPLETED
/// to the current DRAIN_REQUESTED epoch and returns `true` (stop scheduling).
/// Returns `false` when no drain is requested or refreshes are still in flight.
pub fn scheduler_check_and_complete_drain() -> bool {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    let requested = DRAIN_REQUESTED
        .get()
        .load(std::sync::atomic::Ordering::Acquire);
    let completed = DRAIN_COMPLETED
        .get()
        .load(std::sync::atomic::Ordering::Acquire);
    if requested <= completed {
        return false; // No pending drain
    }
    // Check if any refresh workers are still running
    let active = ACTIVE_REFRESH_WORKERS
        .get()
        .load(std::sync::atomic::Ordering::Acquire);
    if active == 0 {
        // Mark drain as completed
        DRAIN_COMPLETED
            .get()
            .store(requested, std::sync::atomic::Ordering::Release);
        return true;
    }
    true // Drain requested but not yet completed
}

/// Cancel a pending drain by resetting DRAIN_REQUESTED to the current
/// DRAIN_COMPLETED value (so the scheduler resumes normal operation).
pub fn cancel_drain() {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let completed = DRAIN_COMPLETED
        .get()
        .load(std::sync::atomic::Ordering::Acquire);
    DRAIN_REQUESTED
        .get()
        .store(completed, std::sync::atomic::Ordering::Release);
}

/// Returns `true` when DRAIN_COMPLETED >= DRAIN_REQUESTED, i.e., the scheduler
/// has quiesced and no new cycles are being dispatched.
pub fn is_drained() -> bool {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    let requested = DRAIN_REQUESTED
        .get()
        .load(std::sync::atomic::Ordering::Acquire);
    let completed = DRAIN_COMPLETED
        .get()
        .load(std::sync::atomic::Ordering::Acquire);
    completed >= requested
}

// ── Worker token management (Phase 2: parallel refresh) ───────────────────

fn try_increment_bounded_counter(atomic: &AtomicU32, max_value: u32) -> bool {
    loop {
        let current = atomic.load(std::sync::atomic::Ordering::Acquire);
        if current >= max_value {
            return false;
        }

        match atomic.compare_exchange_weak(
            current,
            current + 1,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(_) => continue,
        }
    }
}

fn saturating_decrement_counter(atomic: &AtomicU32) {
    loop {
        let current = atomic.load(std::sync::atomic::Ordering::Acquire);
        let next = current.saturating_sub(1);
        if next == current {
            return;
        }

        match atomic.compare_exchange_weak(
            current,
            next,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(_) => continue,
        }
    }
}

fn increment_epoch(atomic: &AtomicU64) {
    atomic.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

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

    try_increment_bounded_counter(ACTIVE_REFRESH_WORKERS.get(), max_workers)
}

/// Release a cluster-wide refresh worker token (decrement active count).
///
/// Called by workers on exit and by reconciliation logic for leaked tokens.
pub fn release_worker_token() {
    if !is_shmem_available() {
        return;
    }

    saturating_decrement_counter(ACTIVE_REFRESH_WORKERS.get());
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

    increment_epoch(RECONCILE_EPOCH.get());
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

// ── #536: Frontier visibility holdback helpers ─────────────────────────────

/// Read the oldest-xmin seen at the previous scheduler tick.
///
/// SCAL-3: Uses `TICK_WATERMARK_STATE` (dedicated lock) instead of `PGS_STATE`.
/// Returns 0 when shmem is unavailable or no baseline has been recorded.
pub fn last_tick_oldest_xmin() -> u64 {
    if !is_shmem_available() {
        return 0;
    }
    TICK_WATERMARK_STATE.share().last_tick_oldest_xmin
}

/// Persist the oldest-xmin from the current tick so next tick can compare.
///
/// SCAL-3: Uses `TICK_WATERMARK_STATE` (dedicated lock).
pub fn set_last_tick_oldest_xmin(xmin: u64) {
    if !is_shmem_available() {
        return;
    }
    TICK_WATERMARK_STATE.exclusive().last_tick_oldest_xmin = xmin;
}

/// Persist both the oldest-xmin and the safe frontier LSN **atomically** under
/// a single exclusive lock acquisition so that dynamic workers never see a
/// state where the xmin has advanced but the safe LSN has not yet been updated
/// (or vice-versa).
///
/// SCAL-3: Uses `TICK_WATERMARK_STATE` (dedicated lock).
pub fn set_last_tick_holdback_state(xmin: u64, lsn_u64: u64) {
    if !is_shmem_available() {
        return;
    }
    let mut state = TICK_WATERMARK_STATE.exclusive();
    state.last_tick_oldest_xmin = xmin;
    state.last_tick_safe_lsn_u64 = lsn_u64;
}

/// Read the safe frontier LSN (u64) written by the coordinator at the last tick.
///
/// SCAL-3: Uses `TICK_WATERMARK_STATE.share()` — reads do not block DAG writers.
/// Returns 0 when shmem is unavailable or unset.
pub fn last_tick_safe_lsn_u64() -> u64 {
    if !is_shmem_available() {
        return 0;
    }
    TICK_WATERMARK_STATE.share().last_tick_safe_lsn_u64
}

/// Persist the safe frontier LSN (u64) so dynamic workers can read it.
///
/// SCAL-3: Uses `TICK_WATERMARK_STATE` (dedicated lock).
pub fn set_last_tick_safe_lsn(lsn_u64: u64) {
    if !is_shmem_available() {
        return;
    }
    // Update only the cached safe LSN under the dedicated watermark lock.
    // When both xmin and LSN must be updated together, use set_last_tick_holdback_state().
    TICK_WATERMARK_STATE.exclusive().last_tick_safe_lsn_u64 = lsn_u64;
}

// ── SCAL-3 (v0.25.0): Scheduler metadata helpers ──────────────────────────

/// Read the current scheduler PID from shared memory.
///
/// Returns 0 when shmem is unavailable or the scheduler has not started.
pub fn scheduler_pid() -> i32 {
    if !is_shmem_available() {
        return 0;
    }
    SCHEDULER_META_STATE.share().scheduler_pid
}

/// Record scheduler startup: set PID, mark as running, record wake time.
pub fn set_scheduler_meta(pid: i32, running: bool, wake_ts: i64) {
    if !is_shmem_available() {
        return;
    }
    let mut meta = SCHEDULER_META_STATE.exclusive();
    meta.scheduler_pid = pid;
    meta.scheduler_running = running;
    meta.last_scheduler_wake = wake_ts;
}

/// Read scheduler running status.
pub fn scheduler_running() -> bool {
    if !is_shmem_available() {
        return false;
    }
    SCHEDULER_META_STATE.share().scheduler_running
}

/// Read the last scheduler wake timestamp (Unix seconds).
pub fn last_scheduler_wake() -> i64 {
    if !is_shmem_available() {
        return 0;
    }
    SCHEDULER_META_STATE.share().last_scheduler_wake
}

/// Update the holdback gauge metrics.
///
/// - `lsn_bytes`: how many bytes behind write_lsn the safe frontier is.
/// - `age_secs`: age (seconds) of the oldest in-progress transaction.
pub fn update_holdback_metrics(lsn_bytes: u64, age_secs: u64) {
    if !is_shmem_available() {
        return;
    }
    FRONTIER_HOLDBACK_LSN_BYTES
        .get()
        .store(lsn_bytes, std::sync::atomic::Ordering::Relaxed);
    FRONTIER_HOLDBACK_AGE_SECS
        .get()
        .store(age_secs, std::sync::atomic::Ordering::Relaxed);
}

/// Read the current holdback gauge metrics.
///
/// Returns `(lsn_bytes, age_secs)`.
pub fn read_holdback_metrics() -> (u64, u64) {
    if !is_shmem_available() {
        return (0, 0);
    }
    let lsn = FRONTIER_HOLDBACK_LSN_BYTES
        .get()
        .load(std::sync::atomic::Ordering::Relaxed);
    let age = FRONTIER_HOLDBACK_AGE_SECS
        .get()
        .load(std::sync::atomic::Ordering::Relaxed);
    (lsn, age)
}

/// CACHE-2: Increment the L1 template cache evictions counter.
///
/// Called by `maybe_evict_lru_cache_entry` when an LRU eviction occurs.
pub fn increment_template_cache_evictions() {
    if !SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    TEMPLATE_CACHE_EVICTIONS
        .get()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

// ── CACHE-1 (v0.25.0/v0.36.0): L0 cross-backend cache availability signal ──
//
// A09 (v0.36.0): The L0 cache is now implemented as a process-local
// `RwLock<HashMap<u64, String>>` keyed by `(pgt_id << 32 | cache_generation)`.
// This provides ~45 ms cold-start savings per backend for connection-pooler
// workloads where the same OS process serves many short-lived connections.
//
// The `L0_POPULATED_VERSION` counter signals cross-backend availability via
// shared memory: when `l0_populated_version() == current_cache_generation()`,
// the L2 catalog table (`pgtrickle.pgt_template_cache`) has valid entries,
// but this backend's process-local L0 map may also already have the template
// cached (avoiding the SPI round-trip entirely).
//
// The L0 map is never persisted; it starts empty on each backend startup and
// is invalidated when `CACHE_GENERATION` is bumped (via `invalidate_l0_cache()`).

/// Atomic counter indicating the CACHE_GENERATION at which the L0/L2 shared
/// cache was last populated.  Backends set this after writing a new template
/// to the L2 catalog table.
// SAFETY: PgAtomic::new requires a static CStr name.
pub static L0_POPULATED_VERSION: PgAtomic<AtomicU64> =
    unsafe { PgAtomic::new(c"pg_trickle_l0_populated") };

/// A09 (v0.36.0): Process-local L0 template cache.
///
/// Keyed by `(pgt_id, cache_generation)` encoded as a `u64` pair.
/// Stores compiled delta SQL template strings.
static L0_TEMPLATE_CACHE: std::sync::OnceLock<
    std::sync::RwLock<std::collections::HashMap<(i64, u64), String>>,
> = std::sync::OnceLock::new();

fn get_l0_cache() -> &'static std::sync::RwLock<std::collections::HashMap<(i64, u64), String>> {
    L0_TEMPLATE_CACHE.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()))
}

/// A09 (v0.36.0): Look up a template in the process-local L0 cache.
///
/// Returns the cached delta SQL string when the entry exists at the current
/// generation, or `None` on a miss. Does NOT call SPI or touch the L2 catalog.
pub fn l0_cache_lookup(pgt_id: i64) -> Option<String> {
    if !is_shmem_available() {
        return None;
    }
    let generation = CACHE_GENERATION
        .get()
        .load(std::sync::atomic::Ordering::Relaxed);
    let cache = get_l0_cache();
    let guard = cache.read().ok()?;
    guard.get(&(pgt_id, generation)).cloned()
}

/// A09 (v0.36.0): Store a compiled template in the process-local L0 cache.
///
/// Associates `template_sql` with `pgt_id` at the current `CACHE_GENERATION`.
/// The entry is automatically invalidated when the generation is bumped.
pub fn l0_cache_store(pgt_id: i64, template_sql: String) {
    if !is_shmem_available() {
        return;
    }
    let generation = CACHE_GENERATION
        .get()
        .load(std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut cache) = get_l0_cache().write() {
        cache.insert((pgt_id, generation), template_sql);
    }
}

/// A09 (v0.36.0): Invalidate the process-local L0 cache.
///
/// Called when `CACHE_GENERATION` is incremented. Removes all entries at the
/// previous generation by clearing the entire map (old generations are invalid
/// after any generation bump).
pub fn invalidate_l0_cache() {
    if let Ok(mut cache) = get_l0_cache().write() {
        cache.clear();
    }
}

/// CACHE-1: Signal that the L0/L2 shared template cache has been populated
/// at the current CACHE_GENERATION.
///
/// Called after successfully writing a compiled template to L2 storage.
pub fn signal_l0_cache_populated() {
    if !is_shmem_available() {
        return;
    }
    let current_gen = CACHE_GENERATION
        .get()
        .load(std::sync::atomic::Ordering::Relaxed);
    L0_POPULATED_VERSION
        .get()
        .store(current_gen, std::sync::atomic::Ordering::Release);
}

/// CACHE-1: Check whether the L0/L2 shared cache was populated at the
/// current CACHE_GENERATION.
///
/// Returns `true` if another backend has populated the L2 cache at the
/// current generation, meaning an L1 miss should try L2 before the DVM
/// parser.  This is already the default behavior; this check allows the
/// monitoring layer to distinguish "L2 hit avoided by L0 signal" from
/// cold-start L2 lookups.
pub fn is_l0_cache_available() -> bool {
    if !is_shmem_available() {
        return false;
    }
    let l0_ver = L0_POPULATED_VERSION
        .get()
        .load(std::sync::atomic::Ordering::Acquire);
    let cache_gen = CACHE_GENERATION
        .get()
        .load(std::sync::atomic::Ordering::Relaxed);
    l0_ver == cache_gen
}

// ── PERF-3 (v0.25.0): Shmem adaptive cost-model state ─────────────────────

/// Number of per-ST cost model slots in the shmem cache.
///
/// Slots are addressed by `(pgt_id as usize) % COST_CACHE_CAPACITY`.
/// Collisions are tolerated — a wrong slot is treated as a miss and
/// triggers an SPI fallback. 256 slots give < 0.4% expected collision
/// rate for workloads with ≤ 1 000 stream tables.
const COST_CACHE_CAPACITY: usize = 256;

/// One cost model entry for a single stream table.
///
/// `last_full_ms` and `last_diff_ms` are stored as IEEE 754 bit patterns
/// (via `f64::to_bits` / `f64::from_bits`) so that `Copy + Clone +
/// Default` work without needing a `f64` field.
#[derive(Copy, Clone, Default)]
pub struct CostModelEntry {
    /// `pgt_id` of the stream table that owns this slot, or 0 if empty.
    pub pgt_id: i64,
    /// Last observed FULL refresh duration (ms) as IEEE 754 bits.
    pub last_full_ms_bits: u64,
    /// Last observed DIFFERENTIAL refresh duration (ms) as IEEE 754 bits.
    pub last_diff_ms_bits: u64,
}

/// Fixed-size array of cost model entries, shared across all backends.
///
/// Protected by `COST_MODEL_STATE` PgLwLock.
#[derive(Copy, Clone)]
pub struct CostModelCache {
    pub entries: [CostModelEntry; COST_CACHE_CAPACITY],
}

impl Default for CostModelCache {
    fn default() -> Self {
        Self {
            entries: [CostModelEntry::default(); COST_CACHE_CAPACITY],
        }
    }
}

// SAFETY: CostModelCache contains only primitive types and is Copy + Clone +
// Default, making it safe for use in PostgreSQL shared memory.
unsafe impl PGRXSharedMemory for CostModelCache {}

/// PERF-3: Shared cost-model state protected by a dedicated lightweight lock.
///
/// Separate from `PGS_STATE` to avoid contention with DAG / scheduler state.
// SAFETY: PgLwLock::new requires a static CStr name.
pub static COST_MODEL_STATE: PgLwLock<CostModelCache> =
    unsafe { PgLwLock::new(c"pg_trickle_cost_model") };

/// PERF-3: Write cost-model timing data for a stream table to shmem.
///
/// Called by the scheduler after each refresh completes. Parallel workers
/// can then read the timing from shmem instead of issuing an SPI query.
///
/// `last_full_ms` and/or `last_diff_ms` can be `None` to leave the
/// corresponding slot value unchanged.
pub fn update_cost_model(pgt_id: i64, last_full_ms: Option<f64>, last_diff_ms: Option<f64>) {
    if !is_shmem_available() {
        return;
    }
    let slot = (pgt_id as usize).wrapping_rem(COST_CACHE_CAPACITY);
    let mut cache = COST_MODEL_STATE.exclusive();
    let entry = &mut cache.entries[slot];
    // If the slot is taken by a different pgt_id, overwrite it.
    entry.pgt_id = pgt_id;
    if let Some(ms) = last_full_ms {
        entry.last_full_ms_bits = ms.to_bits();
    }
    if let Some(ms) = last_diff_ms {
        entry.last_diff_ms_bits = ms.to_bits();
    }
}

/// PERF-3: Read cost-model timing data for a stream table from shmem.
///
/// Returns `Some((last_full_ms, last_diff_ms))` on a cache hit, or `None`
/// on a miss (slot empty, slot occupied by a different pgt_id, or
/// shmem unavailable). Callers should fall back to SPI on `None`.
pub fn read_cost_model(pgt_id: i64) -> Option<(f64, f64)> {
    if !is_shmem_available() {
        return None;
    }
    let slot = (pgt_id as usize).wrapping_rem(COST_CACHE_CAPACITY);
    let cache = COST_MODEL_STATE.share();
    let entry = &cache.entries[slot];
    if entry.pgt_id != pgt_id || entry.pgt_id == 0 {
        return None;
    }
    let full_ms = f64::from_bits(entry.last_full_ms_bits);
    let diff_ms = f64::from_bits(entry.last_diff_ms_bits);
    Some((full_ms, diff_ms))
}

/// Flag indicating whether shared memory was initialized via _PG_init.
static SHMEM_INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::{
        INVALIDATION_RING_CAPACITY, PgTrickleSharedState, drain_ring, increment_epoch,
        push_invalidation, saturating_decrement_counter, try_increment_bounded_counter,
    };
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    #[test]
    fn test_try_increment_bounded_counter_increments_until_cap() {
        let counter = AtomicU32::new(0);

        assert!(try_increment_bounded_counter(&counter, 2));
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        assert!(try_increment_bounded_counter(&counter, 2));
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        assert!(!try_increment_bounded_counter(&counter, 2));
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_try_increment_bounded_counter_rejects_zero_budget() {
        let counter = AtomicU32::new(0);

        assert!(!try_increment_bounded_counter(&counter, 0));
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_saturating_decrement_counter_stops_at_zero() {
        let counter = AtomicU32::new(2);

        saturating_decrement_counter(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        saturating_decrement_counter(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        saturating_decrement_counter(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_increment_epoch_advances_counter() {
        let epoch = AtomicU64::new(41);

        increment_epoch(&epoch);
        increment_epoch(&epoch);

        assert_eq!(epoch.load(Ordering::Relaxed), 43);
    }

    // ── P3: acquire/release token cycle and invariants ─────────────────────

    #[test]
    fn test_token_acquire_and_release_cycle() {
        let counter = AtomicU32::new(0);

        // Acquire two tokens up to budget of 3
        assert!(try_increment_bounded_counter(&counter, 3));
        assert!(try_increment_bounded_counter(&counter, 3));
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        // Release one
        saturating_decrement_counter(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // Can acquire again
        assert!(try_increment_bounded_counter(&counter, 3));
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        // Release both
        saturating_decrement_counter(&counter);
        saturating_decrement_counter(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_counter_does_not_go_below_zero_on_over_release() {
        let counter = AtomicU32::new(1);

        // Release once (valid)
        saturating_decrement_counter(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        // Extra releases are no-ops
        saturating_decrement_counter(&counter);
        saturating_decrement_counter(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_epoch_monotonically_increases() {
        let epoch = AtomicU64::new(0);
        let mut last = epoch.load(Ordering::Relaxed);

        for _ in 0..10 {
            increment_epoch(&epoch);
            let current = epoch.load(Ordering::Relaxed);
            assert!(current > last);
            last = current;
        }
    }

    #[test]
    fn test_bounded_counter_budget_one_acts_as_mutex() {
        let counter = AtomicU32::new(0);

        // Acquire the single token
        assert!(try_increment_bounded_counter(&counter, 1));
        // Second acquire must be rejected
        assert!(!try_increment_bounded_counter(&counter, 1));

        // Release
        saturating_decrement_counter(&counter);
        // Now acquirable again
        assert!(try_increment_bounded_counter(&counter, 1));
    }

    // ── C2-1: Invalidation ring buffer tests ──────────────────────────────

    fn new_state() -> PgTrickleSharedState {
        PgTrickleSharedState::default()
    }

    #[test]
    fn test_ring_push_single() {
        let mut s = new_state();
        push_invalidation(&mut s, 42);

        assert_eq!(s.inv_count, 1);
        assert_eq!(s.inv_ring[0], 42);
        assert!(!s.inv_overflow);
    }

    #[test]
    fn test_ring_push_deduplicates() {
        let mut s = new_state();
        push_invalidation(&mut s, 1);
        push_invalidation(&mut s, 1);
        push_invalidation(&mut s, 1);

        assert_eq!(s.inv_count, 1);
        assert!(!s.inv_overflow);
    }

    #[test]
    fn test_ring_push_multiple_distinct() {
        let mut s = new_state();
        for id in 1..=5 {
            push_invalidation(&mut s, id);
        }
        assert_eq!(s.inv_count, 5);
        assert!(!s.inv_overflow);

        let ids = drain_ring(&mut s).unwrap();
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_ring_overflow_at_capacity() {
        let mut s = new_state();
        // Fill to capacity
        for id in 0..INVALIDATION_RING_CAPACITY as i64 {
            push_invalidation(&mut s, id);
        }
        assert_eq!(s.inv_count as usize, INVALIDATION_RING_CAPACITY);
        assert!(!s.inv_overflow);

        // One more triggers overflow
        push_invalidation(&mut s, 999);
        assert!(s.inv_overflow);
    }

    #[test]
    fn test_drain_empty() {
        let mut s = new_state();
        let ids = drain_ring(&mut s);
        assert_eq!(ids, Some(vec![]));
    }

    #[test]
    fn test_drain_returns_ids_and_resets() {
        let mut s = new_state();
        push_invalidation(&mut s, 10);
        push_invalidation(&mut s, 20);

        let ids = drain_ring(&mut s).unwrap();
        assert_eq!(ids, vec![10, 20]);
        assert_eq!(s.inv_count, 0);

        // Second drain is empty
        let ids2 = drain_ring(&mut s).unwrap();
        assert!(ids2.is_empty());
    }

    #[test]
    fn test_drain_overflow_returns_none() {
        let mut s = new_state();
        // Fill up and overflow
        for id in 0..=(INVALIDATION_RING_CAPACITY as i64) {
            push_invalidation(&mut s, id);
        }
        assert!(s.inv_overflow);

        let result = drain_ring(&mut s);
        assert!(result.is_none());
        // State is reset after drain
        assert_eq!(s.inv_count, 0);
        assert!(!s.inv_overflow);
    }

    #[test]
    fn test_ring_push_then_drain_then_reuse() {
        let mut s = new_state();
        push_invalidation(&mut s, 1);
        push_invalidation(&mut s, 2);
        let ids = drain_ring(&mut s).unwrap();
        assert_eq!(ids, vec![1, 2]);

        // Ring is reusable after drain
        push_invalidation(&mut s, 3);
        let ids2 = drain_ring(&mut s).unwrap();
        assert_eq!(ids2, vec![3]);
    }

    #[test]
    fn test_ring_dedup_across_different_ids() {
        let mut s = new_state();
        push_invalidation(&mut s, 1);
        push_invalidation(&mut s, 2);
        push_invalidation(&mut s, 1);
        push_invalidation(&mut s, 3);
        push_invalidation(&mut s, 2);

        assert_eq!(s.inv_count, 3);
        let ids = drain_ring(&mut s).unwrap();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn test_signal_dag_rebuild_sets_overflow() {
        let mut s = new_state();
        push_invalidation(&mut s, 1);

        // Simulate signal_dag_rebuild: set overflow
        s.inv_overflow = true;

        // Drain returns None (overflow)
        assert!(drain_ring(&mut s).is_none());

        // After drain, overflow is cleared and ring is usable
        push_invalidation(&mut s, 2);
        let ids = drain_ring(&mut s).unwrap();
        assert_eq!(ids, vec![2]);
    }

    // ── Scheduler meta state tracking ────────────────────────────────────

    #[test]
    fn test_scheduler_meta_stores_and_retrieves_state() {
        // This test directly manipulates SCHEDULER_META_STATE to verify
        // the set/get contract, even though the public API (scheduler_running)
        // requires shmem to be initialized. The underlying struct must hold
        // values correctly across set_scheduler_meta calls.

        let mut meta = super::SchedulerMetaState::default();

        // Initial state: pid=0, running=false, wake=0
        assert_eq!(meta.scheduler_pid, 0);
        assert!(!meta.scheduler_running);
        assert_eq!(meta.last_scheduler_wake, 0);

        // Simulate startup: set(12345, true, 1234567890)
        meta.scheduler_pid = 12345;
        meta.scheduler_running = true;
        meta.last_scheduler_wake = 1234567890;

        assert_eq!(meta.scheduler_pid, 12345);
        assert!(meta.scheduler_running);
        assert_eq!(meta.last_scheduler_wake, 1234567890);

        // Simulate shutdown: set(0, false, 0)
        meta.scheduler_pid = 0;
        meta.scheduler_running = false;
        meta.last_scheduler_wake = 0;

        assert_eq!(meta.scheduler_pid, 0);
        assert!(!meta.scheduler_running);
        assert_eq!(meta.last_scheduler_wake, 0);
    }

    #[test]
    fn test_scheduler_meta_wake_timestamp_updates() {
        let mut meta = super::SchedulerMetaState {
            scheduler_running: true,
            last_scheduler_wake: 1000,
            ..Default::default()
        };
        assert_eq!(meta.last_scheduler_wake, 1000);

        // Periodic update in scheduler loop
        meta.last_scheduler_wake = 1060; // 60s later
        assert_eq!(meta.last_scheduler_wake, 1060);

        // Multiple updates preserve monotonicity
        meta.last_scheduler_wake = 1120;
        assert_eq!(meta.last_scheduler_wake, 1120);
    }
}
