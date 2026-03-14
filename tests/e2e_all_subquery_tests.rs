//! E2E tests for ALL (subquery) support in stream tables (EC-32).
//!
//! Validates `WHERE col op ALL (SELECT ...)` patterns with both FULL and
//! DIFFERENTIAL refresh modes, including NULL handling and incremental
//! updates after INSERT, UPDATE, and DELETE on both outer and inner tables.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// Basic ALL (subquery) — DIFFERENTIAL
// ═══════════════════════════════════════════════════════════════════════

/// EC-32: Basic `price < ALL (SELECT ...)` filter in DIFFERENTIAL mode.
/// Mirrors the SQL Reference worked example.
#[tokio::test]
async fn test_all_subquery_less_than_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE all_products (id INT PRIMARY KEY, name TEXT, price NUMERIC)")
        .await;
    db.execute("CREATE TABLE all_competitors (id INT PRIMARY KEY, product_id INT, price NUMERIC)")
        .await;
    db.execute(
        "INSERT INTO all_products VALUES (1, 'Widget', 9.99), (2, 'Gadget', 24.99), (3, 'Gizmo', 14.99)",
    )
    .await;
    db.execute(
        "INSERT INTO all_competitors VALUES (1, 1, 12.99), (2, 1, 11.50), (3, 2, 19.99), (4, 3, 14.99)",
    )
    .await;

    let q = "SELECT p.id, p.name, p.price \
             FROM all_products p \
             WHERE p.price < ALL ( \
                 SELECT cp.price FROM all_competitors cp \
                 WHERE cp.product_id = p.id \
             )";

    db.create_st("all_lt_st", q, "1m", "DIFFERENTIAL").await;

    // Widget (9.99 < all of [12.99, 11.50]) → included
    // Gadget (24.99 < 19.99?) → excluded
    // Gizmo (14.99 < 14.99?) → excluded (not strictly less than)
    db.assert_st_matches_query("public.all_lt_st", q).await;
    assert_eq!(db.count("public.all_lt_st").await, 1);

    let name: String = db.query_scalar("SELECT name FROM public.all_lt_st").await;
    assert_eq!(name, "Widget");
}

/// EC-32: Differential refresh after adding a cheaper competitor price
/// that disqualifies a previously included product.
#[tokio::test]
async fn test_all_subquery_differential_inner_insert() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE all_prod2 (id INT PRIMARY KEY, name TEXT, price INT)")
        .await;
    db.execute("CREATE TABLE all_comp2 (id INT PRIMARY KEY, product_id INT, price INT)")
        .await;
    db.execute("INSERT INTO all_prod2 VALUES (1, 'Alpha', 50), (2, 'Beta', 30)")
        .await;
    db.execute("INSERT INTO all_comp2 VALUES (1, 1, 60), (2, 2, 40)")
        .await;

    let q = "SELECT p.id, p.name FROM all_prod2 p \
             WHERE p.price < ALL (SELECT c.price FROM all_comp2 c WHERE c.product_id = p.id)";

    db.create_st("all_inner_st", q, "1m", "DIFFERENTIAL").await;

    // Alpha: 50 < 60 → included; Beta: 30 < 40 → included
    db.assert_st_matches_query("public.all_inner_st", q).await;
    assert_eq!(db.count("public.all_inner_st").await, 2);

    // Add a competitor price for Alpha that is lower than Alpha's price
    db.execute("INSERT INTO all_comp2 VALUES (3, 1, 40)").await;
    db.refresh_st("all_inner_st").await;

    // Alpha: 50 < ALL(60, 40) → 50 < 40 is false → excluded
    db.assert_st_matches_query("public.all_inner_st", q).await;
    assert_eq!(db.count("public.all_inner_st").await, 1);

    let name: String = db
        .query_scalar("SELECT name FROM public.all_inner_st")
        .await;
    assert_eq!(name, "Beta");
}

