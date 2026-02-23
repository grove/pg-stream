//! E2E tests for Common Table Expression (CTE) support.
//!
//! Validates:
//! - Tier 1: Non-recursive CTEs (inline expansion) — FULL + DIFFERENTIAL
//! - Tier 2: Multi-reference CTEs (shared delta) — DIFFERENTIAL
//! - Tier 3a: Recursive CTEs — FULL mode
//! - Tier 3b: Recursive CTEs — DIFFERENTIAL via recomputation diff
//! - Tier 3c: Semi-naive evaluation (INSERT-only) vs recomputation fallback (DELETE/UPDATE)
//! - Subqueries in FROM (T_RangeSubselect)
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════════
//  Tier 1 — Non-Recursive CTEs (Inline Expansion)
// ═══════════════════════════════════════════════════════════════════════════

// ── Basic CTE: FULL mode ───────────────────────────────────────────────

#[tokio::test]
async fn test_cte_simple_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT, active BOOLEAN)")
        .await;
    db.execute(
        "INSERT INTO users VALUES (1, 'Alice', true), (2, 'Bob', false), \
         (3, 'Charlie', true), (4, 'Diana', true)",
    )
    .await;

    db.create_dt(
        "active_users_dt",
        "WITH active AS (SELECT id, name FROM users WHERE active = true) \
         SELECT id, name FROM active",
        "1m",
        "FULL",
    )
    .await;

    let (status, mode, populated, errors) = db.pgs_status("active_users_dt").await;
    assert_eq!(status, "ACTIVE");
    assert_eq!(mode, "FULL");
    assert!(populated);
    assert_eq!(errors, 0);

    let count = db.count("public.active_users_dt").await;
    assert_eq!(count, 3, "Should have 3 active users");
}

#[tokio::test]
async fn test_cte_simple_full_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE items (id INT PRIMARY KEY, category TEXT, price INT)")
        .await;
    db.execute("INSERT INTO items VALUES (1, 'A', 10), (2, 'B', 20), (3, 'A', 30)")
        .await;

    db.create_dt(
        "cte_items_dt",
        "WITH cat_a AS (SELECT id, price FROM items WHERE category = 'A') \
         SELECT id, price FROM cat_a",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.cte_items_dt").await, 2);

    // Add more category A items
    db.execute("INSERT INTO items VALUES (4, 'A', 40), (5, 'B', 50)")
        .await;
    db.refresh_dt("cte_items_dt").await;

    assert_eq!(
        db.count("public.cte_items_dt").await,
        3,
        "Should have 3 category A items after refresh"
    );
}

// ── Basic CTE: DIFFERENTIAL mode ────────────────────────────────────────

#[tokio::test]
async fn test_cte_simple_differential_create() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE products (id INT PRIMARY KEY, name TEXT, qty INT)")
        .await;
    db.execute(
        "INSERT INTO products VALUES (1, 'Widget', 10), (2, 'Gadget', 20), \
         (3, 'Doohickey', 5)",
    )
    .await;

    db.create_dt(
        "stocked_products_dt",
        "WITH stocked AS (SELECT id, name, qty FROM products WHERE qty > 0) \
         SELECT id, name, qty FROM stocked",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let (status, mode, populated, _) = db.pgs_status("stocked_products_dt").await;
    assert_eq!(status, "ACTIVE");
    assert_eq!(mode, "DIFFERENTIAL");
    assert!(populated);

    assert_eq!(db.count("public.stocked_products_dt").await, 3);
}

#[tokio::test]
async fn test_cte_differential_refresh_inserts() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE inv (id INT PRIMARY KEY, sku TEXT, qty INT)")
        .await;
    db.execute("INSERT INTO inv VALUES (1, 'SKU-A', 10), (2, 'SKU-B', 20)")
        .await;

    db.create_dt(
        "inv_cte_dt",
        "WITH base AS (SELECT id, sku, qty FROM inv) \
         SELECT id, sku, qty FROM base",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.inv_cte_dt").await, 2);

    // Insert new rows
    db.execute("INSERT INTO inv VALUES (3, 'SKU-C', 30), (4, 'SKU-D', 40)")
        .await;
    db.refresh_dt("inv_cte_dt").await;

    assert_eq!(db.count("public.inv_cte_dt").await, 4);
}

#[tokio::test]
async fn test_cte_differential_refresh_updates() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE emp (id INT PRIMARY KEY, name TEXT, dept TEXT)")
        .await;
    db.execute("INSERT INTO emp VALUES (1, 'Alice', 'Eng'), (2, 'Bob', 'Sales')")
        .await;

    db.create_dt(
        "emp_cte_dt",
        "WITH all_emp AS (SELECT id, name, dept FROM emp) \
         SELECT id, name, dept FROM all_emp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Update a row
    db.execute("UPDATE emp SET dept = 'Marketing' WHERE id = 2")
        .await;
    db.refresh_dt("emp_cte_dt").await;

    let dept: String = db
        .query_scalar("SELECT dept FROM public.emp_cte_dt WHERE id = 2")
        .await;
    assert_eq!(dept, "Marketing");
}

#[tokio::test]
async fn test_cte_differential_refresh_deletes() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE logs (id INT PRIMARY KEY, msg TEXT)")
        .await;
    db.execute("INSERT INTO logs VALUES (1, 'first'), (2, 'second'), (3, 'third')")
        .await;

    db.create_dt(
        "logs_cte_dt",
        "WITH recent AS (SELECT id, msg FROM logs) \
         SELECT id, msg FROM recent",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.logs_cte_dt").await, 3);

    db.execute("DELETE FROM logs WHERE id = 2").await;
    db.refresh_dt("logs_cte_dt").await;

    assert_eq!(db.count("public.logs_cte_dt").await, 2);
    let has_2: bool = db
        .query_scalar("SELECT EXISTS(SELECT 1 FROM public.logs_cte_dt WHERE id = 2)")
        .await;
    assert!(!has_2, "Deleted row should not appear in ST");
}

#[tokio::test]
async fn test_cte_differential_mixed_dml() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE tasks (id INT PRIMARY KEY, title TEXT, done BOOLEAN)")
        .await;
    db.execute(
        "INSERT INTO tasks VALUES (1, 'task-a', false), (2, 'task-b', false), \
         (3, 'task-c', true)",
    )
    .await;

    let query = "WITH all_tasks AS (SELECT id, title, done FROM tasks) \
                 SELECT id, title, done FROM all_tasks";

    db.create_dt("tasks_cte_dt", query, "1m", "DIFFERENTIAL")
        .await;

    // Mixed DML
    db.execute("INSERT INTO tasks VALUES (4, 'task-d', false)")
        .await;
    db.execute("UPDATE tasks SET done = true WHERE id = 1")
        .await;
    db.execute("DELETE FROM tasks WHERE id = 2").await;

    db.refresh_dt("tasks_cte_dt").await;

    db.assert_dt_matches_query("public.tasks_cte_dt", query)
        .await;
}

// ── CTE with Filter ────────────────────────────────────────────────────

#[tokio::test]
async fn test_cte_with_where_clause_inside() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE scores (id INT PRIMARY KEY, student TEXT, score INT)")
        .await;
    db.execute(
        "INSERT INTO scores VALUES (1, 'Alice', 90), (2, 'Bob', 60), \
         (3, 'Charlie', 85), (4, 'Diana', 40)",
    )
    .await;

    let query = "WITH passing AS (SELECT id, student, score FROM scores WHERE score >= 70) \
                 SELECT id, student, score FROM passing";

    db.create_dt("passing_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(
        db.count("public.passing_dt").await,
        2,
        "Only Alice (90) and Charlie (85) pass"
    );

    // Add a passing and failing student
    db.execute("INSERT INTO scores VALUES (5, 'Eve', 75), (6, 'Frank', 50)")
        .await;
    db.refresh_dt("passing_dt").await;

    assert_eq!(
        db.count("public.passing_dt").await,
        3,
        "Alice, Charlie, and Eve should pass"
    );
}

#[tokio::test]
async fn test_cte_with_where_clause_outside() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE regions (id INT PRIMARY KEY, region TEXT, revenue INT)")
        .await;
    db.execute(
        "INSERT INTO regions VALUES (1, 'US', 100), (2, 'EU', 200), \
         (3, 'US', 300), (4, 'APAC', 400)",
    )
    .await;

    let query = "WITH all_regions AS (SELECT id, region, revenue FROM regions) \
                 SELECT id, region, revenue FROM all_regions WHERE region = 'US'";

    db.create_dt("us_regions_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(db.count("public.us_regions_dt").await, 2);

    db.execute("INSERT INTO regions VALUES (5, 'US', 500), (6, 'EU', 600)")
        .await;
    db.refresh_dt("us_regions_dt").await;

    assert_eq!(
        db.count("public.us_regions_dt").await,
        3,
        "Should have 3 US regions"
    );
}

// ── CTE with Aggregation ──────────────────────────────────────────────

#[tokio::test]
async fn test_cte_with_aggregation_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE orders (id SERIAL PRIMARY KEY, customer_id INT, amount INT)")
        .await;
    db.execute(
        "INSERT INTO orders (customer_id, amount) VALUES \
         (1, 100), (1, 200), (2, 50), (2, 150), (3, 300)",
    )
    .await;

    let query = "WITH order_data AS (SELECT customer_id, amount FROM orders) \
                 SELECT customer_id, SUM(amount) AS total, COUNT(*) AS cnt \
                 FROM order_data GROUP BY customer_id";

    db.create_dt("order_agg_cte_dt", query, "1m", "DIFFERENTIAL")
        .await;

    let total_1: i64 = db
        .query_scalar("SELECT total::bigint FROM public.order_agg_cte_dt WHERE customer_id = 1")
        .await;
    assert_eq!(total_1, 300, "Customer 1: 100 + 200");

    // Add more orders
    db.execute("INSERT INTO orders (customer_id, amount) VALUES (1, 150), (3, 100)")
        .await;
    db.refresh_dt("order_agg_cte_dt").await;

    let total_1_after: i64 = db
        .query_scalar("SELECT total::bigint FROM public.order_agg_cte_dt WHERE customer_id = 1")
        .await;
    assert_eq!(total_1_after, 450, "Customer 1: 100 + 200 + 150");

    let total_3_after: i64 = db
        .query_scalar("SELECT total::bigint FROM public.order_agg_cte_dt WHERE customer_id = 3")
        .await;
    assert_eq!(total_3_after, 400, "Customer 3: 300 + 100");
}

