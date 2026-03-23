//! B3-3: Property-based correctness tests for simultaneous multi-source
//! changes in diamond-flow topologies.
//!
//! These tests verify the KEY INVARIANT (DBSP §4, Gupta & Mumick 1995 §3):
//!
//! > For every ST, at every data timestamp:
//! >   Contents(ST) = Result(defining_query)   (multiset equality)
//!
//! specifically when **multiple upstream stream tables change in the same
//! refresh cycle**. This occurs in diamond-shaped DAG topologies where a
//! single base table feeds two intermediate STs that converge at a
//! downstream join.
//!
//! The delta for the diamond tip has overlapping corrections from both
//! branches. B3-2 weight aggregation (replacing DISTINCT ON) must correctly
//! combine these algebraically.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::{
    E2eDb,
    property_support::{SeededRng as Rng, TrackedIds, assert_st_query_invariant},
};

const INITIAL_ROWS: usize = 15;
const CYCLES: usize = 8;

// ═══════════════════════════════════════════════════════════════════════
// Test 1: Diamond with INNER JOIN at tip — single root
//
//    root (base table)
//      /        \
//   branch_a   branch_b   (stream tables, both read from root)
//      \        /
//     diamond_tip          (stream table, joins branch_a and branch_b)
//
// A single INSERT/UPDATE/DELETE on root propagates through BOTH branches,
// causing both to have changes when diamond_tip is refreshed.
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_prop_diamond_inner_join_simultaneous() {
    let seed: u64 = 0xD1A0_0001;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    // Base table
    db.execute("CREATE TABLE d1_root (id INT PRIMARY KEY, val INT, cat INT)")
        .await;

    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let val = rng.i32_range(1, 100);
        let cat = rng.i32_range(1, 4);
        db.execute(&format!("INSERT INTO d1_root VALUES ({id}, {val}, {cat})"))
            .await;
    }

    // Branch A: projects id, val, cat
    let q_a = "SELECT id, val AS val_a, cat FROM d1_root";
    db.create_st("d1_branch_a", q_a, "1m", "DIFFERENTIAL").await;

    // Branch B: projects id, val*2, cat
    let q_b = "SELECT id, val * 2 AS val_b, cat FROM d1_root";
    db.create_st("d1_branch_b", q_b, "1m", "DIFFERENTIAL").await;

    // Diamond tip: INNER JOIN on id
    let q_tip = "SELECT a.id, a.val_a, b.val_b, a.cat \
                 FROM d1_branch_a a JOIN d1_branch_b b ON a.id = b.id";
    db.create_st("d1_tip", q_tip, "1m", "DIFFERENTIAL").await;

    // Initial refresh (topological order)
    db.refresh_st("d1_branch_a").await;
    db.refresh_st("d1_branch_b").await;
    db.refresh_st("d1_tip").await;

    assert_st_query_invariant(&db, "d1_branch_a", q_a, seed, 0, "init").await;
    assert_st_query_invariant(&db, "d1_branch_b", q_b, seed, 0, "init").await;
    assert_st_query_invariant(&db, "d1_tip", q_tip, seed, 0, "init").await;

    for cycle in 1..=CYCLES {
        // Random inserts
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let val = rng.i32_range(1, 100);
            let cat = rng.i32_range(1, 4);
            db.execute(&format!("INSERT INTO d1_root VALUES ({id}, {val}, {cat})"))
                .await;
        }

        // Random updates — changes propagate to BOTH branches simultaneously
        let n_upd = rng.usize_range(1, 4);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                db.execute(&format!("UPDATE d1_root SET val = {val} WHERE id = {id}"))
                    .await;
            }
        }

        // Random deletes
        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM d1_root WHERE id = {id}"))
                    .await;
            }
        }

        // Refresh in topological order
        db.refresh_st("d1_branch_a").await;
        db.refresh_st("d1_branch_b").await;
        db.refresh_st("d1_tip").await;

        let step = format!("cycle-{cycle}");
        assert_st_query_invariant(&db, "d1_branch_a", q_a, seed, cycle, &step).await;
        assert_st_query_invariant(&db, "d1_branch_b", q_b, seed, cycle, &step).await;
        assert_st_query_invariant(&db, "d1_tip", q_tip, seed, cycle, &step).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 2: Diamond with LEFT JOIN at tip
