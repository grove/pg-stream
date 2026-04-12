//! TEST-4: Z-set weight algebra property tests — E2E validation.
//!
//! These tests exercise the Z-set weight accounting contract through the full
//! system (CDC triggers → change buffers → weight aggregation → MERGE). They
//! complement the pure-Rust proptest proofs in `src/refresh.rs` (CORR-4) by
//! verifying that the algebra holds under real PostgreSQL execution, including:
//!
//!   1. **I/D cancellation**: rapid insert-then-delete of the same row should
//!      produce zero net change (HAVING <> 0 filters it out).
//!   2. **Multi-update coalescing**: multiple updates to the same row within a
//!      single refresh window should coalesce into a single net action.
//!   3. **Fan-in joins**: 3+ sources merging into a single join, each modified
//!      independently, with the downstream ST remaining correct.
//!   4. **Weight storm**: large batches of interleaved I/D/U actions that
//!      exercise the GROUP BY + SUM(weight) + HAVING pipeline.

mod e2e;

use e2e::{
    E2eDb,
    property_support::{
        SeededRng, TraceConfig, TrackedIds, assert_st_query_invariant, assert_st_query_invariants,
    },
};

// ══════════════════════════════════════════════════════════════════════════
// Test 1: I/D cancellation — insert then delete before refresh
// ══════════════════════════════════════════════════════════════════════════

/// Insert N rows, then delete all N before refresh. The stream table should
/// remain unchanged because the CDC buffer contains matching I/D pairs whose
/// weights cancel to zero. Repeat for multiple cycles.
#[tokio::test]
async fn test_prop_zset_id_cancellation() {
    let config = TraceConfig::from_env();

    for seed in config.seeds(0xB3_0001) {
        run_id_cancellation(seed, config).await;
    }
}