#[tokio::test]
async fn test_cte_aggregation_inside_body() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sales (id SERIAL PRIMARY KEY, product TEXT, qty INT)")
        .await;
    db.execute(
        "INSERT INTO sales (product, qty) VALUES \
         ('A', 10), ('A', 20), ('B', 30), ('B', 15)",
    )
    .await;

    let query = "WITH totals AS (\
                     SELECT product, SUM(qty) AS total_qty FROM sales GROUP BY product\
                 ) SELECT product, total_qty FROM totals";

    db.create_dt("agg_in_cte_dt", query, "1m", "DIFFERENTIAL")
        .await;

    let total_a: i64 = db
        .query_scalar("SELECT total_qty::bigint FROM public.agg_in_cte_dt WHERE product = 'A'")
        .await;
    assert_eq!(total_a, 30);

    db.execute("INSERT INTO sales (product, qty) VALUES ('A', 5), ('C', 100)")
        .await;
    db.refresh_dt("agg_in_cte_dt").await;

    let total_a_after: i64 = db
        .query_scalar("SELECT total_qty::bigint FROM public.agg_in_cte_dt WHERE product = 'A'")
        .await;
    assert_eq!(total_a_after, 35, "Product A: 10 + 20 + 5");

    assert_eq!(
        db.count("public.agg_in_cte_dt").await,
        3,
        "Should have products A, B, C"
    );
}

// ── Multiple CTEs ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_multiple_ctes_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE customers (id INT PRIMARY KEY, name TEXT, tier TEXT)")
        .await;
    db.execute("CREATE TABLE purchases (id INT PRIMARY KEY, cust_id INT, amount INT)")
        .await;
    db.execute(
        "INSERT INTO customers VALUES (1, 'Alice', 'gold'), (2, 'Bob', 'silver'), \
         (3, 'Charlie', 'gold')",
    )
    .await;
    db.execute(
        "INSERT INTO purchases VALUES (1, 1, 100), (2, 1, 200), (3, 2, 50), \
         (4, 3, 300), (5, 3, 150)",
    )
    .await;

    db.create_dt(
        "multi_cte_dt",
        "WITH gold_customers AS (\
             SELECT id, name FROM customers WHERE tier = 'gold'\
         ), gold_purchases AS (\
             SELECT p.cust_id, p.amount \
             FROM purchases p \
             JOIN gold_customers gc ON p.cust_id = gc.id\
         ) SELECT cust_id, SUM(amount) AS total FROM gold_purchases GROUP BY cust_id",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.multi_cte_dt").await;
    assert_eq!(count, 2, "Should have totals for Alice and Charlie");

    let total_alice: i64 = db
        .query_scalar("SELECT total::bigint FROM public.multi_cte_dt WHERE cust_id = 1")
        .await;
    assert_eq!(total_alice, 300, "Alice: 100 + 200");
}

#[tokio::test]
async fn test_multiple_ctes_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dept (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE staff (id INT PRIMARY KEY, name TEXT, dept_id INT)")
        .await;
    db.execute("INSERT INTO dept VALUES (1, 'Engineering'), (2, 'Sales')")
        .await;
    db.execute("INSERT INTO staff VALUES (1, 'Alice', 1), (2, 'Bob', 1), (3, 'Charlie', 2)")
        .await;

    let query = "WITH d AS (\
                     SELECT id AS dept_id, name AS dept_name FROM dept\
                 ), s AS (\
                     SELECT id, name, dept_id FROM staff\
                 ) SELECT s.id, s.name, d.dept_name \
                   FROM s JOIN d ON s.dept_id = d.dept_id";

    db.create_dt("multi_cte_inc_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(db.count("public.multi_cte_inc_dt").await, 3);

    // Add a new employee
    db.execute("INSERT INTO staff VALUES (4, 'Diana', 2)").await;
    db.refresh_dt("multi_cte_inc_dt").await;

    assert_eq!(db.count("public.multi_cte_inc_dt").await, 4);
}

// ── Chained CTEs (CTE referencing earlier CTE) ────────────────────────

#[tokio::test]
async fn test_chained_ctes_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE raw_data (id INT PRIMARY KEY, val INT, category TEXT)")
        .await;
    db.execute(
        "INSERT INTO raw_data VALUES (1, 10, 'A'), (2, 20, 'A'), (3, 30, 'B'), \
         (4, 40, 'B'), (5, 50, 'A')",
    )
    .await;

    // CTE b references CTE a
    db.create_dt(
        "chained_cte_dt",
        "WITH a AS (\
             SELECT id, val, category FROM raw_data WHERE val > 15\
         ), b AS (\
             SELECT category, SUM(val) AS total FROM a GROUP BY category\
         ) SELECT category, total FROM b",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.chained_cte_dt").await;
    assert_eq!(count, 2, "Should have category A and B");

    let total_a: i64 = db
        .query_scalar("SELECT total::bigint FROM public.chained_cte_dt WHERE category = 'A'")
        .await;
    assert_eq!(total_a, 70, "Category A: 20 + 50 (val > 15)");

    let total_b: i64 = db
        .query_scalar("SELECT total::bigint FROM public.chained_cte_dt WHERE category = 'B'")
        .await;
    assert_eq!(total_b, 70, "Category B: 30 + 40");
}

#[tokio::test]
async fn test_chained_ctes_differential_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE metrics (id INT PRIMARY KEY, sensor TEXT, reading INT)")
        .await;
    db.execute(
        "INSERT INTO metrics VALUES (1, 'temp', 20), (2, 'temp', 25), \
         (3, 'humidity', 60), (4, 'humidity', 65)",
    )
    .await;

    let query = "WITH raw AS (\
                     SELECT id, sensor, reading FROM metrics\
                 ), averages AS (\
                     SELECT sensor, SUM(reading) AS total, COUNT(*) AS cnt \
                     FROM raw GROUP BY sensor\
                 ) SELECT sensor, total, cnt FROM averages";

    db.create_dt("chained_cte_inc_dt", query, "1m", "DIFFERENTIAL")
        .await;

    let temp_total: i64 = db
        .query_scalar("SELECT total::bigint FROM public.chained_cte_inc_dt WHERE sensor = 'temp'")
        .await;
    assert_eq!(temp_total, 45, "temp: 20 + 25");

    // Add more readings
    db.execute("INSERT INTO metrics VALUES (5, 'temp', 30), (6, 'pressure', 1013)")
        .await;
    db.refresh_dt("chained_cte_inc_dt").await;

    let temp_total_after: i64 = db
        .query_scalar("SELECT total::bigint FROM public.chained_cte_inc_dt WHERE sensor = 'temp'")
        .await;
    assert_eq!(temp_total_after, 75, "temp: 20 + 25 + 30");

    assert_eq!(
        db.count("public.chained_cte_inc_dt").await,
        3,
        "Should have temp, humidity, pressure"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tier 2 — Multi-Reference CTEs (Shared Delta)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cte_referenced_twice_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE numbers (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO numbers VALUES (1, 10), (2, 20), (3, 30)")
        .await;

    // CTE referenced twice in a self-join
    db.create_dt(
        "multi_ref_dt",
        "WITH nums AS (SELECT id, val FROM numbers) \
         SELECT a.id AS id_a, b.id AS id_b, a.val AS val_a, b.val AS val_b \
         FROM nums a JOIN nums b ON a.id = b.id",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(
        db.count("public.multi_ref_dt").await,
        3,
        "Self-join on same key produces 3 rows"
    );
}

#[tokio::test]
async fn test_cte_referenced_twice_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE vals (id INT PRIMARY KEY, v INT)")
        .await;
    db.execute("INSERT INTO vals VALUES (1, 10), (2, 20)").await;

    let query = "WITH base AS (SELECT id, v FROM vals) \
                 SELECT a.id AS id_a, b.id AS id_b, a.v AS v_a, b.v AS v_b \
                 FROM base a JOIN base b ON a.id = b.id";

    db.create_dt("multi_ref_inc_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(db.count("public.multi_ref_inc_dt").await, 2);

    db.execute("INSERT INTO vals VALUES (3, 30)").await;
    db.refresh_dt("multi_ref_inc_dt").await;

    assert_eq!(
        db.count("public.multi_ref_inc_dt").await,
        3,
        "New row should appear in both sides of the self-join"
    );
}

#[tokio::test]
async fn test_cte_multi_ref_with_different_filters() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE people (id INT PRIMARY KEY, name TEXT, age INT)")
        .await;
    db.execute(
        "INSERT INTO people VALUES \
         (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Charlie', 35), (4, 'Diana', 28)",
    )
    .await;

    // Same CTE referenced twice with different WHERE in outer query
    let query = "WITH all_people AS (SELECT id, name, age FROM people) \
                 SELECT young.name AS young_name, senior.name AS senior_name \
                 FROM all_people young \
                 JOIN all_people senior ON young.age < senior.age \
                 WHERE young.age < 30 AND senior.age >= 30";

    db.create_dt("multi_ref_filter_dt", query, "1m", "FULL")
        .await;

    // Young (<30): Bob(25), Diana(28). Senior (>=30): Alice(30), Charlie(35).
    // Pairs: Bob-Alice, Bob-Charlie, Diana-Alice, Diana-Charlie = 4
    assert_eq!(db.count("public.multi_ref_filter_dt").await, 4);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tier 3a/3b — Recursive CTEs (FULL and DIFFERENTIAL via recomputation)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_recursive_cte_full_mode_succeeds() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE categories (\
             id INT PRIMARY KEY, \
             name TEXT, \
             parent_id INT REFERENCES categories(id)\
         )",
    )
    .await;
    db.execute(
        "INSERT INTO categories VALUES \
         (1, 'Electronics', NULL), \
         (2, 'Computers', 1), \
         (3, 'Laptops', 2), \
         (4, 'Desktops', 2), \
         (5, 'Phones', 1)",
    )
    .await;

    db.create_dt(
        "cat_tree_dt",
        "WITH RECURSIVE cat_tree AS (\
             SELECT id, name, parent_id, 0 AS depth \
             FROM categories WHERE parent_id IS NULL \
             UNION ALL \
             SELECT c.id, c.name, c.parent_id, ct.depth + 1 \
             FROM categories c \
             JOIN cat_tree ct ON c.parent_id = ct.id\
         ) SELECT id, name, depth FROM cat_tree",
        "1m",
        "FULL",
    )
    .await;

    let (status, mode, populated, _) = db.pgs_status("cat_tree_dt").await;
    assert_eq!(status, "ACTIVE");
    assert_eq!(mode, "FULL");
    assert!(populated);

    let count = db.count("public.cat_tree_dt").await;
    assert_eq!(count, 5, "All 5 categories should appear in traversal");

    // Verify depths
    let root_depth: i32 = db
        .query_scalar("SELECT depth FROM public.cat_tree_dt WHERE name = 'Electronics'")
        .await;
    assert_eq!(root_depth, 0);

    let laptop_depth: i32 = db
        .query_scalar("SELECT depth FROM public.cat_tree_dt WHERE name = 'Laptops'")
        .await;
    assert_eq!(laptop_depth, 2);
}

#[tokio::test]
async fn test_recursive_cte_full_refresh_picks_up_changes() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE org (\
             id INT PRIMARY KEY, \
             name TEXT, \
             manager_id INT\
         )",
    )
    .await;
    db.execute("INSERT INTO org VALUES (1, 'CEO', NULL), (2, 'VP', 1), (3, 'Dir', 2)")
        .await;

    let query = "WITH RECURSIVE org_tree AS (\
                     SELECT id, name, manager_id, 1 AS level \
                     FROM org WHERE manager_id IS NULL \
                     UNION ALL \
                     SELECT o.id, o.name, o.manager_id, ot.level + 1 \
                     FROM org o \
                     JOIN org_tree ot ON o.manager_id = ot.id\
                 ) SELECT id, name, level FROM org_tree";

    db.create_dt("org_dt", query, "1m", "FULL").await;

    assert_eq!(db.count("public.org_dt").await, 3);

    // Add a new report
    db.execute("INSERT INTO org VALUES (4, 'Manager', 2), (5, 'IC', 4)")
        .await;
    db.refresh_dt("org_dt").await;

    assert_eq!(
        db.count("public.org_dt").await,
        5,
        "Should include new org members"
    );

    let ic_level: i32 = db
        .query_scalar("SELECT level FROM public.org_dt WHERE name = 'IC'")
        .await;
    assert_eq!(ic_level, 4, "IC should be at level 4 (CEO→VP→Manager→IC)");
}

