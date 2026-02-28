//! Integration tests for end-to-end stream table workflows.
//!
//! These tests simulate the full lifecycle of creating source tables,
//! creating "stream tables" (storage tables + catalog entries),
//! refreshing them, and verifying the results.

mod common;

use common::TestDb;

// ── Full Refresh Workflow ──────────────────────────────────────────────────

#[tokio::test]
async fn test_full_refresh_workflow() {
    let db = TestDb::with_catalog().await;

    // Create source table with data
    db.execute("CREATE TABLE orders (id INT PRIMARY KEY, customer TEXT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO orders VALUES (1, 'Alice', 100), (2, 'Bob', 200), (3, 'Charlie', 300)")
        .await;

    let src_oid: i32 = db.query_scalar("SELECT 'orders'::regclass::oid::int").await;

    // Simulate create_stream_table: create storage table
    db.execute(
        "CREATE TABLE public.enriched_orders (\
         __pgt_row_id BIGINT, id INT, customer TEXT, amount NUMERIC\
        )",
    )
    .await;

    let storage_oid: i32 = db
        .query_scalar("SELECT 'enriched_orders'::regclass::oid::int")
        .await;

    // Insert catalog entry
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, schedule, refresh_mode) \
         VALUES ({}, 'enriched_orders', 'public', \
                 'SELECT id, customer, amount FROM orders WHERE amount > 50', \
                 '1 minute', 'FULL')",
        storage_oid
    ))
    .await;

    // Insert dependency
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_dependencies (pgt_id, source_relid, source_type) \
         VALUES (1, {}, 'TABLE')",
        src_oid
    ))
    .await;

    // Simulate full refresh: populate storage table
    db.execute(
        "INSERT INTO public.enriched_orders (__pgt_row_id, id, customer, amount) \
         SELECT hashtext(row_to_json(sub)::text)::bigint, sub.* \
         FROM (SELECT id, customer, amount FROM orders WHERE amount > 50) sub",
    )
    .await;

    let count = db.count("public.enriched_orders").await;
    assert_eq!(count, 3, "All orders have amount > 50");

    // Update catalog: mark as populated
    db.execute(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET is_populated = true, status = 'ACTIVE', \
         data_timestamp = now(), last_refresh_at = now() \
         WHERE pgt_id = 1",
    )
    .await;

    // Record refresh history
    db.execute(
        "INSERT INTO pgtrickle.pgt_refresh_history \
         (pgt_id, data_timestamp, start_time, end_time, action, status, rows_inserted) \
         VALUES (1, now(), now() - interval '1 second', now(), 'FULL', 'COMPLETED', 3)",
    )
    .await;

    // Verify the refresh was recorded
    let refresh_status: String = db
        .query_scalar(
            "SELECT status FROM pgtrickle.pgt_refresh_history WHERE pgt_id = 1 ORDER BY refresh_id DESC LIMIT 1",
        )
        .await;
    assert_eq!(refresh_status, "COMPLETED");

    // Verify ST is active
    let pgt_status: String = db
        .query_scalar("SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(pgt_status, "ACTIVE");
}

// ── Differential Data Changes ───────────────────────────────────────────────

#[tokio::test]
async fn test_source_data_changes_tracked() {
    let db = TestDb::with_catalog().await;

    // Create source and ST
    db.execute("CREATE TABLE products (id INT PRIMARY KEY, price NUMERIC)")
        .await;
    db.execute("INSERT INTO products VALUES (1, 10.00), (2, 20.00)")
        .await;

    let src_oid: i32 = db
        .query_scalar("SELECT 'products'::regclass::oid::int")
        .await;

    // Create change buffer table (typed columns matching source schema)
    db.execute(&format!(
        "CREATE TABLE pgtrickle_changes.changes_{} (\
         change_id   BIGSERIAL PRIMARY KEY,\
         lsn         PG_LSN NOT NULL,\
         action      CHAR(1) NOT NULL,\
         pk_hash     BIGINT,\
         \"new_id\" INT, \"new_price\" NUMERIC,\
         \"old_id\" INT, \"old_price\" NUMERIC\
        )",
        src_oid
    ))
    .await;

    // Simulate CDC: record an INSERT change
    db.execute(&format!(
        "INSERT INTO pgtrickle_changes.changes_{} (lsn, action, \"new_id\", \"new_price\") \
         VALUES ('0/ABCD', 'I', 3, 30.00)",
        src_oid
    ))
    .await;

    // Simulate CDC: record an UPDATE change
    db.execute(&format!(
        "INSERT INTO pgtrickle_changes.changes_{} (lsn, action, \
         \"new_id\", \"new_price\", \"old_id\", \"old_price\") \
         VALUES ('0/ABCE', 'U', 1, 15.00, 1, 10.00)",
        src_oid
    ))
    .await;

    // Simulate CDC: record a DELETE change
    db.execute(&format!(
        "INSERT INTO pgtrickle_changes.changes_{} (lsn, action, \"old_id\", \"old_price\") \
         VALUES ('0/ABCF', 'D', 2, 20.00)",
        src_oid
    ))
    .await;

    let change_count: i64 = db
        .query_scalar(&format!(
            "SELECT count(*) FROM pgtrickle_changes.changes_{}",
            src_oid
        ))
        .await;
    assert_eq!(change_count, 3);

    // Verify changes are ordered by LSN
    let lsns: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT lsn::text FROM pgtrickle_changes.changes_{} ORDER BY lsn",
        src_oid
    ))
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(lsns.len(), 3);

    // After processing, delete consumed changes
    db.execute(&format!(
        "DELETE FROM pgtrickle_changes.changes_{} WHERE lsn <= '0/ABCF'",
        src_oid
    ))
    .await;

    let remaining: i64 = db
        .query_scalar(&format!(
            "SELECT count(*) FROM pgtrickle_changes.changes_{}",
            src_oid
        ))
        .await;
    assert_eq!(remaining, 0, "All consumed changes should be deleted");
}

