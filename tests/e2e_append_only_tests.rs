//! E2E tests for the append-only INSERT fast path (Phase 5, A-3a).
//!
//! Validates:
//! - Append-only declaration on CREATE STREAM TABLE
//! - Fast INSERT path bypasses MERGE for insert-only workloads
//! - CDC heuristic fallback: reverts to MERGE when DELETE/UPDATE detected
//! - ALTER STREAM TABLE to enable/disable append_only
//! - Validation: append_only rejected for FULL, IMMEDIATE, keyless sources

mod e2e;

use e2e::E2eDb;

/// Basic append-only creation and INSERT-only refresh.
#[tokio::test]
async fn test_append_only_basic_insert_path() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_src (id INT PRIMARY KEY, val TEXT NOT NULL)")
        .await;
    db.execute("INSERT INTO ao_src VALUES (1, 'a'), (2, 'b')")
        .await;

    // Create append-only stream table
    db.execute(
        "SELECT pgtrickle.create_stream_table('ao_basic', \
         $$SELECT id, val FROM ao_src$$, '1m', 'DIFFERENTIAL', true, \
         NULL, NULL, NULL, true)",
    )
    .await;

    // Verify catalog flag is set
    let is_ao: bool = sqlx::query_scalar(
        "SELECT is_append_only FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ao_basic'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(is_ao, "is_append_only should be true after creation");

    // Initial data should be present
    let count: i64 = db.count("public.ao_basic").await;
    assert_eq!(count, 2, "initial population should have 2 rows");

    // Insert more data and refresh — should use append-only INSERT path
    db.execute("INSERT INTO ao_src VALUES (3, 'c'), (4, 'd')")
        .await;
    db.refresh_st("ao_basic").await;

    let count: i64 = db.count("public.ao_basic").await;
    assert_eq!(count, 4, "after append-only refresh, should have 4 rows");
}

/// Verify append-only refresh produces correct data content.
#[tokio::test]
async fn test_append_only_data_correctness() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_data_src (id INT PRIMARY KEY, amount INT NOT NULL)")
        .await;
    db.execute("INSERT INTO ao_data_src VALUES (1, 100)").await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('ao_data', \
         $$SELECT id, amount FROM ao_data_src$$, '1m', 'DIFFERENTIAL', true, \
         NULL, NULL, NULL, true)",
    )
    .await;

    // Add rows across multiple refresh cycles
    db.execute("INSERT INTO ao_data_src VALUES (2, 200), (3, 300)")
        .await;
    db.refresh_st("ao_data").await;

    db.execute("INSERT INTO ao_data_src VALUES (4, 400)").await;
    db.refresh_st("ao_data").await;

    // Verify all data present
    let sum: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(amount), 0)::bigint FROM public.ao_data")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(sum, 1000, "sum of all amounts should be 1000");

    let count: i64 = db.count("public.ao_data").await;
    assert_eq!(count, 4, "should have 4 rows total");
}

/// CDC heuristic fallback: DELETE on source reverts append-only to MERGE.
#[tokio::test]
async fn test_append_only_fallback_on_delete() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_fallback_src (id INT PRIMARY KEY, val TEXT NOT NULL)")
        .await;
    db.execute("INSERT INTO ao_fallback_src VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('ao_fallback', \
         $$SELECT id, val FROM ao_fallback_src$$, '1m', 'DIFFERENTIAL', true, \
         NULL, NULL, NULL, true)",
    )
    .await;

    // Delete a row — should trigger heuristic fallback
    db.execute("DELETE FROM ao_fallback_src WHERE id = 2").await;
    db.refresh_st("ao_fallback").await;

    // Verify the flag was reverted
    let is_ao: bool = sqlx::query_scalar(
        "SELECT is_append_only FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ao_fallback'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        !is_ao,
        "is_append_only should be reverted to false after DELETE detected"
    );

    // Verify data correctness after fallback
    let count: i64 = db.count("public.ao_fallback").await;
    assert_eq!(
        count, 2,
        "should have 2 rows after delete and MERGE fallback"
    );

    // Verify row-level correctness after MERGE fallback — deleted row must be absent
    db.assert_st_matches_query("public.ao_fallback", "SELECT id, val FROM ao_fallback_src")
        .await;
}

