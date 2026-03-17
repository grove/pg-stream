//! Infrastructure smoke tests for PostgreSQL / Testcontainers connectivity.
//!
//! These tests are intentionally kept as a separate file rather than folded
//! into other suites. They serve as **first-to-fail diagnostics**: if Docker
//! is unavailable, the container image is missing, or network routing is
//! broken, these three tests fail immediately with clear messages before the
//! more complex test suites even attempt to start.
//!
//! They are redundant with respect to data-correctness coverage (every other
//! integration test that spins up a container proves the same infrastructure
//! works) but their diagnostic speed and clarity justify their existence.

mod common;

use common::TestDb;

#[tokio::test]
async fn test_infra_container_connects_to_postgres() {
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
async fn test_infra_basic_crud_works() {
    let db = TestDb::new().await;

    db.execute("CREATE TABLE test_orders (id INT PRIMARY KEY, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO test_orders VALUES (1, 100.50), (2, 200.75)")
        .await;

    let count: i64 = db.query_scalar("SELECT count(*) FROM test_orders").await;
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_infra_schema_creation_works() {
    let db = TestDb::new().await;

    db.execute("CREATE SCHEMA IF NOT EXISTS test_schema").await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = 'test_schema')",
        )
        .await;

    assert!(exists, "Schema test_schema should exist");
}
