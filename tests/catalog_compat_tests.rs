//! Integration tests for PostgreSQL catalog behavior that pg_stream relies on.
//!
//! These tests run against a live PostgreSQL 18 container (via Testcontainers)
//! and verify assumptions that previously caused E2E failures:
//!
//! - `pg_get_viewdef()` trailing-semicolon behavior (PG18+)
//! - `pg_namespace.nspname` type (`name` vs `text`) and cast requirements
//! - `pg_class.relkind` values for tables, views, matviews, foreign tables
//!
//! **No pg_stream extension is required** — these tests use only built-in
//! PostgreSQL catalog functions and system tables.

mod common;

use common::TestDb;

// ── pg_get_viewdef behavior ─────────────────────────────────────────

#[tokio::test]
async fn test_pg_get_viewdef_returns_trimmed_definition() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE vd_src (id INT, name TEXT)").await;
    db.execute("CREATE VIEW vd_view AS SELECT id, name FROM vd_src WHERE id > 0")
        .await;

    let raw: String = db
        .query_scalar(
            "SELECT pg_get_viewdef(c.oid, true) \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = 'vd_view'",
        )
        .await;

    // The definition should not end with a semicolon when we intend to use
    // it as a subquery.  PG18 may add one — this test documents the behavior.
    let trimmed = raw.trim_end_matches(';').trim();
    assert!(
        !trimmed.is_empty(),
        "pg_get_viewdef should return a non-empty definition"
    );
    // After stripping, it must be valid SQL (no trailing semicolons)
    assert!(
        !trimmed.ends_with(';'),
        "After stripping, definition should not end with semicolon: {:?}",
        trimmed
    );

    // Verify it can be used as a subquery
    let subquery = format!("SELECT count(*) FROM ({trimmed}) AS sub");
    let count: i64 = db.query_scalar(&subquery).await;
    assert_eq!(count, 0, "subquery wrapping view definition should work");
}

#[tokio::test]
async fn test_pg_get_viewdef_complex_view() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE vd_orders (id INT, amount NUMERIC, region TEXT)")
        .await;
    db.execute(
        "CREATE VIEW vd_summary AS \
         SELECT region, SUM(amount) AS total, COUNT(*) AS cnt \
         FROM vd_orders GROUP BY region",
    )
    .await;

    let raw: String = db
        .query_scalar(
            "SELECT pg_get_viewdef(c.oid, true) \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = 'vd_summary'",
        )
        .await;

    let trimmed = raw.trim_end_matches(';').trim();
    let subquery = format!("SELECT count(*) FROM ({trimmed}) AS sub");
    let count: i64 = db.query_scalar(&subquery).await;
    assert_eq!(count, 0);
}

// ── nspname type casting ────────────────────────────────────────────

#[tokio::test]
async fn test_nspname_text_cast_works() {
    let db = TestDb::new().await;

    // Querying nspname directly (type `name`) should work when cast to text
    let schema: String = db
        .query_scalar(
            "SELECT n.nspname::text FROM pg_namespace n WHERE n.nspname = 'public' LIMIT 1",
        )
        .await;
    assert_eq!(schema, "public");
}

#[tokio::test]
async fn test_nspname_via_search_path_resolution() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE sp_test (id INT)").await;

    // This mirrors the query used by resolve_rangevar_schema():
    // it must return the schema name as TEXT for Rust String decoding
    let schema: String = db
        .query_scalar(
            "SELECT n.nspname::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = 'sp_test' \
             AND n.nspname = ANY(string_to_array(current_setting('search_path'), ', ')::text[] || 'public'::text) \
             ORDER BY array_position(string_to_array(current_setting('search_path'), ', ')::text[] || 'public'::text, n.nspname) \
             LIMIT 1",
        )
        .await;
    assert_eq!(schema, "public");
}

#[tokio::test]
async fn test_nspname_lookup_for_nonexistent_relation_returns_empty() {
    let db = TestDb::new().await;

    // A CTE name won't be found in pg_class — the query should return 0 rows
    let count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = 'this_is_a_cte_name_not_a_real_table'",
        )
        .await;
    assert_eq!(count, 0, "CTE names should not be found in pg_class");
}

// ── relkind values ──────────────────────────────────────────────────

#[tokio::test]
async fn test_relkind_for_regular_table() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE rk_table (id INT)").await;

    let relkind: String = db
        .query_scalar("SELECT relkind::text FROM pg_class WHERE relname = 'rk_table'")
        .await;
    assert_eq!(relkind, "r", "Regular table relkind should be 'r'");
}

