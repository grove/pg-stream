//! Integration tests for basic PostgreSQL connectivity via Testcontainers.
//!
//! These tests verify the Testcontainers setup works correctly with
//! PostgreSQL 18.1 before running more complex extension tests.

mod common;

use common::TestDb;

#[tokio::test]
async fn test_container_starts_and_connects() {
    let db = TestDb::new().await;

    // Verify we can execute a basic query
    let version: String = db.query_scalar("SELECT version()").await;
    assert!(
        version.contains("PostgreSQL"),
        "Expected PostgreSQL version string, got: {}",
        version
    );
}

#[tokio::test]
async fn test_create_table_and_insert() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE test_orders (id INT PRIMARY KEY, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO test_orders VALUES (1, 100.50), (2, 200.75)")
        .await;

    let count: i64 = db.query_scalar("SELECT count(*) FROM test_orders").await;
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_schemas_can_be_created() {
    let db = TestDb::new().await;

    db.execute("CREATE SCHEMA IF NOT EXISTS test_schema").await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = 'test_schema')",
        )
        .await;

    assert!(exists, "Schema test_schema should exist");
}
