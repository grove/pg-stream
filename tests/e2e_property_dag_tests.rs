//! Deterministic property-style DAG invariant tests.
//!
//! These tests extend the existing seeded E2E property harness into
//! multi-layer DAG topologies, where fixed examples are less effective at
//! catching drift across repeated refresh cycles.

mod e2e;

use e2e::{
    E2eDb,
    property_support::{
        SeededRng, TraceConfig, TrackedIds, assert_data_timestamps_stable,
        assert_st_query_invariants, snapshot_data_timestamps,
    },
};

const LINEAR_GROUPS: [&str; 4] = ["alpha", "beta", "gamma", "delta"];
const DIAMOND_GROUPS: [&str; 4] = ["red", "blue", "green", "gold"];
const LINEAR_STS: [&str; 3] = ["prop_lin_l1", "prop_lin_l2", "prop_lin_l3"];
const LINEAR_INVARIANTS: [(&str, &str); 3] = [
    (
        "prop_lin_l1",
        "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM prop_lin_src GROUP BY grp",
    ),
    (
        "prop_lin_l2",
        "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt, SUM(val) + COUNT(*) AS score \
         FROM prop_lin_src GROUP BY grp",
    ),
    (
        "prop_lin_l3",
        "SELECT grp, SUM(val) + COUNT(*) AS score \
         FROM prop_lin_src GROUP BY grp \
         HAVING SUM(val) + COUNT(*) >= 25",
    ),
];
const DIAMOND_INVARIANTS: [(&str, &str); 3] = [
    (
        "prop_dia_l1_sum",
        "SELECT grp, SUM(val) AS total FROM prop_dia_src GROUP BY grp",
    ),
    (
        "prop_dia_l1_count",
        "SELECT grp, COUNT(*) AS cnt FROM prop_dia_src GROUP BY grp",
    ),
    (
        "prop_dia_l2",
        "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt, SUM(val) + COUNT(*) AS checksum \
         FROM prop_dia_src GROUP BY grp",
    ),
];

#[tokio::test]
async fn test_prop_linear_3_layer_mixed_trace() {
    let config = TraceConfig::from_env();

    for seed in config.seeds(0xDAD0_1001) {
        run_linear_mixed_trace(seed, config).await;
    }
}

#[tokio::test]
async fn test_prop_diamond_mixed_trace() {
    let config = TraceConfig::from_env();

    for seed in config.seeds(0xDAD0_2001) {
        run_diamond_mixed_trace(seed, config).await;
    }
}

#[tokio::test]
async fn test_prop_noop_cycles_do_not_drift() {
    let config = TraceConfig::from_env();

    for seed in config.seeds(0xDAD0_3001) {
        run_linear_noop_trace(seed, config).await;
    }
}

async fn run_linear_mixed_trace(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    let mut rng = SeededRng::new(seed);
    let mut ids = TrackedIds::new();

    setup_linear_pipeline(&db, &mut rng, &mut ids, config.initial_rows).await;
    assert_st_query_invariants(&db, &LINEAR_INVARIANTS, seed, 0, "baseline").await;

    for cycle in 1..=config.cycles {
        let step = apply_linear_mutation_batch(&db, &mut rng, &mut ids).await;
        refresh_linear_pipeline(&db).await;
        assert_st_query_invariants(&db, &LINEAR_INVARIANTS, seed, cycle, &step).await;
    }
}

async fn run_diamond_mixed_trace(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    let mut rng = SeededRng::new(seed);
    let mut ids = TrackedIds::new();

    setup_diamond_pipeline(&db, &mut rng, &mut ids, config.initial_rows).await;
    assert_st_query_invariants(&db, &DIAMOND_INVARIANTS, seed, 0, "baseline").await;

    for cycle in 1..=config.cycles {
        let step = apply_diamond_mutation_batch(&db, &mut rng, &mut ids).await;
        refresh_diamond_pipeline(&db).await;
        assert_st_query_invariants(&db, &DIAMOND_INVARIANTS, seed, cycle, &step).await;
    }
}

async fn run_linear_noop_trace(seed: u64, config: TraceConfig) {
    let db = E2eDb::new().await.with_extension().await;
    let mut rng = SeededRng::new(seed);
    let mut ids = TrackedIds::new();

    setup_linear_pipeline(&db, &mut rng, &mut ids, config.initial_rows).await;
    refresh_linear_pipeline(&db).await;
    assert_st_query_invariants(&db, &LINEAR_INVARIANTS, seed, 0, "warmup refresh").await;

    let timestamps = snapshot_data_timestamps(&db, &LINEAR_STS).await;
    for cycle in 1..=config.cycles {
        refresh_linear_pipeline(&db).await;
        assert_st_query_invariants(&db, &LINEAR_INVARIANTS, seed, cycle, "noop refresh").await;
        assert_data_timestamps_stable(&db, &LINEAR_STS, &timestamps, seed, cycle, "noop refresh")
            .await;
    }
}

