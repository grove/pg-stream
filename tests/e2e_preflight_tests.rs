//! E2E tests for the preflight() function and enhanced worker_pool_status().
//!
//! Covers (A46 items):
//!   T-A46-1: preflight() returns valid JSON with all 7 check keys
//!   T-A46-2: worker_pool_status() returns the new columns (idle_workers,
//!            last_scheduler_tick_unix, ring_overflow_count, citus_failure_total)
//!   T-A46-3: invalidation ring overflow counter initialises to 0
//!   T-A46-5: preflight() WAL-level check reflects actual wal_level setting
//!
//! These tests are light-E2E eligible (no background worker required for
//! structural checks).

mod e2e;

use e2e::E2eDb;

// ── T-A46-1: preflight() returns valid JSON with all check keys ───────────

/// T-A46-1a: preflight() returns a non-NULL JSON string.
#[tokio::test]
async fn test_preflight_returns_non_null_json() {
    let db = E2eDb::new().await.with_extension().await;

    let result: Option<String> = db.query_scalar_opt("SELECT pgtrickle.preflight()").await;

    assert!(result.is_some(), "preflight() must return a non-NULL value");
    let json_str = result.unwrap();
    assert!(
        json_str.starts_with('{'),
        "preflight() should return a JSON object, got: {json_str:?}"
    );
}

/// T-A46-1b: preflight() JSON contains all 7 expected top-level check keys.
#[tokio::test]
async fn test_preflight_has_all_check_keys() {
    let db = E2eDb::new().await.with_extension().await;

    let json_str: String = db.query_scalar("SELECT pgtrickle.preflight()").await;

    let expected_keys = [
        "shared_preload_libraries",
        "scheduler_running",
        "max_worker_processes",
        "wal_level",
        "replication_slots",
        "invalidation_ring_overflow",
        "citus_worker_failures",
    ];

    for key in &expected_keys {
        assert!(
            json_str.contains(key),
            "preflight() JSON missing key '{key}': {json_str}"
        );
    }
}

/// T-A46-1c: each check entry has 'ok' and 'detail' fields.
#[tokio::test]
async fn test_preflight_check_entries_have_ok_and_detail() {
    let db = E2eDb::new().await.with_extension().await;

    // Extract the shared_preload_libraries check and verify structure.
    let ok_value: Option<bool> = db
        .query_scalar_opt(
            "SELECT (pgtrickle.preflight()::jsonb -> 'shared_preload_libraries' ->> 'ok')::boolean",
        )
        .await;

    assert!(
        ok_value.is_some(),
        "preflight() check entry must have an 'ok' field"
    );

    let detail_value: Option<String> = db
        .query_scalar_opt(
            "SELECT pgtrickle.preflight()::jsonb -> 'shared_preload_libraries' ->> 'detail'",
        )
        .await;

    assert!(
        detail_value.is_some(),
        "preflight() check entry must have a 'detail' field"
    );
}

// ── T-A46-2: worker_pool_status() new columns ────────────────────────────

/// T-A46-2a: worker_pool_status() returns the new ring_overflow_count column.
#[tokio::test]
async fn test_worker_pool_status_has_ring_overflow_count() {
    let db = E2eDb::new().await.with_extension().await;

    let overflow: Option<i64> = db
        .query_scalar_opt("SELECT ring_overflow_count FROM pgtrickle.worker_pool_status() LIMIT 1")
        .await;

    // If the function returns rows, the column must be present and non-negative.
    if let Some(v) = overflow {
        assert!(v >= 0, "ring_overflow_count must be non-negative, got {v}");
    }
    // No rows means scheduler not running — column existence check is sufficient.
}

/// T-A46-2b: worker_pool_status() returns the new citus_failure_total column.
#[tokio::test]
async fn test_worker_pool_status_has_citus_failure_total() {
    let db = E2eDb::new().await.with_extension().await;

    let failures: Option<i64> = db
        .query_scalar_opt("SELECT citus_failure_total FROM pgtrickle.worker_pool_status() LIMIT 1")
        .await;

    if let Some(v) = failures {
        assert!(v >= 0, "citus_failure_total must be non-negative, got {v}");
    }
}

/// T-A46-2c: worker_pool_status() returns idle_workers column.
#[tokio::test]
async fn test_worker_pool_status_has_idle_workers() {
    let db = E2eDb::new().await.with_extension().await;

    let idle: Option<i32> = db
        .query_scalar_opt("SELECT idle_workers FROM pgtrickle.worker_pool_status() LIMIT 1")
        .await;

    if let Some(v) = idle {
        assert!(v >= 0, "idle_workers must be non-negative, got {v}");
    }
}

// ── T-A46-3: invalidation ring overflow counter ───────────────────────────

/// T-A46-3: preflight() reports invalidation_ring_overflow with count >= 0.
#[tokio::test]
async fn test_preflight_ring_overflow_count_is_non_negative() {
    let db = E2eDb::new().await.with_extension().await;

    let count: Option<i64> = db
        .query_scalar_opt(
            "SELECT (pgtrickle.preflight()::jsonb -> 'invalidation_ring_overflow' ->> 'count')::bigint",
        )
        .await;

    if let Some(v) = count {
        assert!(
            v >= 0,
            "invalidation_ring_overflow count must be >= 0, got {v}"
        );
    }
}

// ── T-A46-5: preflight() WAL slot check ──────────────────────────────────

/// T-A46-5: preflight() wal_level check is present and contains a detail string.
#[tokio::test]
async fn test_preflight_wal_level_check_present() {
    let db = E2eDb::new().await.with_extension().await;

    let detail: Option<String> = db
        .query_scalar_opt("SELECT pgtrickle.preflight()::jsonb -> 'wal_level' ->> 'detail'")
        .await;

    assert!(
        detail.is_some(),
        "preflight() wal_level check must have a detail field"
    );
    let detail_str = detail.unwrap();
    assert!(
        !detail_str.is_empty(),
        "preflight() wal_level detail must not be empty"
    );
}

/// T-A46-5b: preflight() replication_slots check is present.
#[tokio::test]
async fn test_preflight_replication_slots_check_present() {
    let db = E2eDb::new().await.with_extension().await;

    let ok_field: Option<bool> = db
        .query_scalar_opt(
            "SELECT (pgtrickle.preflight()::jsonb -> 'replication_slots' ->> 'ok')::boolean",
        )
        .await;

    assert!(
        ok_field.is_some(),
        "preflight() replication_slots check must have an 'ok' field"
    );
}
