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
/// Protected by `PGS_STATE` lightweight lock for concurrent access.
#[derive(Copy, Clone)]
pub struct PgTrickleSharedState {
    /// Incremented when the DAG changes (create/alter/drop ST).
    pub dag_version: u64,
    /// PID of the scheduler background worker (0 if not running).
    pub scheduler_pid: i32,
    /// Whether the scheduler is currently running.
    pub scheduler_running: bool,
    /// Unix timestamp (seconds) of the scheduler's last wake cycle.
    pub last_scheduler_wake: i64,

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
            scheduler_pid: 0,
            scheduler_running: false,
            last_scheduler_wake: 0,
            inv_ring: [0; INVALIDATION_RING_CAPACITY],
            inv_count: 0,
            inv_overflow: false,
        }
    }
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

/// Register shared memory allocations. Called from `_PG_init()`.
pub fn init_shared_memory() {
    pg_shmem_init!(PGS_STATE);
    pg_shmem_init!(DAG_REBUILD_SIGNAL);
    pg_shmem_init!(CACHE_GENERATION);
    pg_shmem_init!(ACTIVE_REFRESH_WORKERS);
    pg_shmem_init!(RECONCILE_EPOCH);
    pg_shmem_init!(TOTAL_DIFF_REFRESHES);
    pg_shmem_init!(DEDUP_NEEDED_REFRESHES);
    pg_shmem_init!(TEMPLATE_CACHE_L2_HITS);
    pg_shmem_init!(TEMPLATE_CACHE_MISSES);
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
}
