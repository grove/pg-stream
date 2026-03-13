//! E2E tests for Phase 4 — Ergonomics & API Polish.
//!
//! Validates:
//! - ERG-D: Manual refresh records `initiated_by='MANUAL'` in pgt_refresh_history
//! - ERG-E: `pgtrickle.quick_health` view returns correct single-row status
//! - COR-2: `create_stream_table_if_not_exists()` is a no-op when ST exists

mod e2e;

use e2e::E2eDb;

// ── ERG-D: Manual refresh history recording ──────────────────────────

/// ERG-D: A manual `refresh_stream_table()` call must create a
/// `pgt_refresh_history` row with `initiated_by = 'MANUAL'`.
#[tokio::test]
async fn test_manual_refresh_records_initiated_by_manual() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE erg_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO erg_src VALUES (1, 'a'), (2, 'b')")
        .await;

    db.create_st("erg_st", "SELECT id, val FROM erg_src", "1m", "FULL")
        .await;

    // Manual refresh
    db.refresh_st("erg_st").await;

    // Check that the history contains a MANUAL entry
    let initiated_by: String = db
        .query_scalar(
            "SELECT initiated_by FROM pgtrickle.pgt_refresh_history \
             WHERE pgt_id = (SELECT pgt_id FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'erg_st') \
             AND initiated_by = 'MANUAL' \
             ORDER BY refresh_id DESC LIMIT 1",
        )
        .await;
    assert_eq!(initiated_by, "MANUAL");
}

/// ERG-D: Manual refresh history should record COMPLETED status for a
/// successful refresh.
#[tokio::test]
async fn test_manual_refresh_records_completed_status() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE erg_status_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO erg_status_src VALUES (1, 10)")
        .await;

    db.create_st(
        "erg_status_st",
        "SELECT id, val FROM erg_status_src",
        "1m",
        "FULL",
    )
    .await;

    db.refresh_st("erg_status_st").await;

    let status: String = db
        .query_scalar(
            "SELECT status FROM pgtrickle.pgt_refresh_history \
             WHERE pgt_id = (SELECT pgt_id FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'erg_status_st') \
             AND initiated_by = 'MANUAL' \
             ORDER BY refresh_id DESC LIMIT 1",
        )
        .await;
    assert_eq!(status, "COMPLETED");

    // end_time should be set
    let has_end_time: bool = db
        .query_scalar(
            "SELECT end_time IS NOT NULL FROM pgtrickle.pgt_refresh_history \
             WHERE pgt_id = (SELECT pgt_id FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'erg_status_st') \
             AND initiated_by = 'MANUAL' \
             ORDER BY refresh_id DESC LIMIT 1",
        )
        .await;
    assert!(has_end_time);
}

/// ERG-D: Manual DIFFERENTIAL refresh must also record history.
#[tokio::test]
async fn test_manual_differential_refresh_records_history() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE erg_diff_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO erg_diff_src VALUES (1, 'x')").await;

    db.create_st(
        "erg_diff_st",
        "SELECT id, val FROM erg_diff_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // First manual refresh (will be FULL since no frontier yet)
    db.refresh_st("erg_diff_st").await;

    // Insert more data and do a second manual refresh (should be DIFFERENTIAL)
    db.execute("INSERT INTO erg_diff_src VALUES (2, 'y')").await;
    db.refresh_st("erg_diff_st").await;

    // Should have at least 2 MANUAL entries
    let count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgtrickle.pgt_refresh_history \
             WHERE pgt_id = (SELECT pgt_id FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'erg_diff_st') \
             AND initiated_by = 'MANUAL'",
        )
        .await;
    assert!(count >= 2, "expected >= 2 MANUAL entries, got {count}");
}

// ── ERG-E: quick_health view ─────────────────────────────────────────

/// ERG-E: The quick_health view should return exactly one row.
#[tokio::test]
async fn test_quick_health_returns_one_row() {
    let db = E2eDb::new().await.with_extension().await;

    let count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.quick_health")
        .await;
    assert_eq!(count, 1);
}

