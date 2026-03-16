//! Deterministic property-style cross-source snapshot consistency tests.
//!
//! Validates that stream tables built over multiple sources maintain correct
//! contents under seeded random mutation traces, including cases where both
//! sources change in the same cycle (simultaneous mutation).
//!
//! ## Why these tests exist
//!
//! Cross-source correctness is especially vulnerable to partial refreshes,
//! mixed snapshots, simultaneous changes across multiple sources, and
//! diamond fan-in bugs.  Fixed-example tests prove stated stories; these
//! property tests prove the invariant holds across a wide space of traces.
//!
//! ## Topology (shared by all three tests)
//!
//! ```text
//!   snap_alpha (id, grp, val)    snap_beta (id, grp, score)
//!          |                              |
//!   snap_st_alpha_sum            snap_st_beta_sum
//!          |                              |
//!              snap_st_cross (JOIN on grp)
//! ```
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::{
    E2eDb,
    property_support::{SeededRng, TraceConfig, TrackedIds, assert_st_query_invariants},
};

const SNAP_GROUPS: [&str; 4] = ["north", "south", "east", "west"];

const SNAP_INVARIANTS: [(&str, &str); 3] = [
    (
        "snap_st_alpha_sum",
        "SELECT grp, SUM(val) AS total FROM snap_alpha GROUP BY grp",
    ),
    (
        "snap_st_beta_sum",
        "SELECT grp, SUM(score) AS total FROM snap_beta GROUP BY grp",
    ),
    (
        "snap_st_cross",
        "SELECT a.grp, a.total AS alpha_total, b.total AS beta_total \
         FROM (SELECT grp, SUM(val) AS total FROM snap_alpha GROUP BY grp) a \
         JOIN (SELECT grp, SUM(score) AS total FROM snap_beta GROUP BY grp) b \
         ON b.grp = a.grp",
    ),
];

// ══════════════════════════════════════════════════════════════════════
// Test 1 — Two-source atomic consistency
// ══════════════════════════════════════════════════════════════════════

/// Mutate each source independently (or both together) across many cycles.
/// After each mutation batch the full ST chain is refreshed and every ST
/// must match its ground-truth query from the base tables.
///
/// This exercises:
/// - alpha-only cycles (beta ST should be unchanged and still correct)
/// - beta-only cycles (alpha ST should be unchanged and still correct)
/// - same-cycle mutations to both sources (cross ST must see both changes)
#[tokio::test]
async fn test_prop_two_source_atomic_consistency() {
    let config = TraceConfig::from_env();
    for seed in config.seeds(0xF0A0_1001) {
        run_two_source_trace(seed, config).await;
    }
}

// ══════════════════════════════════════════════════════════════════════
// Test 2 — Diamond simultaneous-change invariant
// ══════════════════════════════════════════════════════════════════════

/// In every cycle, BOTH sources are mutated before any refresh.
///
/// This specifically targets the "simultaneous multi-source change" scenario
/// which is the most likely to expose diamond fan-in bugs: the intermediate
/// STs for each source are refreshed sequentially after both sources have
/// already changed, so the apex sees a consistent view of the new state.
#[tokio::test]
async fn test_prop_diamond_same_cycle_multi_source_changes() {
    let config = TraceConfig::from_env();
    for seed in config.seeds(0xF0A0_2001) {
        run_diamond_simultaneous_trace(seed, config).await;
    }
}

// ══════════════════════════════════════════════════════════════════════
// Test 3 — Skewed-churn no-drift invariant
// ══════════════════════════════════════════════════════════════════════

/// One source (alpha) changes heavily every cycle; the other (beta) changes
/// in only ~1 in 4 cycles.  After each cycle:
///
/// - Both STs must match their ground-truth queries.
/// - When beta was NOT mutated, `snap_st_beta_sum` must equal the same
///   value it had on the previous cycle (no spurious drift).
#[tokio::test]
async fn test_prop_cross_source_skewed_churn_no_drift() {
    let config = TraceConfig::from_env();
    for seed in config.seeds(0xF0A0_3001) {
        run_skewed_churn_trace(seed, config).await;
    }
}

// ══════════════════════════════════════════════════════════════════════
// Trace runners
// ══════════════════════════════════════════════════════════════════════

