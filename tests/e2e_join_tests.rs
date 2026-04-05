mod e2e;
use e2e::E2eDb;
#[tokio::test]
async fn test_full_join_multi_row_unmatched() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE fj_multi_left (id INT, val TEXT)")
        .await;
    db.execute("CREATE TABLE fj_multi_right (id INT, val TEXT)")
        .await;

    db.execute("INSERT INTO fj_multi_left VALUES (1, 'L1'), (2, 'L2')")
        .await;
    db.execute("INSERT INTO fj_multi_right VALUES (3, 'R3'), (4, 'R4'), (5, 'R5')")
        .await;

    let q = "SELECT l.id as lid, r.id as rid FROM fj_multi_left l FULL JOIN fj_multi_right r ON l.id = r.id";

    db.create_st("fj_multi_st", q, "1m", "DIFFERENTIAL").await;

    db.assert_st_matches_query("fj_multi_st", q).await;

    // Add match
    db.execute("INSERT INTO fj_multi_left VALUES (3, 'L3')")
        .await;
    db.refresh_st("fj_multi_st").await;

    db.assert_st_matches_query("fj_multi_st", q).await;
}

// ── TG2-JOIN: Multi-cycle UPDATE/DELETE correctness ──────────────────

/// INNER JOIN: source row updated → stream table reflects new join value.
#[tokio::test]
async fn test_inner_join_update_propagation() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ij_orders (id INT PRIMARY KEY, customer_id INT, amount INT)")
        .await;
    db.execute("CREATE TABLE ij_customers (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("INSERT INTO ij_customers VALUES (1, 'Alice'), (2, 'Bob')")
        .await;
    db.execute("INSERT INTO ij_orders VALUES (10, 1, 100), (11, 2, 200), (12, 1, 50)")
        .await;

    let q = "SELECT o.id AS oid, c.name, o.amount \
             FROM ij_orders o JOIN ij_customers c ON o.customer_id = c.id";
    db.create_st("ij_upd_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ij_upd_st", q).await;

    // UPDATE: change customer name → reflected in join result
    db.execute("UPDATE ij_customers SET name = 'ALICE' WHERE id = 1")
        .await;
    db.refresh_st("ij_upd_st").await;
    db.assert_st_matches_query("ij_upd_st", q).await;

    // UPDATE: change join key on order → row moves to different customer
    db.execute("UPDATE ij_orders SET customer_id = 2 WHERE id = 12")
        .await;
    db.refresh_st("ij_upd_st").await;
    db.assert_st_matches_query("ij_upd_st", q).await;

    // DELETE: remove an order → row disappears from join
    db.execute("DELETE FROM ij_orders WHERE id = 10").await;
    db.refresh_st("ij_upd_st").await;
    db.assert_st_matches_query("ij_upd_st", q).await;

    // DELETE: remove a customer with remaining orders → their orders leave join
    db.execute("DELETE FROM ij_customers WHERE id = 2").await;
    db.refresh_st("ij_upd_st").await;
    db.assert_st_matches_query("ij_upd_st", q).await;
}