/// EC-32: Differential refresh after outer table changes.
#[tokio::test]
async fn test_all_subquery_differential_outer_change() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE all_outer (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE all_thresh (id INT PRIMARY KEY, limit_val INT)")
        .await;
    db.execute("INSERT INTO all_outer VALUES (1, 100), (2, 50), (3, 10)")
        .await;
    db.execute("INSERT INTO all_thresh VALUES (1, 30), (2, 60)")
        .await;

    let q = "SELECT o.id, o.val FROM all_outer o \
             WHERE o.val > ALL (SELECT t.limit_val FROM all_thresh t)";

    db.create_st("all_outer_st", q, "1m", "DIFFERENTIAL").await;

    // val > ALL(30, 60): only val=100 qualifies
    db.assert_st_matches_query("public.all_outer_st", q).await;
    assert_eq!(db.count("public.all_outer_st").await, 1);

    // Insert a new outer row that also qualifies
    db.execute("INSERT INTO all_outer VALUES (4, 200)").await;
    db.refresh_st("all_outer_st").await;

    db.assert_st_matches_query("public.all_outer_st", q).await;
    assert_eq!(db.count("public.all_outer_st").await, 2);

    // Delete the high-value threshold — now val=50 also qualifies (50 > 30)
    db.execute("DELETE FROM all_thresh WHERE limit_val = 60")
        .await;
    db.refresh_st("all_outer_st").await;

    db.assert_st_matches_query("public.all_outer_st", q).await;
    assert_eq!(db.count("public.all_outer_st").await, 3);
}

// ═══════════════════════════════════════════════════════════════════════
// NULL handling
// ═══════════════════════════════════════════════════════════════════════

/// EC-32: ALL (subquery) with NULL in the subquery result — per SQL
/// standard, `x > ALL (...)` is false if any subquery row is NULL.
#[tokio::test]
async fn test_all_subquery_null_in_inner() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE all_nullp (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE all_nullt (id INT PRIMARY KEY, threshold INT)")
        .await;
    db.execute("INSERT INTO all_nullp VALUES (1, 100), (2, 50)")
        .await;
    // One threshold is NULL — should cause ALL to return false
    db.execute("INSERT INTO all_nullt VALUES (1, 30), (2, NULL)")
        .await;

    let q = "SELECT p.id, p.val FROM all_nullp p \
             WHERE p.val > ALL (SELECT t.threshold FROM all_nullt t)";

    db.create_st("all_null_st", q, "1m", "DIFFERENTIAL").await;

    // val > ALL(30, NULL): NULL makes ALL comparison yield false for all rows
    db.assert_st_matches_query("public.all_null_st", q).await;
    assert_eq!(
        db.count("public.all_null_st").await,
        0,
        "No rows should match when subquery contains NULL"
    );

    // Remove the NULL row — now both val=100 and val=50 qualify (> 30)
    db.execute("DELETE FROM all_nullt WHERE id = 2").await;
    db.refresh_st("all_null_st").await;

    db.assert_st_matches_query("public.all_null_st", q).await;
    assert_eq!(db.count("public.all_null_st").await, 2);
}

/// EC-32: ALL with empty subquery — per SQL standard, `x > ALL (empty)` is true.
#[tokio::test]
async fn test_all_subquery_empty_inner() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE all_emp (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE all_empty (id INT PRIMARY KEY, limit_val INT)")
        .await;
    db.execute("INSERT INTO all_emp VALUES (1, 10), (2, 20)")
        .await;
    // Inner table is empty

    let q = "SELECT e.id, e.val FROM all_emp e \
             WHERE e.val > ALL (SELECT x.limit_val FROM all_empty x)";

    db.create_st("all_empty_st", q, "1m", "DIFFERENTIAL").await;

    // ALL against empty set → true for all outer rows
    db.assert_st_matches_query("public.all_empty_st", q).await;
    assert_eq!(db.count("public.all_empty_st").await, 2);

    // Insert into inner table — some outer rows may now be excluded
    db.execute("INSERT INTO all_empty VALUES (1, 15)").await;
    db.refresh_st("all_empty_st").await;

    // val > ALL(15): only val=20 qualifies
    db.assert_st_matches_query("public.all_empty_st", q).await;
    assert_eq!(db.count("public.all_empty_st").await, 1);
}

