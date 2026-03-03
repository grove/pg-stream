//! E2E tests for IMMEDIATE-mode (Transactional IVM) stream tables.
//!
//! Validates that IMMEDIATE stream tables:
//! - Are maintained synchronously within the same transaction as DML.
//! - Handle INSERT, UPDATE, DELETE, and TRUNCATE correctly.
//! - Reject unsupported features (TopK, materialized views).
//! - Clean up properly on DROP.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Helper ─────────────────────────────────────────────────────────────

/// Create an IMMEDIATE-mode stream table (schedule = NULL).
async fn create_immediate_st(db: &E2eDb, name: &str, query: &str) {
    let sql = format!(
        "SELECT pgtrickle.create_stream_table('{name}', $${query}$$, \
         NULL, 'IMMEDIATE')"
    );
    db.execute(&sql).await;
}

// ── Basic Creation ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_create_simple_select() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE orders (id INT PRIMARY KEY, customer TEXT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO orders VALUES (1, 'Alice', 100), (2, 'Bob', 200)")
        .await;

    create_immediate_st(&db, "order_imm", "SELECT id, customer, amount FROM orders").await;

    // Verify catalog entry
    let (status, mode, populated, errors) = db.pgt_status("order_imm").await;
    assert_eq!(status, "ACTIVE");
    assert_eq!(mode, "IMMEDIATE");
    assert!(populated, "ST should be populated after create");
    assert_eq!(errors, 0);

    // Verify initial data
    let count = db.count("public.order_imm").await;
    assert_eq!(count, 2, "ST should contain 2 rows after initial populate");

    // Check schedule is NULL for IMMEDIATE
    let schedule_is_null: bool = db
        .query_scalar(
            "SELECT schedule IS NULL FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'order_imm'",
        )
        .await;
    assert!(schedule_is_null, "IMMEDIATE ST should have NULL schedule");
}

// ── INSERT Propagation ─────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_insert_propagates_immediately() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE products (id INT PRIMARY KEY, name TEXT, price NUMERIC)")
        .await;
    db.execute("INSERT INTO products VALUES (1, 'Widget', 10.00)")
        .await;

    create_immediate_st(&db, "product_imm", "SELECT id, name, price FROM products").await;

    let count_before = db.count("public.product_imm").await;
    assert_eq!(count_before, 1);

    // Insert a new row — should immediately appear in the ST.
    db.execute("INSERT INTO products VALUES (2, 'Gadget', 25.00)")
        .await;

    let count_after = db.count("public.product_imm").await;
    assert_eq!(
        count_after, 2,
        "ST should have 2 rows after INSERT on base table"
    );

    // Verify the new value
    let gadget_price: String = db
        .query_scalar("SELECT price::text FROM public.product_imm WHERE name = 'Gadget'")
        .await;
    assert_eq!(gadget_price, "25.00");
}

#[tokio::test]
async fn test_ivm_multi_row_insert() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE items (id INT PRIMARY KEY, val TEXT)")
        .await;

    create_immediate_st(&db, "items_imm", "SELECT id, val FROM items").await;

    // Insert multiple rows in one statement.
    db.execute("INSERT INTO items VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .await;

    let count = db.count("public.items_imm").await;
    assert_eq!(count, 3, "ST should have 3 rows after multi-row INSERT");
}

// ── UPDATE Propagation ─────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_update_propagates_immediately() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE inventory (id INT PRIMARY KEY, product TEXT, qty INT)")
        .await;
    db.execute("INSERT INTO inventory VALUES (1, 'Bolts', 100), (2, 'Nuts', 200)")
        .await;

    create_immediate_st(&db, "inv_imm", "SELECT id, product, qty FROM inventory").await;

    // Update a row.
    db.execute("UPDATE inventory SET qty = 150 WHERE id = 1")
        .await;

    let new_qty: i32 = db
        .query_scalar("SELECT qty FROM public.inv_imm WHERE product = 'Bolts'")
        .await;
    assert_eq!(new_qty, 150, "ST should reflect UPDATE immediately");

    // Unchanged row should remain.
    let nuts_qty: i32 = db
        .query_scalar("SELECT qty FROM public.inv_imm WHERE product = 'Nuts'")
        .await;
    assert_eq!(nuts_qty, 200, "Non-updated row should be unchanged");
}