//
// Same topology but branch_b has a filter, so LEFT JOIN can produce
// NULL-padded rows when branch_b has no matching row.
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_prop_diamond_left_join_simultaneous() {
    let seed: u64 = 0xD1A0_0002;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE d2_root (id INT PRIMARY KEY, val INT, cat INT)")
        .await;

    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let val = rng.i32_range(1, 100);
        let cat = rng.i32_range(1, 4);
        db.execute(&format!("INSERT INTO d2_root VALUES ({id}, {val}, {cat})"))
            .await;
    }

    // Branch A: all rows
    let q_a = "SELECT id, val AS val_a FROM d2_root";
    db.create_st("d2_branch_a", q_a, "1m", "DIFFERENTIAL").await;

    // Branch B: filtered — only val > 50
    let q_b = "SELECT id, val AS val_b FROM d2_root WHERE val > 50";
    db.create_st("d2_branch_b", q_b, "1m", "DIFFERENTIAL").await;

    // Diamond tip: LEFT JOIN (some rows in A have no match in B)
    let q_tip = "SELECT a.id, a.val_a, b.val_b \
                 FROM d2_branch_a a LEFT JOIN d2_branch_b b ON a.id = b.id";
    db.create_st("d2_tip", q_tip, "1m", "DIFFERENTIAL").await;

    db.refresh_st("d2_branch_a").await;
    db.refresh_st("d2_branch_b").await;
    db.refresh_st("d2_tip").await;

    assert_st_query_invariant(&db, "d2_tip", q_tip, seed, 0, "init").await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            // val straddles the filter boundary (50)
            let val = rng.i32_range(20, 80);
            let cat = rng.i32_range(1, 4);
            db.execute(&format!("INSERT INTO d2_root VALUES ({id}, {val}, {cat})"))
                .await;
        }

        // Updates that cross the filter threshold cause rows to appear/disappear
        // in branch_b while remaining in branch_a — LEFT JOIN null-padding transitions
        let n_upd = rng.usize_range(1, 4);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(20, 80);
                db.execute(&format!("UPDATE d2_root SET val = {val} WHERE id = {id}"))
                    .await;
            }
        }

        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM d2_root WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_st("d2_branch_a").await;
        db.refresh_st("d2_branch_b").await;
        db.refresh_st("d2_tip").await;

        let step = format!("cycle-{cycle}");
        assert_st_query_invariant(&db, "d2_branch_a", q_a, seed, cycle, &step).await;
        assert_st_query_invariant(&db, "d2_branch_b", q_b, seed, cycle, &step).await;
        assert_st_query_invariant(&db, "d2_tip", q_tip, seed, cycle, &step).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 3: Diamond with aggregate at tip
//
//    root (base table)
//      /        \
//   branch_a   branch_b
//      \        /
//     agg_tip           (JOIN + GROUP BY + SUM)
//
// The aggregate processes the diamond delta internally, testing that
// both the join correction and aggregate merge handle simultaneous changes.
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_prop_diamond_aggregate_simultaneous() {
    let seed: u64 = 0xD1A0_0003;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE d3_root (id INT PRIMARY KEY, val INT, grp TEXT)")
        .await;

    let groups = ["x", "y", "z"];
    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let val = rng.i32_range(1, 100);
        let grp = rng.choose(&groups);
        db.execute(&format!(
            "INSERT INTO d3_root VALUES ({id}, {val}, '{grp}')"
        ))
        .await;
    }

    let q_a = "SELECT id, val AS va, grp FROM d3_root";
    db.create_st("d3_branch_a", q_a, "1m", "DIFFERENTIAL").await;

    let q_b = "SELECT id, val * 2 AS vb, grp FROM d3_root";
    db.create_st("d3_branch_b", q_b, "1m", "DIFFERENTIAL").await;

    let q_tip = "SELECT a.grp, COUNT(*) AS cnt, SUM(a.va + b.vb) AS total \
                 FROM d3_branch_a a JOIN d3_branch_b b ON a.id = b.id \
                 GROUP BY a.grp";
    db.create_st("d3_agg_tip", q_tip, "1m", "DIFFERENTIAL")
        .await;

    db.refresh_st("d3_branch_a").await;
    db.refresh_st("d3_branch_b").await;
    db.refresh_st("d3_agg_tip").await;

    assert_st_query_invariant(&db, "d3_agg_tip", q_tip, seed, 0, "init").await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let val = rng.i32_range(1, 100);
            let grp = rng.choose(&groups);
            db.execute(&format!(
                "INSERT INTO d3_root VALUES ({id}, {val}, '{grp}')"
            ))
            .await;
        }

        let n_upd = rng.usize_range(1, 4);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                // Optionally also change group membership
                if rng.gen_bool() {
                    let grp = rng.choose(&groups);
                    db.execute(&format!(
                        "UPDATE d3_root SET val = {val}, grp = '{grp}' WHERE id = {id}"
                    ))
                    .await;
                } else {
                    db.execute(&format!("UPDATE d3_root SET val = {val} WHERE id = {id}"))
                        .await;
                }
            }
        }

        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM d3_root WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_st("d3_branch_a").await;
        db.refresh_st("d3_branch_b").await;
        db.refresh_st("d3_agg_tip").await;

        let step = format!("cycle-{cycle}");
        assert_st_query_invariant(&db, "d3_branch_a", q_a, seed, cycle, &step).await;
        assert_st_query_invariant(&db, "d3_branch_b", q_b, seed, cycle, &step).await;
        assert_st_query_invariant(&db, "d3_agg_tip", q_tip, seed, cycle, &step).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 4: Two-root diamond — independent base tables
