//! E2E tests for monitoring views and status functions.
//!
//! Validates `pgtrickle.pgt_status()`, `pgtrickle.stream_tables_info`,
//! `pgtrickle.pg_stat_stream_tables`, and refresh history recording.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

#[tokio::test]
async fn test_pgt_status_returns_rows() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE mon_src (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO mon_src VALUES (1)").await;

    db.create_st("mon_st", "SELECT id FROM mon_src", "1m", "FULL")
        .await;

    let count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.pgt_status()")
        .await;
    assert!(count >= 1, "pgt_status() should return at least 1 row");

    // Verify the row contents
    let (status, mode, populated, errors) = db.pgt_status("mon_st").await;
    assert_eq!(status, "ACTIVE");
    assert_eq!(mode, "FULL");
    assert!(populated);
    assert_eq!(errors, 0);
}

#[tokio::test]
async fn test_pgt_status_multiple_sts() {
    let db = E2eDb::new().await.with_extension().await;

    for i in 1..=3 {
        db.execute(&format!(
            "CREATE TABLE mon_multi_{} (id INT PRIMARY KEY)",
            i
        ))
        .await;
        db.execute(&format!("INSERT INTO mon_multi_{} VALUES (1)", i))
            .await;
        db.create_st(
            &format!("mon_multi_st_{}", i),
            &format!("SELECT id FROM mon_multi_{}", i),
            "1m",
            "FULL",
        )
        .await;
    }

    let count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.pgt_status()")
        .await;
    assert_eq!(count, 3, "pgt_status() should return 3 rows");
}

#[tokio::test]
async fn test_stream_tables_info_view() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE mon_info (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO mon_info VALUES (1)").await;

    db.create_st("mon_info_st", "SELECT id FROM mon_info", "1m", "FULL")
        .await;

    // Refresh to populate data_timestamp
    db.execute("INSERT INTO mon_info VALUES (2)").await;
    db.refresh_st("mon_info_st").await;

    // Verify stream_tables_info view has our ST with staleness columns
    let has_row: bool = db
        .query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM pgtrickle.stream_tables_info \
                WHERE pgt_name = 'mon_info_st' \
            )",
        )
        .await;
    assert!(has_row, "stream_tables_info should contain our ST");

    // Verify staleness and stale columns exist and are queryable
    let stale: bool = db
        .query_scalar(
            "SELECT COALESCE(stale, false) FROM pgtrickle.stream_tables_info \
             WHERE pgt_name = 'mon_info_st'",
        )
        .await;
    // Just after refresh, staleness should not exceed schedule
    assert!(!stale, "stale should be false right after refresh");
}

#[tokio::test]
async fn test_pg_stat_stream_tables_view() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE mon_stat (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO mon_stat VALUES (1)").await;

    db.create_st("mon_stat_st", "SELECT id FROM mon_stat", "1m", "FULL")
        .await;

    // Do a manual refresh
    db.execute("INSERT INTO mon_stat VALUES (2)").await;
    db.refresh_st("mon_stat_st").await;

    // Verify pg_stat_stream_tables view exists and has our ST
    let has_row: bool = db
        .query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM pgtrickle.pg_stat_stream_tables \
                WHERE pgt_name = 'mon_stat_st' \
            )",
        )
        .await;
    assert!(has_row, "pg_stat_stream_tables should contain our ST");

    // Verify key columns exist and are queryable
    let status: String = db
        .query_scalar(
            "SELECT status FROM pgtrickle.pg_stat_stream_tables WHERE pgt_name = 'mon_stat_st'",
        )
        .await;
    assert_eq!(status, "ACTIVE");

    // total_refreshes may be 0 because manual refresh doesn't record history,
    // only the scheduler does. Verify the column is queryable.
    let total: i64 = db
        .query_scalar(
            "SELECT total_refreshes FROM pgtrickle.pg_stat_stream_tables WHERE pgt_name = 'mon_stat_st'",
        )
        .await;
    assert!(
        total >= 0,
        "total_refreshes should be accessible (may be 0 for manual refresh)"
    );
}

#[tokio::test]
async fn test_stale_detection() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE mon_sched (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO mon_sched VALUES (1)").await;

    // Create ST with minimum schedule (60 seconds)
    db.create_st("mon_sched_st", "SELECT id FROM mon_sched", "1m", "FULL")
        .await;

    // The view should show staleness which grows over time.
    // Right after initial populate, staleness should be very small.
    let has_staleness: bool = db
        .query_scalar(
            "SELECT staleness IS NOT NULL FROM pgtrickle.stream_tables_info \
             WHERE pgt_name = 'mon_sched_st'",
        )
        .await;
    assert!(
        has_staleness,
        "staleness should be computed in stream_tables_info"
    );

    // stale should be false right after creation (schedule=60s)
    let stale: bool = db
        .query_scalar(
            "SELECT COALESCE(stale, false) FROM pgtrickle.stream_tables_info \
             WHERE pgt_name = 'mon_sched_st'",
        )
        .await;
    assert!(!stale, "stale should be false immediately after creation");
}

#[tokio::test]
async fn test_refresh_history_records() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE mon_hist (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO mon_hist VALUES (1)").await;

    db.create_st("mon_hist_st", "SELECT id FROM mon_hist", "1m", "FULL")
        .await;

    // Multiple manual refreshes
    for i in 2..=4 {
        db.execute(&format!("INSERT INTO mon_hist VALUES ({})", i))
            .await;
        db.refresh_st("mon_hist_st").await;
    }

    // Manual refresh doesn't write to pgt_refresh_history (only scheduler does).
    // Verify the table exists and is queryable.
    let table_exists = db.table_exists("pgtrickle", "pgt_refresh_history").await;
    assert!(table_exists, "pgt_refresh_history table should exist");

    // Verify the history table has the expected columns
    let col_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = 'pgtrickle' AND table_name = 'pgt_refresh_history'",
        )
        .await;
    assert!(
        col_count >= 5,
        "pgt_refresh_history should have at least 5 columns, got {}",
        col_count,
    );

    // Verify the ST's catalog was correctly updated by manual refresh
    let count = db.count("public.mon_hist_st").await;
    assert_eq!(count, 4, "ST should have all 4 rows after refreshes");
}
