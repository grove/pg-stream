//! E2E tests for D-4: Shared Change Buffers.
//!
//! Validates that multiple stream tables referencing the same source table
//! share a single change buffer (`pgtrickle_changes.changes_{oid}`), with
//! correct multi-frontier cleanup coordination.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════════
// D-4.1 — Fan-out: 10 STs from one source share a single change buffer
// ═══════════════════════════════════════════════════════════════════════════

/// Create 10 stream tables from the same source table. Verify:
/// 1. Only one change buffer table exists (`changes_{oid}`).
/// 2. All 10 STs produce correct results after full + differential refresh.
/// 3. The `pgtrickle.shared_buffer_stats()` function reports accurate metrics.
#[tokio::test]
async fn test_d4_fanout_10_sts_shared_buffer() {
    let db = E2eDb::new().await.with_extension().await;

    // Source table with a few columns.
    db.execute(
        "CREATE TABLE d4_src (
            id   SERIAL PRIMARY KEY,
            grp  TEXT NOT NULL,
            val  INT NOT NULL,
            note TEXT DEFAULT 'x'
        )",
    )
    .await;
    db.execute(
        "INSERT INTO d4_src (grp, val) VALUES
            ('a', 10), ('b', 20), ('c', 30), ('a', 40), ('b', 50)",
    )
    .await;

    // Create 10 STs with different defining queries over the same source.
    let st_names = [
        "d4_sum_grp",
        "d4_cnt_grp",
        "d4_max_grp",
        "d4_min_grp",
        "d4_avg_grp",
        "d4_sum_all",
        "d4_cnt_all",
        "d4_filter_a",
        "d4_filter_b",
        "d4_id_note",
    ];
    let st_queries = [
        "SELECT grp, SUM(val) AS total FROM d4_src GROUP BY grp",
        "SELECT grp, COUNT(*) AS cnt FROM d4_src GROUP BY grp",
        "SELECT grp, MAX(val) AS mx FROM d4_src GROUP BY grp",
        "SELECT grp, MIN(val) AS mn FROM d4_src GROUP BY grp",
        "SELECT grp, AVG(val) AS av FROM d4_src GROUP BY grp",
        "SELECT SUM(val) AS total FROM d4_src",
        "SELECT COUNT(*) AS cnt FROM d4_src",
        "SELECT id, val FROM d4_src WHERE grp = 'a'",
        "SELECT id, val FROM d4_src WHERE grp = 'b'",
        "SELECT id, note FROM d4_src",
    ];

    for (name, query) in st_names.iter().zip(st_queries.iter()) {
        db.create_st(name, query, "1m", "DIFFERENTIAL").await;
    }

    // Full refresh all.
    for name in &st_names {
        db.refresh_st(name).await;
    }

    // Verify: only one change buffer table exists for this source.
    let buf_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = 'pgtrickle_changes'
               AND c.relname LIKE 'changes_%'
               AND c.relname NOT LIKE 'changes_pgt_%'",
        )
        .await;
    assert_eq!(
        buf_count, 1,
        "should have exactly 1 change buffer for the source"
    );

    // Verify: shared_buffer_stats reports 10 consumers.
    let consumer_count: i32 = db
        .query_scalar("SELECT consumer_count FROM pgtrickle.shared_buffer_stats() LIMIT 1")
        .await;
    assert_eq!(consumer_count, 10, "shared buffer should have 10 consumers");

    // Insert new rows and differentially refresh all.
    db.execute("INSERT INTO d4_src (grp, val) VALUES ('a', 100), ('c', 200)")
        .await;

    for name in &st_names {
        db.refresh_st(name).await;
    }

    // Validate a few STs.
    let sum_a: i64 = db
        .query_scalar("SELECT total FROM d4_sum_grp WHERE grp = 'a'")
        .await;
    assert_eq!(sum_a, 150); // 10 + 40 + 100

    let cnt_all: i64 = db.query_scalar("SELECT cnt FROM d4_cnt_all").await;
    assert_eq!(cnt_all, 7);

    let filter_a_cnt: i64 = db.query_scalar("SELECT count(*) FROM d4_filter_a").await;
    assert_eq!(filter_a_cnt, 3); // id 1, 4, and the new one
}

// ═══════════════════════════════════════════════════════════════════════════
// D-4.2 — Multi-frontier cleanup: slowest consumer protects buffer rows
// ═══════════════════════════════════════════════════════════════════════════