async fn run_id_cancellation(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("ALTER SYSTEM SET pg_trickle.enabled = false")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    let mut rng = SeededRng::new(seed);
    let mut ids = TrackedIds::new();

    // Create base table with initial data.
    db.execute(
        "CREATE TABLE zset_cancel_src (
            id  INT PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;

    let groups = ["x", "y", "z"];
    for _ in 0..config.initial_rows {
        let id = ids.alloc();
        let grp = *rng.choose(&groups);
        let val = rng.i32_range(1, 100);
        db.execute(&format!(
            "INSERT INTO zset_cancel_src (id, grp, val) VALUES ({id}, '{grp}', {val})"
        ))
        .await;
    }

    db.create_st(
        "zset_cancel_agg",
        "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM zset_cancel_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.refresh_st_with_retry("zset_cancel_agg").await;
    assert_st_query_invariant(
        &db,
        "zset_cancel_agg",
        "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM zset_cancel_src GROUP BY grp",
        seed,
        0,
        "baseline",
    )
    .await;

    for cycle in 1..=config.cycles {
        // Insert some rows, then immediately delete them — all within one
        // refresh window. The net change is zero.
        let n_phantom = rng.usize_range(1, 6);
        let mut next_phantom_id = (cycle as i32) * 100_000;
        for _ in 0..n_phantom {
            next_phantom_id += 1;
            let pid = next_phantom_id;
            let grp = *rng.choose(&groups);
            let val = rng.i32_range(1, 100);
            db.execute(&format!(
                "INSERT INTO zset_cancel_src (id, grp, val) VALUES ({pid}, '{grp}', {val})"
            ))
            .await;
            db.execute(&format!("DELETE FROM zset_cancel_src WHERE id = {pid}"))
                .await;
        }

        // Also do a real mutation so the test isn't trivially noops.
        if rng.gen_bool()
            && let Some(upd_id) = ids.pick(&mut rng)
        {
            let val = rng.i32_range(1, 100);
            db.execute(&format!(
                "UPDATE zset_cancel_src SET val = {val} WHERE id = {upd_id}"
            ))
            .await;
        }

        db.refresh_st_with_retry("zset_cancel_agg").await;
        assert_st_query_invariant(
            &db,
            "zset_cancel_agg",
            "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM zset_cancel_src GROUP BY grp",
            seed,
            cycle,
            &format!("cancel {n_phantom} phantom rows"),
        )
        .await;
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Test 2: Multi-update coalescing — many updates to same row
// ══════════════════════════════════════════════════════════════════════════

/// Update the same row multiple times within a single refresh window.
/// Each update generates a D+I pair in CDC. The weight aggregation must
/// correctly coalesce all intermediate states, producing a single net
/// action representing the final value.
#[tokio::test]
async fn test_prop_zset_multi_update_coalesce() {
    let config = TraceConfig::from_env();

    for seed in config.seeds(0xB3_0002) {
        run_multi_update_coalesce(seed, config).await;
    }
}

async fn run_multi_update_coalesce(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("ALTER SYSTEM SET pg_trickle.enabled = false")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    let mut rng = SeededRng::new(seed);
    let mut ids = TrackedIds::new();

    db.execute(
        "CREATE TABLE zset_coalesce_src (
            id   INT PRIMARY KEY,
            name TEXT NOT NULL,
            val  INT NOT NULL
        )",
    )
    .await;

    for _ in 0..config.initial_rows {
        let id = ids.alloc();
        let name = rng.gen_alpha(5);
        let val = rng.i32_range(1, 100);
        db.execute(&format!(
            "INSERT INTO zset_coalesce_src (id, name, val) VALUES ({id}, '{name}', {val})"
        ))
        .await;
    }

    let query = "SELECT name, SUM(val) AS total FROM zset_coalesce_src GROUP BY name";
    db.create_st("zset_coalesce_agg", query, "1m", "DIFFERENTIAL")
        .await;
    db.refresh_st_with_retry("zset_coalesce_agg").await;
    assert_st_query_invariant(&db, "zset_coalesce_agg", query, seed, 0, "baseline").await;

    for cycle in 1..=config.cycles {
        // Pick a random existing row and update it N times in quick succession.
        if let Some(target_id) = ids.pick(&mut rng) {
            let n_updates = rng.usize_range(2, 8);
            for _ in 0..n_updates {
                let name = rng.gen_alpha(5);
                let val = rng.i32_range(1, 100);
                db.execute(&format!(
                    "UPDATE zset_coalesce_src SET name = '{name}', val = {val} WHERE id = {target_id}"
                ))
                .await;
            }
        }

        // Also add/remove rows to stress the system alongside coalescing.
        let id = ids.alloc();
        let name = rng.gen_alpha(5);
        let val = rng.i32_range(1, 100);
        db.execute(&format!(
            "INSERT INTO zset_coalesce_src (id, name, val) VALUES ({id}, '{name}', {val})"
        ))
        .await;

        if rng.gen_bool()
            && let Some(del_id) = ids.remove_random(&mut rng)
        {
            db.execute(&format!(
                "DELETE FROM zset_coalesce_src WHERE id = {del_id}"
            ))
            .await;
        }

        db.refresh_st_with_retry("zset_coalesce_agg").await;
        assert_st_query_invariant(
            &db,
            "zset_coalesce_agg",
            query,
            seed,
            cycle,
            "multi-update coalesce",
        )
        .await;
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Test 3: Fan-in join — 3 independent sources → single join ST
// ══════════════════════════════════════════════════════════════════════════

const FANIN_INVARIANTS: [(&str, &str); 1] = [(
    "zset_fanin_apex",
    "SELECT a.grp, a.total_a, b.total_b, c.total_c \
     FROM (SELECT grp, SUM(val) AS total_a FROM zset_fanin_a GROUP BY grp) a \
     JOIN (SELECT grp, SUM(val) AS total_b FROM zset_fanin_b GROUP BY grp) b \
       ON b.grp = a.grp \
     JOIN (SELECT grp, SUM(val) AS total_c FROM zset_fanin_c GROUP BY grp) c \
       ON c.grp = a.grp",
)];

/// Three independent base tables each feed a stream table; a fourth ST
/// joins the three. With random mutations to all three sources in each
/// cycle, the apex must always converge to ground truth.
#[tokio::test]
async fn test_prop_zset_fanin_three_source_join() {
    let config = TraceConfig::from_env();

    for seed in config.seeds(0xB3_0003) {
        run_fanin_three_source(seed, config).await;
    }
}

async fn run_fanin_three_source(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("ALTER SYSTEM SET pg_trickle.enabled = false")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    let mut rng = SeededRng::new(seed);

    let groups = ["north", "south", "east", "west"];
    let tables = ["zset_fanin_a", "zset_fanin_b", "zset_fanin_c"];
    let st_names = ["zset_fanin_st_a", "zset_fanin_st_b", "zset_fanin_st_c"];
    let mut id_sets: Vec<TrackedIds> =
        vec![TrackedIds::new(), TrackedIds::new(), TrackedIds::new()];

    for tbl in &tables {
        db.execute(&format!(
            "CREATE TABLE {tbl} (
                id  INT PRIMARY KEY,
                grp TEXT NOT NULL,
                val INT NOT NULL
            )"
        ))
        .await;
    }

    // Seed initial data — ensure all groups exist in all tables.
    for (i, tbl) in tables.iter().enumerate() {
        for grp in &groups {
            let id = id_sets[i].alloc();
            let val = rng.i32_range(1, 50);
            db.execute(&format!(
                "INSERT INTO {tbl} (id, grp, val) VALUES ({id}, '{grp}', {val})"
            ))
            .await;
        }
        for _ in 0..config.initial_rows {
            let id = id_sets[i].alloc();
            let grp = *rng.choose(&groups);
            let val = rng.i32_range(1, 50);
            db.execute(&format!(
                "INSERT INTO {tbl} (id, grp, val) VALUES ({id}, '{grp}', {val})"
            ))
            .await;
        }
    }

    // Create per-source aggregate STs.
    for (i, tbl) in tables.iter().enumerate() {
        let suffix = ['a', 'b', 'c'][i];
        db.create_st(
            st_names[i],
            &format!("SELECT grp, SUM(val) AS total_{suffix} FROM {tbl} GROUP BY grp"),
            "1m",
            "DIFFERENTIAL",
        )
        .await;
    }

    // Create apex join ST.
    db.create_st(
        "zset_fanin_apex",
        "SELECT a.grp, a.total_a, b.total_b, c.total_c \
         FROM zset_fanin_st_a a \
         JOIN zset_fanin_st_b b ON b.grp = a.grp \
         JOIN zset_fanin_st_c c ON c.grp = a.grp",
        "calculated",
        "DIFFERENTIAL",
    )
    .await;

    // Initial refresh.
    for name in st_names {
        db.refresh_st_with_retry(name).await;
    }
    db.refresh_st_with_retry("zset_fanin_apex").await;
    assert_st_query_invariants(&db, &FANIN_INVARIANTS, seed, 0, "baseline").await;

    for cycle in 1..=config.cycles {
        // Mutate all three sources randomly.
        for (i, tbl) in tables.iter().enumerate() {
            apply_fanin_mutations(&db, &mut rng, &mut id_sets[i], tbl, &groups).await;
        }

        for name in st_names {
            db.refresh_st_with_retry(name).await;
        }
        db.refresh_st_with_retry("zset_fanin_apex").await;
        assert_st_query_invariants(&db, &FANIN_INVARIANTS, seed, cycle, "fanin 3-source").await;
    }
}

async fn apply_fanin_mutations(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids: &mut TrackedIds,
    table: &str,
    groups: &[&str],
) {
    let steps = rng.usize_range(1, 4);
    for _ in 0..steps {
        match rng.usize_range(0, 3) {
            0 => {
                let id = ids.alloc();
                let grp = *rng.choose(groups);
                let val = rng.i32_range(1, 50);
                db.execute(&format!(
                    "INSERT INTO {table} (id, grp, val) VALUES ({id}, '{grp}', {val})"
                ))
                .await;
            }
            1 => {
                if let Some(id) = ids.pick(rng) {
                    let grp = *rng.choose(groups);
                    let val = rng.i32_range(1, 50);
                    db.execute(&format!(
                        "UPDATE {table} SET grp = '{grp}', val = {val} WHERE id = {id}"
                    ))
                    .await;
                }
            }
            2 => {
                if let Some(id) = ids.remove_random(rng) {
                    db.execute(&format!("DELETE FROM {table} WHERE id = {id}"))
                        .await;
                }
            }
            _ => {}
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Test 4: Weight storm — large interleaved I/D/U batches
// ══════════════════════════════════════════════════════════════════════════

/// Exercise the weight pipeline with large batches (10-20 operations per
/// cycle) of interleaved inserts, updates, and deletes. This stresses the
/// GROUP BY + SUM(weight) + HAVING pipeline with many CDC rows per row_id.
#[tokio::test]
async fn test_prop_zset_weight_storm() {
    let config = TraceConfig::from_env();

    for seed in config.seeds(0xB3_0004) {
        run_weight_storm(seed, config).await;
    }
}

async fn run_weight_storm(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("ALTER SYSTEM SET pg_trickle.enabled = false")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    let mut rng = SeededRng::new(seed);
    let mut ids = TrackedIds::new();

    db.execute(
        "CREATE TABLE zset_storm_src (
            id   INT PRIMARY KEY,
            cat  TEXT NOT NULL,
            val  INT NOT NULL
        )",
    )
    .await;

    let categories = ["a", "b", "c", "d", "e"];
    for _ in 0..config.initial_rows {
        let id = ids.alloc();
        let cat = *rng.choose(&categories);
        let val = rng.i32_range(1, 100);
        db.execute(&format!(
            "INSERT INTO zset_storm_src (id, cat, val) VALUES ({id}, '{cat}', {val})"
        ))
        .await;
    }

    let query = "SELECT cat, SUM(val) AS total, COUNT(*) AS cnt, \
                 AVG(val)::INT AS avg_val FROM zset_storm_src GROUP BY cat";
    db.create_st("zset_storm_agg", query, "1m", "DIFFERENTIAL")
        .await;
    db.refresh_st_with_retry("zset_storm_agg").await;
    assert_st_query_invariant(&db, "zset_storm_agg", query, seed, 0, "baseline").await;

    for cycle in 1..=config.cycles {
        // Large mutation batch — 10-20 operations mixing I/D/U.
        let steps = rng.usize_range(10, 20);
        for _ in 0..steps {
            match rng.usize_range(0, 4) {
                0 | 1 => {
                    // Insert (biased towards insert to keep table populated)
                    let id = ids.alloc();
                    let cat = *rng.choose(&categories);
                    let val = rng.i32_range(1, 100);
                    db.execute(&format!(
                        "INSERT INTO zset_storm_src (id, cat, val) VALUES ({id}, '{cat}', {val})"
                    ))
                    .await;
                }
                2 => {
                    // Update
                    if let Some(id) = ids.pick(&mut rng) {
                        let cat = *rng.choose(&categories);
                        let val = rng.i32_range(1, 100);
                        db.execute(&format!(
                            "UPDATE zset_storm_src SET cat = '{cat}', val = {val} WHERE id = {id}"
                        ))
                        .await;
                    }
                }
                3 => {
                    // Delete
                    if let Some(id) = ids.remove_random(&mut rng) {
                        db.execute(&format!("DELETE FROM zset_storm_src WHERE id = {id}"))
                            .await;
                    }
                }
                _ => {}
            }
        }

        db.refresh_st_with_retry("zset_storm_agg").await;
        assert_st_query_invariant(
            &db,
            "zset_storm_agg",
            query,
            seed,
            cycle,
            &format!("storm batch {steps} ops"),
        )
        .await;
    }
}
