//! E2E tests for window function support in stream tables.
//!
//! Tests ROW_NUMBER(), RANK(), DENSE_RANK(), SUM() OVER(), etc.
//! with both FULL and DIFFERENTIAL refresh modes.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── FULL refresh with window functions ────────────────────────────────

#[tokio::test]
async fn test_window_row_number_full_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_rn (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_rn (dept, salary) VALUES
         ('eng', 100), ('eng', 80), ('sales', 90), ('sales', 70)",
    )
    .await;

    db.create_st(
        "wf_rn_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_rn",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.wf_rn_st").await, 4);

    // Verify correctness: in eng partition, salary=100 gets rn=1
    let rn: i64 = db
        .query_scalar("SELECT rn FROM public.wf_rn_st WHERE dept = 'eng' AND salary = 100")
        .await;
    assert_eq!(rn, 1);

    // In sales partition, salary=90 gets rn=1
    let rn: i64 = db
        .query_scalar("SELECT rn FROM public.wf_rn_st WHERE dept = 'sales' AND salary = 90")
        .await;
    assert_eq!(rn, 1);
}

#[tokio::test]
async fn test_window_sum_over_full_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_sum (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_sum (dept, salary) VALUES
         ('eng', 100), ('eng', 80), ('sales', 90)",
    )
    .await;

    db.create_st(
        "wf_sum_st",
        "SELECT dept, salary, SUM(salary) OVER (PARTITION BY dept) AS dept_total FROM wf_sum",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.wf_sum_st").await, 3);

    // eng total = 100 + 80 = 180
    let total: i64 = db
        .query_scalar("SELECT dept_total FROM public.wf_sum_st WHERE dept = 'eng' LIMIT 1")
        .await;
    assert_eq!(total, 180);

    // sales total = 90
    let total: i64 = db
        .query_scalar("SELECT dept_total FROM public.wf_sum_st WHERE dept = 'sales' LIMIT 1")
        .await;
    assert_eq!(total, 90);
}

#[tokio::test]
async fn test_window_rank_full_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_rank (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_rank (dept, salary) VALUES
         ('eng', 100), ('eng', 100), ('eng', 80)",
    )
    .await;

    db.create_st(
        "wf_rank_st",
        "SELECT dept, salary, RANK() OVER (PARTITION BY dept ORDER BY salary DESC) AS rnk FROM wf_rank",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.wf_rank_st").await, 3);

    // Two rows with salary=100 get rank=1
    let cnt: i64 = db
        .query_scalar("SELECT count(*) FROM public.wf_rank_st WHERE rnk = 1")
        .await;
    assert_eq!(cnt, 2);

    // Row with salary=80 gets rank=3 (not 2, because RANK skips)
    let rnk: i64 = db
        .query_scalar("SELECT rnk FROM public.wf_rank_st WHERE salary = 80")
        .await;
    assert_eq!(rnk, 3);
}

// ── FULL refresh with DML + refresh ──────────────────────────────────

#[tokio::test]
async fn test_window_full_refresh_after_insert() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_fi (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute("INSERT INTO wf_fi (dept, salary) VALUES ('eng', 100), ('eng', 80)")
        .await;

    db.create_st(
        "wf_fi_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_fi",
        "1m",
        "FULL",
    )
    .await;
    assert_eq!(db.count("public.wf_fi_st").await, 2);

    // Insert a new highest salary
    db.execute("INSERT INTO wf_fi (dept, salary) VALUES ('eng', 120)")
        .await;
    db.refresh_st("wf_fi_st").await;

    assert_eq!(db.count("public.wf_fi_st").await, 3);

    // New row should be rn=1
    let rn: i64 = db
        .query_scalar("SELECT rn FROM public.wf_fi_st WHERE salary = 120")
        .await;
    assert_eq!(rn, 1);

    // Old top row should be rn=2 now
    let rn: i64 = db
        .query_scalar("SELECT rn FROM public.wf_fi_st WHERE salary = 100")
        .await;
    assert_eq!(rn, 2);
}