#[tokio::test]
async fn test_recursive_cte_differential_mode_succeeds() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE tree_src (id INT PRIMARY KEY, parent_id INT, val TEXT)")
        .await;
    db.execute("INSERT INTO tree_src VALUES (1, NULL, 'root'), (2, 1, 'child')")
        .await;

    // Tier 3b: DIFFERENTIAL mode now supported for recursive CTEs
    // (uses recomputation diff strategy under the hood)
    db.create_dt(
        "recursive_inc_dt",
        "WITH RECURSIVE tree AS (\
             SELECT id, parent_id, val FROM tree_src WHERE parent_id IS NULL \
             UNION ALL \
             SELECT t.id, t.parent_id, t.val FROM tree_src t \
             JOIN tree tr ON t.parent_id = tr.id\
         ) SELECT id, val FROM tree",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let (status, mode, populated, _) = db.pgs_status("recursive_inc_dt").await;
    assert_eq!(status, "ACTIVE");
    assert_eq!(mode, "DIFFERENTIAL");
    assert!(populated);
    assert_eq!(db.count("public.recursive_inc_dt").await, 2);

    // Insert a new descendant — differential refresh should pick it up
    db.execute("INSERT INTO tree_src VALUES (3, 2, 'grandchild')")
        .await;
    db.refresh_dt("recursive_inc_dt").await;

    assert_eq!(
        db.count("public.recursive_inc_dt").await,
        3,
        "Differential refresh should add the new grandchild row"
    );

    // Delete a row — differential refresh should remove it and its descendants
    db.execute("DELETE FROM tree_src WHERE id = 2").await;
    db.refresh_dt("recursive_inc_dt").await;

    assert_eq!(
        db.count("public.recursive_inc_dt").await,
        1,
        "After deleting node 2, only root (1) should remain (node 3 is orphaned)"
    );
}

#[tokio::test]
async fn test_recursive_cte_differential_insert_deep_tree() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE deep_tree (id INT PRIMARY KEY, parent_id INT, label TEXT)")
        .await;
    db.execute("INSERT INTO deep_tree VALUES (1, NULL, 'root')")
        .await;

    db.create_dt(
        "deep_tree_dt",
        "WITH RECURSIVE t AS (\
             SELECT id, parent_id, label, 0 AS depth FROM deep_tree WHERE parent_id IS NULL \
             UNION ALL \
             SELECT d.id, d.parent_id, d.label, t.depth + 1 \
             FROM deep_tree d JOIN t ON d.parent_id = t.id\
         ) SELECT id, label, depth FROM t",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.deep_tree_dt").await, 1);

    // Build a chain: root → a → b → c
    db.execute("INSERT INTO deep_tree VALUES (2, 1, 'a'), (3, 2, 'b'), (4, 3, 'c')")
        .await;
    db.refresh_dt("deep_tree_dt").await;

    assert_eq!(db.count("public.deep_tree_dt").await, 4);

    let c_depth: i32 = db
        .query_scalar("SELECT depth FROM public.deep_tree_dt WHERE label = 'c'")
        .await;
    assert_eq!(c_depth, 3, "Node 'c' should be at depth 3");
}

#[tokio::test]
async fn test_recursive_cte_differential_update() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE upd_tree (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute("INSERT INTO upd_tree VALUES (1, NULL, 'root'), (2, 1, 'child'), (3, 2, 'leaf')")
        .await;

    db.create_dt(
        "upd_tree_dt",
        "WITH RECURSIVE t AS (\
             SELECT id, parent_id, name FROM upd_tree WHERE parent_id IS NULL \
             UNION ALL \
             SELECT u.id, u.parent_id, u.name FROM upd_tree u JOIN t ON u.parent_id = t.id\
         ) SELECT id, name FROM t",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.upd_tree_dt").await, 3);

    // Move node 3 to be a direct child of root (reparent)
    db.execute("UPDATE upd_tree SET parent_id = 1 WHERE id = 3")
        .await;
    // Rename the root
    db.execute("UPDATE upd_tree SET name = 'ROOT' WHERE id = 1")
        .await;
    db.refresh_dt("upd_tree_dt").await;

    // Should still have 3 rows, but names updated
    assert_eq!(db.count("public.upd_tree_dt").await, 3);
    let root_name: String = db
        .query_scalar("SELECT name FROM public.upd_tree_dt WHERE id = 1")
        .await;
    assert_eq!(root_name, "ROOT");
}

#[tokio::test]
async fn test_recursive_cte_alter_to_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE alt_tree (id INT PRIMARY KEY, parent_id INT, val TEXT)")
        .await;
    db.execute("INSERT INTO alt_tree VALUES (1, NULL, 'A'), (2, 1, 'B')")
        .await;

    // Start in FULL mode
    db.create_dt(
        "alt_tree_dt",
        "WITH RECURSIVE t AS (\
             SELECT id, parent_id, val FROM alt_tree WHERE parent_id IS NULL \
             UNION ALL \
             SELECT a.id, a.parent_id, a.val FROM alt_tree a JOIN t ON a.parent_id = t.id\
         ) SELECT id, val FROM t",
        "1m",
        "FULL",
    )
    .await;
    assert_eq!(db.count("public.alt_tree_dt").await, 2);

    // Alter to DIFFERENTIAL — should succeed now (Tier 3b)
    db.alter_dt("alt_tree_dt", "refresh_mode => 'DIFFERENTIAL'")
        .await;

    let (_, mode, _, _) = db.pgs_status("alt_tree_dt").await;
    assert_eq!(mode, "DIFFERENTIAL");

    // Add data and refresh differentially
    db.execute("INSERT INTO alt_tree VALUES (3, 2, 'C')").await;
    db.refresh_dt("alt_tree_dt").await;
    assert_eq!(db.count("public.alt_tree_dt").await, 3);
}

#[tokio::test]
async fn test_recursive_cte_graph_traversal() {
    let db = E2eDb::new().await.with_extension().await;

    // Graph with possible multiple paths (UNION ALL allows duplicates,
    // but still demonstrates graph traversal).
    db.execute(
        "CREATE TABLE edges (from_node INT, to_node INT, weight INT, \
         PRIMARY KEY (from_node, to_node))",
    )
    .await;
    db.execute(
        "INSERT INTO edges VALUES \
         (1, 2, 10), (1, 3, 20), (2, 4, 5), (3, 4, 15), (4, 5, 8)",
    )
    .await;

    db.create_dt(
        "reachable_dt",
        "WITH RECURSIVE reachable AS (\
             SELECT from_node, to_node, weight, 1 AS hops \
             FROM edges WHERE from_node = 1 \
             UNION ALL \
             SELECT e.from_node, e.to_node, e.weight, r.hops + 1 \
             FROM edges e \
             JOIN reachable r ON e.from_node = r.to_node \
             WHERE r.hops < 10\
         ) SELECT to_node, weight, hops FROM reachable",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.reachable_dt").await;
    assert!(
        count >= 4,
        "Should reach nodes 2,3,4,5 (at least 4 edges traversed)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tier 3c — Semi-Naive INSERT-Only Path
// ═══════════════════════════════════════════════════════════════════════════

/// INSERT new root nodes: the base case delta should produce them and
/// semi-naive propagation should not add any recursive descendants.
#[tokio::test]
async fn test_seminaive_insert_new_root_nodes() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_nodes (id INT PRIMARY KEY, parent_id INT, label TEXT)")
        .await;
    db.execute("INSERT INTO sn_nodes VALUES (1, NULL, 'root1'), (2, 1, 'child1')")
        .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, label FROM sn_nodes WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT n.id, n.parent_id, n.label FROM sn_nodes n \
                     JOIN t ON n.parent_id = t.id\
                 ) SELECT id, label FROM t";

    db.create_dt("sn_root_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.sn_root_dt").await, 2);

    // INSERT-only: add two new root nodes (no children)
    db.execute("INSERT INTO sn_nodes VALUES (10, NULL, 'root2'), (11, NULL, 'root3')")
        .await;
    db.refresh_dt("sn_root_dt").await;

    assert_eq!(
        db.count("public.sn_root_dt").await,
        4,
        "Two new roots added (semi-naive path)"
    );
    db.assert_dt_matches_query("public.sn_root_dt", query).await;
}

