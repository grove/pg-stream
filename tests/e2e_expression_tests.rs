//! E2E tests for expression deparsing, semantic validation, and expanded SQL support.
//!
//! Tests for Priority 1-4 fixes from REPORT_SQL_GAPS.md:
//! - P0: Expression deparsing (CASE, COALESCE, NULLIF, IN, BETWEEN, etc.)
//! - P1: Semantic error detection (NATURAL JOIN, DISTINCT ON, FILTER, unknown agg)
//! - P2: Expanded support (RIGHT JOIN swap, window frames, 3-field ColumnRef)
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// Priority 1 (P0): Expression Deparsing — FULL mode
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_case_when_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE orders (id INT PRIMARY KEY, amount NUMERIC, status TEXT)")
        .await;
    db.execute(
        "INSERT INTO orders VALUES
         (1, 100, 'paid'), (2, 200, 'pending'), (3, 50, 'refunded')",
    )
    .await;

    db.create_st(
        "order_labels",
        "SELECT id, CASE WHEN amount > 150 THEN 'high' WHEN amount > 75 THEN 'medium' ELSE 'low' END AS label FROM orders",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.order_labels").await;
    assert_eq!(count, 3);

    let label: String = db
        .query_scalar("SELECT label FROM public.order_labels WHERE id = 2")
        .await;
    assert_eq!(label, "high");
}

#[tokio::test]
async fn test_simple_case_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE tickets (id INT PRIMARY KEY, priority INT)")
        .await;
    db.execute("INSERT INTO tickets VALUES (1, 1), (2, 2), (3, 3)")
        .await;

    db.create_st(
        "ticket_labels",
        "SELECT id, CASE priority WHEN 1 THEN 'urgent' WHEN 2 THEN 'normal' ELSE 'low' END AS prio_label FROM tickets",
        "1m",
        "FULL",
    )
    .await;

    let label: String = db
        .query_scalar("SELECT prio_label FROM public.ticket_labels WHERE id = 1")
        .await;
    assert_eq!(label, "urgent");
}

#[tokio::test]
async fn test_coalesce_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE contacts (id INT PRIMARY KEY, phone TEXT, email TEXT)")
        .await;
    db.execute(
        "INSERT INTO contacts VALUES (1, '555-1234', NULL), (2, NULL, 'bob@ex.com'), (3, NULL, NULL)",
    )
    .await;

    db.create_st(
        "contact_info",
        "SELECT id, COALESCE(phone, email, 'no-contact') AS best_contact FROM contacts",
        "1m",
        "FULL",
    )
    .await;

    let c1: String = db
        .query_scalar("SELECT best_contact FROM public.contact_info WHERE id = 1")
        .await;
    assert_eq!(c1, "555-1234");

    let c3: String = db
        .query_scalar("SELECT best_contact FROM public.contact_info WHERE id = 3")
        .await;
    assert_eq!(c3, "no-contact");
}

#[tokio::test]
async fn test_nullif_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE vals (id INT PRIMARY KEY, v INT)")
        .await;
    db.execute("INSERT INTO vals VALUES (1, 0), (2, 5), (3, 0)")
        .await;

    db.create_st(
        "safe_vals",
        "SELECT id, NULLIF(v, 0) AS safe_v FROM vals",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.safe_vals").await;
    assert_eq!(count, 3);

    let safe_v2: i32 = db
        .query_scalar("SELECT safe_v FROM public.safe_vals WHERE id = 2")
        .await;
    assert_eq!(safe_v2, 5);

    // NULLIF(0, 0) should be NULL
    let is_null: bool = db
        .query_scalar("SELECT safe_v IS NULL FROM public.safe_vals WHERE id = 1")
        .await;
    assert!(is_null);
}

#[tokio::test]
async fn test_greatest_least_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE scores (id INT PRIMARY KEY, a INT, b INT, c INT)")
        .await;
    db.execute("INSERT INTO scores VALUES (1, 10, 20, 5), (2, 30, 15, 25)")
        .await;

    db.create_st(
        "score_bounds",
        "SELECT id, GREATEST(a, b, c) AS max_score, LEAST(a, b, c) AS min_score FROM scores",
        "1m",
        "FULL",
    )
    .await;

    let max1: i32 = db
        .query_scalar("SELECT max_score FROM public.score_bounds WHERE id = 1")
        .await;
    assert_eq!(max1, 20);

    let min2: i32 = db
        .query_scalar("SELECT min_score FROM public.score_bounds WHERE id = 2")
        .await;
    assert_eq!(min2, 15);
}

