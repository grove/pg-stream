//! E2E tests for `pgstream.drop_stream_table()`.
//!
//! Validates that dropping a ST cleans up the storage table, catalog entries,
//! dependencies, CDC triggers, and change tracking — while preserving
//! shared resources when other STs still depend on them.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Basic Drop Cleanup ─────────────────────────────────────────────────

#[tokio::test]
async fn test_drop_removes_storage_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dr_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO dr_src VALUES (1, 'a')").await;

    db.create_st("dr_st", "SELECT id, val FROM dr_src", "1m", "FULL")
        .await;

    assert!(db.table_exists("public", "dr_st").await);

    db.drop_st("dr_st").await;

    // Storage table should be gone
    let result = db.try_execute("SELECT * FROM public.dr_st").await;
    assert!(result.is_err(), "Querying dropped ST storage should fail");
}

#[tokio::test]
async fn test_drop_removes_catalog_entry() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dr_cat (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO dr_cat VALUES (1)").await;

    db.create_st("dr_cat_st", "SELECT id FROM dr_cat", "1m", "FULL")
        .await;

    let before: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgstream.pgs_stream_tables WHERE pgs_name = 'dr_cat_st'",
        )
        .await;
    assert_eq!(before, 1);

    db.drop_st("dr_cat_st").await;

    let after: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgstream.pgs_stream_tables WHERE pgs_name = 'dr_cat_st'",
        )
        .await;
    assert_eq!(after, 0, "Catalog entry should be removed");
}

#[tokio::test]
async fn test_drop_removes_dependencies() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dr_dep (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO dr_dep VALUES (1)").await;

    db.create_st("dr_dep_st", "SELECT id FROM dr_dep", "1m", "FULL")
        .await;

    let pgs_id: i64 = db
        .query_scalar("SELECT pgs_id FROM pgstream.pgs_stream_tables WHERE pgs_name = 'dr_dep_st'")
        .await;

    let deps_before: i64 = db
        .query_scalar(&format!(
            "SELECT count(*) FROM pgstream.pgs_dependencies WHERE pgs_id = {}",
            pgs_id
        ))
        .await;
    assert!(
        deps_before >= 1,
        "Should have dependency entries before drop"
    );

    db.drop_st("dr_dep_st").await;

    let deps_after: i64 = db
        .query_scalar(&format!(
            "SELECT count(*) FROM pgstream.pgs_dependencies WHERE pgs_id = {}",
            pgs_id
        ))
        .await;
    assert_eq!(deps_after, 0, "Dependencies should be removed on drop");
}

#[tokio::test]
async fn test_drop_removes_cdc_trigger() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dr_trig (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO dr_trig VALUES (1)").await;

    db.create_st("dr_trig_st", "SELECT id FROM dr_trig", "1m", "DIFFERENTIAL")
        .await;

    let source_oid = db.table_oid("dr_trig").await;
    let trigger_name = format!("pg_stream_cdc_{}", source_oid);

    assert!(
        db.trigger_exists(&trigger_name, "dr_trig").await,
        "Trigger should exist before drop"
    );

    db.drop_st("dr_trig_st").await;

    assert!(
        !db.trigger_exists(&trigger_name, "dr_trig").await,
        "Trigger should be removed after dropping sole consumer"
    );
}

#[tokio::test]
async fn test_drop_preserves_trigger_for_other_st() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dr_shared (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO dr_shared VALUES (1, 'a'), (2, 'b')")
        .await;

    // Create two STs on the same source
    db.create_st(
        "st_shared_1",
        "SELECT id, val FROM dr_shared",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "st_shared_2",
        "SELECT id FROM dr_shared",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let source_oid = db.table_oid("dr_shared").await;
    let trigger_name = format!("pg_stream_cdc_{}", source_oid);

    // Drop one ST
    db.drop_st("st_shared_1").await;

    // Trigger should still exist for the other ST
    assert!(
        db.trigger_exists(&trigger_name, "dr_shared").await,
        "Trigger should be preserved when another ST still depends on the source"
    );

    // The other ST should still work
    let count = db.count("public.st_shared_2").await;
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_drop_removes_change_tracking() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dr_ct (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO dr_ct VALUES (1)").await;

    db.create_st("dr_ct_st", "SELECT id FROM dr_ct", "1m", "DIFFERENTIAL")
        .await;

    let source_oid = db.table_oid("dr_ct").await;

    let ct_before: i64 = db
        .query_scalar(&format!(
            "SELECT count(*) FROM pgstream.pgs_change_tracking WHERE source_relid = {}::oid",
            source_oid
        ))
        .await;
    assert!(ct_before >= 1, "Change tracking should exist before drop");

    db.drop_st("dr_ct_st").await;

    let ct_after: i64 = db
        .query_scalar(&format!(
            "SELECT count(*) FROM pgstream.pgs_change_tracking WHERE source_relid = {}::oid",
            source_oid
        ))
        .await;
    assert_eq!(
        ct_after, 0,
        "Change tracking should be removed for sole consumer"
    );
}

// ── Error Cases ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_drop_nonexistent_fails() {
    let db = E2eDb::new().await.with_extension().await;

    let result = db
        .try_execute("SELECT pgstream.drop_stream_table('no_such_st')")
        .await;
    assert!(result.is_err(), "Dropping a nonexistent ST should fail");
}

// ── Source Table Preservation ──────────────────────────────────────────

#[tokio::test]
async fn test_drop_preserves_source_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dr_preserved (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO dr_preserved VALUES (1, 'keep_me')")
        .await;

    db.create_st(
        "st_to_drop",
        "SELECT id, val FROM dr_preserved",
        "1m",
        "FULL",
    )
    .await;

    db.drop_st("st_to_drop").await;

    // Source table should be untouched
    let count = db.count("public.dr_preserved").await;
    assert_eq!(count, 1, "Source table should still have its data");

    let val: String = db
        .query_scalar("SELECT val FROM public.dr_preserved WHERE id = 1")
        .await;
    assert_eq!(val, "keep_me");
}