/// INSERT leaf nodes that connect to existing tree nodes.
/// Semi-naive seed part 2 should find these via the ST storage join.
#[tokio::test]
async fn test_seminaive_insert_leaves_connecting_to_existing() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_leaf (id INT PRIMARY KEY, parent_id INT, val INT)")
        .await;
    db.execute(
        "INSERT INTO sn_leaf VALUES \
         (1, NULL, 100), (2, 1, 200), (3, 1, 300)",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, val FROM sn_leaf WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT s.id, s.parent_id, s.val FROM sn_leaf s \
                     JOIN t ON s.parent_id = t.id\
                 ) SELECT id, val FROM t";

    db.create_dt("sn_leaf_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.sn_leaf_dt").await, 3);

    // INSERT-only: add leaves under existing node 2 and node 3
    db.execute("INSERT INTO sn_leaf VALUES (4, 2, 400), (5, 2, 500), (6, 3, 600)")
        .await;
    db.refresh_dt("sn_leaf_dt").await;

    assert_eq!(
        db.count("public.sn_leaf_dt").await,
        6,
        "Three new leaves connected to existing nodes"
    );
    db.assert_dt_matches_query("public.sn_leaf_dt", query).await;
}

/// INSERT an entire subtree in one batch: a new root + children + grandchildren.
/// Semi-naive should propagate through multiple levels in one refresh.
#[tokio::test]
async fn test_seminaive_insert_whole_subtree() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_sub (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute("INSERT INTO sn_sub VALUES (1, NULL, 'A'), (2, 1, 'B')")
        .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name, 0 AS depth FROM sn_sub WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT s.id, s.parent_id, s.name, t.depth + 1 \
                     FROM sn_sub s JOIN t ON s.parent_id = t.id\
                 ) SELECT id, name, depth FROM t";

    db.create_dt("sn_sub_dt", query, "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.sn_sub_dt").await, 2);

    // INSERT a complete new subtree: root → child → grandchild → great-grandchild
    db.execute(
        "INSERT INTO sn_sub VALUES \
         (10, NULL, 'X'), \
         (11, 10, 'X1'), \
         (12, 11, 'X11'), \
         (13, 12, 'X111')",
    )
    .await;
    db.refresh_dt("sn_sub_dt").await;

    assert_eq!(
        db.count("public.sn_sub_dt").await,
        6,
        "Original 2 + new subtree of 4"
    );

    // Verify depths of the new subtree
    let x_depth: i32 = db
        .query_scalar("SELECT depth FROM public.sn_sub_dt WHERE name = 'X'")
        .await;
    assert_eq!(x_depth, 0, "New root X at depth 0");

    let x111_depth: i32 = db
        .query_scalar("SELECT depth FROM public.sn_sub_dt WHERE name = 'X111'")
        .await;
    assert_eq!(x111_depth, 3, "X111 at depth 3");

    db.assert_dt_matches_query("public.sn_sub_dt", query).await;
}

/// Multiple sequential INSERT-only refreshes. Each refresh should
/// differentially add rows without losing previously added data.
#[tokio::test]
async fn test_seminaive_sequential_insert_refreshes() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_seq (id INT PRIMARY KEY, parent_id INT, v TEXT)")
        .await;
    db.execute("INSERT INTO sn_seq VALUES (1, NULL, 'root')")
        .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, v FROM sn_seq WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT s.id, s.parent_id, s.v FROM sn_seq s \
                     JOIN t ON s.parent_id = t.id\
                 ) SELECT id, v FROM t";

    db.create_dt("sn_seq_dt", query, "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.sn_seq_dt").await, 1);

    // Refresh 1: add child
    db.execute("INSERT INTO sn_seq VALUES (2, 1, 'child')")
        .await;
    db.refresh_dt("sn_seq_dt").await;
    assert_eq!(db.count("public.sn_seq_dt").await, 2);

    // Refresh 2: add grandchild
    db.execute("INSERT INTO sn_seq VALUES (3, 2, 'grandchild')")
        .await;
    db.refresh_dt("sn_seq_dt").await;
    assert_eq!(db.count("public.sn_seq_dt").await, 3);

    // Refresh 3: add great-grandchild + sibling of child
    db.execute("INSERT INTO sn_seq VALUES (4, 3, 'great-grandchild'), (5, 1, 'child2')")
        .await;
    db.refresh_dt("sn_seq_dt").await;
    assert_eq!(db.count("public.sn_seq_dt").await, 5);

    // Refresh 4: no changes — should be idempotent
    db.refresh_dt("sn_seq_dt").await;
    assert_eq!(db.count("public.sn_seq_dt").await, 5);

    db.assert_dt_matches_query("public.sn_seq_dt", query).await;
}

/// INSERT wide tree: many siblings at the same level.
/// Exercises the semi-naive seed with a batch of sibling INSERTs.
#[tokio::test]
async fn test_seminaive_insert_wide_siblings() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_wide (id INT PRIMARY KEY, parent_id INT, idx INT)")
        .await;
    db.execute("INSERT INTO sn_wide VALUES (1, NULL, 0)").await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, idx FROM sn_wide WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT w.id, w.parent_id, w.idx FROM sn_wide w \
                     JOIN t ON w.parent_id = t.id\
                 ) SELECT id, idx FROM t";

    db.create_dt("sn_wide_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.sn_wide_dt").await, 1);

    // Insert 50 children of root in one batch
    db.execute("INSERT INTO sn_wide SELECT g, 1, g FROM generate_series(2, 51) g")
        .await;
    db.refresh_dt("sn_wide_dt").await;

    assert_eq!(
        db.count("public.sn_wide_dt").await,
        51,
        "Root + 50 children"
    );
    db.assert_dt_matches_query("public.sn_wide_dt", query).await;

    // Now add children under each of the 50 nodes
    db.execute("INSERT INTO sn_wide SELECT g + 100, g, g FROM generate_series(2, 51) g")
        .await;
    db.refresh_dt("sn_wide_dt").await;

    assert_eq!(
        db.count("public.sn_wide_dt").await,
        101,
        "Root + 50 children + 50 grandchildren"
    );
    db.assert_dt_matches_query("public.sn_wide_dt", query).await;
}

/// Verify that INSERT-only semi-naive result exactly matches what a
/// full re-execution of the query would produce.
#[tokio::test]
async fn test_seminaive_matches_full_reexecution() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_verify (id INT PRIMARY KEY, parent_id INT, val TEXT)")
        .await;
    db.execute(
        "INSERT INTO sn_verify VALUES \
         (1, NULL, 'root'), (2, 1, 'a'), (3, 2, 'b')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, val, 0 AS depth FROM sn_verify WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT v.id, v.parent_id, v.val, t.depth + 1 \
                     FROM sn_verify v JOIN t ON v.parent_id = t.id\
                 ) SELECT id, val, depth FROM t";

    db.create_dt("sn_verify_dt", query, "1m", "DIFFERENTIAL")
        .await;

    // Multiple INSERT-only batches
    db.execute("INSERT INTO sn_verify VALUES (4, 1, 'c'), (5, 4, 'd')")
        .await;
    db.refresh_dt("sn_verify_dt").await;

    db.execute("INSERT INTO sn_verify VALUES (6, 3, 'e'), (7, 5, 'f'), (8, NULL, 'root2')")
        .await;
    db.refresh_dt("sn_verify_dt").await;

    // The differential result must match a fresh execution
    db.assert_dt_matches_query("public.sn_verify_dt", query)
        .await;

    // Also verify specific depth values
    let f_depth: i32 = db
        .query_scalar("SELECT depth FROM public.sn_verify_dt WHERE val = 'f'")
        .await;
    assert_eq!(f_depth, 3, "f is root→a→c→d→f? No: root(0)→c(1)→d(2)→f(3)");
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tier 3d — DRed (DELETE / UPDATE via Delete-and-Rederive)
// ═══════════════════════════════════════════════════════════════════════════

/// DELETE a leaf node. DRed over-deletes it. No rederivation path exists,
/// so net deletion removes exactly that row.
#[tokio::test]
async fn test_recomp_delete_leaf_node() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rc_del (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute(
        "INSERT INTO rc_del VALUES \
         (1, NULL, 'root'), (2, 1, 'A'), (3, 1, 'B'), (4, 2, 'C')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name FROM rc_del WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT d.id, d.parent_id, d.name FROM rc_del d \
                     JOIN t ON d.parent_id = t.id\
                 ) SELECT id, name FROM t";

    db.create_dt("rc_del_dt", query, "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.rc_del_dt").await, 4);

    // DELETE a leaf — triggers recomputation fallback
    db.execute("DELETE FROM rc_del WHERE id = 4").await;
    db.refresh_dt("rc_del_dt").await;

    assert_eq!(
        db.count("public.rc_del_dt").await,
        3,
        "Leaf C removed, rest unchanged"
    );
    db.assert_dt_matches_query("public.rc_del_dt", query).await;
}

/// DELETE an internal node — its descendants become orphaned and should
/// disappear from the result. Recomputation handles this correctly.
#[tokio::test]
async fn test_recomp_delete_internal_node_cascades() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rc_casc (id INT PRIMARY KEY, parent_id INT, label TEXT)")
        .await;
    db.execute(
        "INSERT INTO rc_casc VALUES \
         (1, NULL, 'root'), \
         (2, 1, 'mid'), \
         (3, 2, 'leaf1'), \
         (4, 2, 'leaf2'), \
         (5, 3, 'deep')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, label FROM rc_casc WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT c.id, c.parent_id, c.label FROM rc_casc c \
                     JOIN t ON c.parent_id = t.id\
                 ) SELECT id, label FROM t";

    db.create_dt("rc_casc_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.rc_casc_dt").await, 5);

    // DELETE the internal node (id=2) — orphans 3, 4, 5
    db.execute("DELETE FROM rc_casc WHERE id = 2").await;
    db.refresh_dt("rc_casc_dt").await;

    assert_eq!(
        db.count("public.rc_casc_dt").await,
        1,
        "Only root remains; mid + leaf1 + leaf2 + deep all orphaned"
    );
    db.assert_dt_matches_query("public.rc_casc_dt", query).await;
}

