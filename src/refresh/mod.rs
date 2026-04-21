//! Refresh executor — handles full, differential, and reinitialize refreshes.
//!
//! The executor is called by the scheduler for automated refreshes and by
//! `pgtrickle.refresh_stream_table()` for manual refreshes.
//!
//! ## Delta SQL Caching
//!
//! The differential refresh path caches the delta SQL template and MERGE
//! SQL template per `pgt_id` in thread-local storage. On subsequent
//! refreshes, the cached templates are resolved with actual frontier LSN
//! values — skipping SQL parsing, DVM differentiation, and MERGE SQL
//! string formatting. This eliminates ~45ms of overhead per refresh
//! (29.6ms planning + 15ms generate_delta).
//!
//! ## ARCH-1B: Module structure
//!
//! - [`orchestrator`] — RefreshAction, determine_refresh_action, adaptive cost model,
//!   execute_reinitialize_refresh
//! - [`codegen`]      — SQL template builders, MERGE SQL cache, planner hints,
//!   change-buffer cleanup, ST-to-ST delta capture
//! - [`merge`]        — execute_differential_refresh, execute_full_refresh,
//!   execute_topk_refresh, execute_no_data_refresh,
//!   partition-aware MERGE helpers
//! - [`phd1`]         — PH-D1 phantom-cleanup DELETE+INSERT strategy,
//!   cross-cycle phantom cleanup (EC01-2)

pub(crate) mod codegen;
pub(crate) mod merge;
pub(crate) mod orchestrator;
pub(crate) mod phd1;

// Re-export everything from sub-modules so callers use `refresh::foo` as before.
pub use codegen::*;
pub use merge::*;
pub use orchestrator::*;
// phd1 exports only the cross-cycle phantom cleanup utility used internally.
#[allow(unused_imports)]
pub(crate) use phd1::*;

use std::cell::{Cell, RefCell};

// ── B-4: Query complexity classification ────────────────────────────────

/// Complexity class for a stream table's defining query.
///
/// Used by the cost model to apply per-class cost coefficients.  Higher
/// complexity classes have steeper differential cost curves (more joins /
/// aggregates → more work per delta row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryComplexityClass {
    /// Simple scan: `SELECT cols FROM single_table`
    Scan,
    /// Scan with filter: `SELECT cols FROM single_table WHERE ...`
    Filter,
    /// Aggregate: `SELECT ... GROUP BY ...` (no joins)
    Aggregate,
    /// Join(s) without aggregation
    Join,
    /// Join(s) with GROUP BY aggregation (most expensive differential path)
    JoinAggregate,
}

impl QueryComplexityClass {
    /// Default differential cost scaling factor per class.
    ///
    /// The factor represents the per-delta-row cost multiplier relative to
    /// a plain scan.  Joins and aggregates make each delta row more
    /// expensive to process incrementally.
    pub(crate) fn diff_cost_factor(self) -> f64 {
        match self {
            Self::Scan => 1.0,
            Self::Filter => 1.1,
            Self::Aggregate => 1.5,
            Self::Join => 2.5,
            Self::JoinAggregate => 4.0,
        }
    }
}

/// Classify a defining query's complexity from its SQL text.
///
/// Uses lightweight keyword analysis (no parsing or SPI).  This is
/// intentionally conservative: false positives (over-classifying) are
/// preferable to false negatives because a higher class merely biases
/// the cost model toward FULL at lower change rates, which is always safe.
pub(crate) fn classify_query_complexity(defining_query: &str) -> QueryComplexityClass {
    let upper = defining_query.to_ascii_uppercase();
    let has_join = upper.contains(" JOIN ")
        || upper.contains(" INNER JOIN ")
        || upper.contains(" LEFT JOIN ")
        || upper.contains(" RIGHT JOIN ")
        || upper.contains(" FULL JOIN ")
        || upper.contains(" CROSS JOIN ");
    let has_group_by = upper.contains("GROUP BY");

    match (has_join, has_group_by) {
        (true, true) => QueryComplexityClass::JoinAggregate,
        (true, false) => QueryComplexityClass::Join,
        (false, true) => QueryComplexityClass::Aggregate,
        (false, false) => {
            if upper.contains(" WHERE ") {
                QueryComplexityClass::Filter
            } else {
                QueryComplexityClass::Scan
            }
        }
    }
}

// ── G12-ERM-1: Effective refresh mode tracking ──────────────────────────

// Thread-local that records the mode actually used for the current refresh.
//
// Set by each concrete execution path (`execute_full_refresh`,
// `execute_differential_refresh`, etc.) so the scheduler can write the
// actual mode to `pgt_stream_tables.effective_refresh_mode` after the
// refresh completes — even when an internal fallback changed the mode.
thread_local! {
    static LAST_EFFECTIVE_MODE: Cell<&'static str> = const { Cell::new("") };
}

/// Record the effective refresh mode for the currently-executing refresh.
///
/// Called at the concrete execution point so fallbacks (e.g. adaptive
/// threshold → FULL, CTE → FULL) overwrite the initial mode correctly.
pub(crate) fn set_effective_mode(mode: &'static str) {
    LAST_EFFECTIVE_MODE.with(|m| m.set(mode));
}

/// Take (read and reset) the effective mode recorded by the most recent
/// execution path.  Returns `""` if no refresh has been recorded yet
/// in this thread.
pub fn take_effective_mode() -> &'static str {
    LAST_EFFECTIVE_MODE.with(|m| m.get())
}

// ── PH-E2: Last-refresh spill tracking ──────────────────────────────────

thread_local! {
    /// Temp blocks written during the most recent MERGE execution.
    /// Set after each differential refresh by querying pg_stat_statements.
    /// Read by the scheduler to track per-ST spill history.
    static LAST_TEMP_BLKS_WRITTEN: Cell<i64> = const { Cell::new(-1) };
}

/// Record the temp blocks written for the currently-executing refresh.
pub(crate) fn set_last_temp_blks_written(blks: i64) {
    LAST_TEMP_BLKS_WRITTEN.with(|c| c.set(blks));
}

/// Take the temp blocks written by the most recent differential refresh.
/// Returns -1 if not available (pg_stat_statements not installed, or not
/// a differential refresh).
pub fn take_last_temp_blks_written() -> i64 {
    LAST_TEMP_BLKS_WRITTEN.with(|c| {
        let v = c.get();
        c.set(-1);
        v
    })
}

// ── ARCH-2: Refresh reason tracking ──────────────────────────────────────
//
// Captures the machine-readable reason when the executor takes a non-default
// path (e.g. recomputation fallback for non-monotone recursive CTEs).
// Written to `pgt_refresh_history.refresh_reason` via the scheduler.

thread_local! {
    static LAST_REFRESH_REASON: RefCell<Option<&'static str>> = const { RefCell::new(None) };
}

/// Set the refresh reason for the current execution.
///
/// Called whenever a non-default execution path is taken; the scheduler
/// reads this with `take_refresh_reason()` and writes it to history.
pub(crate) fn set_refresh_reason(reason: &'static str) {
    LAST_REFRESH_REASON.with(|r| *r.borrow_mut() = Some(reason));
}

/// Take (read and reset) the refresh reason set by the current execution path.
/// Returns `None` if the default path was taken.
pub fn take_refresh_reason() -> Option<&'static str> {
    LAST_REFRESH_REASON.with(|r| r.borrow_mut().take())
}

#[cfg(test)]
mod tests;