// ═══════════════════════════════════════════════════════════════════════
// FULL refresh mode
// ═══════════════════════════════════════════════════════════════════════

/// EC-32: ALL (subquery) with FULL refresh mode.
#[tokio::test]
async fn test_all_subquery_full_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE all_full_src (id INT PRIMARY KEY, score INT)")
        .await;
    db.execute("CREATE TABLE all_full_min (id INT PRIMARY KEY, min_score INT)")
        .await;
    db.execute("INSERT INTO all_full_src VALUES (1, 90), (2, 70), (3, 50)")
        .await;
    db.execute("INSERT INTO all_full_min VALUES (1, 60), (2, 80)")
        .await;

    let q = "SELECT s.id, s.score FROM all_full_src s \
             WHERE s.score >= ALL (SELECT m.min_score FROM all_full_min m)";

    db.create_st("all_full_st", q, "1m", "FULL").await;

    // score >= ALL(60, 80): only 90 qualifies
    db.assert_st_matches_query("public.all_full_st", q).await;
    assert_eq!(db.count("public.all_full_st").await, 1);

    // Lower the high threshold
    db.execute("UPDATE all_full_min SET min_score = 50 WHERE id = 2")
        .await;
    db.refresh_st("all_full_st").await;

    // score >= ALL(60, 50): scores 90 and 70 qualify
    db.assert_st_matches_query("public.all_full_st", q).await;
    assert_eq!(db.count("public.all_full_st").await, 2);
}

// ═══════════════════════════════════════════════════════════════════════
// Different comparison operators
// ═══════════════════════════════════════════════════════════════════════

/// EC-32: `= ALL` (value equals every subquery row).
#[tokio::test]
async fn test_all_subquery_equals_operator() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE all_eq (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE all_eq_ref (id INT PRIMARY KEY, ref_val INT)")
        .await;
    db.execute("INSERT INTO all_eq VALUES (1, 10), (2, 20)")
        .await;
    // All ref values are 10 — so only val=10 matches = ALL
    db.execute("INSERT INTO all_eq_ref VALUES (1, 10), (2, 10)")
        .await;

    let q = "SELECT e.id, e.val FROM all_eq e \
             WHERE e.val = ALL (SELECT r.ref_val FROM all_eq_ref r)";

    db.create_st("all_eq_st", q, "1m", "DIFFERENTIAL").await;

    db.assert_st_matches_query("public.all_eq_st", q).await;
    assert_eq!(db.count("public.all_eq_st").await, 1);

    let val: i32 = db.query_scalar("SELECT val FROM public.all_eq_st").await;
    assert_eq!(val, 10);
}

/// EC-32: `<> ALL` (value differs from every subquery row, equivalent to NOT IN).
#[tokio::test]
async fn test_all_subquery_not_equals_operator() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE all_ne (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE all_ne_ref (id INT PRIMARY KEY, ref_val INT)")
        .await;
    db.execute("INSERT INTO all_ne VALUES (1, 10), (2, 20), (3, 30)")
        .await;
    db.execute("INSERT INTO all_ne_ref VALUES (1, 10), (2, 20)")
        .await;

    let q = "SELECT n.id, n.val FROM all_ne n \
             WHERE n.val <> ALL (SELECT r.ref_val FROM all_ne_ref r)";

    db.create_st("all_ne_st", q, "1m", "DIFFERENTIAL").await;

    // val <> ALL(10, 20): only val=30 qualifies
    db.assert_st_matches_query("public.all_ne_st", q).await;
    assert_eq!(db.count("public.all_ne_st").await, 1);

    let val: i32 = db.query_scalar("SELECT val FROM public.all_ne_st").await;
    assert_eq!(val, 30);
}
