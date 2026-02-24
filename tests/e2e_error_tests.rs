//! E2E tests for error handling and edge cases.
//!
//! Validates the extension rejects invalid inputs gracefully and
//! handles edge cases like subqueries, CTEs, and DDL on source tables.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Invalid SQL ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_invalid_sql_in_defining_query() {
    let db = E2eDb::new().await.with_extension().await;

    // Typo: FORM instead of FROM
    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('bad_sql_st', \
             $$ SELECT * FORM orders $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "Typo in SQL should produce a clear error");
}

#[tokio::test]
async fn test_self_referencing_query_fails() {
    let db = E2eDb::new().await.with_extension().await;

    // Self-referencing: the ST in its own defining query
    // Note: the table doesn't exist yet when create is called, so this should fail
    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('self_ref_st', \
             $$ SELECT * FROM self_ref_st $$, '1m', 'FULL')",
        )
        .await;
    assert!(
        result.is_err(),
        "Self-referencing defining query should fail"
    );
}

// ── Valid Complex Queries ──────────────────────────────────────────────

#[tokio::test]
async fn test_subquery_in_defining_query() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sub_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO sub_src VALUES (1, 10), (2, 20), (3, 30)")
        .await;

    // Subqueries in FROM are supported since CTE Tier 1 implementation.
    // FULL mode should work — the raw SQL is valid PostgreSQL.
    db.create_st(
        "subq_st",
        "SELECT * FROM (SELECT id, val FROM sub_src WHERE val > 10) sub",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.subq_st").await;
    assert_eq!(count, 2, "Subquery should return rows where val > 10");
}

#[tokio::test]
async fn test_cte_with_window_function_full_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cte_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO cte_src VALUES (1, 100), (2, 200), (3, 300)")
        .await;

    // CTEs are now supported. Window functions (ROW_NUMBER) are valid SQL
    // so FULL mode should work — the raw query is executed as-is.
    db.create_st(
        "cte_st",
        "WITH ranked AS (SELECT id, val, ROW_NUMBER() OVER (ORDER BY val DESC) AS rn FROM cte_src) SELECT id, val FROM ranked WHERE rn <= 2",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.cte_st").await;
    assert_eq!(count, 2, "Should return top 2 rows by val DESC");
}

// ── Schedule Edge Cases ──────────────────────────────────────────────

#[tokio::test]
async fn test_create_with_zero_schedule() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE zero_sched_src (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO zero_sched_src VALUES (1)").await;

    // Zero schedule — might succeed or be rejected, depending on implementation
    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('zero_sched_st', \
             $$ SELECT id FROM zero_sched_src $$, '0s', 'FULL')",
        )
        .await;

    // Either it succeeds with zero stored, or it's rejected
    // If it succeeds, verify it's stored correctly
    if result.is_ok() {
        let sched: String = db
            .query_scalar(
                "SELECT schedule FROM pgstream.pgs_stream_tables WHERE pgs_name = 'zero_sched_st'",
            )
            .await;
        assert_eq!(sched, "0s");
    }
    // If it fails, that's also acceptable behavior
}

#[tokio::test]
async fn test_create_with_negative_schedule() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE neg_sched_src (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO neg_sched_src VALUES (1)").await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('neg_sched_st', \
             $$ SELECT id FROM neg_sched_src $$, '-1m', 'FULL')",
        )
        .await;

    // Negative durations should be rejected by the parser
    // '-1m' has a '-' which is not a valid digit or unit
    assert!(result.is_err(), "Negative schedule should be rejected");
}

// ── Source Table DDL ───────────────────────────────────────────────────

#[tokio::test]
async fn test_drop_source_table_with_active_st() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE drop_me_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO drop_me_src VALUES (1, 'data')")
        .await;

    db.create_st("orphan_st", "SELECT id, val FROM drop_me_src", "1m", "FULL")
        .await;

    // Drop the source table while ST exists
    // The event trigger should handle this (or CASCADE might clean up)
    let result = db.try_execute("DROP TABLE drop_me_src CASCADE").await;

    // Whether this succeeds or not depends on event trigger behavior.
    // If it succeeds, the ST might be invalidated or auto-dropped.
    // If it fails, the extension prevents orphaning.
    if result.is_ok() {
        // Check if ST was cleaned up or marked for reinit
        let st_exists: bool = db
            .query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pgstream.pgs_stream_tables WHERE pgs_name = 'orphan_st')",
            )
            .await;
        // Either the ST was cleaned up, or it may still exist in error state
        if st_exists {
            let (status, _, _, _) = db.pgs_status("orphan_st").await;
            // The ST should be in some error/invalid state
            assert!(
                status == "ERROR" || status == "SUSPENDED" || status == "ACTIVE",
                "ST should be in a recognizable state after source drop: {}",
                status
            );
        }
    }
}

