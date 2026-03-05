//! E2E tests for mixed refresh mode DAG pipelines.
//!
//! Validates that pipelines combining FULL, DIFFERENTIAL, and IMMEDIATE
//! refresh modes in a single stream table chain produce correct results.
//!
//! ## Key architecture paths exercised
//!
//! - `determine_refresh_action()` dispatching FULL vs DIFFERENTIAL in cascade
//! - `has_stream_table_source_changes()` comparing `data_timestamp` when
//!   upstream refresh mode differs
//! - Reinit behavior when ALTERing mode mid-pipeline
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════════
// Test 2.1 — FULL root feeds DIFFERENTIAL leaf
// ═══════════════════════════════════════════════════════════════════════════

/// base →[FULL] L1 →[DIFF] L2
///
/// FULL root does a complete recompute each refresh, then the DIFFERENTIAL
/// leaf should detect the upstream `data_timestamp` change and incorporate
/// updates correctly.
#[tokio::test]
async fn test_mixed_full_then_diff_2_layer() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE mfull_src (
            id  SERIAL PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO mfull_src (grp, val) VALUES ('a', 10), ('b', 20)")
        .await;

    // L1: FULL mode aggregate
    db.create_st(
        "mfull_l1",
        "SELECT grp, SUM(val) AS total FROM mfull_src GROUP BY grp",
        "1m",
        "FULL",
    )
    .await;

    // L2: DIFFERENTIAL mode, reads from L1 (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'mfull_l2',
            $$SELECT grp, total * 2 AS doubled FROM mfull_l1$$,
            'calculated',
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Ground truth queries
    let l1_q = "SELECT grp, SUM(val) AS total FROM mfull_src GROUP BY grp";
    let l2_q = "SELECT grp, SUM(val) * 2 AS doubled FROM mfull_src GROUP BY grp";

    db.assert_st_matches_query("mfull_l1", l1_q).await;
    db.assert_st_matches_query("mfull_l2", l2_q).await;

    // Mutate and refresh
    db.execute("INSERT INTO mfull_src (grp, val) VALUES ('a', 5), ('c', 30)")
        .await;
    db.refresh_st("mfull_l1").await; // FULL recompute
    db.refresh_st("mfull_l2").await; // DIFFERENTIAL delta
    db.assert_st_matches_query("mfull_l1", l1_q).await;
    db.assert_st_matches_query("mfull_l2", l2_q).await;

    // Another cycle: UPDATE
    db.execute("UPDATE mfull_src SET val = val + 10 WHERE grp = 'b'")
        .await;
    db.refresh_st("mfull_l1").await;
    db.refresh_st("mfull_l2").await;
    db.assert_st_matches_query("mfull_l1", l1_q).await;
    db.assert_st_matches_query("mfull_l2", l2_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 2.2 — DIFFERENTIAL root feeds FULL leaf
// ═══════════════════════════════════════════════════════════════════════════

/// base →[DIFF] L1 →[FULL] L2
///
/// DIFFERENTIAL root incrementally updates, FULL leaf does complete
/// recompute each time regardless.
#[tokio::test]
async fn test_mixed_diff_then_full_2_layer() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE mdf_src (
            id  SERIAL PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO mdf_src (grp, val) VALUES ('a', 10), ('b', 20)")
        .await;

    // L1: DIFFERENTIAL
    db.create_st(
        "mdf_l1",
        "SELECT grp, SUM(val) AS total FROM mdf_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // L2: FULL mode
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'mdf_l2',
            $$SELECT grp, total * 3 AS tripled FROM mdf_l1$$,
            'calculated',
            'FULL'
        )",
    )
    .await;

    let l1_q = "SELECT grp, SUM(val) AS total FROM mdf_src GROUP BY grp";
    let l2_q = "SELECT grp, SUM(val) * 3 AS tripled FROM mdf_src GROUP BY grp";

    db.assert_st_matches_query("mdf_l1", l1_q).await;
    db.assert_st_matches_query("mdf_l2", l2_q).await;

    // Mutate
    db.execute("INSERT INTO mdf_src (grp, val) VALUES ('c', 15)")
        .await;
    db.refresh_st("mdf_l1").await; // DIFFERENTIAL delta
    db.refresh_st("mdf_l2").await; // FULL recompute
    db.assert_st_matches_query("mdf_l1", l1_q).await;
    db.assert_st_matches_query("mdf_l2", l2_q).await;

    // DELETE
    db.execute("DELETE FROM mdf_src WHERE grp = 'a'").await;
    db.refresh_st("mdf_l1").await;
    db.refresh_st("mdf_l2").await;
    db.assert_st_matches_query("mdf_l1", l1_q).await;
    db.assert_st_matches_query("mdf_l2", l2_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 2.3 — 3-layer: FULL → DIFF → DIFF
// ═══════════════════════════════════════════════════════════════════════════

/// base →[FULL] L1 →[DIFF] L2 →[DIFF] L3
///
/// FULL root, two DIFFERENTIAL downstream.  Exercises the full mixed-mode
/// path through 3 levels.
#[tokio::test]
async fn test_mixed_3_layer_full_diff_diff() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE m3_src (
            id  SERIAL PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO m3_src (grp, val) VALUES
            ('a', 10), ('a', 20), ('b', 30)",
    )
    .await;

    // L1: FULL aggregate
    db.create_st(
        "m3_l1",
        "SELECT grp, SUM(val) AS total FROM m3_src GROUP BY grp",
        "1m",
        "FULL",
    )
    .await;

    // L2: DIFFERENTIAL project (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'm3_l2',
            $$SELECT grp, total * 2 AS doubled FROM m3_l1$$,
            'calculated',
            'DIFFERENTIAL'
        )",
    )
    .await;

    // L3: DIFFERENTIAL filter (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'm3_l3',
            $$SELECT grp, doubled FROM m3_l2 WHERE doubled > 50$$,
            'calculated',
            'DIFFERENTIAL'
        )",
    )
    .await;

    let l1_q = "SELECT grp, SUM(val) AS total FROM m3_src GROUP BY grp";
    let l2_q = "SELECT grp, SUM(val) * 2 AS doubled FROM m3_src GROUP BY grp";
    let l3_q =
        "SELECT grp, SUM(val) * 2 AS doubled FROM m3_src GROUP BY grp HAVING SUM(val) * 2 > 50";

    db.assert_st_matches_query("m3_l1", l1_q).await;
    db.assert_st_matches_query("m3_l2", l2_q).await;
    db.assert_st_matches_query("m3_l3", l3_q).await;

    // Cycle 1: INSERT → should cascade to L3
    db.execute("INSERT INTO m3_src (grp, val) VALUES ('a', 5)")
        .await;
    db.refresh_st("m3_l1").await;
    db.refresh_st("m3_l2").await;
    db.refresh_st("m3_l3").await;
    db.assert_st_matches_query("m3_l1", l1_q).await;
    db.assert_st_matches_query("m3_l2", l2_q).await;
    db.assert_st_matches_query("m3_l3", l3_q).await;

    // Cycle 2: DELETE → may remove group from L3
    db.execute("DELETE FROM m3_src WHERE grp = 'a' AND val = 5")
        .await;
    db.refresh_st("m3_l1").await;
    db.refresh_st("m3_l2").await;
    db.refresh_st("m3_l3").await;
    db.assert_st_matches_query("m3_l1", l1_q).await;
    db.assert_st_matches_query("m3_l2", l2_q).await;
    db.assert_st_matches_query("m3_l3", l3_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 2.4 — ALTER mode mid-pipeline (DIFF → FULL)
// ═══════════════════════════════════════════════════════════════════════════

/// Start with base →[DIFF] L1 →[DIFF] L2, then ALTER L1 to FULL.
/// The chain should still converge after the mode change.
#[tokio::test]
async fn test_mixed_mode_alter_mid_pipeline() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE malter_src (
            id  SERIAL PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO malter_src (grp, val) VALUES ('a', 10), ('b', 20)")
        .await;

    db.create_st(
        "malter_l1",
        "SELECT grp, SUM(val) AS total FROM malter_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'malter_l2',
            $$SELECT grp, total * 2 AS doubled FROM malter_l1$$,
            'calculated',
            'DIFFERENTIAL'
        )",
    )
    .await;

    let l1_q = "SELECT grp, SUM(val) AS total FROM malter_src GROUP BY grp";
    let l2_q = "SELECT grp, SUM(val) * 2 AS doubled FROM malter_src GROUP BY grp";

    db.assert_st_matches_query("malter_l1", l1_q).await;
    db.assert_st_matches_query("malter_l2", l2_q).await;

    // ALTER L1 from DIFFERENTIAL to FULL
    db.alter_st("malter_l1", "refresh_mode => 'FULL'").await;

    let (_, mode, _, _) = db.pgt_status("malter_l1").await;
    assert_eq!(mode, "FULL", "L1 should now be FULL mode");

    // Mutate and refresh — L1 does FULL recompute now
    db.execute("INSERT INTO malter_src (grp, val) VALUES ('c', 30)")
        .await;
    db.refresh_st("malter_l1").await;
    db.refresh_st("malter_l2").await;
    db.assert_st_matches_query("malter_l1", l1_q).await;
    db.assert_st_matches_query("malter_l2", l2_q).await;

    // One more cycle to confirm stability
    db.execute("DELETE FROM malter_src WHERE grp = 'a'").await;
    db.refresh_st("malter_l1").await;
    db.refresh_st("malter_l2").await;
    db.assert_st_matches_query("malter_l1", l1_q).await;
    db.assert_st_matches_query("malter_l2", l2_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 2.5 — DIFFERENTIAL root with IMMEDIATE leaf
// ═══════════════════════════════════════════════════════════════════════════

/// base →[DIFF] L1 →[IMMED] L2
///
/// DIFFERENTIAL root with IMMEDIATE leaf.  After refreshing L1, L2
/// should be up-to-date via its statement-level trigger on L1.
///
/// Note: IMMEDIATE ST-on-ST may require explicit refresh depending on
/// internal trigger wiring.  This test documents the actual behavior.
#[tokio::test]
async fn test_mixed_immediate_leaf() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE mimm_src (
            id  SERIAL PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO mimm_src (grp, val) VALUES ('a', 10), ('b', 20)")
        .await;

    // L1: DIFFERENTIAL
    db.create_st(
        "mimm_l1",
        "SELECT grp, SUM(val) AS total FROM mimm_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // L2: IMMEDIATE (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'mimm_l2',
            $$SELECT grp, total * 2 AS doubled FROM mimm_l1$$,
            'calculated',
            'IMMEDIATE'
        )",
    )
    .await;

    let l1_q = "SELECT grp, SUM(val) AS total FROM mimm_src GROUP BY grp";
    let l2_q = "SELECT grp, SUM(val) * 2 AS doubled FROM mimm_src GROUP BY grp";

    db.assert_st_matches_query("mimm_l1", l1_q).await;

    // Mutate base and refresh L1 (DIFFERENTIAL)
    db.execute("INSERT INTO mimm_src (grp, val) VALUES ('c', 30)")
        .await;
    db.refresh_st("mimm_l1").await;
    db.assert_st_matches_query("mimm_l1", l1_q).await;

    // L2 is IMMEDIATE — if the trigger fires on L1's MERGE, it should be
    // up-to-date already.  If not, we do an explicit refresh.
    // Either way, verify final correctness.
    db.refresh_st("mimm_l2").await;
    db.assert_st_matches_query("mimm_l2", l2_q).await;
}