//
//   root_l (base)    root_r (base)
//       \               /
//        join_st (joins root_l and root_r directly)
//
// Both base tables receive simultaneous DML in each cycle, verifying
// correctness when independent sources contribute overlapping deltas.
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_prop_multi_root_join_simultaneous() {
    let seed: u64 = 0xD1A0_0004;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE d4_left (id INT PRIMARY KEY, val INT, key INT)")
        .await;
    db.execute("CREATE TABLE d4_right (id INT PRIMARY KEY, val INT, key INT)")
        .await;

    let mut l_ids = TrackedIds::new();
    let mut r_ids = TrackedIds::new();

    for _ in 0..INITIAL_ROWS {
        let id = l_ids.alloc();
        let val = rng.i32_range(1, 100);
        let key = rng.i32_range(1, 5);
        db.execute(&format!("INSERT INTO d4_left VALUES ({id}, {val}, {key})"))
            .await;
    }
    for _ in 0..INITIAL_ROWS {
        let id = r_ids.alloc();
        let val = rng.i32_range(1, 100);
        let key = rng.i32_range(1, 5);
        db.execute(&format!("INSERT INTO d4_right VALUES ({id}, {val}, {key})"))
            .await;
    }

    let q = "SELECT l.id AS lid, r.id AS rid, l.val AS lval, r.val AS rval \
             FROM d4_left l JOIN d4_right r ON l.key = r.key";
    db.create_st("d4_join", q, "1m", "DIFFERENTIAL").await;
    db.refresh_st("d4_join").await;
    assert_st_query_invariant(&db, "d4_join", q, seed, 0, "init").await;

    for cycle in 1..=CYCLES {
        // DML on BOTH tables in the same cycle
        let n_ins_l = rng.usize_range(1, 4);
        for _ in 0..n_ins_l {
            let id = l_ids.alloc();
            let val = rng.i32_range(1, 100);
            let key = rng.i32_range(1, 5);
            db.execute(&format!("INSERT INTO d4_left VALUES ({id}, {val}, {key})"))
                .await;
        }

        let n_ins_r = rng.usize_range(1, 4);
        for _ in 0..n_ins_r {
            let id = r_ids.alloc();
            let val = rng.i32_range(1, 100);
            let key = rng.i32_range(1, 5);
            db.execute(&format!("INSERT INTO d4_right VALUES ({id}, {val}, {key})"))
                .await;
        }

        // Updates on both sides
        let n_upd = rng.usize_range(0, 3);
        for _ in 0..n_upd {
            if let Some(id) = l_ids.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                let key = rng.i32_range(1, 5);
                db.execute(&format!(
                    "UPDATE d4_left SET val = {val}, key = {key} WHERE id = {id}"
                ))
                .await;
            }
            if let Some(id) = r_ids.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                db.execute(&format!("UPDATE d4_right SET val = {val} WHERE id = {id}"))
                    .await;
            }
        }

        // Deletes on both sides
        if rng.gen_bool()
            && let Some(id) = l_ids.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM d4_left WHERE id = {id}"))
                .await;
        }
        if rng.gen_bool()
            && let Some(id) = r_ids.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM d4_right WHERE id = {id}"))
                .await;
        }

        db.refresh_st("d4_join").await;
        let step = format!("cycle-{cycle}");
        assert_st_query_invariant(&db, "d4_join", q, seed, cycle, &step).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 5: Diamond with FULL OUTER JOIN at tip
