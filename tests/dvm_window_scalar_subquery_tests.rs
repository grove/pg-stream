#![cfg(not(target_os = "macos"))]

//! Execution-backed tests for window and scalar-subquery DVM SQL.
//!
//! These tests execute generated delta SQL against a standalone PostgreSQL
//! container so we can validate result rows for the remaining thin operators
//! called out in PLAN_TEST_EVALS_UNIT.md.

mod common;

use common::TestDb;
use pg_trickle::dvm::DiffContext;
use pg_trickle::dvm::parser::{Column, Expr, OpTree, SortExpr, WindowExpr};
use pg_trickle::version::Frontier;

fn int_col(name: &str) -> Column {
    Column {
        name: name.to_string(),
        type_oid: 23,
        is_nullable: true,
    }
}

fn text_col(name: &str) -> Column {
    Column {
        name: name.to_string(),
        type_oid: 25,
        is_nullable: true,
    }
}

fn colref(name: &str) -> Expr {
    Expr::ColumnRef {
        table_alias: None,
        column_name: name.to_string(),
    }
}

fn sort_asc(name: &str) -> SortExpr {
    SortExpr {
        expr: colref(name),
        ascending: true,
        nulls_first: false,
    }
}

fn scan_with_pk(
    oid: u32,
    table_name: &str,
    alias: &str,
    columns: Vec<Column>,
    pk_columns: &[&str],
) -> OpTree {
    OpTree::Scan {
        table_oid: oid,
        table_name: table_name.to_string(),
        schema: "public".to_string(),
        columns,
        pk_columns: pk_columns.iter().map(|c| (*c).to_string()).collect(),
        alias: alias.to_string(),
    }
}

fn make_window_ctx(st_name: &str) -> DiffContext {
    let mut prev_frontier = Frontier::new();
    prev_frontier.set_source(1, "0/0".to_string(), "2025-01-01T00:00:00Z".to_string());

    let mut new_frontier = Frontier::new();
    new_frontier.set_source(1, "0/10".to_string(), "2025-01-01T00:00:10Z".to_string());

    DiffContext::new_standalone(prev_frontier, new_frontier).with_pgt_name("public", st_name)
}

fn make_scalar_ctx() -> DiffContext {
    let mut prev_frontier = Frontier::new();
    prev_frontier.set_source(1, "0/0".to_string(), "2025-01-01T00:00:00Z".to_string());
    prev_frontier.set_source(2, "0/0".to_string(), "2025-01-01T00:00:00Z".to_string());

    let mut new_frontier = Frontier::new();
    new_frontier.set_source(1, "0/10".to_string(), "2025-01-01T00:00:10Z".to_string());
    new_frontier.set_source(2, "0/10".to_string(), "2025-01-01T00:00:10Z".to_string());

    DiffContext::new_standalone(prev_frontier, new_frontier)
}

fn build_row_number_window_tree() -> OpTree {
    let child = scan_with_pk(
        1,
        "orders",
        "o",
        vec![int_col("id"), text_col("region"), int_col("amount")],
        &["id"],
    );

    OpTree::Window {
        window_exprs: vec![WindowExpr {
            func_name: "ROW_NUMBER".to_string(),
            args: vec![],
            partition_by: vec![colref("region")],
            order_by: vec![sort_asc("amount")],
            frame_clause: None,
            alias: "rn".to_string(),
        }],
        partition_by: vec![colref("region")],
        pass_through: vec![
            (colref("id"), "id".to_string()),
            (colref("region"), "region".to_string()),
            (colref("amount"), "amount".to_string()),
        ],
        child: Box::new(child),
    }
}

fn build_running_sum_window_tree() -> OpTree {
    let child = scan_with_pk(
        1,
        "orders",
        "o",
        vec![int_col("id"), text_col("region"), int_col("amount")],
        &["id"],
    );

    OpTree::Window {
        window_exprs: vec![WindowExpr {
            func_name: "SUM".to_string(),
            args: vec![colref("amount")],
            partition_by: vec![colref("region")],
            order_by: vec![sort_asc("amount")],
            frame_clause: Some("ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW".to_string()),
            alias: "running_total".to_string(),
        }],
        partition_by: vec![colref("region")],
        pass_through: vec![
            (colref("id"), "id".to_string()),
            (colref("region"), "region".to_string()),
            (colref("amount"), "amount".to_string()),
        ],
        child: Box::new(child),
    }
}

fn build_scalar_subquery_tree() -> OpTree {
    let outer = scan_with_pk(
        1,
        "orders",
        "o",
        vec![int_col("id"), int_col("amount")],
        &["id"],
    );
    let inner = scan_with_pk(2, "config", "c", vec![int_col("tax_rate")], &["id"]);

    OpTree::ScalarSubquery {
        subquery: Box::new(inner),
        alias: "current_tax".to_string(),
        subquery_source_oids: vec![2],
        child: Box::new(outer),
    }
}