/// UPDATE that changes the join key (parent_id), effectively reparenting
/// a subtree. The recomputation fallback should produce the correct new tree.
#[tokio::test]
async fn test_recomp_update_reparent_subtree() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rc_repar (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute(
        "INSERT INTO rc_repar VALUES \
         (1, NULL, 'root'), \
         (2, 1, 'branchA'), \
         (3, 1, 'branchB'), \
         (4, 2, 'leafA'), \
         (5, 3, 'leafB')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name, 0 AS depth FROM rc_repar WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT r.id, r.parent_id, r.name, t.depth + 1 \
                     FROM rc_repar r JOIN t ON r.parent_id = t.id\
                 ) SELECT id, name, depth FROM t";

    db.create_dt("rc_repar_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.rc_repar_dt").await, 5);

    // Move leafA (id=4) from branchA to branchB
    db.execute("UPDATE rc_repar SET parent_id = 3 WHERE id = 4")
        .await;
    db.refresh_dt("rc_repar_dt").await;

    // Still 5 rows, but leafA now under branchB
    assert_eq!(db.count("public.rc_repar_dt").await, 5);

    let leaf_a_depth: i32 = db
        .query_scalar("SELECT depth FROM public.rc_repar_dt WHERE name = 'leafA'")
        .await;
    assert_eq!(
        leaf_a_depth, 2,
        "leafA still at depth 2 (root→branchB→leafA)"
    );

    db.assert_dt_matches_query("public.rc_repar_dt", query)
        .await;
}

/// Mixed INSERT + DELETE in the same refresh cycle.
/// Should trigger recomputation fallback.
#[tokio::test]
async fn test_recomp_mixed_insert_and_delete() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rc_mix (id INT PRIMARY KEY, parent_id INT, v TEXT)")
        .await;
    db.execute(
        "INSERT INTO rc_mix VALUES \
         (1, NULL, 'root'), (2, 1, 'A'), (3, 2, 'B'), (4, 3, 'C')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, v FROM rc_mix WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT m.id, m.parent_id, m.v FROM rc_mix m \
                     JOIN t ON m.parent_id = t.id\
                 ) SELECT id, v FROM t";

    db.create_dt("rc_mix_dt", query, "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.rc_mix_dt").await, 4);

    // DELETE B (id=3) and C (id=4) but add new D under A
    db.execute("DELETE FROM rc_mix WHERE id IN (3, 4)").await;
    db.execute("INSERT INTO rc_mix VALUES (5, 2, 'D'), (6, 5, 'E')")
        .await;
    db.refresh_dt("rc_mix_dt").await;

    // root→A→D→E = 4 rows
    assert_eq!(db.count("public.rc_mix_dt").await, 4, "root + A + D + E");
    db.assert_dt_matches_query("public.rc_mix_dt", query).await;
}

/// DELETE all rows then re-insert from scratch. Recomputation should
/// handle the complete replacement.
#[tokio::test]
async fn test_recomp_delete_all_then_reinsert() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rc_repl (id INT PRIMARY KEY, parent_id INT, tag TEXT)")
        .await;
    db.execute("INSERT INTO rc_repl VALUES (1, NULL, 'old_root'), (2, 1, 'old_child')")
        .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, tag FROM rc_repl WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT r.id, r.parent_id, r.tag FROM rc_repl r \
                     JOIN t ON r.parent_id = t.id\
                 ) SELECT id, tag FROM t";

    db.create_dt("rc_repl_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.rc_repl_dt").await, 2);

    // Delete everything
    db.execute("DELETE FROM rc_repl").await;
    db.refresh_dt("rc_repl_dt").await;
    assert_eq!(db.count("public.rc_repl_dt").await, 0, "All rows deleted");

    // Re-insert completely new data
    db.execute(
        "INSERT INTO rc_repl VALUES \
         (10, NULL, 'new_root'), (11, 10, 'new_A'), (12, 11, 'new_B')",
    )
    .await;
    db.refresh_dt("rc_repl_dt").await;

    assert_eq!(
        db.count("public.rc_repl_dt").await,
        3,
        "Replaced with 3 new rows"
    );
    db.assert_dt_matches_query("public.rc_repl_dt", query).await;
}

/// UPDATE a non-join column (no reparenting). Still triggers recomputation
/// because the change buffer has 'U' actions.
#[tokio::test]
async fn test_recomp_update_non_join_column() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rc_upd (id INT PRIMARY KEY, parent_id INT, score INT)")
        .await;
    db.execute(
        "INSERT INTO rc_upd VALUES \
         (1, NULL, 10), (2, 1, 20), (3, 2, 30)",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, score FROM rc_upd WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT u.id, u.parent_id, u.score FROM rc_upd u \
                     JOIN t ON u.parent_id = t.id\
                 ) SELECT id, score FROM t";

    db.create_dt("rc_upd_dt", query, "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.rc_upd_dt").await, 3);

    // Update scores (non-join column)
    db.execute("UPDATE rc_upd SET score = score * 10").await;
    db.refresh_dt("rc_upd_dt").await;

    assert_eq!(db.count("public.rc_upd_dt").await, 3);
    let root_score: i32 = db
        .query_scalar("SELECT score FROM public.rc_upd_dt WHERE id = 1")
        .await;
    assert_eq!(root_score, 100, "Root score updated from 10 to 100");
    db.assert_dt_matches_query("public.rc_upd_dt", query).await;
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tier 3d — DRed Rederivation (multi-path / diamond scenarios)
// ═══════════════════════════════════════════════════════════════════════════

/// Diamond-shaped graph: node D reachable via two paths (A→D, B→D).
/// Delete path through A but B still reaches D — DRed should rederive D.
///
///   root(1)
///   ├─ A(2)
///   │  └─ D(5)   ← reachable via A OR B
///   ├─ B(3)
///   │  └─ D(5)   ← duplicate link
///   └─ C(4)
#[tokio::test]
async fn test_dred_diamond_rederivation() {
    let db = E2eDb::new().await.with_extension().await;

    // Use an adjacency list (edges) rather than a single parent_id to model
    // the diamond. The recursive CTE joins edges → nodes.
    db.execute("CREATE TABLE dred_diamond_nodes (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute(
        "INSERT INTO dred_diamond_nodes VALUES \
         (1, 'root'), (2, 'A'), (3, 'B'), (4, 'C'), (5, 'D')",
    )
    .await;

    db.execute(
        "CREATE TABLE dred_diamond_edges (parent_id INT, child_id INT, \
         PRIMARY KEY (parent_id, child_id))",
    )
    .await;
    db.execute(
        "INSERT INTO dred_diamond_edges VALUES \
         (1, 2), (1, 3), (1, 4), (2, 5), (3, 5)",
    )
    .await;

    // Recursive CTE traverses via edges
    let query = "WITH RECURSIVE t AS (\
                     SELECT id, name FROM dred_diamond_nodes WHERE id = 1 \
                     UNION \
                     SELECT n.id, n.name \
                     FROM dred_diamond_edges e \
                     JOIN t ON e.parent_id = t.id \
                     JOIN dred_diamond_nodes n ON n.id = e.child_id\
                 ) SELECT id, name FROM t";

    db.create_dt("dred_diamond_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.dred_diamond_dt").await, 5);

    // Delete edge A→D (2,5). D is still reachable via B→D (3,5).
    db.execute("DELETE FROM dred_diamond_edges WHERE parent_id = 2 AND child_id = 5")
        .await;
    db.refresh_dt("dred_diamond_dt").await;

    // D should survive via the B→D path
    assert_eq!(
        db.count("public.dred_diamond_dt").await,
        5,
        "D rederived through B→D path"
    );
    db.assert_dt_matches_query("public.dred_diamond_dt", query)
        .await;
}

/// Delete a subtree root — all descendants become unreachable.
/// DRed should cascade-delete the entire subtree.
///
///   root(1) → A(2) → B(3) → C(4) → D(5)
///
/// Delete A(2) → B, C, D all become unreachable.
#[tokio::test]
async fn test_dred_deep_cascade_delete() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dred_deep (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute(
        "INSERT INTO dred_deep VALUES \
         (1, NULL, 'root'), (2, 1, 'A'), (3, 2, 'B'), (4, 3, 'C'), (5, 4, 'D')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name FROM dred_deep WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT d.id, d.parent_id, d.name FROM dred_deep d \
                     JOIN t ON d.parent_id = t.id\
                 ) SELECT id, name FROM t";

    db.create_dt("dred_deep_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.dred_deep_dt").await, 5);

    // Delete A (id=2) — cascades orphan B, C, D
    db.execute("DELETE FROM dred_deep WHERE id = 2").await;
    db.refresh_dt("dred_deep_dt").await;

    assert_eq!(
        db.count("public.dred_deep_dt").await,
        1,
        "Only root remains after cascade"
    );
    db.assert_dt_matches_query("public.dred_deep_dt", query)
        .await;
}

/// Delete and insert in the same batch — DRed handles inserts (semi-naive)
/// and deletes (over-delete + rederive) independently, then combines.
///
///   root(1) → A(2), B(3)
///
/// Delete B, insert C under A → result: root, A, C
#[tokio::test]
async fn test_dred_simultaneous_insert_and_delete() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dred_simul (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute(
        "INSERT INTO dred_simul VALUES \
         (1, NULL, 'root'), (2, 1, 'A'), (3, 1, 'B')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name FROM dred_simul WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT s.id, s.parent_id, s.name FROM dred_simul s \
                     JOIN t ON s.parent_id = t.id\
                 ) SELECT id, name FROM t";

    db.create_dt("dred_simul_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.dred_simul_dt").await, 3);

    // Delete B and insert C under A in the same batch
    db.execute("DELETE FROM dred_simul WHERE id = 3").await;
    db.execute("INSERT INTO dred_simul VALUES (4, 2, 'C')")
        .await;
    db.refresh_dt("dred_simul_dt").await;

    assert_eq!(
        db.count("public.dred_simul_dt").await,
        3,
        "root + A + C (B removed, C added)"
    );
    db.assert_dt_matches_query("public.dred_simul_dt", query)
        .await;
}

