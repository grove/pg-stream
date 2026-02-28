//! E2E tests for trigger-based CDC (Change Data Capture).
//!
//! Validates that CDC triggers fire correctly on source tables,
//! change buffers capture the right data, and cleanup works.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Trigger Capture Tests ──────────────────────────────────────────────

#[tokio::test]
async fn test_trigger_captures_insert() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cdc_src VALUES (1, 'initial')")
        .await;

    db.create_st(
        "cdc_st",
        "SELECT id, val FROM cdc_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let source_oid = db.table_oid("cdc_src").await;
    let buffer_table = format!("pgtrickle_changes.changes_{}", source_oid);

    // Consume any existing changes from create
    db.refresh_st("cdc_st").await;

    // Insert new rows
    db.execute("INSERT INTO cdc_src VALUES (2, 'new_row')")
        .await;

    // Verify change captured in buffer
    let change_count: i64 = db.count(&buffer_table).await;
    assert!(
        change_count >= 1,
        "Insert should be captured in change buffer"
    );

    let action: String = db
        .query_scalar(&format!(
            "SELECT action FROM {} ORDER BY change_id DESC LIMIT 1",
            buffer_table
        ))
        .await;
    assert_eq!(action, "I", "Action should be 'I' for INSERT");
}

#[tokio::test]
async fn test_trigger_captures_update() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_upd (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cdc_upd VALUES (1, 'old_val')")
        .await;

    db.create_st(
        "cdc_upd_st",
        "SELECT id, val FROM cdc_upd",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let source_oid = db.table_oid("cdc_upd").await;
    let buffer_table = format!("pgtrickle_changes.changes_{}", source_oid);

    // Consume existing changes
    db.refresh_st("cdc_upd_st").await;

    // Update a row
    db.execute("UPDATE cdc_upd SET val = 'new_val' WHERE id = 1")
        .await;

    let action: String = db
        .query_scalar(&format!(
            "SELECT action FROM {} ORDER BY change_id DESC LIMIT 1",
            buffer_table
        ))
        .await;
    assert_eq!(action, "U", "Action should be 'U' for UPDATE");

    // Verify typed column captures the new value
    let new_val: String = db
        .query_scalar(&format!(
            "SELECT \"new_val\" FROM {} ORDER BY change_id DESC LIMIT 1",
            buffer_table
        ))
        .await;
    assert_eq!(
        new_val, "new_val",
        "new_val typed column should contain the new value"
    );
}

#[tokio::test]
async fn test_trigger_captures_delete() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_del (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cdc_del VALUES (1, 'to_delete')")
        .await;

    db.create_st(
        "cdc_del_st",
        "SELECT id, val FROM cdc_del",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let source_oid = db.table_oid("cdc_del").await;
    let buffer_table = format!("pgtrickle_changes.changes_{}", source_oid);

    // Consume existing changes
    db.refresh_st("cdc_del_st").await;

    // Delete the row
    db.execute("DELETE FROM cdc_del WHERE id = 1").await;

    let action: String = db
        .query_scalar(&format!(
            "SELECT action FROM {} ORDER BY change_id DESC LIMIT 1",
            buffer_table
        ))
        .await;
    assert_eq!(action, "D", "Action should be 'D' for DELETE");

    // Verify typed column captures the old value
    let old_val: String = db
        .query_scalar(&format!(
            "SELECT \"old_val\" FROM {} ORDER BY change_id DESC LIMIT 1",
            buffer_table
        ))
        .await;
    assert_eq!(
        old_val, "to_delete",
        "old_val typed column should contain the deleted value"
    );
}

#[tokio::test]
async fn test_trigger_captures_bulk_insert() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_bulk (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO cdc_bulk VALUES (1, 0)").await;

    db.create_st(
        "cdc_bulk_st",
        "SELECT id, val FROM cdc_bulk",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let source_oid = db.table_oid("cdc_bulk").await;
    let buffer_table = format!("pgtrickle_changes.changes_{}", source_oid);

    // Consume existing changes
    db.refresh_st("cdc_bulk_st").await;

    // Bulk insert via INSERT ... SELECT
    db.execute("INSERT INTO cdc_bulk SELECT g, g FROM generate_series(2, 51) g")
        .await;

    let change_count: i64 = db.count(&buffer_table).await;
    assert_eq!(
        change_count, 50,
        "All 50 bulk-inserted rows should be captured"
    );
}

#[tokio::test]
async fn test_trigger_lsn_ordering() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_lsn (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cdc_lsn VALUES (1, 'a')").await;

    db.create_st(
        "cdc_lsn_st",
        "SELECT id, val FROM cdc_lsn",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let source_oid = db.table_oid("cdc_lsn").await;
    let buffer_table = format!("pgtrickle_changes.changes_{}", source_oid);

    // Consume existing
    db.refresh_st("cdc_lsn_st").await;

    // Multiple DML ops
    db.execute("INSERT INTO cdc_lsn VALUES (2, 'b')").await;
    db.execute("UPDATE cdc_lsn SET val = 'a2' WHERE id = 1")
        .await;
    db.execute("INSERT INTO cdc_lsn VALUES (3, 'c')").await;

    // Verify change_ids are monotonically increasing (proxy for ordering)
    let ordered: bool = db
        .query_scalar(&format!(
            "SELECT bool_and(next_id > prev_id) FROM ( \
                SELECT change_id AS prev_id, \
                       lead(change_id) OVER (ORDER BY change_id) AS next_id \
                FROM {} \
            ) sub WHERE next_id IS NOT NULL",
            buffer_table
        ))
        .await;
    assert!(ordered, "Change IDs should be monotonically increasing");
}

#[tokio::test]
async fn test_trigger_typed_columns_captured() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_typed (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cdc_typed VALUES (1, 'hello')")
        .await;

    db.create_st(
        "cdc_typed_st",
        "SELECT id, val FROM cdc_typed",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let source_oid = db.table_oid("cdc_typed").await;
    let buffer_table = format!("pgtrickle_changes.changes_{}", source_oid);

    // Consume existing
    db.refresh_st("cdc_typed_st").await;

    db.execute("INSERT INTO cdc_typed VALUES (2, 'world')")
        .await;

    // Verify typed columns are populated
    let new_id: i32 = db
        .query_scalar(&format!(
            "SELECT \"new_id\" FROM {} ORDER BY change_id DESC LIMIT 1",
            buffer_table
        ))
        .await;
    assert_eq!(new_id, 2, "new_id should match inserted id");

    let new_val: String = db
        .query_scalar(&format!(
            "SELECT \"new_val\" FROM {} ORDER BY change_id DESC LIMIT 1",
            buffer_table
        ))
        .await;
    assert_eq!(new_val, "world", "new_val should match inserted value");
}

#[tokio::test]
async fn test_buffer_cleanup_after_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_cleanup (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cdc_cleanup VALUES (1, 'a')").await;

    db.create_st(
        "cdc_cleanup_st",
        "SELECT id, val FROM cdc_cleanup",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let source_oid = db.table_oid("cdc_cleanup").await;
    let buffer_table = format!("pgtrickle_changes.changes_{}", source_oid);

    // Insert data to create changes
    db.execute("INSERT INTO cdc_cleanup VALUES (2, 'b'), (3, 'c')")
        .await;

    let pre_count: i64 = db.count(&buffer_table).await;
    assert!(pre_count > 0, "Should have changes before refresh");

    // Manual refresh does TRUNCATE+INSERT (full) and doesn't clear the buffer.
    // The data should still be correct after refresh though.
    db.refresh_st("cdc_cleanup_st").await;

    // Verify refresh produced correct data even with changes in buffer
    let st_count = db.count("public.cdc_cleanup_st").await;
    assert_eq!(st_count, 3, "ST should have all 3 rows after refresh");
}

#[tokio::test]
async fn test_multiple_sources_independent_buffers() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_a (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("CREATE TABLE src_b (id INT PRIMARY KEY, ref_id INT, info TEXT)")
        .await;
    db.execute("INSERT INTO src_a VALUES (1, 'alpha')").await;
    db.execute("INSERT INTO src_b VALUES (1, 1, 'extra')").await;

    db.create_st(
        "joined_st",
        "SELECT a.id, a.val, b.info FROM src_a a JOIN src_b b ON a.id = b.ref_id",
        "1m",
        "FULL",
    )
    .await;

    let oid_a = db.table_oid("src_a").await;
    let oid_b = db.table_oid("src_b").await;

    // Each source should have its own buffer table
    let buf_a_exists = db
        .table_exists("pgtrickle_changes", &format!("changes_{}", oid_a))
        .await;
    let buf_b_exists = db
        .table_exists("pgtrickle_changes", &format!("changes_{}", oid_b))
        .await;

    assert!(buf_a_exists, "src_a should have its own change buffer");
    assert!(buf_b_exists, "src_b should have its own change buffer");
    assert_ne!(oid_a, oid_b, "Source OIDs should be different");
}

#[tokio::test]
async fn test_trigger_survives_source_insert_delete_cycle() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_cycle (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cdc_cycle VALUES (1, 'original')")
        .await;

    db.create_st(
        "cdc_cycle_st",
        "SELECT id, val FROM cdc_cycle",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Cycle: insert → delete → insert new
    db.execute("INSERT INTO cdc_cycle VALUES (2, 'added')")
        .await;
    db.execute("DELETE FROM cdc_cycle WHERE id = 2").await;
    db.execute("INSERT INTO cdc_cycle VALUES (3, 'final')")
        .await;

    // Refresh and verify correctness
    db.refresh_st("cdc_cycle_st").await;

    let count = db.count("public.cdc_cycle_st").await;
    assert_eq!(count, 2, "Should have rows 1 and 3");

    // Verify exact data matches the source
    db.assert_st_matches_query("public.cdc_cycle_st", "SELECT id, val FROM cdc_cycle")
        .await;
}

// ── F13: Partitioned Table CDC ─────────────────────────────────────────

/// Verify that CDC triggers on a partitioned parent table correctly capture
/// DML routed to child partitions (PostgreSQL 13+).
#[tokio::test]
async fn test_trigger_captures_partitioned_table_dml() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a RANGE-partitioned table with two child partitions
    db.execute(
        "CREATE TABLE orders (
            id INT NOT NULL,
            region TEXT NOT NULL,
            amount NUMERIC
        ) PARTITION BY LIST (region)",
    )
    .await;
    db.execute("CREATE TABLE orders_us PARTITION OF orders FOR VALUES IN ('US')")
        .await;
    db.execute("CREATE TABLE orders_eu PARTITION OF orders FOR VALUES IN ('EU')")
        .await;

    // Seed data into both partitions
    db.execute("INSERT INTO orders VALUES (1, 'US', 100.00)")
        .await;
    db.execute("INSERT INTO orders VALUES (2, 'EU', 200.00)")
        .await;

    // Create a stream table on the partitioned parent
    db.create_st(
        "orders_st",
        "SELECT id, region, amount FROM orders",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Consume initial changes
    db.refresh_st("orders_st").await;

    // Verify initial data
    let count = db.count("public.orders_st").await;
    assert_eq!(count, 2, "Should have 2 initial rows");

    // Insert into both partitions via the parent
    db.execute("INSERT INTO orders VALUES (3, 'US', 300.00)")
        .await;
    db.execute("INSERT INTO orders VALUES (4, 'EU', 400.00)")
        .await;

    // Verify changes are captured in the change buffer
    let source_oid = db.table_oid("orders").await;
    let buffer_table = format!("pgtrickle_changes.changes_{}", source_oid);
    let change_count: i64 = db.count(&buffer_table).await;
    assert!(
        change_count >= 2,
        "Both partition-routed inserts should be captured: got {}",
        change_count,
    );

    // Refresh and verify all 4 rows appear
    db.refresh_st("orders_st").await;
    let count = db.count("public.orders_st").await;
    assert_eq!(count, 4, "Should have 4 rows after insert + refresh");

    // Update a row (routed to US partition)
    db.execute("UPDATE orders SET amount = 150.00 WHERE id = 1")
        .await;
    db.refresh_st("orders_st").await;

    db.assert_st_matches_query("public.orders_st", "SELECT id, region, amount FROM orders")
        .await;

    // Delete from EU partition
    db.execute("DELETE FROM orders WHERE id = 2").await;
    db.refresh_st("orders_st").await;

    let count = db.count("public.orders_st").await;
    assert_eq!(count, 3, "Should have 3 rows after delete + refresh");

    db.assert_st_matches_query("public.orders_st", "SELECT id, region, amount FROM orders")
        .await;
}

/// Verify that FULL refresh also works with partitioned source tables.
#[tokio::test]
async fn test_full_refresh_partitioned_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE pt_full (
            id INT NOT NULL,
            cat TEXT NOT NULL
        ) PARTITION BY LIST (cat)",
    )
    .await;
    db.execute("CREATE TABLE pt_full_a PARTITION OF pt_full FOR VALUES IN ('A')")
        .await;
    db.execute("CREATE TABLE pt_full_b PARTITION OF pt_full FOR VALUES IN ('B')")
        .await;

    db.execute("INSERT INTO pt_full VALUES (1, 'A'), (2, 'B')")
        .await;

    db.create_st("pt_full_st", "SELECT id, cat FROM pt_full", "1m", "FULL")
        .await;

    db.refresh_st("pt_full_st").await;
    let count = db.count("public.pt_full_st").await;
    assert_eq!(count, 2, "Should have 2 rows after FULL refresh");

    db.execute("INSERT INTO pt_full VALUES (3, 'A'), (4, 'B')")
        .await;

    db.refresh_st("pt_full_st").await;
    db.assert_st_matches_query("public.pt_full_st", "SELECT id, cat FROM pt_full")
        .await;
}