#[tokio::test]
async fn test_alter_source_table_add_column() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE alter_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO alter_src VALUES (1, 'hello')")
        .await;

    db.create_st(
        "alter_src_st",
        "SELECT id, val FROM alter_src",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.alter_src_st").await, 1);

    // Add a column to the source table
    db.execute("ALTER TABLE alter_src ADD COLUMN new_col INT DEFAULT 42")
        .await;

    // Insert a row with the new column
    db.execute("INSERT INTO alter_src VALUES (2, 'world', 99)")
        .await;

    // Refresh should still work (defining query only selects id, val)
    db.refresh_st("alter_src_st").await;

    assert_eq!(db.count("public.alter_src_st").await, 2);
}

// ── ORDER BY / LIMIT / OFFSET ──────────────────────────────────────────

#[tokio::test]
async fn test_order_by_without_limit_is_accepted() {
    // ORDER BY is silently ignored for stream tables (like CREATE MATERIALIZED VIEW)
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE orderby_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO orderby_src VALUES (1, 'a'), (2, 'b')")
        .await;

    // Should succeed — ORDER BY is accepted and discarded
    db.create_st(
        "orderby_st",
        "SELECT id, val FROM orderby_src ORDER BY id",
        "1m",
        "FULL",
    )
    .await;

    let count = db.count("public.orderby_st").await;
    assert_eq!(count, 2, "ORDER BY query should create ST with all rows");
}

#[tokio::test]
async fn test_limit_returns_unsupported_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE limit_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO limit_src SELECT g, g FROM generate_series(1, 10) g")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('limit_st', \
             $$ SELECT id, val FROM limit_src LIMIT 5 $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "LIMIT should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("LIMIT"),
        "Error should mention LIMIT, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_offset_returns_unsupported_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE offset_src (id INT PRIMARY KEY, val INT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('offset_st', \
             $$ SELECT id, val FROM offset_src OFFSET 5 $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "OFFSET should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("OFFSET"),
        "Error should mention OFFSET, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_order_by_with_limit_returns_unsupported_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE orderlimit_src (id INT PRIMARY KEY, val INT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('orderlimit_st', \
             $$ SELECT id, val FROM orderlimit_src ORDER BY id LIMIT 10 $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "ORDER BY + LIMIT should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("LIMIT"),
        "Error should mention LIMIT, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_limit_offset_combined_returns_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE limoff_src (id INT PRIMARY KEY, val INT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('limoff_st', \
             $$ SELECT id, val FROM limoff_src LIMIT 10 OFFSET 5 $$, '1m', 'FULL')",
        )
        .await;
    assert!(
        result.is_err(),
        "LIMIT + OFFSET together should be rejected"
    );
}

// ── FOR UPDATE / FOR SHARE rejection ───────────────────────────────────

#[tokio::test]
async fn test_for_update_rejected_with_clear_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE forupd_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO forupd_src VALUES (1, 'a')").await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('forupd_st', \
             $$ SELECT id, val FROM forupd_src FOR UPDATE $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "FOR UPDATE should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("FOR UPDATE") || err_msg.contains("FOR SHARE"),
        "Error should mention FOR UPDATE/FOR SHARE, got: {err_msg}"
    );
    assert!(
        err_msg.contains("row-level locking"),
        "Error should explain the reason, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_for_share_rejected_with_clear_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE forshr_src (id INT PRIMARY KEY, val TEXT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('forshr_st', \
             $$ SELECT id, val FROM forshr_src FOR SHARE $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "FOR SHARE should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("FOR UPDATE") || err_msg.contains("FOR SHARE"),
        "Error should mention FOR UPDATE/FOR SHARE, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_for_no_key_update_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE fornku_src (id INT PRIMARY KEY, val TEXT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('fornku_st', \
             $$ SELECT id, val FROM fornku_src FOR NO KEY UPDATE $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "FOR NO KEY UPDATE should be rejected");
}

#[tokio::test]
async fn test_for_key_share_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE forks_src (id INT PRIMARY KEY, val TEXT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('forks_st', \
             $$ SELECT id, val FROM forks_src FOR KEY SHARE $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "FOR KEY SHARE should be rejected");
}

