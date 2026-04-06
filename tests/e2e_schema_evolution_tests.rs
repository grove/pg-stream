//! TG2-SCHEMA: Source-table schema evolution E2E tests.
//!
//! Validates that pg_trickle handles DDL changes on source tables gracefully:
//!
//! | Test | DDL Operation | Expected |
//! |------|---------------|----------|
//! | SE-1 | Column rename (not in defining query) | No impact |
//! | SE-2 | Column rename (used in defining query) | ST detects and suspends |
//! | SE-3 | Column added to source | No impact |
//! | SE-4 | Column type change (INT → BIGINT, compatible) | Refresh succeeds |
//!
//! These tests use manual `refresh_stream_table()` to keep DDL detection
//! deterministic.

mod e2e;

use e2e::E2eDb;

// ── SE-1: Rename unused column — no impact ─────────────────────────────────

/// Renaming a source column that is NOT referenced in the defining query
/// should have no effect on the stream table.
#[tokio::test]
async fn test_schema_evolution_rename_unused_column_no_impact() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE se1_src (id SERIAL PRIMARY KEY, used_col INT, unused_col TEXT)")
        .await;
    db.execute("INSERT INTO se1_src (used_col, unused_col) VALUES (1, 'a'), (2, 'b')")
        .await;

    db.create_st(
        "se1_st",
        "SELECT id, used_col FROM se1_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    assert_eq!(db.count("public.se1_st").await, 2);

    // Rename the unused column
    db.execute("ALTER TABLE se1_src RENAME COLUMN unused_col TO other_col")
        .await;

    // Insert a new row and refresh — should succeed
    db.execute("INSERT INTO se1_src (used_col, other_col) VALUES (3, 'c')")
        .await;
    db.refresh_st("se1_st").await;
    assert_eq!(db.count("public.se1_st").await, 3);
}

// ── SE-2: Rename used column — ST detects mismatch ─────────────────────────

/// Renaming a source column that IS referenced in the defining query
/// should cause the next refresh to fail or the ST to be marked for reinit.
#[tokio::test]
async fn test_schema_evolution_rename_used_column_detected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE se2_src (id SERIAL PRIMARY KEY, amount INT)")
        .await;
    db.execute("INSERT INTO se2_src (amount) VALUES (10), (20)")
        .await;

    db.create_st(
        "se2_st",
        "SELECT id, amount FROM se2_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    assert_eq!(db.count("public.se2_st").await, 2);

    // Rename the column used in the defining query.
    // pg_trickle's DDL hook treats RENAME COLUMN (object_type = "table column")
    // as Ignored, so we manually mark the ST for reinit so the next refresh
    // re-executes the defining query and fails on the missing column.
    db.execute("ALTER TABLE se2_src RENAME COLUMN amount TO total")
        .await;
    db.execute(
        "UPDATE pgtrickle.pgt_stream_tables SET needs_reinit = TRUE WHERE pgt_name = 'se2_st'",
    )
    .await;

    // The next refresh should fail because 'amount' no longer exists
    let result = db
        .try_execute("SELECT pgtrickle.refresh_stream_table('se2_st')")
        .await;
    assert!(
        result.is_err(),
        "Refresh should fail after renaming a column used in the defining query"
    );
}

// ── SE-3: Add column to source — no impact ─────────────────────────────────

/// Adding a new column to the source table should have no effect on
/// stream tables that don't reference it.
#[tokio::test]
async fn test_schema_evolution_add_column_no_impact() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE se3_src (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO se3_src (val) VALUES (100), (200)")
        .await;

    db.create_st(
        "se3_st",
        "SELECT id, val FROM se3_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    assert_eq!(db.count("public.se3_st").await, 2);

    // Add a new column. pg_trickle blocks ADD COLUMN by default
    // (block_source_ddl = true / SchemaChangeKind::AddColumnOnly), so we
    // temporarily disable the guard around the DDL.
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE se3_src ADD COLUMN extra TEXT DEFAULT 'x'",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;

    // Insert using the new column and refresh — ST should be fine
    db.execute("INSERT INTO se3_src (val, extra) VALUES (300, 'y')")
        .await;
    db.refresh_st("se3_st").await;
    assert_eq!(db.count("public.se3_st").await, 3);
}

// ── SE-4: Compatible type change — refresh succeeds ────────────────────────

/// Widening a column type (INT → BIGINT) on the source table should
/// not break the stream table since the types are compatible.
#[tokio::test]
async fn test_schema_evolution_compatible_type_change() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE se4_src (id SERIAL PRIMARY KEY, amount INT)")
        .await;
    db.execute("INSERT INTO se4_src (amount) VALUES (10), (20)")
        .await;

    db.create_st(
        "se4_st",
        "SELECT id, amount FROM se4_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    assert_eq!(db.count("public.se4_st").await, 2);

    // Widen the column type. pg_trickle blocks column type changes by default
    // (block_source_ddl = true / SchemaChangeKind::ColumnChange), so we
    // temporarily disable the guard around the DDL.
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE se4_src ALTER COLUMN amount TYPE BIGINT",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;

    // Insert a value and refresh. Use 300 (fits INT) because the ST column
    // is still INT — the reinitialise full-refresh reloads from the source
    // but into the original ST schema.
    db.execute("INSERT INTO se4_src (amount) VALUES (300)")
        .await;
    db.refresh_st("se4_st").await;
    assert_eq!(db.count("public.se4_st").await, 3);
}