async fn setup_window_db() -> TestDb {
    let db = TestDb::new().await;

    sqlx::raw_sql(
        r#"
CREATE SCHEMA IF NOT EXISTS pgtrickle;
CREATE SCHEMA IF NOT EXISTS pgtrickle_changes;

CREATE OR REPLACE FUNCTION pgtrickle.pg_trickle_hash(val TEXT)
RETURNS BIGINT
LANGUAGE SQL
IMMUTABLE
AS $$
    SELECT hashtextextended(COALESCE(val, ''), 0)::BIGINT
$$;

CREATE OR REPLACE FUNCTION pgtrickle.pg_trickle_hash_multi(vals TEXT[])
RETURNS BIGINT
LANGUAGE SQL
IMMUTABLE
AS $$
    SELECT hashtextextended(COALESCE(array_to_string(vals, '|', '<NULL>'), ''), 0)::BIGINT
$$;

CREATE TABLE public.orders (
    id INT PRIMARY KEY,
    region TEXT NOT NULL,
    amount INT NOT NULL
);

CREATE TABLE public.window_row_number_st (
    __pgt_row_id BIGINT PRIMARY KEY,
    id INT NOT NULL,
    region TEXT NOT NULL,
    amount INT NOT NULL,
    rn BIGINT NOT NULL
);

CREATE TABLE public.window_running_sum_st (
    __pgt_row_id BIGINT PRIMARY KEY,
    id INT NOT NULL,
    region TEXT NOT NULL,
    amount INT NOT NULL,
    running_total BIGINT NOT NULL
);

CREATE TABLE pgtrickle_changes.changes_1 (
    change_id BIGSERIAL PRIMARY KEY,
    lsn PG_LSN NOT NULL,
    action CHAR(1) NOT NULL,
    pk_hash BIGINT,
    new_id INT,
    new_region TEXT,
    new_amount INT,
    old_id INT,
    old_region TEXT,
    old_amount INT
);
"#,
    )
    .execute(&db.pool)
    .await
    .expect("failed to set up window execution database");

    db
}

async fn setup_scalar_db() -> TestDb {
    let db = TestDb::new().await;

    sqlx::raw_sql(
        r#"
CREATE SCHEMA IF NOT EXISTS pgtrickle;
CREATE SCHEMA IF NOT EXISTS pgtrickle_changes;

CREATE OR REPLACE FUNCTION pgtrickle.pg_trickle_hash(val TEXT)
RETURNS BIGINT
LANGUAGE SQL
IMMUTABLE
AS $$
    SELECT hashtextextended(COALESCE(val, ''), 0)::BIGINT
$$;

CREATE OR REPLACE FUNCTION pgtrickle.pg_trickle_hash_multi(vals TEXT[])
RETURNS BIGINT
LANGUAGE SQL
IMMUTABLE
AS $$
    SELECT hashtextextended(COALESCE(array_to_string(vals, '|', '<NULL>'), ''), 0)::BIGINT
$$;

CREATE TABLE public.orders (
    id INT PRIMARY KEY,
    amount INT NOT NULL
);

CREATE TABLE public.config (
    id INT PRIMARY KEY,
    tax_rate INT NOT NULL
);

CREATE TABLE pgtrickle_changes.changes_1 (
    change_id BIGSERIAL PRIMARY KEY,
    lsn PG_LSN NOT NULL,
    action CHAR(1) NOT NULL,
    pk_hash BIGINT,
    new_id INT,
    new_amount INT,
    old_id INT,
    old_amount INT
);

CREATE TABLE pgtrickle_changes.changes_2 (
    change_id BIGSERIAL PRIMARY KEY,
    lsn PG_LSN NOT NULL,
    action CHAR(1) NOT NULL,
    pk_hash BIGINT,
    new_id INT,
    new_tax_rate INT,
    old_id INT,
    old_tax_rate INT
);
"#,
    )
    .execute(&db.pool)
    .await
    .expect("failed to set up scalar-subquery execution database");

    db
}

async fn query_window_rows(
    db: &TestDb,
    sql: &str,
    value_column: &str,
) -> Vec<(String, i32, String, i32, i64)> {
    sqlx::query_as::<_, (String, i32, String, i32, i64)>(&format!(
        "SELECT __pgt_action, id, region, amount, {value_column} FROM ({sql}) delta ORDER BY __pgt_action, id, {value_column}"
    ))
    .fetch_all(&db.pool)
    .await
    .expect("failed to execute generated window delta SQL")
}

async fn query_scalar_rows(db: &TestDb, sql: &str) -> Vec<(String, i32, i32, i32)> {
    sqlx::query_as::<_, (String, i32, i32, i32)>(&format!(
        "SELECT __pgt_action, id, amount, current_tax FROM ({sql}) delta ORDER BY __pgt_action, id"
    ))
    .fetch_all(&db.pool)
    .await
    .expect("failed to execute generated scalar-subquery delta SQL")
}

