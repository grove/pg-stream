//! E2E tests for GUC-variation differential correctness (F23: G8.1).
//!
//! Validates that differential refresh produces correct results under
//! different GUC configurations: block_source_ddl, use_prepared_statements,
//! merge_planner_hints, cleanup_use_truncate, merge_work_mem_mb.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

/// Helper: create a standard table, seed data, create ST, verify.
async fn setup_guc_test(db: &E2eDb) {
    db.execute("CREATE TABLE guc_src (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute(
        "INSERT INTO guc_src (grp, val) VALUES \
         ('a', 10), ('a', 20), ('b', 30), ('b', 40), ('c', 50)",
    )
    .await;
}

const GUC_QUERY: &str = "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM guc_src GROUP BY grp";

/// Helper: mutate then refresh and verify.
async fn mutate_and_verify(db: &E2eDb) {
    db.execute("INSERT INTO guc_src (grp, val) VALUES ('a', 5), ('d', 99)")
        .await;
    db.execute("UPDATE guc_src SET val = 100 WHERE grp = 'b' AND val = 30")
        .await;
    db.execute("DELETE FROM guc_src WHERE grp = 'c'").await;
    db.refresh_st("guc_st").await;
    db.assert_st_matches_query("guc_st", GUC_QUERY).await;
}

// ═══════════════════════════════════════════════════════════════════════
// use_prepared_statements = off
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_guc_prepared_statements_off() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("SET pg_trickle.use_prepared_statements = off")
        .await;
    setup_guc_test(&db).await;
    db.create_st("guc_st", GUC_QUERY, "1m", "DIFFERENTIAL")
        .await;
    db.assert_st_matches_query("guc_st", GUC_QUERY).await;
    mutate_and_verify(&db).await;
}

// ═══════════════════════════════════════════════════════════════════════
// merge_planner_hints = off
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_guc_merge_planner_hints_off() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("SET pg_trickle.merge_planner_hints = off").await;
    setup_guc_test(&db).await;
    db.create_st("guc_st", GUC_QUERY, "1m", "DIFFERENTIAL")
        .await;
    db.assert_st_matches_query("guc_st", GUC_QUERY).await;
    mutate_and_verify(&db).await;
}

// ═══════════════════════════════════════════════════════════════════════
// cleanup_use_truncate = off
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_guc_cleanup_use_truncate_off() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("SET pg_trickle.cleanup_use_truncate = off")
        .await;
    setup_guc_test(&db).await;
    db.create_st("guc_st", GUC_QUERY, "1m", "DIFFERENTIAL")
        .await;
    db.assert_st_matches_query("guc_st", GUC_QUERY).await;
    mutate_and_verify(&db).await;
}

// ═══════════════════════════════════════════════════════════════════════
// merge_work_mem_mb non-default
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_guc_merge_work_mem_mb_custom() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("SET pg_trickle.merge_work_mem_mb = 16").await;
    setup_guc_test(&db).await;
    db.create_st("guc_st", GUC_QUERY, "1m", "DIFFERENTIAL")
        .await;
    db.assert_st_matches_query("guc_st", GUC_QUERY).await;
    mutate_and_verify(&db).await;
}

// ═══════════════════════════════════════════════════════════════════════
// block_source_ddl = on — DDL blocked after ST creation
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_guc_block_source_ddl_on() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("SET pg_trickle.block_source_ddl = on").await;
    setup_guc_test(&db).await;
    db.create_st("guc_st", GUC_QUERY, "1m", "DIFFERENTIAL")
        .await;
    db.assert_st_matches_query("guc_st", GUC_QUERY).await;

    // Column-altering DDL should be blocked
    let result = db
        .try_execute("ALTER TABLE guc_src ADD COLUMN new_col TEXT")
        .await;
    assert!(
        result.is_err(),
        "DDL should be blocked when block_source_ddl=on"
    );

    // Data DML should still work
    mutate_and_verify(&db).await;
}

// ═══════════════════════════════════════════════════════════════════════
// differential_max_change_ratio = 0.0 (never fall back to FULL)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_guc_differential_max_change_ratio_zero() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("SET pg_trickle.differential_max_change_ratio = 0.0")
        .await;
    setup_guc_test(&db).await;
    db.create_st("guc_st", GUC_QUERY, "1m", "DIFFERENTIAL")
        .await;
    db.assert_st_matches_query("guc_st", GUC_QUERY).await;
    mutate_and_verify(&db).await;

    // Verify mode is still differential
    let (_, mode, _, _) = db.pgt_status("guc_st").await;
    assert_eq!(mode, "DIFFERENTIAL");
}

// ═══════════════════════════════════════════════════════════════════════
// Combined: multiple GUCs changed at once
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_guc_combined_non_default() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("SET pg_trickle.use_prepared_statements = off")
        .await;
    db.execute("SET pg_trickle.merge_planner_hints = off").await;
    db.execute("SET pg_trickle.cleanup_use_truncate = off")
        .await;
    db.execute("SET pg_trickle.merge_work_mem_mb = 8").await;
    setup_guc_test(&db).await;
    db.create_st("guc_st", GUC_QUERY, "1m", "DIFFERENTIAL")
        .await;
    db.assert_st_matches_query("guc_st", GUC_QUERY).await;
    mutate_and_verify(&db).await;
}