async fn run_two_source_trace(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    let mut rng = SeededRng::new(seed);
    let mut ids_alpha = TrackedIds::new();
    let mut ids_beta = TrackedIds::new();

    setup_two_source_pipeline(
        &db,
        &mut rng,
        &mut ids_alpha,
        &mut ids_beta,
        config.initial_rows,
    )
    .await;
    assert_st_query_invariants(&db, &SNAP_INVARIANTS, seed, 0, "baseline").await;

    for cycle in 1..=config.cycles {
        // 0 = alpha only, 1 = beta only, 2/3 = both sources
        let which = rng.usize_range(0, 3);
        let mut legs: Vec<String> = Vec::new();

        if which != 1 {
            legs.push(
                apply_source_mutations(&db, &mut rng, &mut ids_alpha, "snap_alpha", "val").await,
            );
        }
        if which != 0 {
            legs.push(
                apply_source_mutations(&db, &mut rng, &mut ids_beta, "snap_beta", "score").await,
            );
        }
        let step = legs.join(" | ");

        refresh_two_source_pipeline(&db).await;
        assert_st_query_invariants(&db, &SNAP_INVARIANTS, seed, cycle, &step).await;
    }
}

async fn run_diamond_simultaneous_trace(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    let mut rng = SeededRng::new(seed);
    let mut ids_alpha = TrackedIds::new();
    let mut ids_beta = TrackedIds::new();

    setup_two_source_pipeline(
        &db,
        &mut rng,
        &mut ids_alpha,
        &mut ids_beta,
        config.initial_rows,
    )
    .await;
    assert_st_query_invariants(&db, &SNAP_INVARIANTS, seed, 0, "baseline").await;

    for cycle in 1..=config.cycles {
        // Always mutate BOTH sources before any refresh to exercise simultaneous
        // multi-source delta handling in the differential engine.
        let step_a =
            apply_source_mutations(&db, &mut rng, &mut ids_alpha, "snap_alpha", "val").await;
        let step_b =
            apply_source_mutations(&db, &mut rng, &mut ids_beta, "snap_beta", "score").await;
        let step = format!("a:[{step_a}] b:[{step_b}]");

        refresh_two_source_pipeline(&db).await;
        assert_st_query_invariants(&db, &SNAP_INVARIANTS, seed, cycle, &step).await;
    }
}

async fn run_skewed_churn_trace(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    let mut rng = SeededRng::new(seed);
    let mut ids_alpha = TrackedIds::new();
    let mut ids_beta = TrackedIds::new();

    setup_two_source_pipeline(
        &db,
        &mut rng,
        &mut ids_alpha,
        &mut ids_beta,
        config.initial_rows,
    )
    .await;
    assert_st_query_invariants(&db, &SNAP_INVARIANTS, seed, 0, "baseline").await;

    for cycle in 1..=config.cycles {
        // Alpha always churns heavily (3-5 ops per cycle).
        let step_a =
            apply_heavy_source_mutations(&db, &mut rng, &mut ids_alpha, "snap_alpha", "val").await;

        // Beta mutates in roughly 1 in 4 cycles.
        let step_b = if rng.usize_range(0, 3) == 0 {
            apply_source_mutations(&db, &mut rng, &mut ids_beta, "snap_beta", "score").await
        } else {
            "noop".to_string()
        };
        let step = format!("heavy-a:[{step_a}] light-b:[{step_b}]");

        refresh_two_source_pipeline(&db).await;
        assert_st_query_invariants(&db, &SNAP_INVARIANTS, seed, cycle, &step).await;
    }
}

// ══════════════════════════════════════════════════════════════════════
// Setup helpers
// ══════════════════════════════════════════════════════════════════════

async fn setup_two_source_pipeline(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids_alpha: &mut TrackedIds,
    ids_beta: &mut TrackedIds,
    initial_rows: usize,
) {
    db.execute(
        "CREATE TABLE snap_alpha (
            id  INT PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute(
        "CREATE TABLE snap_beta (
            id    INT PRIMARY KEY,
            grp   TEXT NOT NULL,
            score INT NOT NULL
        )",
    )
    .await;

    for _ in 0..initial_rows {
        let id = ids_alpha.alloc();
        let grp = *rng.choose(&SNAP_GROUPS);
        let val = rng.i32_range(1, 50);
        db.execute(&format!(
            "INSERT INTO snap_alpha (id, grp, val) VALUES ({id}, '{grp}', {val})"
        ))
        .await;
    }
    for _ in 0..initial_rows {
        let id = ids_beta.alloc();
        let grp = *rng.choose(&SNAP_GROUPS);
        let score = rng.i32_range(1, 100);
        db.execute(&format!(
            "INSERT INTO snap_beta (id, grp, score) VALUES ({id}, '{grp}', {score})"
        ))
        .await;
    }

    db.create_st(
        "snap_st_alpha_sum",
        "SELECT grp, SUM(val) AS total FROM snap_alpha GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "snap_st_beta_sum",
        "SELECT grp, SUM(score) AS total FROM snap_beta GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "snap_st_cross",
        "SELECT a.grp, a.total AS alpha_total, b.total AS beta_total \
         FROM snap_st_alpha_sum a \
         JOIN snap_st_beta_sum b ON b.grp = a.grp",
        "calculated",
        "DIFFERENTIAL",
    )
    .await;
}

