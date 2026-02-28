//! Integration tests for the catalog layer.
//!
//! These tests verify that the pg_trickle catalog tables enforce constraints
//! correctly and that the SQL-level operations work as expected.

mod common;

use common::TestDb;

// ── Schema & Table Existence ───────────────────────────────────────────────

#[tokio::test]
async fn test_catalog_schemas_created() {
    let db = TestDb::with_catalog().await;

    let pg_trickle_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = 'pgtrickle')",
        )
        .await;
    assert!(pg_trickle_exists, "pg_trickle schema should exist");

    let changes_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = 'pgtrickle_changes')",
        )
        .await;
    assert!(changes_exists, "pgtrickle_changes schema should exist");
}

#[tokio::test]
async fn test_catalog_tables_exist() {
    let db = TestDb::with_catalog().await;

    let tables = [
        ("pgtrickle", "pgt_stream_tables"),
        ("pgtrickle", "pgt_dependencies"),
        ("pgtrickle", "pgt_refresh_history"),
        ("pgtrickle", "pgt_change_tracking"),
    ];

    for (schema, table) in tables {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = '{}' AND table_name = '{}')",
                schema, table
            ))
            .await;
        assert!(exists, "Table {}.{} should exist", schema, table);
    }
}

#[tokio::test]
async fn test_stream_tables_info_view_exists() {
    let db = TestDb::with_catalog().await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.views \
             WHERE table_schema = 'pgtrickle' AND table_name = 'stream_tables_info')",
        )
        .await;
    assert!(exists, "pgtrickle.stream_tables_info view should exist");
}

// ── Catalog CRUD Operations ────────────────────────────────────────────────

#[tokio::test]
async fn test_insert_stream_table() {
    let db = TestDb::with_catalog().await;

    // Create a source table for the OID
    db.execute("CREATE TABLE test_source (id INT PRIMARY KEY, val TEXT)")
        .await;

    let source_oid: i32 = db
        .query_scalar("SELECT 'test_source'::regclass::oid::int")
        .await;

    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, schedule, refresh_mode) \
         VALUES ({}, 'test_st', 'public', 'SELECT * FROM test_source', '1m', 'FULL')",
        source_oid
    ))
    .await;

    let count = db.count("pgtrickle.pgt_stream_tables").await;
    assert_eq!(count, 1);

    let name: String = db
        .query_scalar("SELECT pgt_name FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(name, "test_st");

    let status: String = db
        .query_scalar("SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(status, "INITIALIZING");
}

#[tokio::test]
async fn test_unique_name_constraint() {
    let db = TestDb::with_catalog().await;

    db.execute("CREATE TABLE t1 (id INT PRIMARY KEY)").await;
    db.execute("CREATE TABLE t2 (id INT PRIMARY KEY)").await;

    let oid1: i32 = db.query_scalar("SELECT 't1'::regclass::oid::int").await;
    let oid2: i32 = db.query_scalar("SELECT 't2'::regclass::oid::int").await;

    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, refresh_mode) \
         VALUES ({}, 'dup_st', 'public', 'SELECT * FROM t1', 'FULL')",
        oid1
    ))
    .await;

    // Should fail: same name + schema
    let result = db
        .try_execute(&format!(
            "INSERT INTO pgtrickle.pgt_stream_tables \
             (pgt_relid, pgt_name, pgt_schema, defining_query, refresh_mode) \
             VALUES ({}, 'dup_st', 'public', 'SELECT * FROM t2', 'FULL')",
            oid2
        ))
        .await;
    assert!(
        result.is_err(),
        "Duplicate name should fail unique constraint"
    );
}

#[tokio::test]
async fn test_refresh_mode_check_constraint() {
    let db = TestDb::with_catalog().await;
    db.execute("CREATE TABLE t_check (id INT PRIMARY KEY)")
        .await;
    let oid: i32 = db
        .query_scalar("SELECT 't_check'::regclass::oid::int")
        .await;

    // Invalid mode should fail
    let result = db
        .try_execute(&format!(
            "INSERT INTO pgtrickle.pgt_stream_tables \
             (pgt_relid, pgt_name, pgt_schema, defining_query, refresh_mode) \
             VALUES ({}, 'bad_mode', 'public', 'SELECT 1', 'INVALID_MODE')",
            oid
        ))
        .await;
    assert!(
        result.is_err(),
        "Invalid refresh_mode should fail check constraint"
    );
}

#[tokio::test]
async fn test_status_check_constraint() {
    let db = TestDb::with_catalog().await;
    db.execute("CREATE TABLE t_status (id INT PRIMARY KEY)")
        .await;
    let oid: i32 = db
        .query_scalar("SELECT 't_status'::regclass::oid::int")
        .await;

    // Invalid status should fail
    let result = db
        .try_execute(&format!(
            "INSERT INTO pgtrickle.pgt_stream_tables \
             (pgt_relid, pgt_name, pgt_schema, defining_query, status) \
             VALUES ({}, 'bad_status', 'public', 'SELECT 1', 'RUNNING')",
            oid
        ))
        .await;
    assert!(
        result.is_err(),
        "Invalid status should fail check constraint"
    );
}