/// Delete a node that has siblings. Only the deleted node and its
/// descendants should be removed; siblings remain untouched.
///
///   root(1)
///   ├─ A(2) → D(5), E(6)
///   └─ B(3) → F(7)    ← delete B
///   └─ C(4)
#[tokio::test]
async fn test_dred_delete_preserves_siblings() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dred_sib (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute(
        "INSERT INTO dred_sib VALUES \
         (1, NULL, 'root'), (2, 1, 'A'), (3, 1, 'B'), (4, 1, 'C'), \
         (5, 2, 'D'), (6, 2, 'E'), (7, 3, 'F')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name FROM dred_sib WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT s.id, s.parent_id, s.name FROM dred_sib s \
                     JOIN t ON s.parent_id = t.id\
                 ) SELECT id, name FROM t";

    db.create_dt("dred_sib_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.dred_sib_dt").await, 7);

    // Delete B (id=3) — orphans F (id=7), siblings A,C,D,E untouched
    db.execute("DELETE FROM dred_sib WHERE id = 3").await;
    db.refresh_dt("dred_sib_dt").await;

    assert_eq!(
        db.count("public.dred_sib_dt").await,
        5,
        "root + A + C + D + E (B and F removed)"
    );
    db.assert_dt_matches_query("public.dred_sib_dt", query)
        .await;
}

/// UPDATE the join column (parent_id) to reparent a subtree. DRed treats
/// UPDATE as DELETE old + INSERT new, so the node appears at its new
/// position in the tree.
#[tokio::test]
async fn test_dred_update_reparent_via_dred() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dred_repar (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute(
        "INSERT INTO dred_repar VALUES \
         (1, NULL, 'root'), (2, 1, 'A'), (3, 1, 'B'), \
         (4, 2, 'C'), (5, 3, 'D')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name, 0 AS depth FROM dred_repar WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT r.id, r.parent_id, r.name, t.depth + 1 \
                     FROM dred_repar r JOIN t ON r.parent_id = t.id\
                 ) SELECT id, name, depth FROM t";

    db.create_dt("dred_repar_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.dred_repar_dt").await, 5);

    // Move C (id=4) from A to B: changes depth from 2 to 2 (same depth, different parent)
    db.execute("UPDATE dred_repar SET parent_id = 3 WHERE id = 4")
        .await;
    db.refresh_dt("dred_repar_dt").await;

    assert_eq!(db.count("public.dred_repar_dt").await, 5);
    db.assert_dt_matches_query("public.dred_repar_dt", query)
        .await;
}

/// Sequential delete-then-insert cycles. Each refresh should correctly
/// apply DRed differentially.
#[tokio::test]
async fn test_dred_sequential_delete_refresh_cycles() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dred_seq (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute(
        "INSERT INTO dred_seq VALUES \
         (1, NULL, 'root'), (2, 1, 'A'), (3, 1, 'B'), (4, 2, 'C')",
    )
    .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name FROM dred_seq WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT s.id, s.parent_id, s.name FROM dred_seq s \
                     JOIN t ON s.parent_id = t.id\
                 ) SELECT id, name FROM t";

    db.create_dt("dred_seq_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.dred_seq_dt").await, 4);

    // Cycle 1: delete C
    db.execute("DELETE FROM dred_seq WHERE id = 4").await;
    db.refresh_dt("dred_seq_dt").await;
    assert_eq!(db.count("public.dred_seq_dt").await, 3);

    // Cycle 2: delete A — should only remove A now (C already gone)
    db.execute("DELETE FROM dred_seq WHERE id = 2").await;
    db.refresh_dt("dred_seq_dt").await;
    assert_eq!(db.count("public.dred_seq_dt").await, 2, "root + B remain");

    // Cycle 3: insert new subtree under B
    db.execute("INSERT INTO dred_seq VALUES (5, 3, 'D'), (6, 5, 'E')")
        .await;
    db.refresh_dt("dred_seq_dt").await;
    assert_eq!(db.count("public.dred_seq_dt").await, 4, "root + B + D + E");

    db.assert_dt_matches_query("public.dred_seq_dt", query)
        .await;
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tier 3e — Non-linear Detection & Linear Transitive Closure
// ═══════════════════════════════════════════════════════════════════════════

/// Linear transitive closure using standard PostgreSQL pattern:
/// `FROM reach r JOIN edges e ON r.dst = e.src` (single self-reference).
/// Full mode initial load.
#[tokio::test]
async fn test_linear_transitive_closure_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE tc_edges (id SERIAL PRIMARY KEY, src INT NOT NULL, dst INT NOT NULL)")
        .await;
    db.execute(
        "INSERT INTO tc_edges (src, dst) VALUES \
         (1, 2), (2, 3), (3, 4)",
    )
    .await;

    // Linear transitive closure: extend paths one hop at a time
    let query = "WITH RECURSIVE reach(src, dst) AS (\
                     SELECT src, dst FROM tc_edges \
                     UNION \
                     SELECT r.src, e.dst \
                     FROM reach r \
                     JOIN tc_edges e ON r.dst = e.src\
                 ) SELECT src, dst FROM reach";

    db.create_dt("tc_full_dt", query, "1m", "FULL").await;

    // Edges: 1→2, 2→3, 3→4
    // Transitive closure: (1,2),(2,3),(3,4),(1,3),(2,4),(1,4)
    assert_eq!(db.count("public.tc_full_dt").await, 6);
    db.assert_dt_matches_query("public.tc_full_dt", query).await;
}

/// Linear transitive closure in DIFFERENTIAL mode — initial load.
#[tokio::test]
async fn test_linear_transitive_closure_differential_create() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE tc_edges2 (id SERIAL PRIMARY KEY, src INT NOT NULL, dst INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO tc_edges2 (src, dst) VALUES \
         (1, 2), (2, 3), (3, 4)",
    )
    .await;

    let query = "WITH RECURSIVE reach(src, dst) AS (\
                     SELECT src, dst FROM tc_edges2 \
                     UNION \
                     SELECT r.src, e.dst \
                     FROM reach r \
                     JOIN tc_edges2 e ON r.dst = e.src\
                 ) SELECT src, dst FROM reach";

    db.create_dt("tc_inc_dt", query, "1m", "DIFFERENTIAL").await;

    assert_eq!(db.count("public.tc_inc_dt").await, 6);
    db.assert_dt_matches_query("public.tc_inc_dt", query).await;
}

/// Differential INSERT: add a new edge that extends transitive paths.
/// Semi-naive should discover all new reachable pairs.
#[tokio::test]
async fn test_linear_tc_differential_insert_extends_chain() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE tc_edges3 (id SERIAL PRIMARY KEY, src INT NOT NULL, dst INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO tc_edges3 (src, dst) VALUES \
         (1, 2), (2, 3)",
    )
    .await;

    let query = "WITH RECURSIVE reach(src, dst) AS (\
                     SELECT src, dst FROM tc_edges3 \
                     UNION \
                     SELECT r.src, e.dst \
                     FROM reach r \
                     JOIN tc_edges3 e ON r.dst = e.src\
                 ) SELECT src, dst FROM reach";

    db.create_dt("tc_ext_dt", query, "1m", "DIFFERENTIAL").await;

    // Initial: (1,2),(2,3),(1,3) = 3 paths
    assert_eq!(db.count("public.tc_ext_dt").await, 3);

    // Add edge 3→4 — creates new paths: (3,4),(2,4),(1,4)
    db.execute("INSERT INTO tc_edges3 (src, dst) VALUES (3, 4)")
        .await;
    db.refresh_dt("tc_ext_dt").await;

    assert_eq!(
        db.count("public.tc_ext_dt").await,
        6,
        "New edge 3→4 creates transitive paths (2,4) and (1,4)"
    );
    db.assert_dt_matches_query("public.tc_ext_dt", query).await;
}

/// Differential INSERT: add a bridge edge connecting two disconnected
/// components. Should discover all cross-component paths.
#[tokio::test]
async fn test_linear_tc_differential_bridge_components() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE tc_edges4 (id SERIAL PRIMARY KEY, src INT NOT NULL, dst INT NOT NULL)",
    )
    .await;
    // Two disconnected components: 1→2→3 and 10→20
    db.execute(
        "INSERT INTO tc_edges4 (src, dst) VALUES \
         (1, 2), (2, 3), (10, 20)",
    )
    .await;

    let query = "WITH RECURSIVE reach(src, dst) AS (\
                     SELECT src, dst FROM tc_edges4 \
                     UNION \
                     SELECT r.src, e.dst \
                     FROM reach r \
                     JOIN tc_edges4 e ON r.dst = e.src\
                 ) SELECT src, dst FROM reach";

    db.create_dt("tc_bridge_dt", query, "1m", "DIFFERENTIAL")
        .await;

    // Initial: {(1,2),(2,3),(1,3),(10,20)} = 4 paths
    assert_eq!(db.count("public.tc_bridge_dt").await, 4);

    // Bridge edge 3→10 connects the two components
    db.execute("INSERT INTO tc_edges4 (src, dst) VALUES (3, 10)")
        .await;
    db.refresh_dt("tc_bridge_dt").await;

    // New paths: (3,10),(3,20),(2,10),(2,20),(1,10),(1,20) = 6 new
    // Total: 4 + 6 = 10
    assert_eq!(
        db.count("public.tc_bridge_dt").await,
        10,
        "Bridge 3→10 connects components, creating cross-paths"
    );
    db.assert_dt_matches_query("public.tc_bridge_dt", query)
        .await;
}