async fn refresh_two_source_pipeline(db: &E2eDb) {
    db.refresh_st_with_retry("snap_st_alpha_sum").await;
    db.refresh_st_with_retry("snap_st_beta_sum").await;
    db.refresh_st_with_retry("snap_st_cross").await;
}

// ══════════════════════════════════════════════════════════════════════
// Mutation helpers
// ══════════════════════════════════════════════════════════════════════

/// Apply 1–3 random mutations (insert / update / delete / noop) to a source.
async fn apply_source_mutations(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids: &mut TrackedIds,
    table: &str,
    val_col: &str,
) -> String {
    let steps = rng.usize_range(1, 3);
    let mut trace = Vec::with_capacity(steps);

    for _ in 0..steps {
        match rng.usize_range(0, 3) {
            0 => {
                let id = ids.alloc();
                let grp = *rng.choose(&SNAP_GROUPS);
                let val = rng.i32_range(1, 100);
                db.execute(&format!(
                    "INSERT INTO {table} (id, grp, {val_col}) VALUES ({id}, '{grp}', {val})"
                ))
                .await;
                trace.push(format!("ins(id={id})"));
            }
            1 => {
                if let Some(id) = ids.pick(rng) {
                    let grp = *rng.choose(&SNAP_GROUPS);
                    let val = rng.i32_range(1, 100);
                    db.execute(&format!(
                        "UPDATE {table} SET grp = '{grp}', {val_col} = {val} WHERE id = {id}"
                    ))
                    .await;
                    trace.push(format!("upd(id={id})"));
                } else {
                    trace.push("upd(skip)".to_string());
                }
            }
            2 => {
                if let Some(id) = ids.remove_random(rng) {
                    db.execute(&format!("DELETE FROM {table} WHERE id = {id}"))
                        .await;
                    trace.push(format!("del(id={id})"));
                } else {
                    trace.push("del(skip)".to_string());
                }
            }
            _ => {
                trace.push("noop".to_string());
            }
        }
    }

    trace.join(", ")
}

/// Apply 3–5 random mutations — biased toward inserts — for the heavy-churn
/// scenario so the source table does not gradually deplete to zero rows.
async fn apply_heavy_source_mutations(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids: &mut TrackedIds,
    table: &str,
    val_col: &str,
) -> String {
    let steps = rng.usize_range(3, 5);
    let mut trace = Vec::with_capacity(steps);

    for _ in 0..steps {
        // 0,3 → insert (50%), 1 → update (25%), 2 → delete (25%)
        match rng.usize_range(0, 3) {
            0 | 3 => {
                let id = ids.alloc();
                let grp = *rng.choose(&SNAP_GROUPS);
                let val = rng.i32_range(1, 100);
                db.execute(&format!(
                    "INSERT INTO {table} (id, grp, {val_col}) VALUES ({id}, '{grp}', {val})"
                ))
                .await;
                trace.push(format!("ins(id={id})"));
            }
            1 => {
                if let Some(id) = ids.pick(rng) {
                    let grp = *rng.choose(&SNAP_GROUPS);
                    let val = rng.i32_range(1, 100);
                    db.execute(&format!(
                        "UPDATE {table} SET grp = '{grp}', {val_col} = {val} WHERE id = {id}"
                    ))
                    .await;
                    trace.push(format!("upd(id={id})"));
                } else {
                    trace.push("upd(skip)".to_string());
                }
            }
            2 => {
                if let Some(id) = ids.remove_random(rng) {
                    db.execute(&format!("DELETE FROM {table} WHERE id = {id}"))
                        .await;
                    trace.push(format!("del(id={id})"));
                } else {
                    trace.push("del(skip)".to_string());
                }
            }
            _ => unreachable!(),
        }
    }

    trace.join(", ")
}
