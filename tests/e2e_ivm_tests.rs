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

// ── Mode Switching (alter_stream_table) ────────────────────────────────

#[tokio::test]
async fn test_ivm_alter_differential_to_immediate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sw_d2i (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO sw_d2i VALUES (1, 'a'), (2, 'b')")
        .await;

    // Start as DIFFERENTIAL.
    db.execute(
        "SELECT pgtrickle.create_stream_table('sw_d2i_st', \
         $$SELECT id, val FROM sw_d2i$$, '5m', 'DIFFERENTIAL')",
    )
    .await;

    let (_, mode, _, _) = db.pgt_status("sw_d2i_st").await;
    assert_eq!(mode, "DIFFERENTIAL");

    // Switch to IMMEDIATE.
    db.alter_st("sw_d2i_st", "refresh_mode => 'IMMEDIATE'")
        .await;

    let (status, mode, populated, _) = db.pgt_status("sw_d2i_st").await;
    assert_eq!(mode, "IMMEDIATE");
    assert_eq!(status, "ACTIVE");
    assert!(populated, "ST should be populated after mode switch");

    // Verify existing data is intact.
    assert_eq!(db.count("public.sw_d2i_st").await, 2);

    // Verify IVM triggers are active — INSERT should propagate immediately.
    db.execute("INSERT INTO sw_d2i VALUES (3, 'c')").await;
    assert_eq!(
        db.count("public.sw_d2i_st").await,
        3,
        "INSERT should propagate immediately after switch to IMMEDIATE"
    );
}

#[tokio::test]
async fn test_ivm_alter_immediate_to_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sw_i2d (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO sw_i2d VALUES (1, 'x')").await;

    create_immediate_st(&db, "sw_i2d_st", "SELECT id, val FROM sw_i2d").await;

    let (_, mode, _, _) = db.pgt_status("sw_i2d_st").await;
    assert_eq!(mode, "IMMEDIATE");

    // Switch to DIFFERENTIAL with a schedule.
    db.execute(
        "SELECT pgtrickle.alter_stream_table('sw_i2d_st', \
         refresh_mode => 'DIFFERENTIAL', schedule => '10m')",
    )
    .await;

    let (status, mode, populated, _) = db.pgt_status("sw_i2d_st").await;
    assert_eq!(mode, "DIFFERENTIAL");
    assert_eq!(status, "ACTIVE");
    assert!(populated, "ST should remain populated after mode switch");
    assert_eq!(db.count("public.sw_i2d_st").await, 1);

    // IVM triggers should be gone — INSERT should NOT propagate immediately.
    db.execute("INSERT INTO sw_i2d VALUES (2, 'y')").await;
    assert_eq!(
        db.count("public.sw_i2d_st").await,
        1,
        "INSERT should NOT propagate in DIFFERENTIAL mode"
    );
}

#[tokio::test]
async fn test_ivm_alter_full_to_immediate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sw_f2i (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO sw_f2i VALUES (1, 'p'), (2, 'q')")
        .await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('sw_f2i_st', \
         $$SELECT id, val FROM sw_f2i$$, '5m', 'FULL')",
    )
    .await;

    let (_, mode, _, _) = db.pgt_status("sw_f2i_st").await;
    assert_eq!(mode, "FULL");

    // Switch to IMMEDIATE.
    db.alter_st("sw_f2i_st", "refresh_mode => 'IMMEDIATE'")
        .await;

    let (_, mode, populated, _) = db.pgt_status("sw_f2i_st").await;
    assert_eq!(mode, "IMMEDIATE");
    assert!(populated);
    assert_eq!(db.count("public.sw_f2i_st").await, 2);

    // Verify IVM triggers are active.
    db.execute("INSERT INTO sw_f2i VALUES (3, 'r')").await;
    assert_eq!(db.count("public.sw_f2i_st").await, 3);
}

#[tokio::test]
async fn test_ivm_alter_immediate_to_full() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sw_i2f (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO sw_i2f VALUES (1, 'z')").await;

    create_immediate_st(&db, "sw_i2f_st", "SELECT id, val FROM sw_i2f").await;

    // Switch to FULL.
    db.execute(
        "SELECT pgtrickle.alter_stream_table('sw_i2f_st', \
         refresh_mode => 'FULL', schedule => '5m')",
    )
    .await;

    let (_, mode, _, _) = db.pgt_status("sw_i2f_st").await;
    assert_eq!(mode, "FULL");

    // IVM triggers should be removed — manual INSERT shouldn't propagate.
    db.execute("INSERT INTO sw_i2f VALUES (2, 'w')").await;
    assert_eq!(
        db.count("public.sw_i2f_st").await,
        1,
        "INSERT should NOT propagate in FULL mode"
    );
}

// ── IMMEDIATE Query Restriction Validation ─────────────────────────────

#[tokio::test]
async fn test_ivm_reject_window_function_immediate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE win_src (id INT PRIMARY KEY, val INT, grp TEXT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('win_imm', \
             $$SELECT id, val, row_number() OVER (PARTITION BY grp ORDER BY val) AS rn FROM win_src$$, \
             NULL, 'IMMEDIATE')",
        )
        .await;

    assert!(
        result.is_err(),
        "Window functions should be rejected in IMMEDIATE mode"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Window") || err_msg.contains("window") || err_msg.contains("IMMEDIATE"),
        "Error should mention window functions or IMMEDIATE mode: {err_msg}"
    );
}