async fn setup_linear_pipeline(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids: &mut TrackedIds,
    initial_rows: usize,
) {
    db.execute(
        "CREATE TABLE prop_lin_src (
            id  INT PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;

    for _ in 0..initial_rows {
        let id = ids.alloc();
        let grp = *rng.choose(&LINEAR_GROUPS);
        let val = rng.i32_range(1, 40);
        db.execute(&format!(
            "INSERT INTO prop_lin_src (id, grp, val) VALUES ({id}, '{grp}', {val})"
        ))
        .await;
    }

    db.create_st(
        "prop_lin_l1",
        "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM prop_lin_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "prop_lin_l2",
        "SELECT grp, total, cnt, total + cnt AS score FROM prop_lin_l1",
        "calculated",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "prop_lin_l3",
        "SELECT grp, score FROM prop_lin_l2 WHERE score >= 25",
        "calculated",
        "DIFFERENTIAL",
    )
    .await;
}

async fn setup_diamond_pipeline(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids: &mut TrackedIds,
    initial_rows: usize,
) {
    db.execute(
        "CREATE TABLE prop_dia_src (
            id  INT PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;

    for _ in 0..initial_rows {
        let id = ids.alloc();
        let grp = *rng.choose(&DIAMOND_GROUPS);
        let val = rng.i32_range(1, 50);
        db.execute(&format!(
            "INSERT INTO prop_dia_src (id, grp, val) VALUES ({id}, '{grp}', {val})"
        ))
        .await;
    }

    db.create_st(
        "prop_dia_l1_sum",
        "SELECT grp, SUM(val) AS total FROM prop_dia_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "prop_dia_l1_count",
        "SELECT grp, COUNT(*) AS cnt FROM prop_dia_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "prop_dia_l2",
        "SELECT s.grp, s.total, c.cnt, s.total + c.cnt AS checksum \
         FROM prop_dia_l1_sum s \
         JOIN prop_dia_l1_count c ON c.grp = s.grp",
        "calculated",
        "DIFFERENTIAL",
    )
    .await;
}

async fn refresh_linear_pipeline(db: &E2eDb) {
    for name in LINEAR_STS {
        db.refresh_st_with_retry(name).await;
    }
}

async fn refresh_diamond_pipeline(db: &E2eDb) {
    for name in ["prop_dia_l1_sum", "prop_dia_l1_count", "prop_dia_l2"] {
        db.refresh_st_with_retry(name).await;
    }
}

async fn apply_linear_mutation_batch(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids: &mut TrackedIds,
) -> String {
    apply_mutation_batch(db, rng, ids, "prop_lin_src", &LINEAR_GROUPS).await
}

async fn apply_diamond_mutation_batch(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids: &mut TrackedIds,
) -> String {
    apply_mutation_batch(db, rng, ids, "prop_dia_src", &DIAMOND_GROUPS).await
}

async fn apply_mutation_batch(
    db: &E2eDb,
    rng: &mut SeededRng,
    ids: &mut TrackedIds,
    table: &str,
    groups: &[&str],
) -> String {
    let steps = rng.usize_range(1, 4);
    let mut trace = Vec::with_capacity(steps);

    for _ in 0..steps {
        match rng.usize_range(0, 3) {
            0 => {
                let id = ids.alloc();
                let grp = *rng.choose(groups);
                let val = rng.i32_range(1, 60);
                db.execute(&format!(
                    "INSERT INTO {table} (id, grp, val) VALUES ({id}, '{grp}', {val})"
                ))
                .await;
                trace.push(format!("insert(id={id}, grp={grp}, val={val})"));
            }
            1 => {
                if let Some(id) = ids.pick(rng) {
                    let grp = *rng.choose(groups);
                    let val = rng.i32_range(1, 60);
                    db.execute(&format!(
                        "UPDATE {table} SET grp = '{grp}', val = {val} WHERE id = {id}"
                    ))
                    .await;
                    trace.push(format!("update(id={id}, grp={grp}, val={val})"));
                } else {
                    trace.push("update(skipped-empty)".to_string());
                }
            }
            2 => {
                if let Some(id) = ids.remove_random(rng) {
                    db.execute(&format!("DELETE FROM {table} WHERE id = {id}"))
                        .await;
                    trace.push(format!("delete(id={id})"));
                } else {
                    trace.push("delete(skipped-empty)".to_string());
                }
            }
            _ => {
                trace.push("noop".to_string());
            }
        }
    }

    trace.join("; ")
}
