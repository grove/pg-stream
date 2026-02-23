//! Integration tests that replace the former `#[pg_test]` tests.
//!
//! These tests verify that the pg_stream catalog schema, tables, views, and
//! hash function logic work correctly inside a real PostgreSQL 18.1
//! container managed by Testcontainers.
//!
//! Replaces the previous `#[pg_test]` tests from `lib.rs`:
//! - test_extension_loads          → test_pg_stream_schema_exists
//! - test_pg_stream_hash_function_exists → test_xxhash_deterministic
//! - test_pg_stream_hash_deterministic  → test_xxhash_deterministic
//! - test_catalog_tables_exist     → test_all_catalog_objects_exist

mod common;

use common::TestDb;

// ── Schema / Catalog Existence ─────────────────────────────────────────────

/// Equivalent of the old `test_extension_loads` pg_test.
/// Verifies the pg_stream schema and core infrastructure are created.
#[tokio::test]
async fn test_pg_stream_schema_exists() {
    let db = TestDb::with_catalog().await;

    let pg_stream_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = 'pgstream')",
        )
        .await;
    assert!(pg_stream_exists, "pg_stream schema should exist");

    let changes_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = 'pgstream_changes')",
        )
        .await;
    assert!(changes_exists, "pgstream_changes schema should exist");
}

/// Equivalent of the old `test_catalog_tables_exist` pg_test.
/// Verifies all catalog tables, indexes, and views are present.
#[tokio::test]
async fn test_all_catalog_objects_exist() {
    let db = TestDb::with_catalog().await;

    // Tables
    let tables = [
        ("pgstream", "pgs_stream_tables"),
        ("pgstream", "pgs_dependencies"),
        ("pgstream", "pgs_refresh_history"),
        ("pgstream", "pgs_change_tracking"),
    ];

    for (schema, table) in tables {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = '{}' AND table_name = '{}')",
                schema, table
            ))
            .await;
        assert!(exists, "Table {}.{} should exist", schema, table);
    }

    // View
    let view_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.views \
             WHERE table_schema = 'pgstream' AND table_name = 'stream_tables_info')",
        )
        .await;
    assert!(view_exists, "pgstream.stream_tables_info view should exist");

    // Indexes (check key ones via pg_indexes)
    let idx_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pg_indexes \
             WHERE schemaname = 'pgstream' AND tablename = 'pgs_stream_tables'",
        )
        .await;
    // At least: PK, idx_pgs_status, idx_pgs_name
    assert!(
        idx_count >= 3,
        "Expected at least 3 indexes on pgstream.pgs_stream_tables, found {}",
        idx_count
    );
}

// ── Hash Function Logic ────────────────────────────────────────────────────

/// Equivalent of `test_pg_stream_hash_function_exists` and
/// `test_pg_stream_hash_deterministic` pg_tests.
///
/// Since the `pgstream.pg_stream_hash()` SQL function is a pgrx-generated wrapper
/// around `xxhash_rust::xxh64`, we test the PostgreSQL-native `hashtext()`
/// as the hash mechanism used in our storage tables, and independently
/// verify that xxh64 is deterministic.
#[tokio::test]
async fn test_xxhash_deterministic() {
    // Verify xxh64 directly (the Rust implementation used by pg_stream_hash)
    use xxhash_rust::xxh64;
    let h1 = xxh64::xxh64(b"hello", 0) as i64;
    let h2 = xxh64::xxh64(b"hello", 0) as i64;
    assert_eq!(h1, h2, "xxh64 should be deterministic");
    assert_ne!(h1, 0, "xxh64 should produce a non-zero hash for 'hello'");

    // Different inputs should produce different hashes
    let h3 = xxh64::xxh64(b"world", 0) as i64;
    assert_ne!(h1, h3, "Different inputs should produce different hashes");
}

/// Verify that PostgreSQL's hashtext (used in the refresh SQL) also
/// works as a fallback hashing mechanism inside a real container.
#[tokio::test]
async fn test_pg_hashtext_works_in_container() {
    let db = TestDb::new().await;

    let h1: i32 = db.query_scalar("SELECT hashtext('hello')").await;
    let h2: i32 = db.query_scalar("SELECT hashtext('hello')").await;
    assert_eq!(h1, h2, "hashtext should be deterministic");

    let h3: i32 = db.query_scalar("SELECT hashtext('world')").await;
    assert_ne!(h1, h3, "Different inputs should produce different hashes");
}