#[tokio::test]
async fn test_diff_window_executes_partition_local_row_number_recompute() {
    let db = setup_window_db().await;
    let sql = make_window_ctx("window_row_number_st")
        .differentiate(&build_row_number_window_tree())
        .expect("window differentiation should succeed");

    db.execute(
        "TRUNCATE TABLE pgtrickle_changes.changes_1, public.window_row_number_st, public.orders RESTART IDENTITY",
    )
    .await;

    db.execute(
        "INSERT INTO public.orders VALUES \
         (1, 'east', 10), \
         (2, 'east', 20), \
         (3, 'west', 15), \
         (4, 'east', 15)",
    )
    .await;
    db.execute(
        "INSERT INTO public.window_row_number_st VALUES \
         (1, 1, 'east', 10, 1), \
         (2, 2, 'east', 20, 2), \
         (3, 3, 'west', 15, 1)",
    )
    .await;
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_1 \
         (lsn, action, pk_hash, new_id, new_region, new_amount) \
         VALUES ('0/1', 'I', 4, 4, 'east', 15)",
    )
    .await;

    assert_eq!(
        query_window_rows(&db, &sql, "rn").await,
        vec![
            ("D".to_string(), 1, "east".to_string(), 10, 1),
            ("D".to_string(), 2, "east".to_string(), 20, 2),
            ("I".to_string(), 1, "east".to_string(), 10, 1),
            ("I".to_string(), 2, "east".to_string(), 20, 3),
            ("I".to_string(), 4, "east".to_string(), 15, 2),
        ]
    );
}

#[tokio::test]
async fn test_diff_window_executes_frame_sensitive_running_sum_recompute() {
    let db = setup_window_db().await;
    let sql = make_window_ctx("window_running_sum_st")
        .differentiate(&build_running_sum_window_tree())
        .expect("window differentiation should succeed");

    db.execute(
        "TRUNCATE TABLE pgtrickle_changes.changes_1, public.window_running_sum_st, public.orders RESTART IDENTITY",
    )
    .await;

    db.execute(
        "INSERT INTO public.orders VALUES \
         (1, 'east', 10), \
         (3, 'west', 15), \
         (5, 'west', 30), \
         (6, 'west', 25)",
    )
    .await;
    db.execute(
        "INSERT INTO public.window_running_sum_st VALUES \
         (1, 1, 'east', 10, 10), \
         (3, 3, 'west', 15, 15), \
         (5, 5, 'west', 30, 45)",
    )
    .await;
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_1 \
         (lsn, action, pk_hash, new_id, new_region, new_amount) \
         VALUES ('0/1', 'I', 6, 6, 'west', 25)",
    )
    .await;

    assert_eq!(
        query_window_rows(&db, &sql, "running_total").await,
        vec![
            ("D".to_string(), 3, "west".to_string(), 15, 15),
            ("D".to_string(), 5, "west".to_string(), 30, 45),
            ("I".to_string(), 3, "west".to_string(), 15, 15),
            ("I".to_string(), 5, "west".to_string(), 30, 70),
            ("I".to_string(), 6, "west".to_string(), 25, 40),
        ]
    );
}

#[tokio::test]
async fn test_diff_scalar_subquery_executes_inner_change_recompute() {
    let db = setup_scalar_db().await;
    let sql = make_scalar_ctx()
        .differentiate(&build_scalar_subquery_tree())
        .expect("scalar-subquery differentiation should succeed");

    db.execute(
        "TRUNCATE TABLE pgtrickle_changes.changes_1, pgtrickle_changes.changes_2, public.orders, public.config RESTART IDENTITY",
    )
    .await;
    db.execute("INSERT INTO public.orders VALUES (1, 100), (2, 200)")
        .await;
    db.execute("INSERT INTO public.config VALUES (1, 20)").await;
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_2 \
         (lsn, action, pk_hash, new_id, new_tax_rate, old_id, old_tax_rate) \
         VALUES ('0/1', 'U', 1, 1, 20, 1, 10)",
    )
    .await;

    assert_eq!(
        query_scalar_rows(&db, &sql).await,
        vec![
            ("D".to_string(), 1, 100, 10),
            ("D".to_string(), 2, 200, 10),
            ("I".to_string(), 1, 100, 20),
            ("I".to_string(), 2, 200, 20),
        ]
    );
}

#[tokio::test]
async fn test_diff_scalar_subquery_executes_outer_insert_with_current_scalar() {
    let db = setup_scalar_db().await;
    let sql = make_scalar_ctx()
        .differentiate(&build_scalar_subquery_tree())
        .expect("scalar-subquery differentiation should succeed");

    db.execute(
        "TRUNCATE TABLE pgtrickle_changes.changes_1, pgtrickle_changes.changes_2, public.orders, public.config RESTART IDENTITY",
    )
    .await;
    db.execute("INSERT INTO public.orders VALUES (1, 100), (2, 200), (3, 300)")
        .await;
    db.execute("INSERT INTO public.config VALUES (1, 10)").await;
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_1 \
         (lsn, action, pk_hash, new_id, new_amount) \
         VALUES ('0/1', 'I', 3, 3, 300)",
    )
    .await;

    assert_eq!(
        query_scalar_rows(&db, &sql).await,
        vec![("I".to_string(), 3, 300, 10)]
    );
}
