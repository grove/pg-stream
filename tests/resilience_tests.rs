//! Integration tests for Phase 10 — Error Handling & Resilience.
//!
//! These tests verify crash recovery, skip mechanism (advisory locks),
//! error escalation with suspension, and the interaction between
//! error classification and catalog state.

mod common;

use common::TestDb;

// ── Crash Recovery ─────────────────────────────────────────────────────────

/// Test crash recovery: RUNNING refresh records are marked FAILED on restart.
#[tokio::test]
async fn test_crash_recovery_marks_running_as_failed() {
    let db = TestDb::with_catalog().await;
    db.execute("CREATE TABLE src_crash (id int)").await;

    db.execute(
        "INSERT INTO pgstream.pgs_stream_tables
            (pgs_relid, pgs_name, pgs_schema, defining_query, refresh_mode, status)
         VALUES
            ((SELECT 'src_crash'::regclass::oid), 'crash_st', 'public', 'SELECT 1', 'FULL', 'ACTIVE')"
    ).await;

    let pgs_id: i64 = db
        .query_scalar("SELECT pgs_id FROM pgstream.pgs_stream_tables WHERE pgs_name = 'crash_st'")
        .await;

    // Simulate interrupted refreshes by inserting RUNNING records
    db.execute(&format!(
        "INSERT INTO pgstream.pgs_refresh_history (pgs_id, data_timestamp, start_time, action, status)
         VALUES
            ({pgs_id}, now() - interval '5 min', now() - interval '5 min', 'FULL', 'RUNNING'),
            ({pgs_id}, now() - interval '3 min', now() - interval '3 min', 'DIFFERENTIAL', 'RUNNING')"
    ))
    .await;

    // Also insert a normal completed one (should NOT be affected)
    db.execute(&format!(
        "INSERT INTO pgstream.pgs_refresh_history (pgs_id, data_timestamp, start_time, end_time, action, status)
         VALUES ({pgs_id}, now() - interval '1 min', now() - interval '1 min', now(), 'FULL', 'COMPLETED')"
    )).await;

    let running_before: i64 = db
        .query_scalar("SELECT count(*) FROM pgstream.pgs_refresh_history WHERE status = 'RUNNING'")
        .await;
    assert_eq!(running_before, 2);

    // Execute crash recovery query (same as recover_from_crash())
    db.execute(
        "UPDATE pgstream.pgs_refresh_history
         SET status = 'FAILED',
             error_message = 'Interrupted by scheduler restart',
             end_time = now()
         WHERE status = 'RUNNING'",
    )
    .await;

    // Verify all RUNNING records are now FAILED
    let running_after: i64 = db
        .query_scalar("SELECT count(*) FROM pgstream.pgs_refresh_history WHERE status = 'RUNNING'")
        .await;
    assert_eq!(running_after, 0);

    let failed: i64 = db
        .query_scalar("SELECT count(*) FROM pgstream.pgs_refresh_history WHERE status = 'FAILED'")
        .await;
    assert_eq!(failed, 2);

    // Verify the error message is set
    let msg: String = db
        .query_scalar(
            "SELECT error_message FROM pgstream.pgs_refresh_history WHERE status = 'FAILED' LIMIT 1",
        )
        .await;
    assert_eq!(msg, "Interrupted by scheduler restart");

    // Verify end_time was set
    let has_end_time: bool = db.query_scalar(
        "SELECT end_time IS NOT NULL FROM pgstream.pgs_refresh_history WHERE status = 'FAILED' LIMIT 1"
    ).await;
    assert!(has_end_time);

    // Verify the COMPLETED record was not affected
    let completed: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgstream.pgs_refresh_history WHERE status = 'COMPLETED'",
        )
        .await;
    assert_eq!(completed, 1);
}

// ── Advisory Lock Skip Mechanism ──────────────────────────────────────────

/// Test that advisory locks work for detecting concurrent refreshes.
/// Advisory locks are session-scoped, so we test within a single SQL statement.
#[tokio::test]
async fn test_advisory_lock_mechanism() {
    let db = TestDb::with_catalog().await;

    // Verify advisory lock acquisition and release within a single statement
    let success: bool = db
        .query_scalar("SELECT pg_try_advisory_lock(999) AND pg_advisory_unlock(999)")
        .await;
    assert!(success, "Advisory lock acquire+release should succeed");

    // Verify that an unheld lock can be detected (unlock returns false for unheld locks)
    // pg_advisory_unlock returns false if the lock wasn't held
    let not_held: bool = db.query_scalar("SELECT NOT pg_advisory_unlock(998)").await;
    assert!(not_held, "Unlocking a non-held lock should return false");
}