// ── DELETE Propagation ─────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_delete_propagates_immediately() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE tasks (id INT PRIMARY KEY, title TEXT)")
        .await;
    db.execute("INSERT INTO tasks VALUES (1, 'Task A'), (2, 'Task B'), (3, 'Task C')")
        .await;

    create_immediate_st(&db, "tasks_imm", "SELECT id, title FROM tasks").await;

    let count_before = db.count("public.tasks_imm").await;
    assert_eq!(count_before, 3);

    // Delete a row.
    db.execute("DELETE FROM tasks WHERE id = 2").await;

    let count_after = db.count("public.tasks_imm").await;
    assert_eq!(
        count_after, 2,
        "ST should have 2 rows after DELETE on base table"
    );

    // Verify the deleted row is gone.
    let has_b: i64 = db
        .query_scalar("SELECT count(*) FROM public.tasks_imm WHERE title = 'Task B'")
        .await;
    assert_eq!(has_b, 0, "Deleted row should not be in ST");
}

// ── TRUNCATE Handling ──────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_truncate_clears_and_repopulates() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE logs (id INT PRIMARY KEY, msg TEXT)")
        .await;
    db.execute("INSERT INTO logs VALUES (1, 'Entry 1'), (2, 'Entry 2')")
        .await;

    create_immediate_st(&db, "logs_imm", "SELECT id, msg FROM logs").await;
    assert_eq!(db.count("public.logs_imm").await, 2);

    // TRUNCATE the base table — ST should be emptied.
    db.execute("TRUNCATE logs").await;

    let count = db.count("public.logs_imm").await;
    assert_eq!(count, 0, "ST should be empty after base table TRUNCATE");
}

// ── DROP Cleanup ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_drop_cleans_up_triggers() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cleanup_test (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cleanup_test VALUES (1, 'x')").await;

    create_immediate_st(&db, "cleanup_imm", "SELECT id, val FROM cleanup_test").await;

    // Drop the stream table.
    db.drop_st("cleanup_imm").await;

    // Verify catalog entry removed.
    let cat_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'cleanup_imm'",
        )
        .await;
    assert_eq!(cat_count, 0, "Catalog entry should be removed after DROP");

    // Verify IVM triggers are cleaned up — regular DML should work fine.
    db.execute("INSERT INTO cleanup_test VALUES (2, 'y')").await;
    let base_count: i64 = db.query_scalar("SELECT count(*) FROM cleanup_test").await;
    assert_eq!(base_count, 2, "Base table DML should work after ST drop");
}

// ── Validation Errors ──────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_reject_topk_immediate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE scores (id INT PRIMARY KEY, name TEXT, score INT)")
        .await;

    // TopK + IMMEDIATE should be rejected.
    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('top_scores', \
             $$SELECT name, score FROM scores ORDER BY score DESC LIMIT 10$$, \
             NULL, 'IMMEDIATE')",
        )
        .await;

    assert!(
        result.is_err(),
        "TopK with IMMEDIATE mode should be rejected"
    );
}

// ── Manual Refresh ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_manual_refresh_does_full_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE refresh_test (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO refresh_test VALUES (1, 10), (2, 20)")
        .await;

    create_immediate_st(&db, "refresh_imm", "SELECT id, val FROM refresh_test").await;
    assert_eq!(db.count("public.refresh_imm").await, 2);

    // Manual refresh should work (does a full refresh)
    db.refresh_st("refresh_imm").await;

    let count = db.count("public.refresh_imm").await;
    assert_eq!(count, 2, "ST should still have 2 rows after manual refresh");
}

// ── Mixed Operations ───────────────────────────────────────────────────

#[tokio::test]
async fn test_ivm_mixed_operations_in_sequence() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE accounts (id INT PRIMARY KEY, name TEXT, balance NUMERIC)")
        .await;

    create_immediate_st(&db, "acct_imm", "SELECT id, name, balance FROM accounts").await;
    assert_eq!(db.count("public.acct_imm").await, 0);

    // INSERT
    db.execute("INSERT INTO accounts VALUES (1, 'Alice', 1000), (2, 'Bob', 2000)")
        .await;
    assert_eq!(db.count("public.acct_imm").await, 2);

    // UPDATE
    db.execute("UPDATE accounts SET balance = balance + 500 WHERE id = 1")
        .await;
    let alice_bal: String = db
        .query_scalar("SELECT balance::text FROM public.acct_imm WHERE name = 'Alice'")
        .await;
    assert_eq!(alice_bal, "1500");

    // DELETE
    db.execute("DELETE FROM accounts WHERE id = 2").await;
    assert_eq!(db.count("public.acct_imm").await, 1);

    // INSERT again
    db.execute("INSERT INTO accounts VALUES (3, 'Charlie', 3000)")
        .await;
    assert_eq!(db.count("public.acct_imm").await, 2);
}