/// Differential: cycle-forming edge with UNION (deduplicated).
/// UNION prevents infinite recursion even with cycles.
#[tokio::test]
async fn test_linear_tc_differential_cycle_with_union() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE tc_edges5 (id SERIAL PRIMARY KEY, src INT NOT NULL, dst INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO tc_edges5 (src, dst) VALUES \
         (1, 2), (2, 3)",
    )
    .await;

    let query = "WITH RECURSIVE reach(src, dst) AS (\
                     SELECT src, dst FROM tc_edges5 \
                     UNION \
                     SELECT r.src, e.dst \
                     FROM reach r \
                     JOIN tc_edges5 e ON r.dst = e.src\
                 ) SELECT src, dst FROM reach";

    db.create_dt("tc_cycle_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(db.count("public.tc_cycle_dt").await, 3);

    // Insert edge forming a cycle: 3→1
    db.execute("INSERT INTO tc_edges5 (src, dst) VALUES (3, 1)")
        .await;
    db.refresh_dt("tc_cycle_dt").await;

    // All pairs should be reachable now (3-node cycle):
    // (1,2),(2,3),(1,3),(3,1),(3,2),(2,1),(1,1),(2,2),(3,3) = 9
    assert_eq!(
        db.count("public.tc_cycle_dt").await,
        9,
        "Full self-loop closure in a 3-node cycle"
    );
    db.assert_dt_matches_query("public.tc_cycle_dt", query)
        .await;
}

/// DRed with transitive closure: DELETE an edge, verify paths through
/// that edge are removed.
#[tokio::test]
async fn test_linear_tc_dred_delete_edge() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE tc_edges6 (id SERIAL PRIMARY KEY, src INT NOT NULL, dst INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO tc_edges6 (src, dst) VALUES \
         (1, 2), (2, 3), (3, 4)",
    )
    .await;

    let query = "WITH RECURSIVE reach(src, dst) AS (\
                     SELECT src, dst FROM tc_edges6 \
                     UNION \
                     SELECT r.src, e.dst \
                     FROM reach r \
                     JOIN tc_edges6 e ON r.dst = e.src\
                 ) SELECT src, dst FROM reach";

    db.create_dt("tc_del_dt", query, "1m", "DIFFERENTIAL").await;

    // Initial: 6 paths
    assert_eq!(db.count("public.tc_del_dt").await, 6);

    // Delete edge 2→3 — breaks paths through that edge
    db.execute("DELETE FROM tc_edges6 WHERE src = 2 AND dst = 3")
        .await;
    db.refresh_dt("tc_del_dt").await;

    // Remaining: (1,2),(3,4) — only direct edges with no transitive paths
    assert_eq!(
        db.count("public.tc_del_dt").await,
        2,
        "Deleting 2→3 breaks transitive paths"
    );
    db.assert_dt_matches_query("public.tc_del_dt", query).await;
}

/// Multiple sequential differential refreshes building up a chain.
/// Verify result matches fresh query after each step.
#[tokio::test]
async fn test_linear_tc_sequential_refreshes() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE tc_edges7 (id SERIAL PRIMARY KEY, src INT NOT NULL, dst INT NOT NULL)",
    )
    .await;
    db.execute("INSERT INTO tc_edges7 (src, dst) VALUES (1, 2)")
        .await;

    let query = "WITH RECURSIVE reach(src, dst) AS (\
                     SELECT src, dst FROM tc_edges7 \
                     UNION \
                     SELECT r.src, e.dst \
                     FROM reach r \
                     JOIN tc_edges7 e ON r.dst = e.src\
                 ) SELECT src, dst FROM reach";

    db.create_dt("tc_seq_dt", query, "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.tc_seq_dt").await, 1);

    // Add edges one at a time
    db.execute("INSERT INTO tc_edges7 (src, dst) VALUES (2, 3)")
        .await;
    db.refresh_dt("tc_seq_dt").await;
    db.assert_dt_matches_query("public.tc_seq_dt", query).await;

    db.execute("INSERT INTO tc_edges7 (src, dst) VALUES (3, 4)")
        .await;
    db.refresh_dt("tc_seq_dt").await;
    db.assert_dt_matches_query("public.tc_seq_dt", query).await;

    db.execute("INSERT INTO tc_edges7 (src, dst) VALUES (4, 5)")
        .await;
    db.refresh_dt("tc_seq_dt").await;

    // Chain 1→2→3→4→5: C(5,2) = 10 directed reachable pairs
    assert_eq!(db.count("public.tc_seq_dt").await, 10);
    db.assert_dt_matches_query("public.tc_seq_dt", query).await;
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tier 3c — Edge Cases & Data Integrity
// ═══════════════════════════════════════════════════════════════════════════

/// Insert an orphan node (parent_id references non-existent node).
/// The orphan should NOT appear in the recursive traversal.
#[tokio::test]
async fn test_seminaive_insert_orphan_excluded() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_orphan (id INT PRIMARY KEY, parent_id INT, name TEXT)")
        .await;
    db.execute("INSERT INTO sn_orphan VALUES (1, NULL, 'root'), (2, 1, 'child')")
        .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, name FROM sn_orphan WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT o.id, o.parent_id, o.name FROM sn_orphan o \
                     JOIN t ON o.parent_id = t.id\
                 ) SELECT id, name FROM t";

    db.create_dt("sn_orphan_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.sn_orphan_dt").await, 2);

    // Insert an orphan (parent_id=999 doesn't exist) + a valid node
    db.execute("INSERT INTO sn_orphan VALUES (3, 999, 'orphan'), (4, 2, 'grandchild')")
        .await;
    db.refresh_dt("sn_orphan_dt").await;

    assert_eq!(
        db.count("public.sn_orphan_dt").await,
        3,
        "Only root + child + grandchild; orphan excluded"
    );
    db.assert_dt_matches_query("public.sn_orphan_dt", query)
        .await;
}

/// Empty refresh (no changes) should be a no-op for both code paths.
#[tokio::test]
async fn test_seminaive_empty_refresh_noop() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_noop (id INT PRIMARY KEY, parent_id INT, val TEXT)")
        .await;
    db.execute("INSERT INTO sn_noop VALUES (1, NULL, 'root'), (2, 1, 'child')")
        .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, val FROM sn_noop WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT n.id, n.parent_id, n.val FROM sn_noop n \
                     JOIN t ON n.parent_id = t.id\
                 ) SELECT id, val FROM t";

    db.create_dt("sn_noop_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.sn_noop_dt").await, 2);

    // Multiple refreshes with no changes
    db.refresh_dt("sn_noop_dt").await;
    db.refresh_dt("sn_noop_dt").await;

    assert_eq!(db.count("public.sn_noop_dt").await, 2);
    db.assert_dt_matches_query("public.sn_noop_dt", query).await;
}

/// Semi-naive then recomputation in sequence: first INSERT-only, then
/// DELETE, verifying both paths work sequentially on the same ST.
#[tokio::test]
async fn test_seminaive_then_recomp_sequence() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_seq2 (id INT PRIMARY KEY, parent_id INT, v TEXT)")
        .await;
    db.execute("INSERT INTO sn_seq2 VALUES (1, NULL, 'root')")
        .await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id, v FROM sn_seq2 WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT s.id, s.parent_id, s.v FROM sn_seq2 s \
                     JOIN t ON s.parent_id = t.id\
                 ) SELECT id, v FROM t";

    db.create_dt("sn_seq2_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_eq!(db.count("public.sn_seq2_dt").await, 1);

    // Refresh 1: INSERT-only (semi-naive path)
    db.execute("INSERT INTO sn_seq2 VALUES (2, 1, 'A'), (3, 2, 'B'), (4, 3, 'C')")
        .await;
    db.refresh_dt("sn_seq2_dt").await;
    assert_eq!(db.count("public.sn_seq2_dt").await, 4);

    // Refresh 2: DELETE (recomputation path)
    db.execute("DELETE FROM sn_seq2 WHERE id = 3").await;
    db.refresh_dt("sn_seq2_dt").await;
    assert_eq!(
        db.count("public.sn_seq2_dt").await,
        2,
        "root + A remain; B deleted and C orphaned"
    );

    // Refresh 3: INSERT-only again (back to semi-naive)
    db.execute("INSERT INTO sn_seq2 VALUES (5, 2, 'D'), (6, 5, 'E')")
        .await;
    db.refresh_dt("sn_seq2_dt").await;
    assert_eq!(db.count("public.sn_seq2_dt").await, 4, "root + A + D + E");

    db.assert_dt_matches_query("public.sn_seq2_dt", query).await;
}

/// Large batch INSERT: 500 nodes forming a balanced binary tree.
/// Exercises semi-naive propagation at scale.
#[tokio::test]
async fn test_seminaive_large_batch_binary_tree() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sn_big (id INT PRIMARY KEY, parent_id INT)")
        .await;
    // Create root
    db.execute("INSERT INTO sn_big VALUES (1, NULL)").await;

    let query = "WITH RECURSIVE t AS (\
                     SELECT id, parent_id FROM sn_big WHERE parent_id IS NULL \
                     UNION ALL \
                     SELECT b.id, b.parent_id FROM sn_big b \
                     JOIN t ON b.parent_id = t.id\
                 ) SELECT id FROM t";

    db.create_dt("sn_big_dt", query, "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.sn_big_dt").await, 1);

    // Insert nodes 2-255 as a binary tree (node N's children are 2N and 2N+1)
    // Build levels 1-7 (127 interior + 128 leaves = 255 total including root)
    db.execute(
        "INSERT INTO sn_big \
         SELECT g, g / 2 FROM generate_series(2, 255) g",
    )
    .await;
    db.refresh_dt("sn_big_dt").await;

    assert_eq!(
        db.count("public.sn_big_dt").await,
        255,
        "Full binary tree of depth 7 (255 nodes)"
    );
    db.assert_dt_matches_query("public.sn_big_dt", query).await;
}

// ═══════════════════════════════════════════════════════════════════════════
//  Subqueries in FROM (T_RangeSubselect)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_subquery_in_from_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE events (id INT PRIMARY KEY, event_type TEXT, ts INT)")
        .await;
    db.execute(
        "INSERT INTO events VALUES \
         (1, 'click', 100), (2, 'view', 200), (3, 'click', 300), (4, 'view', 400)",
    )
    .await;

    db.create_dt(
        "subq_events_dt",
        "SELECT sub.event_type, sub.cnt \
         FROM (SELECT event_type, COUNT(*) AS cnt FROM events GROUP BY event_type) sub",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.subq_events_dt").await, 2);
}