// ── Dependency Tracking ────────────────────────────────────────────────────

#[tokio::test]
async fn test_dependency_insertion_and_cascade_delete() {
    let db = TestDb::with_catalog().await;

    db.execute("CREATE TABLE dep_source (id INT PRIMARY KEY)")
        .await;
    let src_oid: i32 = db
        .query_scalar("SELECT 'dep_source'::regclass::oid::int")
        .await;

    db.execute("CREATE TABLE dep_storage (id INT PRIMARY KEY)")
        .await;
    let storage_oid: i32 = db
        .query_scalar("SELECT 'dep_storage'::regclass::oid::int")
        .await;

    // Insert ST
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, refresh_mode) \
         VALUES ({}, 'dep_test', 'public', 'SELECT * FROM dep_source', 'DIFFERENTIAL')",
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

    let dep_count = db.count("pgtrickle.pgt_dependencies").await;
    assert_eq!(dep_count, 1);

    // Delete ST — dependencies should cascade
    db.execute("DELETE FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;

    let dep_count = db.count("pgtrickle.pgt_dependencies").await;
    assert_eq!(dep_count, 0, "Dependencies should be cascade deleted");
}

// ── Refresh History ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_refresh_history_recording() {
    let db = TestDb::with_catalog().await;

    db.execute("CREATE TABLE hist_source (id INT PRIMARY KEY)")
        .await;
    let oid: i32 = db
        .query_scalar("SELECT 'hist_source'::regclass::oid::int")
        .await;

    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, refresh_mode) \
         VALUES ({}, 'hist_st', 'public', 'SELECT * FROM hist_source', 'FULL')",
        oid
    ))
    .await;

    // Record a refresh
    db.execute(
        "INSERT INTO pgtrickle.pgt_refresh_history \
         (pgt_id, data_timestamp, start_time, action, status, rows_inserted, rows_deleted) \
         VALUES (1, now(), now(), 'FULL', 'RUNNING', 0, 0)",
    )
    .await;

    let count = db.count("pgtrickle.pgt_refresh_history").await;
    assert_eq!(count, 1);

    // Complete the refresh
    db.execute(
        "UPDATE pgtrickle.pgt_refresh_history \
         SET end_time = now(), status = 'COMPLETED', rows_inserted = 42 \
         WHERE refresh_id = 1",
    )
    .await;

    let rows_inserted: i64 = db
        .query_scalar(
            "SELECT rows_inserted FROM pgtrickle.pgt_refresh_history WHERE refresh_id = 1",
        )
        .await;
    assert_eq!(rows_inserted, 42);
}

#[tokio::test]
async fn test_refresh_history_action_check_constraint() {
    let db = TestDb::with_catalog().await;

    let result = db
        .try_execute(
            "INSERT INTO pgtrickle.pgt_refresh_history \
             (pgt_id, data_timestamp, start_time, action, status) \
             VALUES (1, now(), now(), 'INVALID', 'RUNNING')",
        )
        .await;
    assert!(
        result.is_err(),
        "Invalid action should fail check constraint"
    );
}

// ── stream_tables_info View ───────────────────────────────────────────────

#[tokio::test]
async fn test_stream_tables_info_view() {
    let db = TestDb::with_catalog().await;

    db.execute("CREATE TABLE view_source (id INT)").await;
    let oid: i32 = db
        .query_scalar("SELECT 'view_source'::regclass::oid::int")
        .await;

    // Insert with a data timestamp in the past
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, schedule, \
          refresh_mode, status, is_populated, data_timestamp) \
         VALUES ({}, 'view_st', 'public', 'SELECT * FROM view_source', \
                 '5m', 'FULL', 'ACTIVE', true, now() - interval '10 minutes')",
        oid
    ))
    .await;

    // Query the info view
    let stale: bool = db
        .query_scalar("SELECT stale FROM pgtrickle.stream_tables_info WHERE pgt_name = 'view_st'")
        .await;
    assert!(
        stale,
        "10-minute old data with 5-minute target should be stale"
    );
}

// ── CDC Tracking Table ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_change_tracking_crud() {
    let db = TestDb::with_catalog().await;

    db.execute(
        "INSERT INTO pgtrickle.pgt_change_tracking (source_relid, slot_name, tracked_by_pgt_ids) \
         VALUES (12345, 'pg_trickle_slot_12345', ARRAY[1, 2])",
    )
    .await;

    let slot_name: String = db
        .query_scalar(
            "SELECT slot_name FROM pgtrickle.pgt_change_tracking WHERE source_relid = 12345",
        )
        .await;
    assert_eq!(slot_name, "pg_trickle_slot_12345");

    // Add another ST to tracking
    db.execute(
        "UPDATE pgtrickle.pgt_change_tracking \
         SET tracked_by_pgt_ids = array_append(tracked_by_pgt_ids, 3) \
         WHERE source_relid = 12345",
    )
    .await;

    let pgt_ids: Vec<i64> = db
        .query_scalar(
            "SELECT tracked_by_pgt_ids FROM pgtrickle.pgt_change_tracking WHERE source_relid = 12345",
        )
        .await;
    assert_eq!(pgt_ids, vec![1, 2, 3]);
}