/// CDC heuristic fallback: UPDATE on source reverts append-only to MERGE.
#[tokio::test]
async fn test_append_only_fallback_on_update() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_upd_src (id INT PRIMARY KEY, val TEXT NOT NULL)")
        .await;
    db.execute("INSERT INTO ao_upd_src VALUES (1, 'old'), (2, 'old')")
        .await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('ao_upd', \
         $$SELECT id, val FROM ao_upd_src$$, '1m', 'DIFFERENTIAL', true, \
         NULL, NULL, NULL, true)",
    )
    .await;

    // Update a row — should trigger heuristic fallback
    db.execute("UPDATE ao_upd_src SET val = 'new' WHERE id = 1")
        .await;
    db.refresh_st("ao_upd").await;

    // Verify the flag was reverted
    let is_ao: bool = sqlx::query_scalar(
        "SELECT is_append_only FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ao_upd'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        !is_ao,
        "is_append_only should be reverted to false after UPDATE detected"
    );

    // Verify data correctness
    let val: String = sqlx::query_scalar("SELECT val FROM public.ao_upd WHERE id = 1")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        val, "new",
        "updated value should be reflected after MERGE fallback"
    );

    // Verify full multiset: no stale old-value rows and correct updated-value row
    db.assert_st_matches_query("public.ao_upd", "SELECT id, val FROM ao_upd_src")
        .await;
}

/// ALTER STREAM TABLE: enable append_only on existing stream table.
#[tokio::test]
async fn test_alter_enable_append_only() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_alter_src (id INT PRIMARY KEY, val TEXT NOT NULL)")
        .await;
    db.execute("INSERT INTO ao_alter_src VALUES (1, 'x')").await;

    // Create without append_only
    db.create_st(
        "ao_alter",
        "SELECT id, val FROM ao_alter_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Enable append_only via ALTER
    db.alter_st("ao_alter", "append_only => true").await;

    let is_ao: bool = sqlx::query_scalar(
        "SELECT is_append_only FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ao_alter'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(is_ao, "append_only should be true after ALTER");

    // Refresh should work with append-only path
    db.execute("INSERT INTO ao_alter_src VALUES (2, 'y')").await;
    db.refresh_st("ao_alter").await;

    let count: i64 = db.count("public.ao_alter").await;
    assert_eq!(count, 2);

    // Verify data correctness after INSERT via append-only path
    db.assert_st_matches_query("public.ao_alter", "SELECT id, val FROM ao_alter_src")
        .await;
}

/// Validation: append_only rejected for FULL refresh mode.
#[tokio::test]
async fn test_append_only_rejected_for_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_full_src (id INT PRIMARY KEY)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('ao_full', \
             $$SELECT id FROM ao_full_src$$, '1m', 'FULL', true, \
             NULL, NULL, NULL, true)",
        )
        .await;
    assert!(
        result.is_err(),
        "append_only should be rejected for FULL refresh mode"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("FULL"),
        "error should mention FULL mode: {}",
        err
    );
}

/// Validation: append_only rejected for IMMEDIATE refresh mode.
#[tokio::test]
async fn test_append_only_rejected_for_immediate_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_imm_src (id INT PRIMARY KEY)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('ao_imm', \
             $$SELECT id FROM ao_imm_src$$, '1m', 'IMMEDIATE', true, \
             NULL, NULL, NULL, true)",
        )
        .await;
    assert!(
        result.is_err(),
        "append_only should be rejected for IMMEDIATE refresh mode"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("IMMEDIATE"),
        "error should mention IMMEDIATE mode: {}",
        err
    );
}

/// Validation: append_only rejected for keyless sources.
#[tokio::test]
async fn test_append_only_rejected_for_keyless_source() {
    let db = E2eDb::new().await.with_extension().await;

    // Create table without primary key
    db.execute("CREATE TABLE ao_keyless_src (id INT, val TEXT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('ao_keyless', \
             $$SELECT id, val FROM ao_keyless_src$$, '1m', 'DIFFERENTIAL', true, \
             NULL, NULL, NULL, true)",
        )
        .await;
    assert!(
        result.is_err(),
        "append_only should be rejected for keyless sources"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("keyless"),
        "error should mention keyless: {}",
        err
    );
}

/// Validation: ALTER to append_only rejected for FULL mode stream table.
#[tokio::test]
async fn test_alter_append_only_rejected_for_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_alt_full_src (id INT PRIMARY KEY)")
        .await;

    db.create_st(
        "ao_alt_full",
        "SELECT id FROM ao_alt_full_src",
        "1m",
        "FULL",
    )
    .await;

    let result = db
        .try_execute("SELECT pgtrickle.alter_stream_table('ao_alt_full', append_only => true)")
        .await;
    assert!(
        result.is_err(),
        "ALTER append_only=true should be rejected for FULL mode"
    );
}

/// No-data cycle: append-only path correctly returns (0, 0) when no changes.
#[tokio::test]
async fn test_append_only_no_data_cycle() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ao_nodata_src (id INT PRIMARY KEY, val TEXT NOT NULL)")
        .await;
    db.execute("INSERT INTO ao_nodata_src VALUES (1, 'a')")
        .await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('ao_nodata', \
         $$SELECT id, val FROM ao_nodata_src$$, '1m', 'DIFFERENTIAL', true, \
         NULL, NULL, NULL, true)",
    )
    .await;

    // Refresh with no changes — should be no-op
    db.refresh_st("ao_nodata").await;

    let count: i64 = db.count("public.ao_nodata").await;
    assert_eq!(count, 1, "no-data cycle should not change row count");
}