#[tokio::test]
async fn test_in_list_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE items (id INT PRIMARY KEY, category TEXT)")
        .await;
    db.execute("INSERT INTO items VALUES (1, 'books'), (2, 'toys'), (3, 'food'), (4, 'books')")
        .await;

    db.create_st(
        "filtered_items",
        "SELECT id, category FROM items WHERE category IN ('books', 'food')",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.filtered_items").await;
    assert_eq!(count, 3); // items 1, 3, 4
}

#[tokio::test]
async fn test_between_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE products (id INT PRIMARY KEY, price NUMERIC)")
        .await;
    db.execute("INSERT INTO products VALUES (1, 10), (2, 50), (3, 100), (4, 200)")
        .await;

    db.create_st(
        "mid_priced",
        "SELECT id, price FROM products WHERE price BETWEEN 20 AND 150",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.mid_priced").await;
    assert_eq!(count, 2); // products 2, 3
}

#[tokio::test]
async fn test_is_distinct_from_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE nulltest (id INT PRIMARY KEY, a INT, b INT)")
        .await;
    db.execute("INSERT INTO nulltest VALUES (1, 1, 1), (2, 1, 2), (3, NULL, NULL), (4, 1, NULL)")
        .await;

    db.create_st(
        "distinct_check",
        "SELECT id, a IS DISTINCT FROM b AS is_diff FROM nulltest",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.distinct_check").await;
    assert_eq!(count, 4);

    let diff2: bool = db
        .query_scalar("SELECT is_diff FROM public.distinct_check WHERE id = 2")
        .await;
    assert!(diff2);

    // NULL IS DISTINCT FROM NULL → false
    let diff3: bool = db
        .query_scalar("SELECT is_diff FROM public.distinct_check WHERE id = 3")
        .await;
    assert!(!diff3);
}

#[tokio::test]
async fn test_boolean_test_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE flags (id INT PRIMARY KEY, active BOOLEAN)")
        .await;
    db.execute("INSERT INTO flags VALUES (1, true), (2, false), (3, NULL)")
        .await;

    db.create_st(
        "active_items",
        "SELECT id FROM flags WHERE active IS TRUE",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.active_items").await;
    assert_eq!(count, 1);

    let active_id: i32 = db.query_scalar("SELECT id FROM public.active_items").await;
    assert_eq!(active_id, 1);
}

#[tokio::test]
async fn test_sql_value_function_current_date_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE events (id INT PRIMARY KEY, event_date DATE)")
        .await;
    db.execute("INSERT INTO events VALUES (1, CURRENT_DATE), (2, CURRENT_DATE - 1)")
        .await;

    db.create_st(
        "recent_events",
        "SELECT id, event_date FROM events WHERE event_date >= CURRENT_DATE",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.recent_events").await;
    assert!(count >= 1);
}