/// Two STs share a buffer. Refresh only ST1 (ST2 falls behind). Insert more
/// rows. Refresh ST1 again. Buffer rows needed by ST2 must NOT be cleaned up.
/// Then refresh ST2 — it should see all changes.
#[tokio::test]
async fn test_d4_multi_frontier_slowest_consumer_protection() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE d4_mf_src (
            id  SERIAL PRIMARY KEY,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO d4_mf_src (val) VALUES (10), (20)")
        .await;

    db.create_st(
        "d4_mf_fast",
        "SELECT SUM(val) AS total FROM d4_mf_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "d4_mf_slow",
        "SELECT COUNT(*) AS cnt FROM d4_mf_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Full refresh both.
    db.refresh_st("d4_mf_fast").await;
    db.refresh_st("d4_mf_slow").await;

    // Wave 1: insert + refresh only the fast consumer.
    db.execute("INSERT INTO d4_mf_src (val) VALUES (30)").await;
    db.refresh_st("d4_mf_fast").await;

    let fast_total: i64 = db.query_scalar("SELECT total FROM d4_mf_fast").await;
    assert_eq!(fast_total, 60); // 10 + 20 + 30

    // Wave 2: more inserts + refresh only fast consumer again.
    db.execute("INSERT INTO d4_mf_src (val) VALUES (40)").await;
    db.refresh_st("d4_mf_fast").await;

    let fast_total2: i64 = db.query_scalar("SELECT total FROM d4_mf_fast").await;
    assert_eq!(fast_total2, 100); // 10 + 20 + 30 + 40

    // Now refresh the slow consumer — it should catch up with ALL changes.
    db.refresh_st("d4_mf_slow").await;

    let slow_cnt: i64 = db.query_scalar("SELECT cnt FROM d4_mf_slow").await;
    assert_eq!(slow_cnt, 4); // all 4 rows
}

// ═══════════════════════════════════════════════════════════════════════════
// D-4.3 — Column superset: buffer tracks union of all consumer columns
// ═══════════════════════════════════════════════════════════════════════════

/// Two STs reference different columns from the same source. The shared buffer
/// should track the union of both column sets.
#[tokio::test]
async fn test_d4_column_superset_tracked() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE d4_cs_src (
            id   SERIAL PRIMARY KEY,
            col_a TEXT NOT NULL DEFAULT 'x',
            col_b INT NOT NULL DEFAULT 0,
            col_c NUMERIC NOT NULL DEFAULT 0
        )",
    )
    .await;
    db.execute("INSERT INTO d4_cs_src (col_a, col_b, col_c) VALUES ('hello', 1, 10.5)")
        .await;

    // ST1 uses col_a, ST2 uses col_b and col_c.
    db.create_st(
        "d4_cs_st1",
        "SELECT id, col_a FROM d4_cs_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "d4_cs_st2",
        "SELECT id, col_b, col_c FROM d4_cs_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("d4_cs_st1").await;
    db.refresh_st("d4_cs_st2").await;

    // The shared buffer should track col_a, col_b, col_c (superset).
    let columns_tracked: i32 = db
        .query_scalar("SELECT columns_tracked FROM pgtrickle.shared_buffer_stats() LIMIT 1")
        .await;
    // id (PK) + col_a + col_b + col_c = 4 columns
    assert!(
        columns_tracked >= 3,
        "buffer should track at least 3 user columns (union), got {}",
        columns_tracked
    );

    // Both STs work correctly with differential refresh.
    db.execute("INSERT INTO d4_cs_src (col_a, col_b, col_c) VALUES ('world', 2, 20.5)")
        .await;

    db.refresh_st("d4_cs_st1").await;
    db.refresh_st("d4_cs_st2").await;

    let st1_cnt: i64 = db.query_scalar("SELECT count(*) FROM d4_cs_st1").await;
    assert_eq!(st1_cnt, 2);

    let st2_sum: i64 = db
        .query_scalar("SELECT SUM(col_b)::bigint FROM d4_cs_st2")
        .await;
    assert_eq!(st2_sum, 3); // 1 + 2
}

// ═══════════════════════════════════════════════════════════════════════════
// D-4.4 — Shared buffer stats function returns correct data
// ═══════════════════════════════════════════════════════════════════════════

/// Verify that `pgtrickle.shared_buffer_stats()` returns accurate metadata.
#[tokio::test]
async fn test_d4_shared_buffer_stats_accuracy() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE d4_stats_src (
            id  SERIAL PRIMARY KEY,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO d4_stats_src (val) VALUES (1), (2), (3)")
        .await;

    db.create_st(
        "d4_stats_st1",
        "SELECT SUM(val) AS total FROM d4_stats_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "d4_stats_st2",
        "SELECT COUNT(*) AS cnt FROM d4_stats_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("d4_stats_st1").await;
    db.refresh_st("d4_stats_st2").await;

    // Insert data to put rows in the buffer.
    db.execute("INSERT INTO d4_stats_src (val) VALUES (4), (5)")
        .await;

    // Verify stats function returns a row.
    let consumer_count: i32 = db
        .query_scalar("SELECT consumer_count FROM pgtrickle.shared_buffer_stats() LIMIT 1")
        .await;
    assert_eq!(consumer_count, 2, "should have 2 consumers");

    let source_table: String = db
        .query_scalar("SELECT source_table FROM pgtrickle.shared_buffer_stats() LIMIT 1")
        .await;
    assert!(
        source_table.contains("d4_stats_src"),
        "source_table should contain the table name, got: {}",
        source_table
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// D-4.5 — DROP ST reduces consumer count
// ═══════════════════════════════════════════════════════════════════════════

/// When a stream table is dropped, the consumer count for the shared buffer
/// should decrease. When the last consumer is dropped, the buffer is removed.
#[tokio::test]
async fn test_d4_drop_st_reduces_consumer_count() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE d4_drop_src (
            id  SERIAL PRIMARY KEY,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO d4_drop_src (val) VALUES (1)").await;

    db.create_st(
        "d4_drop_st1",
        "SELECT SUM(val) AS s FROM d4_drop_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "d4_drop_st2",
        "SELECT COUNT(*) AS c FROM d4_drop_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("d4_drop_st1").await;
    db.refresh_st("d4_drop_st2").await;

    let cnt_before: i32 = db
        .query_scalar("SELECT consumer_count FROM pgtrickle.shared_buffer_stats() LIMIT 1")
        .await;
    assert_eq!(cnt_before, 2);

    // Drop one ST.
    db.drop_st("d4_drop_st1").await;

    let cnt_after: i32 = db
        .query_scalar("SELECT consumer_count FROM pgtrickle.shared_buffer_stats() LIMIT 1")
        .await;
    assert_eq!(cnt_after, 1);

    // Drop the last ST — buffer tracking should be removed.
    db.drop_st("d4_drop_st2").await;

    let stats_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.shared_buffer_stats()")
        .await;
    assert_eq!(stats_count, 0, "no shared buffers should remain");
}