#[tokio::test]
async fn test_ivm_reject_lateral_join_immediate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE lat_parent (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE lat_child (id INT PRIMARY KEY, parent_id INT, score INT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('lat_imm', \
             $$SELECT p.id, t.score FROM lat_parent p, \
             LATERAL (SELECT score FROM lat_child c WHERE c.parent_id = p.id ORDER BY score DESC LIMIT 1) t$$, \
             NULL, 'IMMEDIATE')",
        )
        .await;

    assert!(
        result.is_err(),
        "LATERAL joins should be rejected in IMMEDIATE mode"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("LATERAL") || err_msg.contains("lateral") || err_msg.contains("IMMEDIATE"),
        "Error should mention LATERAL or IMMEDIATE mode: {err_msg}"
    );
}

#[tokio::test]
async fn test_ivm_reject_scalar_subquery_immediate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ssq_main (id INT PRIMARY KEY, cat TEXT)")
        .await;
    db.execute("CREATE TABLE ssq_counts (cat TEXT PRIMARY KEY, cnt INT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('ssq_imm', \
             $$SELECT id, cat, (SELECT cnt FROM ssq_counts sc WHERE sc.cat = m.cat) AS cat_count FROM ssq_main m$$, \
             NULL, 'IMMEDIATE')",
        )
        .await;

    assert!(
        result.is_err(),
        "Scalar subqueries should be rejected in IMMEDIATE mode"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("subquer") || err_msg.contains("IMMEDIATE"),
        "Error should mention scalar subqueries or IMMEDIATE mode: {err_msg}"
    );
}

#[tokio::test]
async fn test_ivm_allow_aggregate_in_immediate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE agg_src (id INT PRIMARY KEY, category TEXT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO agg_src VALUES (1, 'A', 10), (2, 'B', 20), (3, 'A', 30)")
        .await;

    // Aggregates should be allowed in IMMEDIATE mode.
    create_immediate_st(
        &db,
        "agg_imm",
        "SELECT category, SUM(amount) AS total FROM agg_src GROUP BY category",
    )
    .await;

    let (_, mode, populated, _) = db.pgt_status("agg_imm").await;
    assert_eq!(mode, "IMMEDIATE");
    assert!(populated);

    let count = db.count("public.agg_imm").await;
    assert_eq!(count, 2, "Should have 2 groups (A, B)");

    // INSERT should propagate and update aggregate.
    db.execute("INSERT INTO agg_src VALUES (4, 'A', 40)").await;

    let total_a: String = db
        .query_scalar("SELECT total::text FROM public.agg_imm WHERE category = 'A'")
        .await;
    assert_eq!(total_a, "80", "SUM for category A should be 10+30+40=80");
}

#[tokio::test]
async fn test_ivm_allow_join_in_immediate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE join_left (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE join_right (id INT PRIMARY KEY, left_id INT, val INT)")
        .await;
    db.execute("INSERT INTO join_left VALUES (1, 'Alpha'), (2, 'Beta')")
        .await;
    db.execute("INSERT INTO join_right VALUES (1, 1, 100), (2, 2, 200)")
        .await;

    // Joins should be allowed in IMMEDIATE mode.
    create_immediate_st(
        &db,
        "join_imm",
        "SELECT l.id, l.name, r.val FROM join_left l INNER JOIN join_right r ON r.left_id = l.id",
    )
    .await;

    let (_, mode, populated, _) = db.pgt_status("join_imm").await;
    assert_eq!(mode, "IMMEDIATE");
    assert!(populated);
    assert_eq!(db.count("public.join_imm").await, 2);

    // INSERT into right table should propagate.
    db.execute("INSERT INTO join_right VALUES (3, 1, 300)")
        .await;
    assert_eq!(
        db.count("public.join_imm").await,
        3,
        "Join ST should have 3 rows after INSERT into right table"
    );
}

// ── Alter Mode Switching Validation ────────────────────────────────────

#[tokio::test]
async fn test_ivm_alter_to_immediate_rejects_window() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sw_rej (id INT PRIMARY KEY, val INT, grp TEXT)")
        .await;

    // Create as DIFFERENTIAL with a window function query.
    db.execute(
        "SELECT pgtrickle.create_stream_table('sw_rej_st', \
         $$SELECT id, val, row_number() OVER (PARTITION BY grp ORDER BY val) AS rn FROM sw_rej$$, \
         '5m', 'DIFFERENTIAL')",
    )
    .await;

    // Attempt to switch to IMMEDIATE — should be rejected.
    let result = db
        .try_execute(
            "SELECT pgtrickle.alter_stream_table('sw_rej_st', refresh_mode => 'IMMEDIATE')",
        )
        .await;

    assert!(
        result.is_err(),
        "Switching a window-function ST to IMMEDIATE should be rejected"
    );

    // Verify mode didn't change.
    let (_, mode, _, _) = db.pgt_status("sw_rej_st").await;
    assert_eq!(mode, "DIFFERENTIAL", "Mode should remain DIFFERENTIAL");
}
