//! E2E tests for DDL event trigger behavior.
//!
//! Validates that event triggers detect source table drops, alters,
//! and direct manipulation of ST storage tables.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

#[tokio::test]
async fn test_drop_source_fires_event_trigger() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE evt_drop_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO evt_drop_src VALUES (1, 'data')")
        .await;

    db.create_st(
        "evt_drop_st",
        "SELECT id, val FROM evt_drop_src",
        "1m",
        "FULL",
    )
    .await;

    // Verify event triggers are installed
    let ddl_trigger: bool = db
        .query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM pg_event_trigger WHERE evtname = 'pg_trickle_ddl_tracker' \
            )",
        )
        .await;
    assert!(ddl_trigger, "DDL event trigger should be installed");

    let drop_trigger: bool = db
        .query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM pg_event_trigger WHERE evtname = 'pg_trickle_drop_tracker' \
            )",
        )
        .await;
    assert!(drop_trigger, "Drop event trigger should be installed");

    // Drop the source table — event trigger should fire
    let result = db.try_execute("DROP TABLE evt_drop_src CASCADE").await;

    // The event trigger should handle this gracefully
    // Whether it succeeds or is prevented depends on implementation
    if result.is_ok() {
        // If allowed, the ST catalog entry may still exist with status=ERROR,
        // or the storage table may have been cascade-dropped too (cleaning up the catalog).
        let st_count: i64 = db
            .query_scalar(
                "SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'evt_drop_st'",
            )
            .await;
        if st_count > 0 {
            // The event trigger sets status to ERROR when a source is dropped
            let status: String = db
                .query_scalar(
                    "SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'evt_drop_st'",
                )
                .await;
            assert_eq!(
                status, "ERROR",
                "ST should be set to ERROR after source drop"
            );
        }
        // If st_count == 0, CASCADE dropped the storage table too,
        // and the drop event trigger cleaned up the catalog — also valid.
    }
    // If result is Err, the extension prevented the drop — that's valid too
}

#[tokio::test]
async fn test_alter_source_fires_event_trigger() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE evt_alter_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO evt_alter_src VALUES (1, 'data')")
        .await;

    db.create_st(
        "evt_alter_st",
        "SELECT id, val FROM evt_alter_src",
        "1m",
        "FULL",
    )
    .await;

    // ALTER the source table — event trigger should fire
    db.execute("ALTER TABLE evt_alter_src ADD COLUMN extra INT")
        .await;

    // ST should still be queryable (the added column isn't part of the defining query)
    let count = db.count("public.evt_alter_st").await;
    assert_eq!(count, 1, "ST should still be valid after compatible ALTER");
}

#[tokio::test]
async fn test_drop_st_storage_by_sql() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE evt_storage_src (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO evt_storage_src VALUES (1)").await;

    db.create_st(
        "evt_storage_st",
        "SELECT id FROM evt_storage_src",
        "1m",
        "FULL",
    )
    .await;

    // Drop the ST storage table directly (bypassing pgtrickle.drop_stream_table)
    let result = db
        .try_execute("DROP TABLE public.evt_storage_st CASCADE")
        .await;

    if result.is_ok() {
        // The event trigger should have cleaned up the catalog
        // Give a tiny moment for event trigger processing
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let cat_count: i64 = db
            .query_scalar(
                "SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'evt_storage_st'",
            )
            .await;
        assert_eq!(
            cat_count, 0,
            "Catalog entry should be cleaned up by event trigger"
        );
    }
    // If the DROP fails, the extension is protecting its tables — also valid
}

#[tokio::test]
async fn test_rename_source_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE evt_rename_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO evt_rename_src VALUES (1, 'data')")
        .await;

    db.create_st(
        "evt_rename_st",
        "SELECT id, val FROM evt_rename_src",
        "1m",
        "FULL",
    )
    .await;

    // Rename the source table — triggers DDL event
    db.execute("ALTER TABLE evt_rename_src RENAME TO evt_renamed_src")
        .await;

    // The ST may or may not still work after renaming the source.
    // The defining query still references 'evt_rename_src' which is now gone.
    // Refresh should reveal the problem.
    let result = db
        .try_execute("SELECT pgtrickle.refresh_stream_table('evt_rename_st')")
        .await;

    // After renaming source, refresh with old name should fail
    assert!(
        result.is_err(),
        "Refresh should fail after source table rename since defining query references old name"
    );
}

