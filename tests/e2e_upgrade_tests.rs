//! E2E tests for extension upgrade/migration safety (Category L).
//!
//! Validates that extension lifecycle operations (DROP + CREATE, schema
//! verification) don't break existing data or leave orphaned objects.
//!
//! True version-to-version upgrade tests require two Docker images (old
//! version and new version). These tests cover the foundational upgrade
//! safety properties using the current single version.
//!
//! See `plans/sql/PLAN_UPGRADE_MIGRATIONS.md` for the full upgrade strategy.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ══════════════════════════════════════════════════════════════════════
// L1 — Catalog schema matches expected columns
// ══════════════════════════════════════════════════════════════════════

/// Verify that the `pgt_stream_tables` catalog table has all expected
/// columns with correct types. This catches accidental schema drift
/// and ensures migration scripts know the baseline schema.
#[tokio::test]
async fn test_upgrade_catalog_schema_stability() {
    let db = E2eDb::new().await.with_extension().await;

    // Expected columns in pgt_stream_tables (alphabetical)
    let expected_columns = vec![
        ("auto_threshold", "double precision"),
        ("consecutive_errors", "integer"),
        ("created_at", "timestamp with time zone"),
        ("data_timestamp", "timestamp with time zone"),
        ("defining_query", "text"),
        ("diamond_consistency", "text"),
        ("diamond_schedule_policy", "text"),
        ("frontier", "jsonb"),
        ("functions_used", "ARRAY"),
        ("is_populated", "boolean"),
        ("last_full_ms", "double precision"),
        ("last_refresh_at", "timestamp with time zone"),
        ("needs_reinit", "boolean"),
        ("original_query", "text"),
        ("pgt_id", "bigint"),
        ("pgt_name", "text"),
        ("pgt_relid", "oid"),
        ("pgt_schema", "text"),
        ("refresh_mode", "text"),
        ("schedule", "text"),
        ("status", "text"),
        ("topk_limit", "integer"),
        ("topk_order_by", "text"),
        ("updated_at", "timestamp with time zone"),
    ];

    for (col_name, expected_type) in &expected_columns {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS( \
                    SELECT 1 FROM information_schema.columns \
                    WHERE table_schema = 'pgtrickle' \
                      AND table_name = 'pgt_stream_tables' \
                      AND column_name = '{col_name}' \
                      AND data_type LIKE '%{expected_type}%' \
                )"
            ))
            .await;
        assert!(
            exists,
            "Column '{col_name}' with type containing '{expected_type}' not found in catalog"
        );
    }

    // Verify total column count to detect unexpected additions
    let col_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = 'pgtrickle' AND table_name = 'pgt_stream_tables'",
        )
        .await;
    assert_eq!(
        col_count,
        expected_columns.len() as i64,
        "Unexpected number of columns in pgt_stream_tables (schema drift?)"
    );
}

// ══════════════════════════════════════════════════════════════════════
// L2 — Catalog indexes survive extension lifecycle
// ══════════════════════════════════════════════════════════════════════

/// Verify that expected indexes exist on catalog tables.
#[tokio::test]
async fn test_upgrade_catalog_indexes_present() {
    let db = E2eDb::new().await.with_extension().await;

    let expected_indexes = vec![
        ("pgt_stream_tables", "idx_pgt_status"),
        ("pgt_stream_tables", "idx_pgt_name"),
        ("pgt_dependencies", "idx_deps_source"),
    ];

    for (table, index) in &expected_indexes {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS( \
                    SELECT 1 FROM pg_indexes \
                    WHERE schemaname = 'pgtrickle' \
                      AND tablename = '{table}' \
                      AND indexname = '{index}' \
                )"
            ))
            .await;
        assert!(exists, "Index '{index}' on '{table}' not found");
    }
}

// ══════════════════════════════════════════════════════════════════════
// L3 — DROP EXTENSION CASCADE + CREATE EXTENSION round-trip
// ══════════════════════════════════════════════════════════════════════

