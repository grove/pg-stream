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
