//! Cost module: predictive cost model helpers for the scheduler.
//!
//! Contains pure functions for computing refresh cost quotas and thresholds.

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

/// A46-10: Compute a lag-aware per-database quota boost factor.
///
/// When `pg_trickle.lag_aware_scheduling` is enabled, this function returns a
/// multiplier (≥ 1.0) based on the ratio of observed max lag to target schedule.
/// A database with 3× the target lag gets up to 2× the base quota.
///
/// `max_lag_secs`: observed maximum stream table lag in this database (seconds).
/// `target_schedule_secs`: typical target schedule period for stream tables (seconds).
///
/// Returns a boost factor in the range [1.0, 2.0].
///
/// Pure logic — extracted for unit-testability.
pub fn lag_aware_quota_boost(max_lag_secs: f64, target_schedule_secs: f64) -> f64 {
    if target_schedule_secs <= 0.0 || max_lag_secs <= 0.0 {
        return 1.0;
    }
    let ratio = max_lag_secs / target_schedule_secs;
    // Boost = 1 + min(1, ratio * 0.5): up to 2× at 2× lag, capped at 2×.
    (1.0_f64 + (ratio * 0.5).min(1.0)).min(2.0)
}

/// A46-10: Compute the effective per-database quota with lag-aware adjustment.
///
/// Combines `compute_per_db_quota` with an optional lag-aware boost when
/// `pg_trickle.lag_aware_scheduling` is enabled.
pub fn compute_per_db_quota_with_lag(
    per_db_quota: i32,
    max_concurrent_refreshes: i32,
    max_cluster: u32,
    current_active: u32,
    max_lag_secs: f64,
    target_schedule_secs: f64,
    lag_aware: bool,
) -> u32 {
    let base = compute_per_db_quota(
        per_db_quota,
        max_concurrent_refreshes,
        max_cluster,
        current_active,
    );
    if !lag_aware || max_lag_secs <= 0.0 {
        return base;
    }
    let boost = lag_aware_quota_boost(max_lag_secs, target_schedule_secs);
    ((base as f64 * boost).round() as u32).max(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_per_db_quota_disabled() {
        // When quota is 0, falls back to max_concurrent_refreshes.
        assert_eq!(compute_per_db_quota(0, 4, 10, 0), 4);
        assert_eq!(compute_per_db_quota(-1, 4, 10, 0), 4);
        // Minimum 1 even when max_concurrent_refreshes is 0
        assert_eq!(compute_per_db_quota(0, 0, 10, 0), 1);
    }

    #[test]
    fn test_compute_per_db_quota_burst_under_80_percent() {
        // Under burst threshold (80% of 10 = 8), quota is 150% of base.
        // base = 4, 150% = 6
        assert_eq!(compute_per_db_quota(4, 2, 10, 5), 6);
    }

    #[test]
    fn test_compute_per_db_quota_at_burst_threshold() {
        // At or over burst threshold, use base quota.
        // 80% of 10 = 8, current_active=8 → at threshold → base quota
        assert_eq!(compute_per_db_quota(4, 2, 10, 8), 4);
        assert_eq!(compute_per_db_quota(4, 2, 10, 10), 4);
    }

    #[test]
    fn test_compute_per_db_quota_base_one() {
        // base=1, burst: max(1*3/2=1, 1+1=2) = 2
        assert_eq!(compute_per_db_quota(1, 2, 10, 0), 2);
    }

    // ── A46-10: Lag-aware quota tests ────────────────────────────────────

    #[test]
    fn test_lag_aware_quota_boost_no_lag() {
        // No lag → boost = 1.0
        assert!((lag_aware_quota_boost(0.0, 60.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_lag_aware_quota_boost_equal_to_schedule() {
        // lag == schedule → ratio=1, boost = 1 + 0.5 = 1.5
        assert!((lag_aware_quota_boost(60.0, 60.0) - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_lag_aware_quota_boost_capped_at_two() {
        // lag >> schedule → boost capped at 2.0
        assert!((lag_aware_quota_boost(600.0, 60.0) - 2.0).abs() < 0.001);
    }

    #[test]
    fn test_lag_aware_quota_boost_zero_target() {
        // Zero target → no boost (safe default)
        assert!((lag_aware_quota_boost(60.0, 0.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_per_db_quota_with_lag_disabled() {
        // lag_aware=false → same as compute_per_db_quota
        assert_eq!(
            compute_per_db_quota_with_lag(4, 2, 10, 5, 120.0, 60.0, false),
            compute_per_db_quota(4, 2, 10, 5)
        );
    }

    #[test]
    fn test_compute_per_db_quota_with_lag_enabled() {
        // lag_aware=true, lag=60s, target=60s → boost=1.5 → floor(base*1.5)
        let base = compute_per_db_quota(4, 2, 10, 5); // = 6 (burst mode)
        let with_lag = compute_per_db_quota_with_lag(4, 2, 10, 5, 60.0, 60.0, true);
        assert!(with_lag >= base);
    }
}