#[tokio::test]
async fn test_window_full_refresh_after_delete() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_fd (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_fd (dept, salary) VALUES
         ('eng', 100), ('eng', 80), ('eng', 60)",
    )
    .await;

    db.create_st(
        "wf_fd_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_fd",
        "1m",
        "FULL",
    )
    .await;

    // Delete top salary
    db.execute("DELETE FROM wf_fd WHERE salary = 100").await;
    db.refresh_st("wf_fd_st").await;

    assert_eq!(db.count("public.wf_fd_st").await, 2);

    // salary=80 should now be rn=1
    let rn: i64 = db
        .query_scalar("SELECT rn FROM public.wf_fd_st WHERE salary = 80")
        .await;
    assert_eq!(rn, 1);
}

// ── DIFFERENTIAL refresh with window functions ────────────────────────

#[tokio::test]
async fn test_window_differential_insert() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_ii (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_ii (dept, salary) VALUES
         ('eng', 100), ('eng', 80), ('sales', 90)",
    )
    .await;

    db.create_st(
        "wf_ii_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_ii",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.wf_ii_st").await, 3);

    // Insert into eng department — should trigger recomputation of eng partition
    db.execute("INSERT INTO wf_ii (dept, salary) VALUES ('eng', 95)")
        .await;
    db.refresh_st("wf_ii_st").await;

    assert_eq!(db.count("public.wf_ii_st").await, 4);

    // Verify eng partition is correctly recomputed
    let rn_100: i64 = db
        .query_scalar("SELECT rn FROM public.wf_ii_st WHERE dept = 'eng' AND salary = 100")
        .await;
    assert_eq!(rn_100, 1, "salary=100 should be rn=1");

    let rn_95: i64 = db
        .query_scalar("SELECT rn FROM public.wf_ii_st WHERE dept = 'eng' AND salary = 95")
        .await;
    assert_eq!(rn_95, 2, "salary=95 should be rn=2");

    let rn_80: i64 = db
        .query_scalar("SELECT rn FROM public.wf_ii_st WHERE dept = 'eng' AND salary = 80")
        .await;
    assert_eq!(rn_80, 3, "salary=80 should be rn=3");

    // Sales partition should be unchanged
    let rn_sales: i64 = db
        .query_scalar("SELECT rn FROM public.wf_ii_st WHERE dept = 'sales' AND salary = 90")
        .await;
    assert_eq!(rn_sales, 1, "sales partition should be unaffected");
}

#[tokio::test]
async fn test_window_differential_delete() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_id (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_id (dept, salary) VALUES
         ('eng', 100), ('eng', 80), ('eng', 60), ('sales', 90)",
    )
    .await;

    db.create_st(
        "wf_id_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_id",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.wf_id_st").await, 4);

    // Delete middle row from eng partition
    db.execute("DELETE FROM wf_id WHERE dept = 'eng' AND salary = 80")
        .await;
    db.refresh_st("wf_id_st").await;

    assert_eq!(db.count("public.wf_id_st").await, 3);

    // salary=60 should now be rn=2 (was rn=3)
    let rn_60: i64 = db
        .query_scalar("SELECT rn FROM public.wf_id_st WHERE dept = 'eng' AND salary = 60")
        .await;
    assert_eq!(rn_60, 2, "salary=60 should be rn=2 after delete");
}

