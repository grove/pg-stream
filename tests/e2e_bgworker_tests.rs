//! E2E tests for the background worker (scheduler) and GUC configuration.
//!
//! These tests verify that:
//! - The extension loads correctly with `shared_preload_libraries`
//! - GUC parameters are registered and queryable
//! - The background scheduler automatically refreshes stale STs
//! - The scheduler respects the `pg_stream.enabled` GUC
//!
//! **Note:** Background worker tests are timing-dependent. They use generous
//! timeouts and retry loops. The scheduler interval and minimum schedule
//! are lowered via `ALTER SYSTEM` to speed up tests.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;
use std::time::Duration;

// ── Helper ─────────────────────────────────────────────────────────────────

/// Configure the scheduler for fast testing:
/// - `pg_stream.scheduler_interval_ms = 100` (wake every 100ms)
/// - `pg_stream.min_schedule_seconds = 1` (allow 1-second schedule)
///
/// Uses `ALTER SYSTEM` + `pg_reload_conf()` so the background worker
/// picks up the changes.
async fn configure_fast_scheduler(db: &E2eDb) {
    db.execute("ALTER SYSTEM SET pg_stream.scheduler_interval_ms = 100")
        .await;
    db.execute("ALTER SYSTEM SET pg_stream.min_schedule_seconds = 1")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    // Give the bgworker a moment to pick up new config
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// Wait until a ST has been auto-refreshed by checking pgs_refresh_history.
/// The scheduler (unlike manual refresh) writes history records.
/// Returns true if a completed record appears within the timeout.
#[allow(dead_code)]
async fn wait_for_scheduler_refresh(db: &E2eDb, pgs_name: &str, timeout: Duration) -> bool {
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        let count: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM pgstream.pgs_refresh_history h \
                 JOIN pgstream.pgs_stream_tables d ON h.pgs_id = d.pgs_id \
                 WHERE d.pgs_name = '{pgs_name}' AND h.status = 'COMPLETED'"
            ))
            .await;

        if count > 0 {
            return true;
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Verify the container starts with `shared_preload_libraries` configured
/// and the extension can be created without errors.
#[tokio::test]
async fn test_extension_loads_with_shared_preload() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    // Verify shared_preload_libraries includes our extension
    let spl: String = db.query_scalar("SHOW shared_preload_libraries").await;
    assert!(
        spl.contains("pg_stream"),
        "shared_preload_libraries should contain pg_stream, got: {}",
        spl,
    );

    // Verify the extension is listed in pg_extension
    let ext_exists: bool = db
        .query_scalar("SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'pg_stream')")
        .await;
    assert!(ext_exists, "Extension should be installed");

    // Verify no ERROR-level messages — check that we can use the API
    let st_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgstream.pgs_stream_tables")
        .await;
    assert_eq!(st_count, 0, "Fresh install should have 0 STs");
}

/// Verify all GUC parameters are registered and return expected defaults.
#[tokio::test]
async fn test_gucs_registered() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    // pg_stream.enabled — default: on
    let enabled: String = db.query_scalar("SHOW pg_stream.enabled").await;
    assert_eq!(enabled, "on", "pg_stream.enabled default should be 'on'");

    // pg_stream.scheduler_interval_ms — default: 1000
    let interval: String = db
        .query_scalar("SHOW pg_stream.scheduler_interval_ms")
        .await;
    assert_eq!(
        interval, "1000",
        "pg_stream.scheduler_interval_ms default should be '1000'"
    );

    // pg_stream.min_schedule_seconds — default: 60
    let min_schedule: String = db.query_scalar("SHOW pg_stream.min_schedule_seconds").await;
    assert_eq!(
        min_schedule, "60",
        "pg_stream.min_schedule_seconds default should be '60'"
    );

    // pg_stream.max_consecutive_errors — default: 3
    let max_errors: String = db
        .query_scalar("SHOW pg_stream.max_consecutive_errors")
        .await;
    assert_eq!(
        max_errors, "3",
        "pg_stream.max_consecutive_errors default should be '3'"
    );

    // pg_stream.change_buffer_schema — default: pgstream_changes
    let buf_schema: String = db.query_scalar("SHOW pg_stream.change_buffer_schema").await;
    assert_eq!(
        buf_schema, "pgstream_changes",
        "pg_stream.change_buffer_schema default should be 'pgstream_changes'"
    );

    // pg_stream.max_concurrent_refreshes — default: 4
    let max_conc: String = db
        .query_scalar("SHOW pg_stream.max_concurrent_refreshes")
        .await;
    assert_eq!(
        max_conc, "4",
        "pg_stream.max_concurrent_refreshes default should be '4'"
    );
}

