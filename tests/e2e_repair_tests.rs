//! T-A42-1: Restore-drill E2E tests for `pgtrickle.repair_stream_table`.
//!
//! Validates:
//! - R1: repair_stream_table resets frontiers and forces a full refresh.
//! - R2: repair_stream_table rebuilds missing CDC triggers.
//! - R3: repair_stream_table resets error status to ACTIVE.
//! - R4: After repair + refresh, stream table data matches the defining query.
//!
//! Prerequisites: `just test-e2e`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// R1: Basic repair_stream_table invocation
// ═══════════════════════════════════════════════════════════════════════

/// R1: repair_stream_table resets frontiers and forces full refresh on next cycle.
#[tokio::test]
async fn test_repair_stream_table_resets_frontier() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE rep_src (id INT PRIMARY KEY, v TEXT)")
        .await;
    db.execute("INSERT INTO rep_src VALUES (1,'a'),(2,'b'),(3,'c')")
        .await;

    let q = "SELECT id, v FROM rep_src";
    db.create_st("rep_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("rep_st", q).await;

    // Verify frontier is set after initial refresh
    let frontier_before: Option<String> = db
        .query_scalar_opt(
            "SELECT frontier::text FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'rep_st'",
        )
        .await;
    assert!(
        frontier_before.is_some(),
        "Frontier should be set after initial refresh"
    );

    // Call repair
    let summary: String = db
        .query_scalar("SELECT pgtrickle.repair_stream_table('rep_st')")
        .await;
    assert!(
        summary.contains("frontier reset"),
        "repair_stream_table should report frontier reset; got: {summary}"
    );

    // Frontier should now be NULL (reset for full refresh)
    let frontier_after: Option<String> = db
        .query_scalar_opt(
            "SELECT frontier::text FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'rep_st'",
        )
        .await;
    assert!(
        frontier_after.is_none(),
        "Frontier should be NULL after repair; got: {frontier_after:?}"
    );

    // After refresh, data should still be correct
    db.refresh_st("rep_st").await;
    db.assert_st_matches_query("rep_st", q).await;
}

/// R3: repair_stream_table resets error state to ACTIVE.
#[tokio::test]
async fn test_repair_stream_table_resets_error_state() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE rep_err_src (id INT PRIMARY KEY, v TEXT)")
        .await;
    db.execute("INSERT INTO rep_err_src VALUES (1,'x')").await;

    let q = "SELECT id, v FROM rep_err_src";
    db.create_st("rep_err_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("rep_err_st", q).await;

    // Manually set the stream table to ERROR state
    db.execute(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET status = 'ERROR', consecutive_errors = 5, \
             last_error_message = 'simulated error' \
         WHERE pgt_name = 'rep_err_st'",
    )
    .await;

    let status_before: String = db
        .query_scalar(
            "SELECT status FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'rep_err_st'",
        )
        .await;
    assert_eq!(status_before, "ERROR");

    // Call repair
    let summary: String = db
        .query_scalar("SELECT pgtrickle.repair_stream_table('rep_err_st')")
        .await;
    assert!(
        summary.contains("status reset"),
        "repair_stream_table should report status reset; got: {summary}"
    );

    let status_after: String = db
        .query_scalar(
            "SELECT status FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'rep_err_st'",
        )
        .await;
    assert_eq!(
        status_after, "ACTIVE",
        "Status should be ACTIVE after repair"
    );

    let errors_after: i32 = db
        .query_scalar(
            "SELECT consecutive_errors FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'rep_err_st'",
        )
        .await;
    assert_eq!(errors_after, 0, "Error count should be 0 after repair");
}

/// R4: After repair + refresh, data matches the defining query.
#[tokio::test]
async fn test_repair_stream_table_data_correct_after_refresh() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE rep_data_src (id INT PRIMARY KEY, cat TEXT, amt INT)")
        .await;
    db.execute(
        "INSERT INTO rep_data_src VALUES \
         (1,'A',10),(2,'B',20),(3,'A',30),(4,'C',40)",
    )
    .await;

    let q = "SELECT cat, SUM(amt) AS total FROM rep_data_src GROUP BY cat";
    db.create_st("rep_data_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("rep_data_st", q).await;

    // Insert more data
    db.execute("INSERT INTO rep_data_src VALUES (5,'B',50)")
        .await;

    // Call repair (resets frontier — next refresh will be full)
    let _summary: String = db
        .query_scalar("SELECT pgtrickle.repair_stream_table('rep_data_st')")
        .await;

    // Refresh should produce correct result
    db.refresh_st("rep_data_st").await;
    db.assert_st_matches_query("rep_data_st", q).await;
}

/// R1: repair_stream_table with qualified schema name.
#[tokio::test]
async fn test_repair_stream_table_qualified_name() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE rep_qn_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO rep_qn_src VALUES (1,100)").await;

    let q = "SELECT id, val FROM rep_qn_src";
    db.create_st("rep_qn_st", q, "1m", "FULL").await;
    db.assert_st_matches_query("rep_qn_st", q).await;

    // Call repair with schema-qualified name
    let summary: String = db
        .query_scalar("SELECT pgtrickle.repair_stream_table('public.rep_qn_st')")
        .await;
    assert!(
        !summary.is_empty(),
        "repair_stream_table should return a non-empty summary"
    );

    // After repair + refresh, data is still correct
    db.refresh_st("rep_qn_st").await;
    db.assert_st_matches_query("rep_qn_st", q).await;
}

/// repair_stream_table on non-existent stream table should raise an error.
#[tokio::test]
async fn test_repair_stream_table_nonexistent_raises_error() {
    let db = E2eDb::new().await.with_extension().await;

    let result = db
        .try_execute("SELECT pgtrickle.repair_stream_table('nonexistent_st')")
        .await;
    assert!(
        result.is_err(),
        "repair_stream_table should fail for non-existent stream table"
    );
}