// ── DAG-like Dependency Chain ──────────────────────────────────────────────

#[tokio::test]
async fn test_chained_stream_tables() {
    let db = TestDb::with_catalog().await;

    // Base table -> ST1 -> ST2 (chained)
    db.execute("CREATE TABLE base_data (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE st1_storage (id INT, val INT)")
        .await;
    db.execute("CREATE TABLE st2_storage (total_val BIGINT)")
        .await;

    let base_oid: i32 = db
        .query_scalar("SELECT 'base_data'::regclass::oid::int")
        .await;
    let st1_oid: i32 = db
        .query_scalar("SELECT 'st1_storage'::regclass::oid::int")
        .await;
    let st2_oid: i32 = db
        .query_scalar("SELECT 'st2_storage'::regclass::oid::int")
        .await;

    // Create ST1: SELECT * FROM base_data
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, schedule, refresh_mode, status) \
         VALUES ({}, 'st1', 'public', 'SELECT * FROM base_data', '1m', 'FULL', 'ACTIVE')",
        st1_oid
    ))
    .await;

    // Create ST2: SELECT SUM(val) FROM st1 (depends on ST1)
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, schedule, refresh_mode, status) \
         VALUES ({}, 'st2', 'public', 'SELECT SUM(val) FROM st1_storage', '5m', 'FULL', 'ACTIVE')",
        st2_oid
    ))
    .await;

    // Dependencies:
    // ST1 -> base_data (TABLE)
    // ST2 -> st1_storage (STREAM_TABLE)
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_dependencies (pgt_id, source_relid, source_type) VALUES \
         (1, {}, 'TABLE'), (2, {}, 'STREAM_TABLE')",
        base_oid, st1_oid
    ))
    .await;

    // Verify the dependency chain
    let st1_sources: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.pgt_dependencies WHERE pgt_id = 1")
        .await;
    let st2_sources: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.pgt_dependencies WHERE pgt_id = 2")
        .await;
    assert_eq!(st1_sources, 1);
    assert_eq!(st2_sources, 1);

    // Verify we can query the dependency graph
    let graph: Vec<(i64, String)> = sqlx::query_as(
        "SELECT d.pgt_id, d.source_type \
         FROM pgtrickle.pgt_dependencies d \
         ORDER BY d.pgt_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();

    assert_eq!(graph.len(), 2);
    assert_eq!(graph[0], (1, "TABLE".to_string()));
    assert_eq!(graph[1], (2, "STREAM_TABLE".to_string()));
}

// ── Error Handling and Suspension ──────────────────────────────────────────

#[tokio::test]
async fn test_error_escalation_and_suspension() {
    let db = TestDb::with_catalog().await;

    db.execute("CREATE TABLE err_src (id INT)").await;
    let oid: i32 = db
        .query_scalar("SELECT 'err_src'::regclass::oid::int")
        .await;

    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, refresh_mode, status) \
         VALUES ({}, 'err_st', 'public', 'SELECT * FROM err_src', 'FULL', 'ACTIVE')",
        oid
    ))
    .await;

    // Simulate 3 consecutive failures with refresh history
    for i in 1..=3 {
        db.execute(&format!(
            "INSERT INTO pgtrickle.pgt_refresh_history \
             (pgt_id, data_timestamp, start_time, end_time, action, status, error_message) \
             VALUES (1, now(), now() - interval '{} seconds', now(), 'FULL', 'FAILED', \
                     'Connection refused')",
            i
        ))
        .await;

        db.execute(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET consecutive_errors = consecutive_errors + 1 WHERE pgt_id = 1",
        )
        .await;
    }

    // After 3 errors, auto-suspend
    let errors: i32 = db
        .query_scalar("SELECT consecutive_errors FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(errors, 3);

    db.execute(
        "UPDATE pgtrickle.pgt_stream_tables SET status = 'SUSPENDED' WHERE pgt_id = 1 AND consecutive_errors >= 3",
    )
    .await;

    let status: String = db
        .query_scalar("SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(status, "SUSPENDED");

    // Verify failure history
    let failure_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgtrickle.pgt_refresh_history \
             WHERE pgt_id = 1 AND status = 'FAILED'",
        )
        .await;
    assert_eq!(failure_count, 3);
}