/// Verify that GUCs can be changed via ALTER SYSTEM and take effect.
#[tokio::test]
async fn test_gucs_can_be_altered() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    // Change scheduler_interval_ms
    db.execute("ALTER SYSTEM SET pg_stream.scheduler_interval_ms = 200")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let interval: String = db
        .query_scalar("SHOW pg_stream.scheduler_interval_ms")
        .await;
    assert_eq!(
        interval, "200",
        "scheduler_interval_ms should be updated to 200"
    );

    // Change min_schedule_seconds
    db.execute("ALTER SYSTEM SET pg_stream.min_schedule_seconds = 5")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let min_schedule: String = db.query_scalar("SHOW pg_stream.min_schedule_seconds").await;
    assert_eq!(
        min_schedule, "5",
        "min_schedule_seconds should be updated to 5"
    );

    // Change enabled
    db.execute("ALTER SYSTEM SET pg_stream.enabled = false")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let enabled: String = db.query_scalar("SHOW pg_stream.enabled").await;
    assert_eq!(enabled, "off", "pg_stream.enabled should be 'off'");

    // Reset back
    db.execute("ALTER SYSTEM SET pg_stream.enabled = true")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
}

/// Create a ST with a short schedule (after lowering the minimum),
/// insert source data, and verify the background scheduler automatically
/// refreshes the ST within the expected timeframe.
#[tokio::test]
async fn test_auto_refresh_within_schedule() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    // Speed up the scheduler for testing
    configure_fast_scheduler(&db).await;

    // Create source table and ST with 1-second schedule
    db.execute("CREATE TABLE auto_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO auto_src VALUES (1, 'initial')")
        .await;

    db.create_st("auto_st", "SELECT id, val FROM auto_src", "1s", "FULL")
        .await;

    // Verify initial population
    let count = db.count("public.auto_st").await;
    assert_eq!(count, 1, "ST should be populated initially");

    // Insert new data into source — this should trigger CDC
    db.execute("INSERT INTO auto_src VALUES (2, 'new_data')")
        .await;

    // Wait for the scheduler to auto-refresh
    // The scheduler detects: (now() - data_timestamp) > schedule (1s)
    // With 100ms interval, this should happen within a few seconds
    let refreshed = db
        .wait_for_auto_refresh("auto_st", Duration::from_secs(30))
        .await;
    assert!(refreshed, "Scheduler should auto-refresh the ST");

    // Verify the new data is materialized
    let count = db.count("public.auto_st").await;
    assert_eq!(count, 2, "ST should contain 2 rows after auto-refresh");

    // Verify refresh history was written by the scheduler
    let history_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgstream.pgs_refresh_history h \
             JOIN pgstream.pgs_stream_tables d ON h.pgs_id = d.pgs_id \
             WHERE d.pgs_name = 'auto_st' AND h.status = 'COMPLETED'",
        )
        .await;
    assert!(
        history_count >= 1,
        "Scheduler should have written at least 1 refresh history record"
    );
}

/// Verify that the scheduler fires differential refresh when the ST
/// is configured with DIFFERENTIAL mode.
#[tokio::test]
async fn test_auto_refresh_differential_mode() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE inc_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO inc_src VALUES (1, 100), (2, 200)")
        .await;

    db.create_st(
        "inc_st",
        "SELECT id, val FROM inc_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.inc_st").await, 2);

    // Insert more data
    db.execute("INSERT INTO inc_src VALUES (3, 300)").await;

    let refreshed = db
        .wait_for_auto_refresh("inc_st", Duration::from_secs(30))
        .await;
    assert!(refreshed, "Scheduler should auto-refresh differential ST");

    assert_eq!(
        db.count("public.inc_st").await,
        3,
        "Differential ST should have 3 rows after auto-refresh"
    );

    // Verify data correctness
    db.assert_st_matches_query("public.inc_st", "SELECT id, val FROM inc_src")
        .await;
}

/// Verify that the scheduler writes refresh history records for
/// successful auto-refreshes (unlike manual refresh which does not).
#[tokio::test]
async fn test_scheduler_writes_refresh_history() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE hist_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO hist_src VALUES (1, 'init')").await;

    db.create_st("hist_st", "SELECT id, val FROM hist_src", "1s", "FULL")
        .await;

    // Initial population does NOT write to history (done by create_stream_table)
    let initial_history: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgstream.pgs_refresh_history h \
             JOIN pgstream.pgs_stream_tables d ON h.pgs_id = d.pgs_id \
             WHERE d.pgs_name = 'hist_st'",
        )
        .await;

    // Insert new data to trigger scheduler refresh
    db.execute("INSERT INTO hist_src VALUES (2, 'new')").await;

    // Wait for the scheduler to refresh
    let refreshed = db
        .wait_for_auto_refresh("hist_st", Duration::from_secs(30))
        .await;
    assert!(refreshed, "Scheduler should auto-refresh");

    // Verify refresh history was written
    let new_history: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgstream.pgs_refresh_history h \
             JOIN pgstream.pgs_stream_tables d ON h.pgs_id = d.pgs_id \
             WHERE d.pgs_name = 'hist_st' AND h.status = 'COMPLETED'",
        )
        .await;
    assert!(
        new_history > initial_history,
        "Scheduler should write COMPLETED records to pgs_refresh_history \
         (initial={}, after={})",
        initial_history,
        new_history,
    );
}