#[tokio::test]
async fn test_window_differential_update() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_iu (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_iu (dept, salary) VALUES
         ('eng', 100), ('eng', 80), ('eng', 60)",
    )
    .await;

    db.create_st(
        "wf_iu_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_iu",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Update salary=60 to 110, making it the new top
    db.execute("UPDATE wf_iu SET salary = 110 WHERE salary = 60")
        .await;
    db.refresh_st("wf_iu_st").await;

    let rn_110: i64 = db
        .query_scalar("SELECT rn FROM public.wf_iu_st WHERE salary = 110")
        .await;
    assert_eq!(rn_110, 1, "salary=110 should be rn=1 after update");

    let rn_100: i64 = db
        .query_scalar("SELECT rn FROM public.wf_iu_st WHERE salary = 100")
        .await;
    assert_eq!(rn_100, 2, "salary=100 should be rn=2 after update");
}

#[tokio::test]
async fn test_window_differential_multiple_partitions_changed() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_mp (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_mp (dept, salary) VALUES
         ('eng', 100), ('eng', 80),
         ('sales', 90), ('sales', 70)",
    )
    .await;

    db.create_st(
        "wf_mp_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_mp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Insert into both partitions
    db.execute("INSERT INTO wf_mp (dept, salary) VALUES ('eng', 95), ('sales', 85)")
        .await;
    db.refresh_st("wf_mp_st").await;

    assert_eq!(db.count("public.wf_mp_st").await, 6);

    // Verify both partitions recomputed
    db.assert_st_matches_query(
        "public.wf_mp_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_mp",
    )
    .await;
}

#[tokio::test]
async fn test_window_differential_sum_over() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_is (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_is (dept, salary) VALUES
         ('eng', 100), ('eng', 80), ('sales', 90)",
    )
    .await;

    db.create_st(
        "wf_is_st",
        "SELECT dept, salary, SUM(salary) OVER (PARTITION BY dept) AS dept_total FROM wf_is",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Insert new eng row
    db.execute("INSERT INTO wf_is (dept, salary) VALUES ('eng', 50)")
        .await;
    db.refresh_st("wf_is_st").await;

    // eng total should now be 100+80+50=230
    let total: i64 = db
        .query_scalar("SELECT dept_total FROM public.wf_is_st WHERE dept = 'eng' LIMIT 1")
        .await;
    assert_eq!(total, 230);

    // sales should be unchanged at 90
    let total: i64 = db
        .query_scalar("SELECT dept_total FROM public.wf_is_st WHERE dept = 'sales' LIMIT 1")
        .await;
    assert_eq!(total, 90);
}

// ── Window function with filter ──────────────────────────────────────

#[tokio::test]
async fn test_window_with_where_clause() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_wh (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL, active BOOL NOT NULL DEFAULT TRUE)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_wh (dept, salary, active) VALUES
         ('eng', 100, true), ('eng', 80, false), ('eng', 60, true), ('sales', 90, true)",
    )
    .await;

    db.create_st(
        "wf_wh_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_wh WHERE active = true",
        "1m",
        "FULL",
    )
    .await;

    // eng: only 100 and 60 are active → rn 1 and 2
    assert_eq!(db.count("public.wf_wh_st").await, 3);

    let rn: i64 = db
        .query_scalar("SELECT rn FROM public.wf_wh_st WHERE dept = 'eng' AND salary = 100")
        .await;
    assert_eq!(rn, 1);

    let rn: i64 = db
        .query_scalar("SELECT rn FROM public.wf_wh_st WHERE dept = 'eng' AND salary = 60")
        .await;
    assert_eq!(rn, 2);
}

// ── DENSE_RANK ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_window_dense_rank() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_dr (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO wf_dr (dept, salary) VALUES
         ('eng', 100), ('eng', 100), ('eng', 80)",
    )
    .await;

    db.create_st(
        "wf_dr_st",
        "SELECT dept, salary, DENSE_RANK() OVER (PARTITION BY dept ORDER BY salary DESC) AS drnk FROM wf_dr",
        "1m",
        "FULL",
    )
    .await;

    // salary=80 should get dense_rank=2 (not 3 like RANK)
    let drnk: i64 = db
        .query_scalar("SELECT drnk FROM public.wf_dr_st WHERE salary = 80")
        .await;
    assert_eq!(drnk, 2);
}