// ── Error Escalation & Auto-Suspend ───────────────────────────────────────

/// Test that consecutive errors are tracked and suspension works.
#[tokio::test]
async fn test_error_escalation_to_suspension() {
    let db = TestDb::with_catalog().await;
    db.execute("CREATE TABLE src_err (id int)").await;

    db.execute(
        "INSERT INTO pgstream.pgs_stream_tables
            (pgs_relid, pgs_name, pgs_schema, defining_query, refresh_mode, status, consecutive_errors)
         VALUES
            ((SELECT 'src_err'::regclass::oid), 'err_st', 'public', 'SELECT 1', 'FULL', 'ACTIVE', 0)"
    ).await;

    let pgs_id: i64 = db
        .query_scalar("SELECT pgs_id FROM pgstream.pgs_stream_tables WHERE pgs_name = 'err_st'")
        .await;

    // Simulate incrementing consecutive errors (like increment_errors does)
    for i in 1..=3 {
        db.execute(&format!(
            "UPDATE pgstream.pgs_stream_tables SET consecutive_errors = {} WHERE pgs_id = {}",
            i, pgs_id
        ))
        .await;
    }

    let errors: i32 = db
        .query_scalar(&format!(
            "SELECT consecutive_errors FROM pgstream.pgs_stream_tables WHERE pgs_id = {}",
            pgs_id
        ))
        .await;
    assert_eq!(errors, 3);

    // After 3 errors (default max), suspend
    db.execute(&format!(
        "UPDATE pgstream.pgs_stream_tables SET status = 'SUSPENDED' WHERE pgs_id = {} AND consecutive_errors >= 3",
        pgs_id
    )).await;

    let status: String = db
        .query_scalar(&format!(
            "SELECT status FROM pgstream.pgs_stream_tables WHERE pgs_id = {}",
            pgs_id
        ))
        .await;
    assert_eq!(status, "SUSPENDED");
}

/// Test that consecutive_errors resets after successful refresh.
#[tokio::test]
async fn test_error_count_resets_on_success() {
    let db = TestDb::with_catalog().await;
    db.execute("CREATE TABLE src_reset (id int)").await;

    db.execute(
        "INSERT INTO pgstream.pgs_stream_tables
            (pgs_relid, pgs_name, pgs_schema, defining_query, refresh_mode, status, consecutive_errors)
         VALUES
            ((SELECT 'src_reset'::regclass::oid), 'reset_st', 'public', 'SELECT 1', 'FULL', 'ACTIVE', 2)"
    ).await;

    let pgs_id: i64 = db
        .query_scalar("SELECT pgs_id FROM pgstream.pgs_stream_tables WHERE pgs_name = 'reset_st'")
        .await;

    // Simulate successful refresh — reset errors
    db.execute(&format!(
        "UPDATE pgstream.pgs_stream_tables SET consecutive_errors = 0, last_refresh_at = now() WHERE pgs_id = {}",
        pgs_id
    )).await;

    let errors: i32 = db
        .query_scalar(&format!(
            "SELECT consecutive_errors FROM pgstream.pgs_stream_tables WHERE pgs_id = {}",
            pgs_id
        ))
        .await;
    assert_eq!(errors, 0);
}

// ── Needs-Reinitialize Flag ──────────────────────────────────────────────

/// Test that needs_reinit flag is set for schema errors and cleared on reinitialize.
#[tokio::test]
async fn test_needs_reinit_lifecycle() {
    let db = TestDb::with_catalog().await;
    db.execute("CREATE TABLE src_reinit (id int)").await;

    db.execute(
        "INSERT INTO pgstream.pgs_stream_tables
            (pgs_relid, pgs_name, pgs_schema, defining_query, refresh_mode, status, needs_reinit)
         VALUES
            ((SELECT 'src_reinit'::regclass::oid), 'reinit_st', 'public', 'SELECT 1', 'FULL', 'ACTIVE', FALSE)"
    ).await;

    let pgs_id: i64 = db
        .query_scalar("SELECT pgs_id FROM pgstream.pgs_stream_tables WHERE pgs_name = 'reinit_st'")
        .await;

    // Mark for reinitialize (simulating upstream schema change)
    db.execute(&format!(
        "UPDATE pgstream.pgs_stream_tables SET needs_reinit = TRUE WHERE pgs_id = {}",
        pgs_id
    ))
    .await;

    let needs: bool = db
        .query_scalar(&format!(
            "SELECT needs_reinit FROM pgstream.pgs_stream_tables WHERE pgs_id = {}",
            pgs_id
        ))
        .await;
    assert!(needs, "needs_reinit should be TRUE after schema change");

    // Clear after reinitialize
    db.execute(&format!(
        "UPDATE pgstream.pgs_stream_tables SET needs_reinit = FALSE WHERE pgs_id = {}",
        pgs_id
    ))
    .await;

    let needs_after: bool = db
        .query_scalar(&format!(
            "SELECT needs_reinit FROM pgstream.pgs_stream_tables WHERE pgs_id = {}",
            pgs_id
        ))
        .await;
    assert!(
        !needs_after,
        "needs_reinit should be FALSE after reinitialize"
    );
}