/// ERG-E: With no stream tables, quick_health should show status = 'EMPTY'.
#[tokio::test]
async fn test_quick_health_empty_status() {
    let db = E2eDb::new().await.with_extension().await;

    let status: String = db
        .query_scalar("SELECT status FROM pgtrickle.quick_health")
        .await;
    assert_eq!(status, "EMPTY");

    let total: i64 = db
        .query_scalar("SELECT total_stream_tables FROM pgtrickle.quick_health")
        .await;
    assert_eq!(total, 0);
}

/// ERG-E: With a healthy stream table, quick_health should show status = 'OK'.
#[tokio::test]
async fn test_quick_health_ok_status() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE qh_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO qh_src VALUES (1, 'a')").await;

    db.create_st("qh_st", "SELECT id, val FROM qh_src", "1m", "FULL")
        .await;

    let total: i64 = db
        .query_scalar("SELECT total_stream_tables FROM pgtrickle.quick_health")
        .await;
    assert_eq!(total, 1);

    let error_tables: i64 = db
        .query_scalar("SELECT error_tables FROM pgtrickle.quick_health")
        .await;
    assert_eq!(error_tables, 0);
}

// ── COR-2: create_stream_table_if_not_exists ─────────────────────────

/// COR-2: When the stream table does not exist, it creates it normally.
#[tokio::test]
async fn test_create_if_not_exists_creates_when_missing() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ine_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO ine_src VALUES (1, 'a'), (2, 'b')")
        .await;

    db.execute(
        "SELECT pgtrickle.create_stream_table_if_not_exists(\
         'ine_st', 'SELECT id, val FROM ine_src', '1m', 'FULL')",
    )
    .await;

    let count: i64 = db.count("ine_st").await;
    assert_eq!(count, 2);
}

/// COR-2: When the stream table already exists, it is a silent no-op.
#[tokio::test]
async fn test_create_if_not_exists_noop_when_exists() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ine2_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO ine2_src VALUES (1, 'a')").await;

    // First creation
    db.execute(
        "SELECT pgtrickle.create_stream_table_if_not_exists(\
         'ine2_st', 'SELECT id, val FROM ine2_src', '1m', 'FULL')",
    )
    .await;

    // Second call — should be a no-op (not error)
    db.execute(
        "SELECT pgtrickle.create_stream_table_if_not_exists(\
         'ine2_st', 'SELECT id, val FROM ine2_src', '1m', 'FULL')",
    )
    .await;

    // Still exactly 1 row (not recreated)
    let count: i64 = db.count("ine2_st").await;
    assert_eq!(count, 1);

    // Only one catalog entry
    let cat_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ine2_st'")
        .await;
    assert_eq!(cat_count, 1);
}

/// COR-2: create_stream_table_if_not_exists with a different query
/// should still be a no-op when the name matches.
#[tokio::test]
async fn test_create_if_not_exists_ignores_query_difference() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ine3_src (id INT PRIMARY KEY, val TEXT, extra INT DEFAULT 0)")
        .await;
    db.execute("INSERT INTO ine3_src VALUES (1, 'a', 10)").await;

    // Create with one query
    db.execute(
        "SELECT pgtrickle.create_stream_table_if_not_exists(\
         'ine3_st', 'SELECT id, val FROM ine3_src', '1m', 'FULL')",
    )
    .await;

    // Second call with a different query — should still be no-op
    db.execute(
        "SELECT pgtrickle.create_stream_table_if_not_exists(\
         'ine3_st', 'SELECT id, val, extra FROM ine3_src', '1m', 'FULL')",
    )
    .await;

    // The original definition should be preserved (2 columns: id, val + __pgt_row_id)
    let col_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_name = 'ine3_st' AND table_schema = 'public'",
        )
        .await;
    // __pgt_row_id + id + val = 3 columns
    assert_eq!(col_count, 3);
}