#[tokio::test]
async fn test_order_by_in_differential_mode_is_accepted() {
    // ORDER BY should also be accepted (and ignored) in DIFFERENTIAL mode
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE orderby_diff_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO orderby_diff_src VALUES (1, 10), (2, 20)")
        .await;

    db.create_st(
        "orderby_diff_st",
        "SELECT id, val FROM orderby_diff_src ORDER BY val DESC",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let count = db.count("public.orderby_diff_st").await;
    assert_eq!(count, 2, "ORDER BY in DIFFERENTIAL mode should be accepted");
}

// ── Concurrent Refresh Safety ──────────────────────────────────────────

#[tokio::test]
async fn test_concurrent_refresh_safety() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE conc_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO conc_src SELECT g, g FROM generate_series(1, 100) g")
        .await;

    db.create_st("conc_st", "SELECT id, val FROM conc_src", "1m", "FULL")
        .await;

    // Insert more data
    db.execute("INSERT INTO conc_src SELECT g, g FROM generate_series(101, 200) g")
        .await;

    // Two concurrent refreshes — use advisory lock so one waits
    let pool = db.pool.clone();
    let pool2 = db.pool.clone();

    let h1 = tokio::spawn(async move {
        sqlx::query("SELECT pgstream.refresh_stream_table('conc_st')")
            .execute(&pool)
            .await
    });
    let h2 = tokio::spawn(async move {
        sqlx::query("SELECT pgstream.refresh_stream_table('conc_st')")
            .execute(&pool2)
            .await
    });

    let (r1, r2) = tokio::join!(h1, h2);

    // At least one should succeed. Both might succeed (serialized by advisory lock).
    // We just need no data corruption.
    let success_count = [r1, r2]
        .iter()
        .filter(|r| r.as_ref().map(|inner| inner.is_ok()).unwrap_or(false))
        .count();
    assert!(
        success_count >= 1,
        "At least one concurrent refresh should succeed"
    );

    // Verify data integrity
    let count = db.count("public.conc_st").await;
    assert_eq!(count, 200, "All 200 rows should be present after refresh");
}

// ── GROUPING SETS / CUBE / ROLLUP rejection ─────────────────────────────

#[tokio::test]
async fn test_grouping_sets_rejected_with_clear_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE gs_src (dept TEXT, region TEXT, amount NUMERIC)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('gs_st', \
             $$ SELECT dept, SUM(amount) FROM gs_src \
             GROUP BY GROUPING SETS ((dept), (region)) $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "GROUPING SETS should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("GROUPING SETS"),
        "Error should mention GROUPING SETS, got: {err_msg}"
    );
    assert!(
        err_msg.contains("UNION ALL") || err_msg.contains("FULL"),
        "Error should suggest alternatives (UNION ALL / FULL mode), got: {err_msg}"
    );
}

#[tokio::test]
async fn test_rollup_rejected_with_clear_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rollup_src (dept TEXT, region TEXT, amount NUMERIC)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('rollup_st', \
             $$ SELECT dept, region, SUM(amount) FROM rollup_src \
             GROUP BY ROLLUP (dept, region) $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "ROLLUP should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("ROLLUP"),
        "Error should mention ROLLUP, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_cube_rejected_with_clear_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cube_src (dept TEXT, region TEXT, amount NUMERIC)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('cube_st', \
             $$ SELECT dept, region, SUM(amount) FROM cube_src \
             GROUP BY CUBE (dept, region) $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "CUBE should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("CUBE"),
        "Error should mention CUBE, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_grouping_sets_differential_mode_also_rejected() {
    // Verify rejection also fires in DIFFERENTIAL mode, not just FULL
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE gsd_src (dept TEXT, amount NUMERIC)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('gsd_st', \
             $$ SELECT dept, SUM(amount) FROM gsd_src \
             GROUP BY GROUPING SETS ((dept), ()) $$, '1m', 'DIFFERENTIAL')",
        )
        .await;
    assert!(
        result.is_err(),
        "GROUPING SETS should be rejected in DIFFERENTIAL mode too"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("GROUPING SETS"),
        "Error should mention GROUPING SETS, got: {err_msg}"
    );
}

// ── TABLESAMPLE rejection ───────────────────────────────────────────────

#[tokio::test]
async fn test_tablesample_bernoulli_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ts_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO ts_src SELECT generate_series(1,100), generate_series(1,100)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('ts_st', \
             $$ SELECT id, val FROM ts_src TABLESAMPLE BERNOULLI(10) $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "TABLESAMPLE should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("TABLESAMPLE"),
        "Error should mention TABLESAMPLE, got: {err_msg}"
    );
    assert!(
        err_msg.contains("random()") || err_msg.contains("WHERE"),
        "Error should suggest random()/WHERE alternative, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_tablesample_system_rejected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE tss_src (id INT PRIMARY KEY, val INT)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('tss_st', \
             $$ SELECT id, val FROM tss_src TABLESAMPLE SYSTEM(50) $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "TABLESAMPLE SYSTEM should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("TABLESAMPLE"),
        "Error should mention TABLESAMPLE, got: {err_msg}"
    );
}

// ── Ordered-set aggregate now supported ──────────────────────────────────

#[tokio::test]
async fn test_percentile_cont_now_supported_in_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE pct_src (dept TEXT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO pct_src VALUES ('A', 100), ('A', 200), ('B', 300)")
        .await;

    let result = db
        .try_execute(
            "SELECT pgstream.create_stream_table('pct_st', \
             $$ SELECT dept, PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY amount) AS median \
             FROM pct_src GROUP BY dept $$, '1m', 'DIFFERENTIAL')",
        )
        .await;
    assert!(
        result.is_ok(),
        "PERCENTILE_CONT in DIFFERENTIAL mode should now be supported, got: {:?}",
        result.err()
    );
}