#[tokio::test]
async fn test_subquery_in_from_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE readings (id INT PRIMARY KEY, sensor TEXT, value INT)")
        .await;
    db.execute("INSERT INTO readings VALUES (1, 'temp', 20), (2, 'temp', 25), (3, 'humid', 60)")
        .await;

    let query = "SELECT s.id, s.sensor, s.value \
                 FROM (SELECT id, sensor, value FROM readings) s";

    db.create_dt("subq_inc_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(db.count("public.subq_inc_dt").await, 3);

    db.execute("INSERT INTO readings VALUES (4, 'temp', 30)")
        .await;
    db.refresh_dt("subq_inc_dt").await;

    assert_eq!(db.count("public.subq_inc_dt").await, 4);
}

#[tokio::test]
async fn test_subquery_with_filter_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE stock (id INT PRIMARY KEY, symbol TEXT, price INT)")
        .await;
    db.execute(
        "INSERT INTO stock VALUES (1, 'AAPL', 150), (2, 'GOOG', 100), \
         (3, 'MSFT', 200), (4, 'AMZN', 80)",
    )
    .await;

    let query = "SELECT s.symbol, s.price \
                 FROM (SELECT symbol, price FROM stock WHERE price > 90) s";

    db.create_dt("subq_filter_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(
        db.count("public.subq_filter_dt").await,
        3,
        "AAPL, GOOG, MSFT have price > 90"
    );

    db.execute("INSERT INTO stock VALUES (5, 'TSLA', 250)")
        .await;
    db.refresh_dt("subq_filter_dt").await;

    assert_eq!(db.count("public.subq_filter_dt").await, 4);
}

// ═══════════════════════════════════════════════════════════════════════════
//  CTE + Join Combinations
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cte_joined_with_base_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE authors (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE books (id INT PRIMARY KEY, title TEXT, author_id INT)")
        .await;
    db.execute("INSERT INTO authors VALUES (1, 'Tolkien'), (2, 'Rowling')")
        .await;
    db.execute("INSERT INTO books VALUES (1, 'Hobbit', 1), (2, 'LOTR', 1), (3, 'HP', 2)")
        .await;

    let query = "WITH prolific AS (\
                     SELECT id, name FROM authors\
                 ) SELECT p.name, b.title \
                   FROM prolific p \
                   JOIN books b ON p.id = b.author_id";

    db.create_dt("cte_join_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(db.count("public.cte_join_dt").await, 3);

    db.execute("INSERT INTO books VALUES (4, 'Silmarillion', 1)")
        .await;
    db.refresh_dt("cte_join_dt").await;

    assert_eq!(db.count("public.cte_join_dt").await, 4);
}

#[tokio::test]
async fn test_cte_left_join() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE depts (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE employees (id INT PRIMARY KEY, name TEXT, dept_id INT)")
        .await;
    db.execute("INSERT INTO depts VALUES (1, 'Eng'), (2, 'Sales'), (3, 'HR')")
        .await;
    db.execute("INSERT INTO employees VALUES (1, 'Alice', 1), (2, 'Bob', 1), (3, 'Charlie', 2)")
        .await;

    let query = "WITH d AS (SELECT id AS dept_id, name AS dept_name FROM depts) \
                 SELECT d.dept_name, e.name AS emp_name \
                 FROM d LEFT JOIN employees e ON d.dept_id = e.dept_id";

    db.create_dt("cte_left_join_dt", query, "1m", "FULL").await;

    // HR has no employees → NULL emp_name, so 4 rows total
    assert_eq!(
        db.count("public.cte_left_join_dt").await,
        4,
        "Eng(2) + Sales(1) + HR(1 null) = 4"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  CTE with DISTINCT
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cte_with_distinct() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE tags (id INT PRIMARY KEY, item_id INT, tag TEXT)")
        .await;
    db.execute(
        "INSERT INTO tags VALUES \
         (1, 1, 'red'), (2, 1, 'blue'), (3, 2, 'red'), (4, 3, 'red'), (5, 3, 'green')",
    )
    .await;

    let query = "WITH all_tags AS (SELECT tag FROM tags) \
                 SELECT DISTINCT tag FROM all_tags";

    db.create_dt("cte_distinct_dt", query, "1m", "FULL").await;

    assert_eq!(
        db.count("public.cte_distinct_dt").await,
        3,
        "Should have red, blue, green"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  CTE with UNION ALL
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cte_with_union_all_outside() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_a (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("CREATE TABLE src_b (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_a VALUES (1, 'a1'), (2, 'a2')")
        .await;
    db.execute("INSERT INTO src_b VALUES (3, 'b1'), (4, 'b2')")
        .await;

    let query = "WITH a AS (SELECT id, val FROM src_a), \
                      b AS (SELECT id, val FROM src_b) \
                 SELECT id, val FROM a \
                 UNION ALL \
                 SELECT id, val FROM b";

    db.create_dt("cte_union_dt", query, "1m", "FULL").await;

    assert_eq!(db.count("public.cte_union_dt").await, 4);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Drop & Lifecycle
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cte_dt_drop_cleans_up() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cleanup_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cleanup_src VALUES (1, 'data')")
        .await;

    db.create_dt(
        "cleanup_cte_dt",
        "WITH w AS (SELECT id, val FROM cleanup_src) SELECT id, val FROM w",
        "1m",
        "FULL",
    )
    .await;

    assert!(db.table_exists("public", "cleanup_cte_dt").await);

    db.drop_dt("cleanup_cte_dt").await;

    assert!(
        !db.table_exists("public", "cleanup_cte_dt").await,
        "Storage table should be gone after drop"
    );

    let cat_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgstream.pgs_stream_tables WHERE pgs_name = 'cleanup_cte_dt'",
        )
        .await;
    assert_eq!(cat_count, 0, "Catalog entry should be removed");
}

// ═══════════════════════════════════════════════════════════════════════════
//  Data Integrity — assert_dt_matches_query
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cte_dt_matches_defining_query_after_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE verify_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO verify_src VALUES (1, 10), (2, 20), (3, 30), (4, 40), (5, 50)")
        .await;

    let query = "WITH filtered AS (\
                     SELECT id, val FROM verify_src WHERE val >= 20\
                 ) SELECT id, val FROM filtered";

    db.create_dt("verify_cte_dt", query, "1m", "FULL").await;

    db.assert_dt_matches_query("public.verify_cte_dt", query)
        .await;

    // Make changes and refresh
    db.execute("DELETE FROM verify_src WHERE id = 2").await;
    db.execute("INSERT INTO verify_src VALUES (6, 60)").await;
    db.execute("UPDATE verify_src SET val = 5 WHERE id = 3")
        .await;

    db.refresh_dt("verify_cte_dt").await;

    db.assert_dt_matches_query("public.verify_cte_dt", query)
        .await;
}

#[tokio::test]
async fn test_cte_differential_matches_full_results() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE compare_src (id INT PRIMARY KEY, grp TEXT, amount INT)")
        .await;
    db.execute(
        "INSERT INTO compare_src VALUES \
         (1, 'X', 10), (2, 'X', 20), (3, 'Y', 30), (4, 'Y', 40)",
    )
    .await;

    let query = "WITH data AS (\
                     SELECT grp, amount FROM compare_src\
                 ) SELECT grp, SUM(amount) AS total FROM data GROUP BY grp";

    // Create as DIFFERENTIAL
    db.create_dt("compare_inc_dt", query, "1m", "DIFFERENTIAL")
        .await;

    // Mutate source data
    db.execute("INSERT INTO compare_src VALUES (5, 'X', 50), (6, 'Z', 100)")
        .await;
    db.execute("DELETE FROM compare_src WHERE id = 1").await;

    db.refresh_dt("compare_inc_dt").await;

    // The differential result should match what a fresh query would produce
    db.assert_dt_matches_query("public.compare_inc_dt", query)
        .await;
}

// ═══════════════════════════════════════════════════════════════════════════
//  Edge Cases
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cte_single_row_source() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE singleton (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO singleton VALUES (1, 'only')").await;

    db.create_dt(
        "single_cte_dt",
        "WITH w AS (SELECT id, val FROM singleton) SELECT id, val FROM w",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.single_cte_dt").await, 1);

    // Delete the only row
    db.execute("DELETE FROM singleton WHERE id = 1").await;
    db.refresh_dt("single_cte_dt").await;

    assert_eq!(
        db.count("public.single_cte_dt").await,
        0,
        "ST should be empty after deleting only row"
    );
}

#[tokio::test]
async fn test_cte_empty_source() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE empty_src (id INT PRIMARY KEY, val TEXT)")
        .await;

    db.create_dt(
        "empty_cte_dt",
        "WITH w AS (SELECT id, val FROM empty_src) SELECT id, val FROM w",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.empty_cte_dt").await, 0);

    // Insert data then refresh
    db.execute("INSERT INTO empty_src VALUES (1, 'appeared')")
        .await;
    db.refresh_dt("empty_cte_dt").await;

    assert_eq!(db.count("public.empty_cte_dt").await, 1);
}

#[tokio::test]
async fn test_cte_idempotent_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE idem_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO idem_src VALUES (1, 'a'), (2, 'b')")
        .await;

    let query = "WITH w AS (SELECT id, val FROM idem_src) SELECT id, val FROM w";

    db.create_dt("idem_cte_dt", query, "1m", "DIFFERENTIAL")
        .await;

    // Multiple refreshes with no changes should be idempotent
    db.refresh_dt("idem_cte_dt").await;
    db.refresh_dt("idem_cte_dt").await;
    db.refresh_dt("idem_cte_dt").await;

    assert_eq!(db.count("public.idem_cte_dt").await, 2);
    db.assert_dt_matches_query("public.idem_cte_dt", query)
        .await;
}

#[tokio::test]
async fn test_cte_large_batch() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE big_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO big_src SELECT g, g * 10 FROM generate_series(1, 100) g")
        .await;

    let query = "WITH all_data AS (SELECT id, val FROM big_src) \
                 SELECT id, val FROM all_data";

    db.create_dt("big_cte_dt", query, "1m", "DIFFERENTIAL")
        .await;

    assert_eq!(db.count("public.big_cte_dt").await, 100);

    // Add 1000 more rows
    db.execute("INSERT INTO big_src SELECT g, g * 10 FROM generate_series(101, 1100) g")
        .await;
    db.refresh_dt("big_cte_dt").await;

    assert_eq!(db.count("public.big_cte_dt").await, 1100);
}
