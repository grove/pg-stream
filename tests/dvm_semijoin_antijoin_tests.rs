#![cfg(not(target_os = "macos"))]

//! Execution-backed tests for semi-join and anti-join DVM SQL.
//!
//! These tests exercise the generated delta SQL directly against a live
//! PostgreSQL container. They target the highest-risk transition cases from
//! PLAN_TEST_EVALS_UNIT.md without requiring the full extension runtime.

mod common;

use common::TestDb;
use pg_trickle::dvm::DiffContext;
use pg_trickle::dvm::parser::{Column, Expr, OpTree};
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

fn eq_cond(left_alias: &str, left_col: &str, right_alias: &str, right_col: &str) -> Expr {
    Expr::BinaryOp {
        op: "=".to_string(),
        left: Box::new(Expr::ColumnRef {
            table_alias: Some(left_alias.to_string()),
            column_name: left_col.to_string(),
        }),
        right: Box::new(Expr::ColumnRef {
            table_alias: Some(right_alias.to_string()),
            column_name: right_col.to_string(),
        }),
    }
}

fn make_ctx() -> DiffContext {
    let mut prev_frontier = Frontier::new();
    prev_frontier.set_source(1, "0/0".to_string(), "2025-01-01T00:00:00Z".to_string());
    prev_frontier.set_source(2, "0/0".to_string(), "2025-01-01T00:00:00Z".to_string());

    let mut new_frontier = Frontier::new();
    new_frontier.set_source(1, "0/10".to_string(), "2025-01-01T00:00:10Z".to_string());
    new_frontier.set_source(2, "0/10".to_string(), "2025-01-01T00:00:10Z".to_string());

    DiffContext::new_standalone(prev_frontier, new_frontier)
}

fn build_semi_join_tree() -> OpTree {
    let left = scan_with_pk(
        1,
        "orders",
        "o",
        vec![int_col("id"), int_col("cust_id"), int_col("amount")],
        &["id"],
    );
    let right = scan_with_pk(
        2,
        "customers",
        "c",
        vec![int_col("id"), text_col("name")],
        &["id"],
    );

    OpTree::SemiJoin {
        condition: eq_cond("o", "cust_id", "c", "id"),
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn build_anti_join_tree() -> OpTree {
    let left = scan_with_pk(
        1,
        "orders",
        "o",
        vec![int_col("id"), int_col("cust_id"), int_col("amount")],
        &["id"],
    );
    let right = scan_with_pk(
        2,
        "customers",
        "c",
        vec![int_col("id"), text_col("name")],
        &["id"],
    );

    OpTree::AntiJoin {
        condition: eq_cond("o", "cust_id", "c", "id"),
        left: Box::new(left),
        right: Box::new(right),
    }
}

async fn setup_operator_db() -> TestDb {
    let db = TestDb::new().await;

    sqlx::raw_sql(
        r#"
CREATE SCHEMA IF NOT EXISTS pgtrickle;
CREATE SCHEMA IF NOT EXISTS pgtrickle_changes;

CREATE OR REPLACE FUNCTION pgtrickle.pg_trickle_hash_multi(vals TEXT[])
RETURNS BIGINT
LANGUAGE SQL
IMMUTABLE
AS $$
    SELECT hashtextextended(COALESCE(array_to_string(vals, '|', '<NULL>'), ''), 0)::BIGINT
$$;

CREATE TABLE public.orders (
    id INT PRIMARY KEY,
    cust_id INT NOT NULL,
    amount INT NOT NULL
);

CREATE TABLE public.customers (
    id INT PRIMARY KEY,
    name TEXT NOT NULL
);

CREATE TABLE pgtrickle_changes.changes_1 (
    change_id BIGSERIAL PRIMARY KEY,
    lsn PG_LSN NOT NULL,
    action CHAR(1) NOT NULL,
    pk_hash BIGINT,
    new_id INT,
    new_cust_id INT,
    new_amount INT,
    old_id INT,
    old_cust_id INT,
    old_amount INT
);

CREATE TABLE pgtrickle_changes.changes_2 (
    change_id BIGSERIAL PRIMARY KEY,
    lsn PG_LSN NOT NULL,
    action CHAR(1) NOT NULL,
    pk_hash BIGINT,
    new_id INT,
    new_name TEXT,
    old_id INT,
    old_name TEXT
);
"#,
    )
    .execute(&db.pool)
    .await
    .expect("failed to set up operator execution database");

    db
}

async fn reset_semijoin_fixture(db: &TestDb) {
    db.execute(
        "TRUNCATE TABLE \
         pgtrickle_changes.changes_1, \
         pgtrickle_changes.changes_2, \
         public.orders, \
         public.customers \
         RESTART IDENTITY",
    )
    .await;

    db.execute("INSERT INTO public.orders VALUES (1, 10, 100), (2, 20, 200)")
        .await;
}

async fn query_rows(db: &TestDb, sql: &str) -> Vec<(String, i32, i32, i32)> {
    sqlx::query_as::<_, (String, i32, i32, i32)>(&format!(
        "SELECT __pgt_action, id, cust_id, amount FROM ({sql}) delta ORDER BY id"
    ))
    .fetch_all(&db.pool)
    .await
    .expect("failed to execute generated delta SQL")
}

#[tokio::test]
async fn test_diff_semi_join_executes_match_gain_and_loss() {
    let db = setup_operator_db().await;
    let sql = make_ctx()
        .differentiate(&build_semi_join_tree())
        .expect("semi-join differentiation should succeed");

    reset_semijoin_fixture(&db).await;
    db.execute("INSERT INTO public.customers VALUES (10, 'Alice'), (20, 'Bob')")
        .await;
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_2 (lsn, action, pk_hash, new_id, new_name) \
         VALUES ('0/1', 'I', 10, 10, 'Alice')",
    )
    .await;

    assert_eq!(
        query_rows(&db, &sql).await,
        vec![("I".to_string(), 1, 10, 100)]
    );

    reset_semijoin_fixture(&db).await;
    db.execute("INSERT INTO public.customers VALUES (20, 'Bob')")
        .await;
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_2 (lsn, action, pk_hash, old_id, old_name) \
         VALUES ('0/1', 'D', 10, 10, 'Alice')",
    )
    .await;

    assert_eq!(
        query_rows(&db, &sql).await,
        vec![("D".to_string(), 1, 10, 100)]
    );
}

#[tokio::test]
async fn test_diff_anti_join_executes_match_gain_and_loss() {
    let db = setup_operator_db().await;
    let sql = make_ctx()
        .differentiate(&build_anti_join_tree())
        .expect("anti-join differentiation should succeed");

    reset_semijoin_fixture(&db).await;
    db.execute("INSERT INTO public.customers VALUES (10, 'Alice'), (20, 'Bob')")
        .await;
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_2 (lsn, action, pk_hash, new_id, new_name) \
         VALUES ('0/1', 'I', 10, 10, 'Alice')",
    )
    .await;

    assert_eq!(
        query_rows(&db, &sql).await,
        vec![("D".to_string(), 1, 10, 100)]
    );

    reset_semijoin_fixture(&db).await;
    db.execute("INSERT INTO public.customers VALUES (20, 'Bob')")
        .await;
    db.execute(
        "INSERT INTO pgtrickle_changes.changes_2 (lsn, action, pk_hash, old_id, old_name) \
         VALUES ('0/1', 'D', 10, 10, 'Alice')",
    )
    .await;

    assert_eq!(
        query_rows(&db, &sql).await,
        vec![("I".to_string(), 1, 10, 100)]
    );
}