#[tokio::test]
async fn test_relkind_for_view() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE rk_src (id INT)").await;
    db.execute("CREATE VIEW rk_view AS SELECT id FROM rk_src")
        .await;

    let relkind: String = db
        .query_scalar("SELECT relkind::text FROM pg_class WHERE relname = 'rk_view'")
        .await;
    assert_eq!(relkind, "v", "View relkind should be 'v'");
}

#[tokio::test]
async fn test_relkind_for_materialized_view() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE rk_mat_src (id INT)").await;
    db.execute("CREATE MATERIALIZED VIEW rk_matview AS SELECT id FROM rk_mat_src")
        .await;

    let relkind: String = db
        .query_scalar("SELECT relkind::text FROM pg_class WHERE relname = 'rk_matview'")
        .await;
    assert_eq!(relkind, "m", "Materialized view relkind should be 'm'");
}

#[tokio::test]
async fn test_relkind_for_partitioned_table() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE rk_part (id INT, region TEXT) PARTITION BY LIST (region)")
        .await;

    let relkind: String = db
        .query_scalar("SELECT relkind::text FROM pg_class WHERE relname = 'rk_part'")
        .await;
    assert_eq!(relkind, "p", "Partitioned table relkind should be 'p'");
}

// ── array_length return type ────────────────────────────────────────

#[tokio::test]
async fn test_array_length_returns_int4() {
    let db = TestDb::new().await;

    // array_length returns INTEGER (INT4), not BIGINT.
    // When comparing with BIGINT columns, an explicit cast is needed.
    let type_name: String = db
        .query_scalar("SELECT pg_typeof(array_length(ARRAY[1,2,3], 1))::text")
        .await;
    assert_eq!(
        type_name, "integer",
        "array_length should return 'integer' (INT4), not 'bigint'"
    );
}

#[tokio::test]
async fn test_array_length_bigint_cast_comparison() {
    let db = TestDb::new().await;

    // Verify that casting to bigint works correctly
    let val: i64 = db
        .query_scalar("SELECT coalesce(array_length(ARRAY[1,2,3], 1), 0)::bigint")
        .await;
    assert_eq!(val, 3);
}

// ── UNION ALL column consistency (grouping sets requirement) ────────

#[tokio::test]
async fn test_union_all_requires_matching_column_count() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE ua_src (dept TEXT, region TEXT, amount NUMERIC)")
        .await;

    // Valid UNION ALL: both branches have the same columns
    let result = db
        .try_execute(
            "SELECT dept, SUM(amount) FROM ua_src GROUP BY dept \
             UNION ALL \
             SELECT dept, SUM(amount) FROM ua_src GROUP BY dept",
        )
        .await;
    assert!(
        result.is_ok(),
        "UNION ALL with matching columns should work"
    );

    // Invalid UNION ALL: different column counts
    let result = db
        .try_execute(
            "SELECT dept, SUM(amount) FROM ua_src GROUP BY dept \
             UNION ALL \
             SELECT dept, region, SUM(amount) FROM ua_src GROUP BY dept, region",
        )
        .await;
    assert!(
        result.is_err(),
        "UNION ALL with mismatched column counts should fail"
    );
}

#[tokio::test]
async fn test_union_all_grouping_sets_manual_rewrite_pattern() {
    let db = TestDb::new().await;

    // This test verifies the pattern that rewrite_grouping_sets must produce:
    // every branch must output ALL grouping columns (with NULL for ungrouped ones)
    db.execute("CREATE TABLE gs_manual (dept TEXT, region TEXT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO gs_manual VALUES ('eng', 'us', 100), ('hr', 'eu', 200)")
        .await;

    // Correct pattern: both branches output (dept, region, sum)
    // Branch 1 groups by dept  → region is NULL
    // Branch 2 groups by region → dept is NULL
    let count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM (\
                SELECT dept AS dept, NULL::text AS region, SUM(amount) AS total \
                FROM gs_manual GROUP BY dept \
                UNION ALL \
                SELECT NULL::text AS dept, region AS region, SUM(amount) AS total \
                FROM gs_manual GROUP BY region\
             ) sub",
        )
        .await;
    assert_eq!(
        count, 4,
        "Manual GROUPING SETS UNION ALL rewrite should produce rows from both branches"
    );
}