/// LEFT JOIN: right-side DELETE → stream table row transitions to NULL.
#[tokio::test]
async fn test_left_join_right_delete_null_transition() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE lj_dept (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE lj_emp (id INT PRIMARY KEY, dept_id INT, emp_name TEXT)")
        .await;
    db.execute("INSERT INTO lj_dept VALUES (1, 'Engineering'), (2, 'Sales'), (3, 'Marketing')")
        .await;
    db.execute("INSERT INTO lj_emp VALUES (10, 1, 'Alice'), (11, 2, 'Bob'), (12, 1, 'Carol')")
        .await;

    let q = "SELECT d.id AS did, d.name AS dept_name, e.emp_name \
             FROM lj_dept d LEFT JOIN lj_emp e ON e.dept_id = d.id";
    db.create_st("lj_del_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("lj_del_st", q).await;

    // DELETE: remove all employees from Engineering → dept rows get NULL emp_name
    db.execute("DELETE FROM lj_emp WHERE dept_id = 1").await;
    db.refresh_st("lj_del_st").await;
    db.assert_st_matches_query("lj_del_st", q).await;

    // UPDATE: move Bob to Marketing
    db.execute("UPDATE lj_emp SET dept_id = 3 WHERE id = 11")
        .await;
    db.refresh_st("lj_del_st").await;
    db.assert_st_matches_query("lj_del_st", q).await;

    // INSERT: add employee to empty dept
    db.execute("INSERT INTO lj_emp VALUES (13, 2, 'Dave')")
        .await;
    db.refresh_st("lj_del_st").await;
    db.assert_st_matches_query("lj_del_st", q).await;
}

/// FULL JOIN: both-side UPDATE in same cycle → no phantom rows.
#[tokio::test]
async fn test_full_join_both_side_update() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE fj_a (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("CREATE TABLE fj_b (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO fj_a VALUES (1, 'A1'), (2, 'A2'), (3, 'A3')")
        .await;
    db.execute("INSERT INTO fj_b VALUES (2, 'B2'), (3, 'B3'), (4, 'B4')")
        .await;

    let q = "SELECT a.id AS aid, a.val AS aval, b.id AS bid, b.val AS bval \
             FROM fj_a a FULL JOIN fj_b b ON a.id = b.id";
    db.create_st("fj_both_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("fj_both_st", q).await;

    // Both-side UPDATE in same cycle: update matched rows on both sides
    db.execute("UPDATE fj_a SET val = 'A2-updated' WHERE id = 2")
        .await;
    db.execute("UPDATE fj_b SET val = 'B2-updated' WHERE id = 2")
        .await;
    db.refresh_st("fj_both_st").await;
    db.assert_st_matches_query("fj_both_st", q).await;

    // DELETE from left side → previously matched row becomes right-only
    db.execute("DELETE FROM fj_a WHERE id = 3").await;
    db.refresh_st("fj_both_st").await;
    db.assert_st_matches_query("fj_both_st", q).await;

    // INSERT + DELETE in same cycle
    db.execute("INSERT INTO fj_a VALUES (4, 'A4')").await;
    db.execute("DELETE FROM fj_b WHERE id = 4").await;
    db.refresh_st("fj_both_st").await;
    db.assert_st_matches_query("fj_both_st", q).await;
}

/// 3-table join chain: delete from middle table → correct propagation.
#[tokio::test]
async fn test_three_table_join_middle_delete() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tj_region (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE tj_store (id INT PRIMARY KEY, region_id INT, store_name TEXT)")
        .await;
    db.execute("CREATE TABLE tj_sale (id INT PRIMARY KEY, store_id INT, amount INT)")
        .await;
    db.execute("INSERT INTO tj_region VALUES (1, 'North'), (2, 'South')")
        .await;
    db.execute(
        "INSERT INTO tj_store VALUES (10, 1, 'Store-A'), (11, 1, 'Store-B'), (12, 2, 'Store-C')",
    )
    .await;
    db.execute(
        "INSERT INTO tj_sale VALUES (100, 10, 500), (101, 11, 300), (102, 12, 700), (103, 10, 200)",
    )
    .await;

    let q = "SELECT r.name AS region, s.store_name, sa.amount \
             FROM tj_region r \
             JOIN tj_store s ON s.region_id = r.id \
             JOIN tj_sale sa ON sa.store_id = s.id";
    db.create_st("tj_mid_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("tj_mid_st", q).await;

    // DELETE from middle table: remove Store-A → its sales disappear
    db.execute("DELETE FROM tj_store WHERE id = 10").await;
    db.refresh_st("tj_mid_st").await;
    db.assert_st_matches_query("tj_mid_st", q).await;

    // UPDATE middle table: move Store-B to South region
    db.execute("UPDATE tj_store SET region_id = 2 WHERE id = 11")
        .await;
    db.refresh_st("tj_mid_st").await;
    db.assert_st_matches_query("tj_mid_st", q).await;

    // DELETE from leaf: remove a sale
    db.execute("DELETE FROM tj_sale WHERE id = 102").await;
    db.refresh_st("tj_mid_st").await;
    db.assert_st_matches_query("tj_mid_st", q).await;

    // INSERT new chain: new store + sale
    db.execute("INSERT INTO tj_store VALUES (13, 1, 'Store-D')")
        .await;
    db.execute("INSERT INTO tj_sale VALUES (104, 13, 999)")
        .await;
    db.refresh_st("tj_mid_st").await;
    db.assert_st_matches_query("tj_mid_st", q).await;
}
