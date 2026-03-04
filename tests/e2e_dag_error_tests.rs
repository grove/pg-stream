//! E2E tests for error resilience in multi-layer DAG pipelines.
//!
//! Validates that failures in one pipeline layer do not corrupt sibling or
//! downstream layers, and that recovery works after fixing the root cause.
//!
//! ## Key architecture paths exercised
//!
//! - `consecutive_errors` catalog tracking after refresh failure
//! - Error isolation between sibling branches (diamond)
//! - Recovery via data fix + re-refresh
//! - Leaf errors not affecting upstream layers
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════════
// Test 6.1 — Error in middle layer does not corrupt siblings
// ═══════════════════════════════════════════════════════════════════════════

/// Diamond: A → B_ok (SUM), A → B_fail (division by zero on bad data).
/// Insert triggering data. Refresh A (succeeds), refresh B_fail (fails),
/// refresh B_ok (succeeds). Verify B_ok is correct and B_fail has errors.
#[tokio::test]
async fn test_error_in_middle_layer_does_not_corrupt_siblings() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE err_sibling_src (
            id    SERIAL PRIMARY KEY,
            grp   TEXT NOT NULL,
            val   INT NOT NULL,
            denom INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO err_sibling_src (grp, val, denom) VALUES
            ('a', 10, 2), ('b', 20, 5)",
    )
    .await;

    // B_ok: simple SUM (won't fail)
    db.create_st(
        "err_b_ok",
        "SELECT grp, SUM(val) AS total FROM err_sibling_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // B_fail: division that will fail on denom=0
    db.create_st(
        "err_b_fail",
        "SELECT grp, SUM(val / denom) AS ratio FROM err_sibling_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial state is correct
    let ok_q = "SELECT grp, SUM(val) AS total FROM err_sibling_src GROUP BY grp";
    db.assert_st_matches_query("err_b_ok", ok_q).await;

    let (_, _, _, errors_before) = db.pgt_status("err_b_fail").await;
    assert_eq!(errors_before, 0, "No errors initially");

    // Insert a row with denom=0 → B_fail's refresh will get division by zero
    db.execute("INSERT INTO err_sibling_src (grp, val, denom) VALUES ('c', 30, 0)")
        .await;

    // Refresh B_ok — should succeed
    db.refresh_st("err_b_ok").await;
    db.assert_st_matches_query("err_b_ok", ok_q).await;

    // Refresh B_fail — should fail
    let result = db
        .try_execute("SELECT pgtrickle.refresh_stream_table('err_b_fail')")
        .await;
    assert!(
        result.is_err(),
        "B_fail refresh should fail on division by zero"
    );

    // Verify B_ok is still correct (not corrupted by sibling failure)
    db.assert_st_matches_query("err_b_ok", ok_q).await;

    // Verify B_fail has consecutive_errors > 0
    let (_, _, _, errors_after) = db.pgt_status("err_b_fail").await;
    assert!(
        errors_after > 0,
        "B_fail should have consecutive_errors > 0 after failure"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 6.2 — Error recovery after data fix
// ═══════════════════════════════════════════════════════════════════════════

/// A → B → C. B's query has a division that fails on bad data.
/// Fix the data at the source, refresh pipeline. Verify full convergence.
#[tokio::test]
async fn test_error_recovery_after_data_fix() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE err_fix_src (
            id    SERIAL PRIMARY KEY,
            grp   TEXT NOT NULL,
            val   INT NOT NULL,
            denom INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO err_fix_src (grp, val, denom) VALUES
            ('a', 10, 2), ('b', 20, 4)",
    )
    .await;

    // B: safe division initially
    db.create_st(
        "err_fix_b",
        "SELECT grp, SUM(val / denom) AS ratio FROM err_fix_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // C: downstream of B
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'err_fix_c',
            $$SELECT grp, ratio * 10 AS scaled FROM err_fix_b$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    let b_q = "SELECT grp, SUM(val / denom) AS ratio FROM err_fix_src GROUP BY grp";
    let c_q = "SELECT grp, SUM(val / denom) * 10 AS scaled FROM err_fix_src GROUP BY grp";

    db.assert_st_matches_query("err_fix_b", b_q).await;
    db.assert_st_matches_query("err_fix_c", c_q).await;

    // Insert bad data
    db.execute("INSERT INTO err_fix_src (grp, val, denom) VALUES ('c', 30, 0)")
        .await;

    // Refresh B should fail
    let result = db
        .try_execute("SELECT pgtrickle.refresh_stream_table('err_fix_b')")
        .await;
    assert!(result.is_err(), "B should fail with division by zero");

    // Fix the data: update the bad row's denominator
    db.execute("UPDATE err_fix_src SET denom = 3 WHERE denom = 0")
        .await;

    // Now B should succeed
    db.refresh_st("err_fix_b").await;
    db.refresh_st("err_fix_c").await;

    // Full convergence
    db.assert_st_matches_query("err_fix_b", b_q).await;
    db.assert_st_matches_query("err_fix_c", c_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 6.3 — Consecutive errors tracked in catalog
// ═══════════════════════════════════════════════════════════════════════════

/// Trigger repeated failures and verify `consecutive_errors` increments.
/// After recovery, verify it resets to 0.
#[tokio::test]
async fn test_consecutive_errors_tracked_and_reset() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE err_cnt_src (
            id    SERIAL PRIMARY KEY,
            val   INT NOT NULL,
            denom INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO err_cnt_src (val, denom) VALUES (10, 0)")
        .await;

    // This ST will fail on refresh due to division by zero
    db.create_st(
        "err_cnt_st",
        "SELECT SUM(val / denom) AS ratio FROM err_cnt_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Try refresh multiple times — each should fail
    for i in 1..=3 {
        let result = db
            .try_execute("SELECT pgtrickle.refresh_stream_table('err_cnt_st')")
            .await;
        assert!(result.is_err(), "Refresh attempt {i} should fail");
    }

    let (_, _, _, errors) = db.pgt_status("err_cnt_st").await;
    assert!(
        errors >= 1,
        "consecutive_errors should be at least 1, got {errors}"
    );

    // Fix the data
    db.execute("UPDATE err_cnt_src SET denom = 2 WHERE denom = 0")
        .await;

    // Successful refresh should reset consecutive_errors
    db.refresh_st("err_cnt_st").await;

    let (_, _, _, errors_after) = db.pgt_status("err_cnt_st").await;
    assert_eq!(
        errors_after, 0,
        "consecutive_errors should reset to 0 after successful refresh"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 6.4 — Error in leaf does not affect upstream
// ═══════════════════════════════════════════════════════════════════════════

/// A → B → C. C's refresh fails. Verify A and B are still correct and
/// can continue to refresh normally.
#[tokio::test]
async fn test_error_in_leaf_does_not_affect_upstream() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE err_leaf_src (
            id    SERIAL PRIMARY KEY,
            grp   TEXT NOT NULL,
            val   INT NOT NULL,
            denom INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO err_leaf_src (grp, val, denom) VALUES
            ('a', 10, 2), ('b', 20, 4)",
    )
    .await;

    // A (L1): simple SUM — always works
    db.create_st(
        "err_leaf_a",
        "SELECT grp, SUM(val) AS total FROM err_leaf_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // B (L2): project on A — always works
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'err_leaf_b',
            $$SELECT grp, total * 2 AS doubled FROM err_leaf_a$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // C (L3): division on source — will fail on bad data
    // Note: C reads from err_leaf_src directly (not from B), so it's a
    // leaf in the diamond sense.
    db.create_st(
        "err_leaf_c",
        "SELECT grp, SUM(val / denom) AS ratio FROM err_leaf_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let a_q = "SELECT grp, SUM(val) AS total FROM err_leaf_src GROUP BY grp";
    let b_q = "SELECT grp, SUM(val) * 2 AS doubled FROM err_leaf_src GROUP BY grp";

    db.assert_st_matches_query("err_leaf_a", a_q).await;
    db.assert_st_matches_query("err_leaf_b", b_q).await;

    // Insert bad data for C's division
    db.execute("INSERT INTO err_leaf_src (grp, val, denom) VALUES ('c', 30, 0)")
        .await;

    // Refresh A and B — should succeed
    db.refresh_st("err_leaf_a").await;
    db.refresh_st("err_leaf_b").await;
    db.assert_st_matches_query("err_leaf_a", a_q).await;
    db.assert_st_matches_query("err_leaf_b", b_q).await;

    // C should fail
    let result = db
        .try_execute("SELECT pgtrickle.refresh_stream_table('err_leaf_c')")
        .await;
    assert!(result.is_err(), "C should fail on division by zero");

    // A and B should still be correct
    db.assert_st_matches_query("err_leaf_a", a_q).await;
    db.assert_st_matches_query("err_leaf_b", b_q).await;

    // Continue mutating — A and B should keep working
    db.execute("INSERT INTO err_leaf_src (grp, val, denom) VALUES ('d', 40, 8)")
        .await;
    db.refresh_st("err_leaf_a").await;
    db.refresh_st("err_leaf_b").await;
    db.assert_st_matches_query("err_leaf_a", a_q).await;
    db.assert_st_matches_query("err_leaf_b", b_q).await;
}