/// Verify that the scheduler correctly handles differential auto-refresh
/// with CDC change buffers, producing correct results.
#[tokio::test]
async fn test_auto_refresh_differential_with_cdc() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE buf_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO buf_src VALUES (1, 'a')").await;

    db.create_st(
        "buf_st",
        "SELECT id, val FROM buf_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.buf_st").await, 1);

    // Insert multiple rows to trigger CDC and differential refresh
    db.execute("INSERT INTO buf_src VALUES (2, 'b')").await;
    db.execute("INSERT INTO buf_src VALUES (3, 'c')").await;

    // Wait for auto-refresh to pick up the new rows
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(30);
    loop {
        if start.elapsed() > timeout {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        if db.count("public.buf_st").await >= 3 {
            break;
        }
    }

    assert_eq!(
        db.count("public.buf_st").await,
        3,
        "Differential auto-refresh should pick up all new rows"
    );

    // Verify data correctness between source and ST
    db.assert_st_matches_query("public.buf_st", "SELECT id, val FROM buf_src")
        .await;
}

/// Verify the scheduler correctly handles two healthy STs, refreshing both.
/// The scheduler processes all STs in a single transaction per tick.
#[tokio::test]
async fn test_scheduler_refreshes_multiple_healthy_sts() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    // Create two independent STs
    db.execute("CREATE TABLE h_src1 (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO h_src1 VALUES (1, 10)").await;

    db.execute("CREATE TABLE h_src2 (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO h_src2 VALUES (1, 20)").await;

    db.create_st("h_st1", "SELECT id, val FROM h_src1", "1s", "FULL")
        .await;

    db.create_st("h_st2", "SELECT id, val FROM h_src2", "1s", "DIFFERENTIAL")
        .await;

    assert_eq!(db.count("public.h_st1").await, 1);
    assert_eq!(db.count("public.h_st2").await, 1);

    // Insert data into both sources
    db.execute("INSERT INTO h_src1 VALUES (2, 11)").await;
    db.execute("INSERT INTO h_src2 VALUES (2, 21)").await;

    // Wait for both to be refreshed (poll row counts)
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(30);
    loop {
        if start.elapsed() > timeout {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let c1 = db.count("public.h_st1").await;
        let c2 = db.count("public.h_st2").await;
        if c1 == 2 && c2 == 2 {
            break;
        }
    }

    assert_eq!(
        db.count("public.h_st1").await,
        2,
        "First ST should have 2 rows after auto-refresh"
    );
    assert_eq!(
        db.count("public.h_st2").await,
        2,
        "Second ST should have 2 rows after auto-refresh"
    );
}

/// Verify the scheduler updates catalog metadata after each refresh:
/// last_refresh_at, data_timestamp, and resets consecutive_errors.
#[tokio::test]
async fn test_auto_refresh_updates_catalog_metadata() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE meta_src (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO meta_src VALUES (1)").await;

    db.create_st("meta_st", "SELECT id FROM meta_src", "1s", "FULL")
        .await;

    // Record initial timestamps
    let _initial_refresh_at: Option<String> = db
        .query_scalar_opt(
            "SELECT last_refresh_at::text FROM pgstream.pgs_stream_tables WHERE pgs_name = 'meta_st'",
        )
        .await;
    let initial_data_ts: Option<String> = db
        .query_scalar_opt(
            "SELECT data_timestamp::text FROM pgstream.pgs_stream_tables WHERE pgs_name = 'meta_st'",
        )
        .await;

    // Insert data and wait for auto-refresh
    db.execute("INSERT INTO meta_src VALUES (2)").await;

    let refreshed = db
        .wait_for_auto_refresh("meta_st", Duration::from_secs(30))
        .await;
    assert!(refreshed, "Scheduler should auto-refresh");

    // Verify timestamps advanced
    let new_refresh_at: Option<String> = db
        .query_scalar_opt(
            "SELECT last_refresh_at::text FROM pgstream.pgs_stream_tables WHERE pgs_name = 'meta_st'",
        )
        .await;
    let new_data_ts: Option<String> = db
        .query_scalar_opt(
            "SELECT data_timestamp::text FROM pgstream.pgs_stream_tables WHERE pgs_name = 'meta_st'",
        )
        .await;

    assert_ne!(
        initial_data_ts, new_data_ts,
        "data_timestamp should advance after auto-refresh"
    );
    // last_refresh_at should be set (might have been NULL initially if
    // the initial population doesn't set it, or it was set)
    assert!(
        new_refresh_at.is_some(),
        "last_refresh_at should be set after auto-refresh"
    );

    // consecutive_errors should be 0
    let (_, _, _, errors) = db.pgs_status("meta_st").await;
    assert_eq!(errors, 0, "consecutive_errors should be 0 after success");
}