// ── Column & Constraint Verification ───────────────────────────────────────

/// Verify the pgs_stream_tables table has all expected columns including
/// the Phase 8 frontier column.
#[tokio::test]
async fn test_stream_tables_columns() {
    let db = TestDb::with_catalog().await;

    let expected_columns = [
        "pgs_id",
        "pgs_relid",
        "pgs_name",
        "pgs_schema",
        "defining_query",
        "schedule",
        "refresh_mode",
        "status",
        "is_populated",
        "data_timestamp",
        "frontier",
        "last_refresh_at",
        "consecutive_errors",
        "needs_reinit",
        "created_at",
        "updated_at",
    ];

    for col in expected_columns {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS(SELECT 1 FROM information_schema.columns \
                 WHERE table_schema = 'pgstream' AND table_name = 'pgs_stream_tables' \
                 AND column_name = '{}')",
                col
            ))
            .await;
        assert!(
            exists,
            "Column pgstream.pgs_stream_tables.{} should exist",
            col
        );
    }
}

/// Verify that the frontier column accepts valid JSONB and stores/retrieves it.
#[tokio::test]
async fn test_frontier_jsonb_column() {
    let db = TestDb::with_catalog().await;

    db.execute("CREATE TABLE fsrc (id INT PRIMARY KEY)").await;
    let oid: i32 = db.query_scalar("SELECT 'fsrc'::regclass::oid::int").await;

    // Insert a ST with a frontier
    let frontier_json = r#"{"sources":{"12345":{"lsn":"0/1A2B","snapshot_ts":"2026-02-17T10:00:00Z"}},"data_timestamp":"2026-02-17T10:00:00Z"}"#;
    db.execute(&format!(
        "INSERT INTO pgstream.pgs_stream_tables \
         (pgs_relid, pgs_name, pgs_schema, defining_query, refresh_mode, frontier) \
         VALUES ({}, 'frontier_dt', 'public', 'SELECT 1', 'FULL', '{}'::jsonb)",
        oid,
        frontier_json.replace('\'', "''")
    ))
    .await;

    // Read it back
    let stored: serde_json::Value = db
        .query_scalar(
            "SELECT frontier FROM pgstream.pgs_stream_tables WHERE pgs_name = 'frontier_dt'",
        )
        .await;

    assert_eq!(
        stored["data_timestamp"].as_str(),
        Some("2026-02-17T10:00:00Z")
    );
    assert_eq!(stored["sources"]["12345"]["lsn"].as_str(), Some("0/1A2B"));

    // Update the frontier
    let new_frontier = r#"{"sources":{"12345":{"lsn":"0/3C4D","snapshot_ts":"2026-02-17T11:00:00Z"}},"data_timestamp":"2026-02-17T11:00:00Z"}"#;
    db.execute(&format!(
        "UPDATE pgstream.pgs_stream_tables SET frontier = '{}'::jsonb WHERE pgs_name = 'frontier_dt'",
        new_frontier.replace('\'', "''")
    ))
    .await;

    let updated: serde_json::Value = db
        .query_scalar(
            "SELECT frontier FROM pgstream.pgs_stream_tables WHERE pgs_name = 'frontier_dt'",
        )
        .await;
    assert_eq!(updated["sources"]["12345"]["lsn"].as_str(), Some("0/3C4D"));
}

// ── Logical Decoding Prerequisites ─────────────────────────────────────────

/// Verify the container supports pg_lsn type (needed for change buffer tables).
#[tokio::test]
async fn test_pg_lsn_type_works() {
    let db = TestDb::new().await;

    let lsn: String = db.query_scalar("SELECT '0/1A2B3C4D'::pg_lsn::text").await;
    assert_eq!(lsn, "0/1A2B3C4D");

    // LSN comparison
    let gt: bool = db
        .query_scalar("SELECT '0/2'::pg_lsn > '0/1'::pg_lsn")
        .await;
    assert!(gt, "LSN 0/2 should be > 0/1");
}