//
// FULL JOIN generates 9 delta branches. With simultaneous changes on
// both branches, the overlap is maximal — the hardest case for weight
// aggregation correctness.
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_prop_diamond_full_join_simultaneous() {
    let seed: u64 = 0xD1A0_0005;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE d5_root (id INT PRIMARY KEY, val INT, side INT)")
        .await;

    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let val = rng.i32_range(1, 100);
        let side = rng.i32_range(1, 3);
        db.execute(&format!("INSERT INTO d5_root VALUES ({id}, {val}, {side})"))
            .await;
    }

    // Branch A: only side <= 2
    let q_a = "SELECT id, val AS va FROM d5_root WHERE side <= 2";
    db.create_st("d5_branch_a", q_a, "1m", "DIFFERENTIAL").await;

    // Branch B: only side >= 2 (overlap on side=2 creates shared rows)
    let q_b = "SELECT id, val AS vb FROM d5_root WHERE side >= 2";
    db.create_st("d5_branch_b", q_b, "1m", "DIFFERENTIAL").await;

    // Diamond tip: FULL JOIN (some rows only in A, some only in B, some in both)
    let q_tip = "SELECT a.id AS aid, b.id AS bid, a.va, b.vb \
                 FROM d5_branch_a a FULL JOIN d5_branch_b b ON a.id = b.id";
    db.create_st("d5_tip", q_tip, "1m", "DIFFERENTIAL").await;

    db.refresh_st("d5_branch_a").await;
    db.refresh_st("d5_branch_b").await;
    db.refresh_st("d5_tip").await;

    assert_st_query_invariant(&db, "d5_tip", q_tip, seed, 0, "init").await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let val = rng.i32_range(1, 100);
            let side = rng.i32_range(1, 3);
            db.execute(&format!("INSERT INTO d5_root VALUES ({id}, {val}, {side})"))
                .await;
        }

        // Updates that change the side column cause rows to cross filter
        // boundaries, creating asymmetric changes across branches
        let n_upd = rng.usize_range(1, 4);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                let side = rng.i32_range(1, 3);
                db.execute(&format!(
                    "UPDATE d5_root SET val = {val}, side = {side} WHERE id = {id}"
                ))
                .await;
            }
        }

        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM d5_root WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_st("d5_branch_a").await;
        db.refresh_st("d5_branch_b").await;
        db.refresh_st("d5_tip").await;

        let step = format!("cycle-{cycle}");
        assert_st_query_invariant(&db, "d5_branch_a", q_a, seed, cycle, &step).await;
        assert_st_query_invariant(&db, "d5_branch_b", q_b, seed, cycle, &step).await;
        assert_st_query_invariant(&db, "d5_tip", q_tip, seed, cycle, &step).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 6: Deep diamond — 3 levels of intermediaries
//
//         root (base table)
//        /       \
//    mid_a       mid_b        (level 1: scan/filter STs)
//       |         |
//    mid_a2      mid_b2       (level 2: projection STs)
//        \       /
//        deep_tip             (level 3: join ST)
//
// Deeper DAGs amplify the chance of delta overlap at each level.
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_prop_diamond_deep_simultaneous() {
    let seed: u64 = 0xD1A0_0006;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE d6_root (id INT PRIMARY KEY, val INT)")
        .await;

    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let val = rng.i32_range(1, 100);
        db.execute(&format!("INSERT INTO d6_root VALUES ({id}, {val})"))
            .await;
    }

    let q_a = "SELECT id, val FROM d6_root";
    db.create_st("d6_mid_a", q_a, "1m", "DIFFERENTIAL").await;

    let q_b = "SELECT id, val FROM d6_root";
    db.create_st("d6_mid_b", q_b, "1m", "DIFFERENTIAL").await;

    let q_a2 = "SELECT id, val + 1 AS va2 FROM d6_mid_a";
    db.create_st("d6_mid_a2", q_a2, "1m", "DIFFERENTIAL").await;

    let q_b2 = "SELECT id, val * 2 AS vb2 FROM d6_mid_b";
    db.create_st("d6_mid_b2", q_b2, "1m", "DIFFERENTIAL").await;

    let q_tip = "SELECT a.id, a.va2, b.vb2 \
                 FROM d6_mid_a2 a JOIN d6_mid_b2 b ON a.id = b.id";
    db.create_st("d6_deep_tip", q_tip, "1m", "DIFFERENTIAL")
        .await;

    // Refresh in topological order: mid_a, mid_b, mid_a2, mid_b2, deep_tip
    for name in &[
        "d6_mid_a",
        "d6_mid_b",
        "d6_mid_a2",
        "d6_mid_b2",
        "d6_deep_tip",
    ] {
        db.refresh_st(name).await;
    }
    assert_st_query_invariant(&db, "d6_deep_tip", q_tip, seed, 0, "init").await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 4);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let val = rng.i32_range(1, 100);
            db.execute(&format!("INSERT INTO d6_root VALUES ({id}, {val})"))
                .await;
        }

        let n_upd = rng.usize_range(1, 3);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                db.execute(&format!("UPDATE d6_root SET val = {val} WHERE id = {id}"))
                    .await;
            }
        }

        if rng.gen_bool()
            && let Some(id) = ids.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM d6_root WHERE id = {id}"))
                .await;
        }

        for name in &[
            "d6_mid_a",
            "d6_mid_b",
            "d6_mid_a2",
            "d6_mid_b2",
            "d6_deep_tip",
        ] {
            db.refresh_st(name).await;
        }

        let step = format!("cycle-{cycle}");
        assert_st_query_invariant(&db, "d6_deep_tip", q_tip, seed, cycle, &step).await;
    }
}