#[tokio::test]
async fn test_array_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE data (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO data VALUES (1, 10), (2, 20), (3, 30)")
        .await;

    db.create_st(
        "array_test",
        "SELECT id, ARRAY[val, val * 2] AS doubled FROM data",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.array_test").await;
    assert_eq!(count, 3);
}

// ═══════════════════════════════════════════════════════════════════════
// Priority 1 (P0): Expression Deparsing — DIFFERENTIAL mode
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_case_when_in_select_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE emp (id INT PRIMARY KEY, salary NUMERIC, dept TEXT)")
        .await;
    db.execute("INSERT INTO emp VALUES (1, 50000, 'eng'), (2, 80000, 'eng'), (3, 60000, 'sales')")
        .await;

    // CASE in WHERE clause with GROUP BY — DIFFERENTIAL mode
    db.create_st(
        "dept_summary",
        "SELECT dept, COUNT(*) AS cnt, SUM(salary) AS total FROM emp WHERE CASE WHEN salary > 70000 THEN TRUE ELSE FALSE END GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let count = db.count("public.dept_summary").await;
    assert_eq!(count, 1); // only 1 employee has salary > 70000 (id=2, dept=eng)

    let total: i64 = db
        .query_scalar("SELECT total::bigint FROM public.dept_summary WHERE dept = 'eng'")
        .await;
    assert_eq!(total, 80000);
}

#[tokio::test]
async fn test_coalesce_in_select_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE orders2 (id INT PRIMARY KEY, customer_id INT, discount NUMERIC)")
        .await;
    db.execute("INSERT INTO orders2 VALUES (1, 1, 10), (2, 1, NULL), (3, 2, 5), (4, 2, NULL)")
        .await;

    db.create_st(
        "customer_discounts",
        "SELECT customer_id, SUM(COALESCE(discount, 0)) AS total_discount FROM orders2 GROUP BY customer_id",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let d1: i64 = db
        .query_scalar(
            "SELECT total_discount::bigint FROM public.customer_discounts WHERE customer_id = 1",
        )
        .await;
    assert_eq!(d1, 10);

    let d2: i64 = db
        .query_scalar(
            "SELECT total_discount::bigint FROM public.customer_discounts WHERE customer_id = 2",
        )
        .await;
    assert_eq!(d2, 5);
}

// ═══════════════════════════════════════════════════════════════════════
// Priority 2 (P1): Semantic Error Detection
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_natural_join_rejected_with_clear_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE t1 (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("CREATE TABLE t2 (id INT PRIMARY KEY, score INT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('nat_join_st', \
             $$ SELECT t1.id, t1.val, t2.score FROM t1 NATURAL JOIN t2 $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "NATURAL JOIN should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("NATURAL JOIN"),
        "Error should mention NATURAL JOIN, got: {err}"
    );
}

#[tokio::test]
async fn test_distinct_on_rejected_with_clear_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE logs (id INT PRIMARY KEY, category TEXT, ts TIMESTAMPTZ)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('distinct_on_st', \
             $$ SELECT DISTINCT ON (category) id, category, ts FROM logs ORDER BY category, ts DESC $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "DISTINCT ON should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("DISTINCT ON"),
        "Error should mention DISTINCT ON, got: {err}"
    );
}