/// F18: CREATE OR REPLACE FUNCTION on a function used by a DIFFERENTIAL
/// stream table should mark the ST for reinitialize.
#[tokio::test]
async fn test_function_change_marks_st_for_reinit() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a custom function
    db.execute(
        "CREATE FUNCTION evt_double(x INT) RETURNS INT AS $$ SELECT x * 2 $$ LANGUAGE SQL IMMUTABLE",
    )
    .await;

    db.execute("CREATE TABLE evt_func_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO evt_func_src VALUES (1, 10), (2, 20)")
        .await;

    db.create_st(
        "evt_func_st",
        "SELECT id, evt_double(val) AS doubled FROM evt_func_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Verify initial data
    let count = db.count("public.evt_func_st").await;
    assert_eq!(count, 2);

    // Verify functions_used was populated
    let func_count: i64 = db
        .query_scalar(
            "SELECT coalesce(array_length(functions_used, 1), 0)::bigint \
             FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'evt_func_st'",
        )
        .await;
    assert!(
        func_count > 0,
        "functions_used should be populated for DIFFERENTIAL STs"
    );

    // Check that evt_double is in the list
    let has_func: bool = db
        .query_scalar(
            "SELECT functions_used @> ARRAY['evt_double']::text[] \
             FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'evt_func_st'",
        )
        .await;
    assert!(has_func, "functions_used should contain 'evt_double'");

    // Replace the function with a different implementation
    db.execute(
        "CREATE OR REPLACE FUNCTION evt_double(x INT) RETURNS INT AS $$ SELECT x * 3 $$ LANGUAGE SQL IMMUTABLE",
    )
    .await;

    // The DDL hook should have marked the ST for reinit
    let needs_reinit: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'evt_func_st'",
        )
        .await;
    assert!(
        needs_reinit,
        "ST should be marked for reinit after function replacement"
    );
}

/// F18: DROP FUNCTION on a function used by a stream table should mark
/// the ST for reinit.
#[tokio::test]
async fn test_drop_function_marks_st_for_reinit() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE FUNCTION evt_triple(x INT) RETURNS INT AS $$ SELECT x * 3 $$ LANGUAGE SQL IMMUTABLE",
    )
    .await;

    db.execute("CREATE TABLE evt_dfunc_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO evt_dfunc_src VALUES (1, 5)").await;

    db.create_st(
        "evt_dfunc_st",
        "SELECT id, evt_triple(val) AS tripled FROM evt_dfunc_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let count = db.count("public.evt_dfunc_st").await;
    assert_eq!(count, 1);

    // Drop the function (CASCADE to avoid dependency errors)
    let _ = db
        .try_execute("DROP FUNCTION evt_triple(INT) CASCADE")
        .await;

    // The drop hook should have marked the ST for reinit
    let needs_reinit: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'evt_dfunc_st'",
        )
        .await;
    assert!(
        needs_reinit,
        "ST should be marked for reinit after function drop"
    );
}

// ══════════════════════════════════════════════════════════════════════
// B1 — ADD COLUMN on a monitored source
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_add_column_on_source_st_still_functional() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ddl_add_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO ddl_add_src VALUES (1, 10), (2, 20)")
        .await;

    db.create_st(
        "ddl_add_st",
        "SELECT id, val FROM ddl_add_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Add a column that is NOT in the defining query
    db.execute("ALTER TABLE ddl_add_src ADD COLUMN extra TEXT")
        .await;

    // After the column change, the ST should still be refreshable
    db.refresh_st("ddl_add_st").await;
    let count: i64 = db
        .query_scalar("SELECT count(*) FROM public.ddl_add_st")
        .await;
    assert_eq!(
        count, 2,
        "ST should still have all rows after ADD COLUMN + refresh"
    );
}

#[tokio::test]
async fn test_add_column_unused_st_survives_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ddl_add2_src (id INT PRIMARY KEY, a INT)")
        .await;
    db.execute("INSERT INTO ddl_add2_src VALUES (1, 1)").await;

    db.create_st(
        "ddl_add2_st",
        "SELECT id, a FROM ddl_add2_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Add column 'b' (not used)
    db.execute("ALTER TABLE ddl_add2_src ADD COLUMN b INT DEFAULT 0")
        .await;
    db.execute("UPDATE ddl_add2_src SET b = 99 WHERE id = 1")
        .await;
    db.refresh_st("ddl_add2_st").await;

    // ST should have 1 row with the original columns intact
    let count: i64 = db
        .query_scalar("SELECT count(*) FROM public.ddl_add2_st")
        .await;
    assert_eq!(count, 1);
    let a_val: i32 = db
        .query_scalar("SELECT a FROM public.ddl_add2_st WHERE id = 1")
        .await;
    assert_eq!(a_val, 1);
}

// ══════════════════════════════════════════════════════════════════════
// B2 — DROP COLUMN not referenced in query → ST remains functional
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_drop_unused_column_st_survives() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ddl_drop_col_src (id INT PRIMARY KEY, used INT, unused TEXT)")
        .await;
    db.execute("INSERT INTO ddl_drop_col_src VALUES (1, 10, 'x'), (2, 20, 'y')")
        .await;

    db.create_st(
        "ddl_drop_col_st",
        "SELECT id, used FROM ddl_drop_col_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Drop the column that is NOT in the defining query
    db.execute("ALTER TABLE ddl_drop_col_src DROP COLUMN unused")
        .await;

    let status: String = db
        .query_scalar(
            "SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ddl_drop_col_st'",
        )
        .await;
    assert_ne!(
        status, "ERROR",
        "ST should not be in ERROR after unused column drop"
    );

    db.refresh_st("ddl_drop_col_st").await;
    let count: i64 = db
        .query_scalar("SELECT count(*) FROM public.ddl_drop_col_st")
        .await;
    assert_eq!(
        count, 2,
        "ST should still have 2 rows after dropping unused column"
    );
}

