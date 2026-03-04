//! E2E tests for wide and deep DAG topologies.
//!
//! Validates correctness for topologies beyond simple linear chains:
//! wide fan-out (1 base → 4+ leaves), fan-out-then-converge (fan-in),
//! deep linear chains (5 layers), and multi-source diamonds.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════════
// Test 5.1 — Wide fan-out: 1 base → 4 leaf STs
// ═══════════════════════════════════════════════════════════════════════════

/// One base table feeds 4 independent leaf STs with different queries.
/// INSERT into base, refresh all leaves, verify each is independently correct.
#[tokio::test]
async fn test_fanout_4_leaves() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE fo4_src (
            id   SERIAL PRIMARY KEY,
            grp  TEXT NOT NULL,
            val  INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO fo4_src (grp, val) VALUES
            ('a', 10), ('a', 20), ('b', 30), ('c', 40)",
    )
    .await;

    // 4 different leaf STs from the same base table
    db.create_st(
        "fo4_sum",
        "SELECT grp, SUM(val) AS total FROM fo4_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "fo4_cnt",
        "SELECT grp, COUNT(*) AS cnt FROM fo4_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "fo4_max",
        "SELECT grp, MAX(val) AS mx FROM fo4_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "fo4_min",
        "SELECT grp, MIN(val) AS mn FROM fo4_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    let queries: Vec<(&str, &str)> = vec![
        (
            "fo4_sum",
            "SELECT grp, SUM(val) AS total FROM fo4_src GROUP BY grp",
        ),
        (
            "fo4_cnt",
            "SELECT grp, COUNT(*) AS cnt FROM fo4_src GROUP BY grp",
        ),
        (
            "fo4_max",
            "SELECT grp, MAX(val) AS mx FROM fo4_src GROUP BY grp",
        ),
        (
            "fo4_min",
            "SELECT grp, MIN(val) AS mn FROM fo4_src GROUP BY grp",
        ),
    ];

    for (name, q) in &queries {
        db.assert_st_matches_query(name, q).await;
    }

    // INSERT new data
    db.execute("INSERT INTO fo4_src (grp, val) VALUES ('a', 5), ('d', 100)")
        .await;

    // Refresh all leaves
    for (name, _) in &queries {
        db.refresh_st(name).await;
    }

    // Verify all independently correct
    for (name, q) in &queries {
        db.assert_st_matches_query(name, q).await;
    }

    // UPDATE
    db.execute("UPDATE fo4_src SET val = val + 10 WHERE grp = 'b'")
        .await;
    for (name, _) in &queries {
        db.refresh_st(name).await;
    }
    for (name, q) in &queries {
        db.assert_st_matches_query(name, q).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 5.2 — Fan-out then converge (3 branches → JOIN)
// ═══════════════════════════════════════════════════════════════════════════

/// base → L1a (SUM), L1b (COUNT), L1c (MAX) → L2 (JOIN all three)
///
/// Tests wide fan-out at L1 that converges at L2 via a 3-way join.
#[tokio::test]
async fn test_fanout_then_converge() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE foc_src (
            id   SERIAL PRIMARY KEY,
            grp  TEXT NOT NULL,
            val  INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO foc_src (grp, val) VALUES
            ('a', 10), ('a', 20), ('b', 30), ('b', 40)",
    )
    .await;

    // L1a: SUM by group
    db.create_st(
        "foc_sum",
        "SELECT grp, SUM(val) AS total FROM foc_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // L1b: COUNT by group
    db.create_st(
        "foc_cnt",
        "SELECT grp, COUNT(*) AS cnt FROM foc_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // L1c: MAX by group
    db.create_st(
        "foc_max",
        "SELECT grp, MAX(val) AS mx FROM foc_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // L2: 3-way JOIN (ST-on-ST × 3)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'foc_merged',
            $$SELECT s.grp, s.total, c.cnt, m.mx
              FROM foc_sum s
              JOIN foc_cnt c ON s.grp = c.grp
              JOIN foc_max m ON s.grp = m.grp$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    let l2_q = "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt, MAX(val) AS mx \
                FROM foc_src GROUP BY grp";

    db.assert_st_matches_query("foc_merged", l2_q).await;

    // INSERT
    db.execute("INSERT INTO foc_src (grp, val) VALUES ('c', 50)")
        .await;
    db.refresh_st("foc_sum").await;
    db.refresh_st("foc_cnt").await;
    db.refresh_st("foc_max").await;
    db.refresh_st("foc_merged").await;
    db.assert_st_matches_query("foc_merged", l2_q).await;

    // DELETE
    db.execute("DELETE FROM foc_src WHERE grp = 'a' AND val = 10")
        .await;
    db.refresh_st("foc_sum").await;
    db.refresh_st("foc_cnt").await;
    db.refresh_st("foc_max").await;
    db.refresh_st("foc_merged").await;
    db.assert_st_matches_query("foc_merged", l2_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 5.3 — Deep 5-layer linear chain
// ═══════════════════════════════════════════════════════════════════════════

/// 5-layer linear chain exercising scan → project → aggregate → window → filter
/// across the chain, covering the full DVM operator repertoire.
///
/// ```text
/// d5_src → L1 (passthrough) → L2 (arithmetic) → L3 (aggregate)
///          → L4 (window: rank) → L5 (filter: top-N)
/// ```
#[tokio::test]
async fn test_deep_linear_5_layers() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE d5_src (
            id   SERIAL PRIMARY KEY,
            grp  TEXT NOT NULL,
            val  INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO d5_src (grp, val) VALUES
            ('a', 10), ('a', 20), ('b', 30), ('b', 40), ('c', 50)",
    )
    .await;

    // L1: passthrough (scan)
    db.create_st(
        "d5_l1",
        "SELECT id, grp, val FROM d5_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // L2: arithmetic (project)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'd5_l2',
            $$SELECT id, grp, val * 2 AS v2 FROM d5_l1$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // L3: aggregate
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'd5_l3',
            $$SELECT grp, SUM(v2) AS total FROM d5_l2 GROUP BY grp$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // L4: window function (rank)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'd5_l4',
            $$SELECT grp, total, RANK() OVER (ORDER BY total DESC) AS rnk FROM d5_l3$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // L5: filter (TopK)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'd5_l5',
            $$SELECT grp, total FROM d5_l4 WHERE rnk <= 2$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Ground truth: compose the full pipeline against the base table
    let l5_q = "SELECT grp, total FROM ( \
                    SELECT grp, SUM(val * 2) AS total, \
                           RANK() OVER (ORDER BY SUM(val * 2) DESC) AS rnk \
                    FROM d5_src GROUP BY grp \
                ) sub WHERE rnk <= 2";

    db.assert_st_matches_query("d5_l5", l5_q).await;

    // Verify intermediate layers too
    let l3_q = "SELECT grp, SUM(val * 2) AS total FROM d5_src GROUP BY grp";
    db.assert_st_matches_query("d5_l3", l3_q).await;

    // Mutate: INSERT
    db.execute("INSERT INTO d5_src (grp, val) VALUES ('d', 100), ('d', 200)")
        .await;

    // Refresh in topological order
    db.refresh_st("d5_l1").await;
    db.refresh_st("d5_l2").await;
    db.refresh_st("d5_l3").await;
    db.refresh_st("d5_l4").await;
    db.refresh_st("d5_l5").await;

    db.assert_st_matches_query("d5_l3", l3_q).await;
    db.assert_st_matches_query("d5_l5", l5_q).await;

    // Mutate: DELETE — remove group 'd' completely
    db.execute("DELETE FROM d5_src WHERE grp = 'd'").await;
    db.refresh_st("d5_l1").await;
    db.refresh_st("d5_l2").await;
    db.refresh_st("d5_l3").await;
    db.refresh_st("d5_l4").await;
    db.refresh_st("d5_l5").await;
    db.assert_st_matches_query("d5_l3", l3_q).await;
    db.assert_st_matches_query("d5_l5", l5_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 5.4 — Multi-source diamond
// ═══════════════════════════════════════════════════════════════════════════

/// Two base tables join at L1, L1 fans out to L2a and L2b.
///
/// ```text
/// msd_left ──┐
///             └─ L1 (JOIN) ──┬── L2a (SUM)
/// msd_right ─┘               └── L2b (COUNT)
/// ```
#[tokio::test]
async fn test_multi_source_diamond() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE msd_left (
            id   SERIAL PRIMARY KEY,
            key  INT NOT NULL,
            lval TEXT NOT NULL
        )",
    )
    .await;
    db.execute(
        "CREATE TABLE msd_right (
            id   SERIAL PRIMARY KEY,
            key  INT NOT NULL,
            rval INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO msd_left (key, lval) VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .await;
    db.execute("INSERT INTO msd_right (key, rval) VALUES (1, 10), (2, 20)")
        .await;

    // L1: inner join of left + right
    db.create_st(
        "msd_l1",
        "SELECT l.key, l.lval, r.rval FROM msd_left l JOIN msd_right r ON l.key = r.key",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // L2a: SUM of rval by lval (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'msd_l2a',
            $$SELECT lval, SUM(rval) AS total FROM msd_l1 GROUP BY lval$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // L2b: COUNT by lval (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'msd_l2b',
            $$SELECT lval, COUNT(*) AS cnt FROM msd_l1 GROUP BY lval$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    let l2a_q = "SELECT l.lval, SUM(r.rval) AS total \
                 FROM msd_left l JOIN msd_right r ON l.key = r.key GROUP BY l.lval";
    let l2b_q = "SELECT l.lval, COUNT(*) AS cnt \
                 FROM msd_left l JOIN msd_right r ON l.key = r.key GROUP BY l.lval";

    db.assert_st_matches_query("msd_l2a", l2a_q).await;
    db.assert_st_matches_query("msd_l2b", l2b_q).await;

    // Mutate left table only
    db.execute("INSERT INTO msd_left (key, lval) VALUES (2, 'b_extra')")
        .await;
    db.refresh_st("msd_l1").await;
    db.refresh_st("msd_l2a").await;
    db.refresh_st("msd_l2b").await;
    db.assert_st_matches_query("msd_l2a", l2a_q).await;
    db.assert_st_matches_query("msd_l2b", l2b_q).await;

    // Mutate right table only
    db.execute("INSERT INTO msd_right (key, rval) VALUES (3, 30)")
        .await;
    db.refresh_st("msd_l1").await;
    db.refresh_st("msd_l2a").await;
    db.refresh_st("msd_l2b").await;
    db.assert_st_matches_query("msd_l2a", l2a_q).await;
    db.assert_st_matches_query("msd_l2b", l2b_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 5.5 — Wide fan-out: deletion isolation
// ═══════════════════════════════════════════════════════════════════════════

/// base → L1..L6 (6 leaf STs with different WHERE filters).
/// Delete data relevant to only L3's query.  Verify L1, L2, L4, L5, L6
/// are unaffected (row counts unchanged).
#[tokio::test]
async fn test_wide_fanout_deletion_isolation() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wf_src (
            id   SERIAL PRIMARY KEY,
            grp  TEXT NOT NULL,
            val  INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO wf_src (grp, val) VALUES
            ('a', 10), ('b', 20), ('c', 30), ('d', 40), ('e', 50), ('f', 60)",
    )
    .await;

    // 6 leaf STs, each filtering a different group
    let groups = ["a", "b", "c", "d", "e", "f"];
    for (i, g) in groups.iter().enumerate() {
        let name = format!("wf_l{}", i + 1);
        db.create_st(
            &name,
            &format!("SELECT id, grp, val FROM wf_src WHERE grp = '{g}'"),
            "1m",
            "DIFFERENTIAL",
        )
        .await;
    }

    // Record initial counts
    let mut initial_counts = Vec::new();
    for i in 1..=6 {
        let cnt = db.count(&format!("public.wf_l{i}")).await;
        initial_counts.push(cnt);
    }

    // Delete only group 'c' (affects wf_l3 only)
    db.execute("DELETE FROM wf_src WHERE grp = 'c'").await;

    // Refresh all
    for i in 1..=6 {
        db.refresh_st(&format!("wf_l{i}")).await;
    }

    // L3 should have 0 rows now
    assert_eq!(
        db.count("public.wf_l3").await,
        0,
        "wf_l3 should be empty after deleting group 'c'"
    );

    // All others should be unchanged
    for i in [1, 2, 4, 5, 6] {
        let cnt = db.count(&format!("public.wf_l{i}")).await;
        assert_eq!(
            cnt,
            initial_counts[i - 1],
            "wf_l{i} should be unaffected by deleting group 'c'"
        );
    }

    // Verify correctness using ground truth
    for (i, g) in groups.iter().enumerate() {
        let name = format!("wf_l{}", i + 1);
        let q = format!("SELECT id, grp, val FROM wf_src WHERE grp = '{g}'");
        db.assert_st_matches_query(&name, &q).await;
    }
}