#[tokio::test]
async fn test_stddev_aggregate_supported_in_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE metrics (id INT PRIMARY KEY, val NUMERIC, grp TEXT)")
        .await;
    db.execute("INSERT INTO metrics VALUES (1, 10, 'a'), (2, 20, 'a'), (3, 30, 'b')")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('stddev_st', \
             $$ SELECT grp, STDDEV(val) AS std FROM metrics GROUP BY grp $$, '1m', 'DIFFERENTIAL')",
        )
        .await;
    assert!(
        result.is_ok(),
        "STDDEV should now be supported in DIFFERENTIAL mode, got: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_filter_clause_supported() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sales2 (id INT PRIMARY KEY, amount INT, region TEXT)")
        .await;
    db.execute("INSERT INTO sales2 VALUES (1, 200, 'east'), (2, 50, 'east'), (3, 300, 'west')")
        .await;

    db.create_st(
        "filter_st",
        "SELECT region, COUNT(*) FILTER (WHERE amount > 100) AS big_count FROM sales2 GROUP BY region",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let count: i64 = db
        .query_scalar("SELECT big_count FROM public.filter_st WHERE region = 'east'")
        .await;
    // Only 1 row (amount=200) passes the filter for 'east'
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_exists_subquery_in_where() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE parent_tbl (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE child_tbl (id INT PRIMARY KEY, parent_id INT)")
        .await;

    // EXISTS subquery should now be supported via SemiJoin
    db.create_st(
        "exists_st",
        "SELECT id, name FROM parent_tbl WHERE EXISTS (SELECT 1 FROM child_tbl WHERE child_tbl.parent_id = parent_tbl.id)",
        "1m",
        "FULL",
    )
    .await;

    // Insert data
    db.execute("INSERT INTO parent_tbl VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Charlie')")
        .await;
    db.execute("INSERT INTO child_tbl VALUES (1, 1), (2, 1), (3, 3)")
        .await;

    // Refresh
    db.execute("SELECT pgstream.refresh_stream_table('exists_st')")
        .await;

    // Only parents with children should appear (1 and 3)
    let count: i64 = db
        .query_scalar("SELECT count(*) FROM public.exists_st")
        .await;
    assert_eq!(count, 2, "Only parents with children should appear");
}

// ═══════════════════════════════════════════════════════════════════════
// Priority 3 (P2): Expanded Support
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_right_join_converted_to_left_join() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE departments (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("INSERT INTO departments VALUES (1, 'eng'), (2, 'sales'), (3, 'hr')")
        .await;
    db.execute("CREATE TABLE employees (id INT PRIMARY KEY, name TEXT, dept_id INT)")
        .await;
    db.execute("INSERT INTO employees VALUES (1, 'Alice', 1), (2, 'Bob', 1), (3, 'Charlie', 2)")
        .await;

    // RIGHT JOIN should be silently converted to LEFT JOIN with swapped operands
    db.create_st(
        "dept_employees",
        "SELECT d.id AS dept_id, d.name AS dept_name, e.name AS emp_name \
         FROM employees e RIGHT JOIN departments d ON e.dept_id = d.id",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.dept_employees").await;
    assert_eq!(count, 4); // eng(Alice,Bob), sales(Charlie), hr(NULL)

    // HR department should show up with NULL employee
    let hr_emp: Option<String> = db
        .query_scalar("SELECT emp_name FROM public.dept_employees WHERE dept_name = 'hr'")
        .await;
    assert!(hr_emp.is_none(), "HR dept should have NULL employee");
}

#[tokio::test]
async fn test_window_frame_rows_between() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE timeseries (id INT PRIMARY KEY, ts DATE, val NUMERIC)")
        .await;
    db.execute(
        "INSERT INTO timeseries VALUES \
         (1, '2024-01-01', 10), (2, '2024-01-02', 20), \
         (3, '2024-01-03', 30), (4, '2024-01-04', 40), (5, '2024-01-05', 50)",
    )
    .await;

    // Window function with explicit frame clause
    db.create_st(
        "running_avg",
        "SELECT id, ts, val, AVG(val) OVER (ORDER BY ts ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS moving_avg FROM timeseries",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.running_avg").await;
    assert_eq!(count, 5);
}

#[tokio::test]
async fn test_three_field_column_ref() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE schema_test (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO schema_test VALUES (1, 'hello'), (2, 'world')")
        .await;

    // 3-field column reference: public.schema_test.id
    db.create_st(
        "schema_ref_st",
        "SELECT public.schema_test.id, public.schema_test.val FROM schema_test",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.schema_ref_st").await;
    assert_eq!(count, 2);
}

// ═══════════════════════════════════════════════════════════════════════
// Combined Expression Tests (multiple expression types in one query)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_combined_case_coalesce_between() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE transactions (id INT PRIMARY KEY, amount NUMERIC, note TEXT)")
        .await;
    db.execute(
        "INSERT INTO transactions VALUES \
         (1, 100, NULL), (2, 250, 'big'), (3, 50, NULL), (4, 500, 'huge')",
    )
    .await;

    db.create_st(
        "txn_summary",
        "SELECT id, \
         CASE WHEN amount > 200 THEN 'high' ELSE 'low' END AS tier, \
         COALESCE(note, 'no-note') AS description \
         FROM transactions WHERE amount BETWEEN 50 AND 300",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.txn_summary").await;
    assert_eq!(count, 3); // ids 1, 2, 3

    let tier2: String = db
        .query_scalar("SELECT tier FROM public.txn_summary WHERE id = 2")
        .await;
    assert_eq!(tier2, "high");

    let desc1: String = db
        .query_scalar("SELECT description FROM public.txn_summary WHERE id = 1")
        .await;
    assert_eq!(desc1, "no-note");
}

#[tokio::test]
async fn test_not_between_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE nums (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO nums VALUES (1, 5), (2, 50), (3, 100), (4, 200)")
        .await;

    db.create_st(
        "excluded_range",
        "SELECT id, val FROM nums WHERE val NOT BETWEEN 10 AND 150",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.excluded_range").await;
    assert_eq!(count, 2); // ids 1 (5) and 4 (200)
}

#[tokio::test]
async fn test_not_in_expression_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE colors (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("INSERT INTO colors VALUES (1, 'red'), (2, 'blue'), (3, 'green'), (4, 'yellow')")
        .await;

    db.create_st(
        "non_primary_colors",
        "SELECT id, name FROM colors WHERE name NOT IN ('red', 'blue', 'yellow')",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.non_primary_colors").await;
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_is_not_true_and_is_unknown() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE bool_data (id INT PRIMARY KEY, flag BOOLEAN)")
        .await;
    db.execute("INSERT INTO bool_data VALUES (1, TRUE), (2, FALSE), (3, NULL)")
        .await;

    db.create_st(
        "not_true_items",
        "SELECT id FROM bool_data WHERE flag IS NOT TRUE",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.not_true_items").await;
    assert_eq!(count, 2); // FALSE and NULL
}

// ═══════════════════════════════════════════════════════════════════════
// Differential mode with new expressions — incremental maintenance
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_between_filter_differential_with_inserts() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sensor (id INT PRIMARY KEY, reading NUMERIC)")
        .await;
    db.execute("INSERT INTO sensor VALUES (1, 50), (2, 75)")
        .await;

    db.create_st(
        "sensor_in_range",
        "SELECT id, reading FROM sensor WHERE reading BETWEEN 40 AND 80",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let count = db.count("public.sensor_in_range").await;
    assert_eq!(count, 2);

    // Insert new data and refresh
    db.execute("INSERT INTO sensor VALUES (3, 90), (4, 60)")
        .await;
    db.execute("SELECT pgstream.refresh_stream_table('sensor_in_range')")
        .await;

    let count = db.count("public.sensor_in_range").await;
    assert_eq!(count, 3); // 50, 75, 60 are in range; 90 is out
}

#[tokio::test]
async fn test_in_list_filter_differential_with_inserts() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE items2 (id INT PRIMARY KEY, cat TEXT)")
        .await;
    db.execute("INSERT INTO items2 VALUES (1, 'A'), (2, 'B')")
        .await;

    db.create_st(
        "cat_filter_st",
        "SELECT id, cat FROM items2 WHERE cat IN ('A', 'C')",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let count = db.count("public.cat_filter_st").await;
    assert_eq!(count, 1);

    db.execute("INSERT INTO items2 VALUES (3, 'C'), (4, 'D')")
        .await;
    db.execute("SELECT pgstream.refresh_stream_table('cat_filter_st')")
        .await;

    let count = db.count("public.cat_filter_st").await;
    assert_eq!(count, 2); // A and C
}

// ═══════════════════════════════════════════════════════════════════════
// DISTINCT (without ON) should still work
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_plain_distinct_still_works() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dup_data (id INT PRIMARY KEY, category TEXT)")
        .await;
    db.execute("INSERT INTO dup_data VALUES (1, 'A'), (2, 'B'), (3, 'A'), (4, 'C'), (5, 'B')")
        .await;

    // Plain DISTINCT (not DISTINCT ON) should still be accepted
    db.create_st(
        "unique_cats",
        "SELECT DISTINCT category FROM dup_data",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.unique_cats").await;
    assert_eq!(count, 3); // A, B, C
}

// ═══════════════════════════════════════════════════════════════════════
// Unsupported aggregate works in FULL mode
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_unsupported_aggregate_works_in_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE numbers (id INT PRIMARY KEY, val NUMERIC, grp TEXT)")
        .await;
    db.execute("INSERT INTO numbers VALUES (1, 10, 'a'), (2, 20, 'a'), (3, 30, 'b'), (4, 40, 'b')")
        .await;

    // string_agg is a recognized but unsupported aggregate — should work in FULL mode
    db.create_st(
        "string_concat",
        "SELECT grp, STRING_AGG(val::text, ', ' ORDER BY val) AS vals FROM numbers GROUP BY grp",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.string_concat").await;
    assert_eq!(count, 2);
}
