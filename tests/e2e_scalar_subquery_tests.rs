//! E2E tests for scalar subquery differential correctness (F20: G3.2).
//!
//! Validates scalar subqueries in SELECT, WHERE, and correlated positions
//! under differential refresh.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// Scalar subquery in SELECT list
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_scalar_subquery_select_list_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ss_orders (id SERIAL PRIMARY KEY, customer TEXT, amount INT)")
        .await;
    db.execute("CREATE TABLE ss_config (id SERIAL PRIMARY KEY, key TEXT, val INT)")
        .await;
    db.execute("INSERT INTO ss_orders (customer, amount) VALUES ('a', 100), ('b', 200)")
        .await;
    db.execute("INSERT INTO ss_config (key, val) VALUES ('tax_rate', 10)")
        .await;

    let q = "SELECT customer, amount, \
             (SELECT val FROM ss_config WHERE key = 'tax_rate') AS tax_rate \
             FROM ss_orders";
    db.create_st("ss_sel_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ss_sel_st", q).await;

    // Change the scalar value
    db.execute("UPDATE ss_config SET val = 15 WHERE key = 'tax_rate'")
        .await;
    db.refresh_st("ss_sel_st").await;
    db.assert_st_matches_query("ss_sel_st", q).await;

    // Add new order
    db.execute("INSERT INTO ss_orders (customer, amount) VALUES ('c', 300)")
        .await;
    db.refresh_st("ss_sel_st").await;
    db.assert_st_matches_query("ss_sel_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Scalar subquery in WHERE
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_scalar_subquery_where_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ss_products (id SERIAL PRIMARY KEY, name TEXT, price INT)")
        .await;
    db.execute("CREATE TABLE ss_thresholds (id SERIAL PRIMARY KEY, min_price INT)")
        .await;
    db.execute("INSERT INTO ss_products (name, price) VALUES ('a', 10), ('b', 50), ('c', 100)")
        .await;
    db.execute("INSERT INTO ss_thresholds (min_price) VALUES (30)")
        .await;

    let q = "SELECT name, price FROM ss_products \
             WHERE price >= (SELECT min_price FROM ss_thresholds LIMIT 1)";
    db.create_st("ss_where_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ss_where_st", q).await;

    // Lower threshold → more rows
    db.execute("UPDATE ss_thresholds SET min_price = 5 WHERE id = 1")
        .await;
    db.refresh_st("ss_where_st").await;
    db.assert_st_matches_query("ss_where_st", q).await;

    // Raise threshold → fewer rows
    db.execute("UPDATE ss_thresholds SET min_price = 80 WHERE id = 1")
        .await;
    db.refresh_st("ss_where_st").await;
    db.assert_st_matches_query("ss_where_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Correlated scalar subquery
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "DVM: correlated scalar subquery differential generates invalid SQL (ROADMAP)"]
async fn test_correlated_scalar_subquery_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ss_dept (id SERIAL PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE ss_emp (id SERIAL PRIMARY KEY, dept_id INT, salary INT)")
        .await;
    db.execute("INSERT INTO ss_dept (id, name) VALUES (1, 'eng'), (2, 'sales')")
        .await;
    db.execute("INSERT INTO ss_emp (dept_id, salary) VALUES (1, 100), (1, 200), (2, 150)")
        .await;

    let q = "SELECT d.name, \
             (SELECT MAX(e.salary) FROM ss_emp e WHERE e.dept_id = d.id) AS max_sal \
             FROM ss_dept d";
    db.create_st("ss_corr_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ss_corr_st", q).await;

    // New high salary
    db.execute("INSERT INTO ss_emp (dept_id, salary) VALUES (1, 500)")
        .await;
    db.refresh_st("ss_corr_st").await;
    db.assert_st_matches_query("ss_corr_st", q).await;

    // Remove top earner
    db.execute("DELETE FROM ss_emp WHERE salary = 500").await;
    db.refresh_st("ss_corr_st").await;
    db.assert_st_matches_query("ss_corr_st", q).await;

    // Add department
    db.execute("INSERT INTO ss_dept (id, name) VALUES (3, 'marketing')")
        .await;
    db.refresh_st("ss_corr_st").await;
    db.assert_st_matches_query("ss_corr_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Scalar subquery returning NULL
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "DVM: correlated scalar subquery differential generates invalid SQL (ROADMAP)"]
async fn test_scalar_subquery_null_result_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ss_main (id SERIAL PRIMARY KEY, cat TEXT, val INT)")
        .await;
    db.execute("CREATE TABLE ss_lookup (id SERIAL PRIMARY KEY, cat TEXT, factor INT)")
        .await;
    db.execute("INSERT INTO ss_main (cat, val) VALUES ('a', 10), ('b', 20), ('c', 30)")
        .await;
    db.execute("INSERT INTO ss_lookup (cat, factor) VALUES ('a', 2), ('c', 5)")
        .await;

    // cat='b' has no match → scalar subquery returns NULL
    let q = "SELECT m.cat, m.val, \
             (SELECT l.factor FROM ss_lookup l WHERE l.cat = m.cat) AS factor \
             FROM ss_main m";
    db.create_st("ss_null_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ss_null_st", q).await;

    // Add lookup for 'b' → NULL → non-NULL
    db.execute("INSERT INTO ss_lookup (cat, factor) VALUES ('b', 3)")
        .await;
    db.refresh_st("ss_null_st").await;
    db.assert_st_matches_query("ss_null_st", q).await;

    // Remove lookup for 'a' → non-NULL → NULL
    db.execute("DELETE FROM ss_lookup WHERE cat = 'a'").await;
    db.refresh_st("ss_null_st").await;
    db.assert_st_matches_query("ss_null_st", q).await;
}