// ── Nested window function detection (Gap 7.4) ──────────────────────
// Note: nested window functions are rejected in DIFFERENTIAL mode because
// the DVM parser cannot extract them for incremental maintenance.
// FULL mode allows them since it just re-runs the query via PostgreSQL.

#[tokio::test]
async fn test_window_in_case_expression_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_nested (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;

    // Window function inside CASE should be rejected in DIFFERENTIAL mode
    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('wf_nested_st', \
             $$ SELECT CASE WHEN ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) <= 3 \
             THEN 'top' ELSE 'other' END AS tier FROM wf_nested $$, '1m', 'DIFFERENTIAL')",
        )
        .await;

    assert!(
        result.is_err(),
        "Nested window function in CASE should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Window functions nested inside expressions"),
        "Error should mention nested window functions, got: {err}"
    );
}

#[tokio::test]
async fn test_window_in_coalesce_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE wf_coal (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, val INT)")
        .await;

    // Window function inside COALESCE should be rejected in DIFFERENTIAL mode
    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('wf_coal_st', \
             $$ SELECT COALESCE(SUM(val) OVER (PARTITION BY dept), 0) AS total FROM wf_coal $$, '1m', 'DIFFERENTIAL')",
        )
        .await;

    assert!(
        result.is_err(),
        "Nested window function in COALESCE should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Window functions nested inside expressions"),
        "Error should mention nested window functions, got: {err}"
    );
}

#[tokio::test]
async fn test_window_in_arithmetic_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_arith (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;

    // Window function inside arithmetic should be rejected in DIFFERENTIAL mode
    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('wf_arith_st', \
             $$ SELECT ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) * 10 AS scaled_rank \
             FROM wf_arith $$, '1m', 'DIFFERENTIAL')",
        )
        .await;

    assert!(
        result.is_err(),
        "Nested window function in arithmetic should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Window functions nested inside expressions"),
        "Error should mention nested window functions, got: {err}"
    );
}

#[tokio::test]
async fn test_window_in_cast_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_cast (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;

    // Window function inside CAST should be rejected in DIFFERENTIAL mode
    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('wf_cast_st', \
             $$ SELECT CAST(ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS TEXT) AS rn_text \
             FROM wf_cast $$, '1m', 'DIFFERENTIAL')",
        )
        .await;

    assert!(
        result.is_err(),
        "Nested window function in CAST should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Window functions nested inside expressions"),
        "Error should mention nested window functions, got: {err}"
    );
}

#[tokio::test]
async fn test_window_deeply_nested_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_deep (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;

    // Window function deeply nested: CASE → COALESCE → window (DIFFERENTIAL mode)
    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('wf_deep_st', \
             $$ SELECT CASE WHEN COALESCE(ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC), 0) <= 3 \
             THEN 'top' ELSE 'other' END AS tier FROM wf_deep $$, '1m', 'DIFFERENTIAL')"
        )
        .await;

    assert!(
        result.is_err(),
        "Deeply nested window function should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Window functions nested inside expressions"),
        "Error should mention nested window functions, got: {err}"
    );
}

#[tokio::test]
async fn test_top_level_window_still_works() {
    // Regression: top-level window functions should still work fine
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_ok (id SERIAL PRIMARY KEY, dept TEXT NOT NULL, salary INT NOT NULL)",
    )
    .await;
    db.execute("INSERT INTO wf_ok (dept, salary) VALUES ('eng', 100), ('eng', 80), ('hr', 90)")
        .await;

    db.create_st(
        "wf_ok_st",
        "SELECT dept, salary, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM wf_ok",
        "1m",
        "FULL",
    )
    .await;

    let rn: i64 = db
        .query_scalar("SELECT rn FROM public.wf_ok_st WHERE dept = 'eng' AND salary = 100")
        .await;
    assert_eq!(rn, 1);
}
