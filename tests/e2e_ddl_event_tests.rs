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
                SELECT 1 FROM pg_event_trigger WHERE evtname = 'pg_stream_ddl_tracker' \
            )",
        )
        .await;
    assert!(ddl_trigger, "DDL event trigger should be installed");

    let drop_trigger: bool = db
        .query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM pg_event_trigger WHERE evtname = 'pg_stream_drop_tracker' \
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
                "SELECT count(*) FROM pgstream.pgs_stream_tables WHERE pgs_name = 'evt_drop_st'",
            )
            .await;
        if st_count > 0 {
            // The event trigger sets status to ERROR when a source is dropped
            let status: String = db
                .query_scalar(
                    "SELECT status FROM pgstream.pgs_stream_tables WHERE pgs_name = 'evt_drop_st'",
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

    // Drop the ST storage table directly (bypassing pgstream.drop_stream_table)
    let result = db
        .try_execute("DROP TABLE public.evt_storage_st CASCADE")
        .await;

    if result.is_ok() {
        // The event trigger should have cleaned up the catalog
        // Give a tiny moment for event trigger processing
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let cat_count: i64 = db
            .query_scalar(
                "SELECT count(*) FROM pgstream.pgs_stream_tables WHERE pgs_name = 'evt_storage_st'",
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
        .try_execute("SELECT pgstream.refresh_stream_table('evt_rename_st')")
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
            "SELECT coalesce(array_length(functions_used, 1), 0) \
             FROM pgstream.pgs_stream_tables WHERE pgs_name = 'evt_func_st'",
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
             FROM pgstream.pgs_stream_tables WHERE pgs_name = 'evt_func_st'",
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
            "SELECT needs_reinit FROM pgstream.pgs_stream_tables WHERE pgs_name = 'evt_func_st'",
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
            "SELECT needs_reinit FROM pgstream.pgs_stream_tables WHERE pgs_name = 'evt_dfunc_st'",
        )
        .await;
    assert!(
        needs_reinit,
        "ST should be marked for reinit after function drop"
    );
}