// ── Status Transitions ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_status_transitions() {
    let db = TestDb::with_catalog().await;

    db.execute("CREATE TABLE trans_src (id INT)").await;
    let oid: i32 = db
        .query_scalar("SELECT 'trans_src'::regclass::oid::int")
        .await;

    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, refresh_mode) \
         VALUES ({}, 'trans_st', 'public', 'SELECT * FROM trans_src', 'FULL')",
        oid
    ))
    .await;

    // Initial status should be INITIALIZING
    let status: String = db
        .query_scalar("SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(status, "INITIALIZING");

    // Transition to ACTIVE
    db.execute("UPDATE pgtrickle.pgt_stream_tables SET status = 'ACTIVE' WHERE pgt_id = 1")
        .await;
    let status: String = db
        .query_scalar("SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(status, "ACTIVE");

    // Increment errors — simulate consecutive failures
    for _ in 0..3 {
        db.execute(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET consecutive_errors = consecutive_errors + 1 WHERE pgt_id = 1",
        )
        .await;
    }

    let errors: i32 = db
        .query_scalar("SELECT consecutive_errors FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(errors, 3);

    // Auto-suspend after errors
    db.execute("UPDATE pgtrickle.pgt_stream_tables SET status = 'SUSPENDED' WHERE pgt_id = 1")
        .await;

    // Resume: set ACTIVE + reset errors
    db.execute(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET status = 'ACTIVE', consecutive_errors = 0 WHERE pgt_id = 1",
    )
    .await;
    let errors: i32 = db
        .query_scalar("SELECT consecutive_errors FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;
    assert_eq!(errors, 0);
}

// ── Change Buffer Table Schema ─────────────────────────────────────────────

#[tokio::test]
async fn test_change_buffer_table_schema() {
    let db = TestDb::with_catalog().await;

    // Simulate creating a change buffer table like cdc.rs does (typed columns)
    db.execute(
        "CREATE TABLE pgtrickle_changes.changes_12345 (\
         change_id   BIGSERIAL,\
         lsn         PG_LSN NOT NULL,\
         action      CHAR(1) NOT NULL,\
         pk_hash     BIGINT,\
         \"new_id\" INT, \"new_name\" TEXT,\
         \"old_id\" INT, \"old_name\" TEXT\
        )",
    )
    .await;

    // Insert a sample change
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_12345 (lsn, action, \"new_id\", \"new_name\") \
         VALUES ('0/1234', 'I', 1, 'Alice')",
    )
    .await;

    let count = db.count("pgtrickle_changes.changes_12345").await;
    assert_eq!(count, 1);

    let action: String = db
        .query_scalar("SELECT action FROM pgtrickle_changes.changes_12345 WHERE change_id = 1")
        .await;
    assert_eq!(action, "I");
}

// ── Multiple STs with Shared Sources ───────────────────────────────────────

#[tokio::test]
async fn test_multiple_sts_sharing_source() {
    let db = TestDb::with_catalog().await;

    db.execute("CREATE TABLE shared_source (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE st_storage_1 (id INT)").await;
    db.execute("CREATE TABLE st_storage_2 (id INT)").await;

    let src_oid: i32 = db
        .query_scalar("SELECT 'shared_source'::regclass::oid::int")
        .await;
    let s1_oid: i32 = db
        .query_scalar("SELECT 'st_storage_1'::regclass::oid::int")
        .await;
    let s2_oid: i32 = db
        .query_scalar("SELECT 'st_storage_2'::regclass::oid::int")
        .await;

    // Create two STs
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_stream_tables \
         (pgt_relid, pgt_name, pgt_schema, defining_query, schedule, refresh_mode, status) \
         VALUES \
         ({}, 'st1', 'public', 'SELECT * FROM shared_source', '1m', 'FULL', 'ACTIVE'), \
         ({}, 'st2', 'public', 'SELECT id FROM shared_source WHERE val > 10', '5m', 'DIFFERENTIAL', 'ACTIVE')",
        s1_oid, s2_oid
    ))
    .await;

    // Both depend on the same source
    db.execute(&format!(
        "INSERT INTO pgtrickle.pgt_dependencies (pgt_id, source_relid, source_type) VALUES \
         (1, {src_oid}, 'TABLE'), (2, {src_oid}, 'TABLE')",
    ))
    .await;

    // Verify both are tracked
    let dep_count = db.count("pgtrickle.pgt_dependencies").await;
    assert_eq!(dep_count, 2);

    // Drop st1 — dependencies for st1 cascade, but st2's remain
    db.execute("DELETE FROM pgtrickle.pgt_stream_tables WHERE pgt_id = 1")
        .await;

    let remaining_deps = db.count("pgtrickle.pgt_dependencies").await;
    assert_eq!(remaining_deps, 1, "Only st2's dependency should remain");

    // Verify remaining dependency belongs to st2
    let remaining_pgt_id: i64 = db
        .query_scalar("SELECT pgt_id FROM pgtrickle.pgt_dependencies LIMIT 1")
        .await;
    assert_eq!(remaining_pgt_id, 2);
}