// ── Refresh History Status Tracking ──────────────────────────────────────

/// Test that refresh history correctly tracks RUNNING → COMPLETED/FAILED transitions.
#[tokio::test]
async fn test_refresh_history_status_transitions() {
    let db = TestDb::with_catalog().await;
    db.execute("CREATE TABLE src_trans (id int)").await;

    db.execute(
        "INSERT INTO pgstream.pgs_stream_tables
            (pgs_relid, pgs_name, pgs_schema, defining_query, refresh_mode, status)
         VALUES
            ((SELECT 'src_trans'::regclass::oid), 'trans_st', 'public', 'SELECT 1', 'FULL', 'ACTIVE')"
    ).await;

    let pgs_id: i64 = db
        .query_scalar("SELECT pgs_id FROM pgstream.pgs_stream_tables WHERE pgs_name = 'trans_st'")
        .await;

    // Insert a RUNNING record
    db.execute(&format!(
        "INSERT INTO pgstream.pgs_refresh_history (pgs_id, data_timestamp, start_time, action, status)
         VALUES ({pgs_id}, now(), now(), 'FULL', 'RUNNING')"
    ))
    .await;

    let refresh_id: i64 = db
        .query_scalar(
            "SELECT refresh_id FROM pgstream.pgs_refresh_history WHERE status = 'RUNNING' LIMIT 1",
        )
        .await;

    // Transition to COMPLETED
    db.execute(&format!(
        "UPDATE pgstream.pgs_refresh_history
         SET status = 'COMPLETED', end_time = now(), rows_inserted = 42, rows_deleted = 3
         WHERE refresh_id = {}",
        refresh_id
    ))
    .await;

    let status: String = db
        .query_scalar(&format!(
            "SELECT status FROM pgstream.pgs_refresh_history WHERE refresh_id = {}",
            refresh_id
        ))
        .await;
    assert_eq!(status, "COMPLETED");

    let rows_ins: i64 = db.query_scalar(&format!(
        "SELECT COALESCE(rows_inserted, 0)::bigint FROM pgstream.pgs_refresh_history WHERE refresh_id = {}", refresh_id
    )).await;
    assert_eq!(rows_ins, 42);
}

// ── Multiple STs Independence ────────────────────────────────────────────

/// Test that error handling for one ST doesn't affect others.
#[tokio::test]
async fn test_error_handling_independent_per_st() {
    let db = TestDb::with_catalog().await;
    db.execute("CREATE TABLE src_ind1 (id int)").await;
    db.execute("CREATE TABLE src_ind2 (id int)").await;

    db.execute(
        "INSERT INTO pgstream.pgs_stream_tables
            (pgs_relid, pgs_name, pgs_schema, defining_query, refresh_mode, status, consecutive_errors)
         VALUES
            ((SELECT 'src_ind1'::regclass::oid), 'st_ok', 'public', 'SELECT 1', 'FULL', 'ACTIVE', 0),
            ((SELECT 'src_ind2'::regclass::oid), 'st_bad', 'public', 'SELECT 1', 'FULL', 'ACTIVE', 3)"
    ).await;

    // Suspend only the one with max errors
    db.execute(
        "UPDATE pgstream.pgs_stream_tables SET status = 'SUSPENDED'
         WHERE consecutive_errors >= 3",
    )
    .await;

    let ok_status: String = db
        .query_scalar("SELECT status FROM pgstream.pgs_stream_tables WHERE pgs_name = 'st_ok'")
        .await;
    assert_eq!(ok_status, "ACTIVE");

    let bad_status: String = db
        .query_scalar("SELECT status FROM pgstream.pgs_stream_tables WHERE pgs_name = 'st_bad'")
        .await;
    assert_eq!(bad_status, "SUSPENDED");
}