/// Simulate the destructive upgrade path: DROP EXTENSION CASCADE + fresh
/// CREATE EXTENSION. After round-trip, the extension must be fully
/// functional (create ST, refresh, verify).
#[tokio::test]
async fn test_upgrade_drop_recreate_roundtrip() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a source + ST to have data in the system
    db.execute("CREATE TABLE lr_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO lr_src VALUES (1, 'hello'), (2, 'world')")
        .await;
    db.create_st("lr_st", "SELECT id, val FROM lr_src", "1m", "FULL")
        .await;
    assert_eq!(db.count("public.lr_st").await, 2);

    // DROP EXTENSION CASCADE — this destroys all pg_trickle objects
    db.execute("DROP EXTENSION pg_trickle CASCADE").await;

    // Verify cleanup: catalog tables gone
    let schemas_exist: bool = db
        .query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM information_schema.schemata \
                WHERE schema_name = 'pgtrickle' \
            )",
        )
        .await;
    assert!(!schemas_exist, "pgtrickle schema should be dropped");

    // Source table must survive the DROP EXTENSION
    assert_eq!(db.count("lr_src").await, 2);

    // Re-create extension
    db.execute("CREATE EXTENSION pg_trickle CASCADE").await;

    // Verify extension is fully functional after round-trip
    db.create_st(
        "lr_st_new",
        "SELECT id, val FROM lr_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    assert_eq!(db.count("public.lr_st_new").await, 2);

    // Mutate and refresh
    db.execute("INSERT INTO lr_src VALUES (3, 'again')").await;
    db.refresh_st("lr_st_new").await;
    db.assert_st_matches_query("public.lr_st_new", "SELECT id, val FROM lr_src")
        .await;
}

// ══════════════════════════════════════════════════════════════════════
// L4 — Extension version matches Cargo.toml
// ══════════════════════════════════════════════════════════════════════

/// Verify the installed extension version matches the expected version
/// from Cargo.toml. This catches version mismatch between the binary
/// and the control file.
#[tokio::test]
async fn test_upgrade_extension_version_consistency() {
    let db = E2eDb::new().await.with_extension().await;

    let version: String = db
        .query_scalar("SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle'")
        .await;

    // The version should be a valid semver string
    assert!(
        version.contains('.'),
        "Extension version '{}' doesn't look like semver",
        version
    );

    // Verify it's accessible via our version function too
    let fn_version: String = db.query_scalar("SELECT pgtrickle.version()").await;
    assert_eq!(
        version, fn_version,
        "pg_extension version ({}) must match pgtrickle.version() ({})",
        version, fn_version
    );
}

// ══════════════════════════════════════════════════════════════════════
// L5 — Dependencies table schema stability
// ══════════════════════════════════════════════════════════════════════

/// Verify that `pgt_dependencies` has all expected columns. Upgrade
/// migrations must know this schema to add/alter columns safely.
#[tokio::test]
async fn test_upgrade_dependencies_schema_stability() {
    let db = E2eDb::new().await.with_extension().await;

    let expected_dep_columns = vec![
        "pgt_id",
        "source_relid",
        "source_type",
        "columns_used",
        "column_snapshot",
        "schema_fingerprint",
        "cdc_mode",
        "slot_name",
        "decoder_confirmed_lsn",
        "transition_started_at",
    ];

    for col_name in &expected_dep_columns {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS( \
                    SELECT 1 FROM information_schema.columns \
                    WHERE table_schema = 'pgtrickle' \
                      AND table_name = 'pgt_dependencies' \
                      AND column_name = '{col_name}' \
                )"
            ))
            .await;
        assert!(exists, "Column '{col_name}' not found in pgt_dependencies");
    }
}

// ══════════════════════════════════════════════════════════════════════
// L6 — Event triggers survive extension lifecycle
// ══════════════════════════════════════════════════════════════════════

/// Verify event triggers are properly installed and functional after
/// CREATE EXTENSION. These triggers are critical for DDL monitoring.
#[tokio::test]
async fn test_upgrade_event_triggers_installed() {
    let db = E2eDb::new().await.with_extension().await;

    let triggers = vec!["pg_trickle_ddl_tracker", "pg_trickle_drop_tracker"];

    for trigger_name in &triggers {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS( \
                    SELECT 1 FROM pg_event_trigger \
                    WHERE evtname = '{trigger_name}' \
                )"
            ))
            .await;
        assert!(
            exists,
            "Event trigger '{trigger_name}' not found after CREATE EXTENSION"
        );
    }
}

// ══════════════════════════════════════════════════════════════════════
// L7 — Views survive extension lifecycle
// ══════════════════════════════════════════════════════════════════════

/// Verify that monitoring views are created and queryable.
#[tokio::test]
async fn test_upgrade_monitoring_views_present() {
    let db = E2eDb::new().await.with_extension().await;

    let views = vec![
        "pgtrickle.stream_tables_info",
        "pgtrickle.pg_stat_stream_tables",
    ];

    for view in &views {
        // Just verify the view is queryable (no rows expected)
        let count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM {view}"))
            .await;
        assert_eq!(count, 0, "View {view} should be empty but queryable");
    }
}