// ══════════════════════════════════════════════════════════════════════
// B3 — ALTER COLUMN TYPE on a used column → reinit
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_alter_column_type_triggers_reinit() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ddl_type_src (id INT PRIMARY KEY, score INT)")
        .await;
    db.execute("INSERT INTO ddl_type_src VALUES (1, 10), (2, 20)")
        .await;

    db.create_st(
        "ddl_type_st",
        "SELECT id, score FROM ddl_type_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Change type of 'score' — column is in defining query
    db.execute("ALTER TABLE ddl_type_src ALTER COLUMN score TYPE BIGINT")
        .await;

    let needs_reinit: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ddl_type_st'",
        )
        .await;
    assert!(
        needs_reinit,
        "ST should be marked for reinit after column type change"
    );
}

// ══════════════════════════════════════════════════════════════════════
// B4 — CREATE INDEX on source → Benign, no reinit
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_create_index_on_source_is_benign() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ddl_idx_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO ddl_idx_src VALUES (1, 10)").await;

    db.create_st(
        "ddl_idx_st",
        "SELECT id, val FROM ddl_idx_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // CREATE INDEX is DDL but purely structural — should be benign
    db.execute("CREATE INDEX ON ddl_idx_src (val)").await;

    let needs_reinit: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ddl_idx_st'",
        )
        .await;
    assert!(
        !needs_reinit,
        "CREATE INDEX on source should not trigger reinit"
    );

    // ST should still be functional
    let count: i64 = db
        .query_scalar("SELECT count(*) FROM public.ddl_idx_st")
        .await;
    assert_eq!(count, 1);
}

// ══════════════════════════════════════════════════════════════════════
// B5 — DROP source with multiple downstream STs
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_drop_source_with_multiple_downstream_sts() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ddl_multi_src (id INT PRIMARY KEY, v INT)")
        .await;
    db.execute("INSERT INTO ddl_multi_src VALUES (1, 1), (2, 2)")
        .await;

    db.create_st(
        "ddl_multi_st1",
        "SELECT id, v FROM ddl_multi_src",
        "1m",
        "FULL",
    )
    .await;
    db.create_st(
        "ddl_multi_st2",
        "SELECT id, v * 2 AS v2 FROM ddl_multi_src",
        "1m",
        "FULL",
    )
    .await;

    let result = db.try_execute("DROP TABLE ddl_multi_src CASCADE").await;

    if result.is_ok() {
        // Both STs should either be gone (cascade) or in ERROR
        for st in ["ddl_multi_st1", "ddl_multi_st2"] {
            let status_opt: Option<String> = db
                .query_scalar_opt(&format!(
                    "SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '{st}'"
                ))
                .await;
            if let Some(status) = status_opt {
                assert_eq!(status, "ERROR", "{st} should be in ERROR after source DROP");
            }
            // If None: cascade cleaned up the catalog entry — also valid
        }
    }
    // If result is Err, the extension prevented the drop — also valid
}

// ══════════════════════════════════════════════════════════════════════
// B6 — pg_trickle.block_source_ddl GUC
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_block_source_ddl_guc_prevents_alter() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ddl_block_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO ddl_block_src VALUES (1, 1)").await;

    db.create_st(
        "ddl_block_st",
        "SELECT id, val FROM ddl_block_src",
        "1m",
        "FULL",
    )
    .await;

    // Enable blocking GUC
    db.execute("SET pg_trickle.block_source_ddl = true").await;

    // ALTER on a monitored source should now return an error
    let result = db
        .try_execute("ALTER TABLE ddl_block_src ADD COLUMN extra TEXT")
        .await;
    assert!(
        result.is_err(),
        "ALTER on monitored source should be blocked when block_source_ddl = true"
    );
}

// ══════════════════════════════════════════════════════════════════════
// B7 — Column change on a joined source
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_add_column_on_joined_source_st_survives() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ddl_nj_a (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE ddl_nj_b (id INT PRIMARY KEY, score INT)")
        .await;
    db.execute("INSERT INTO ddl_nj_a VALUES (1, 'x'), (2, 'y')")
        .await;
    db.execute("INSERT INTO ddl_nj_b VALUES (1, 10), (2, 20)")
        .await;

    db.create_st(
        "ddl_nj_st",
        "SELECT a.id, a.name, b.score FROM ddl_nj_a a JOIN ddl_nj_b b ON a.id = b.id",
        "1m",
        "FULL",
    )
    .await;
    assert_eq!(db.count("public.ddl_nj_st").await, 2);

    // ADD COLUMN to one source
    db.execute("ALTER TABLE ddl_nj_a ADD COLUMN extra TEXT")
        .await;

    let status: String = db
        .query_scalar("SELECT status FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ddl_nj_st'")
        .await;
    assert_ne!(
        status, "ERROR",
        "ST should remain valid after adding unused column to joined source"
    );

    // Refresh should succeed
    db.refresh_st("ddl_nj_st").await;
    assert_eq!(db.count("public.ddl_nj_st").await, 2);
}
