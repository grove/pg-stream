//! E2E tests for auto-refresh chain propagation through multi-layer DAGs.
//!
//! Validates that the background scheduler correctly refreshes 3+ layer
//! stream table chains in topological order, detecting upstream changes
//! via `data_timestamp` comparison.
//!
//! ## Key architecture paths exercised
//!
//! - `has_stream_table_source_changes()` with 3+ topological levels
//! - Topological traversal in the scheduler tick
//! - `CALCULATED` schedule resolution for ST-on-ST
//! - No spurious cascades on no-op cycles
//!
//! ## Important
//!
//! All tests use `E2eDb::new_on_postgres_db()` because the background
//! worker only connects to the `postgres` database.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;
use std::time::Duration;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Configure the scheduler for fast testing:
/// - `pg_trickle.scheduler_interval_ms = 100` (wake every 100ms)
/// - `pg_trickle.min_schedule_seconds = 1` (allow 1-second schedules)
async fn configure_fast_scheduler(db: &E2eDb) {
    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 100")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// Wait until `last_refresh_at` advances for a given ST.
/// Returns the new `last_refresh_at` value.
async fn wait_for_refresh_cycle(db: &E2eDb, pgt_name: &str, timeout: Duration) -> String {
    let initial: String = db
        .query_scalar(&format!(
            "SELECT COALESCE(last_refresh_at::text, 'never') \
             FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '{pgt_name}'"
        ))
        .await;

    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            panic!(
                "Timed out waiting for scheduler refresh of '{pgt_name}' \
                 (initial last_refresh_at = {initial})"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        let current: String = db
            .query_scalar(&format!(
                "SELECT COALESCE(last_refresh_at::text, 'never') \
                 FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '{pgt_name}'"
            ))
            .await;

        if current != initial {
            return current;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 4.1 — 3-layer auto-refresh cascade
// ═══════════════════════════════════════════════════════════════════════════

/// base → L1 → L2 → L3, all with 1s schedule.
/// Insert into base, wait for L3 to auto-refresh, verify correctness.
#[tokio::test]
async fn test_autorefresh_3_layer_cascade() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE ar3_src (id SERIAL PRIMARY KEY, val INT NOT NULL)")
        .await;
    db.execute("INSERT INTO ar3_src VALUES (1, 10), (2, 20)")
        .await;

    // L1: passthrough aggregate
    db.create_st(
        "ar3_l1",
        "SELECT id, val FROM ar3_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // L2: arithmetic (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'ar3_l2',
            $$SELECT id, val * 2 AS doubled FROM ar3_l1$$,
            '1s',
            'DIFFERENTIAL'
        )",
    )
    .await;

    // L3: further transform (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'ar3_l3',
            $$SELECT id, doubled + 1 AS result FROM ar3_l2$$,
            '1s',
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Wait for initial scheduler stabilization
    wait_for_refresh_cycle(&db, "ar3_l3", Duration::from_secs(30)).await;

    // Mutate base
    db.execute("INSERT INTO ar3_src VALUES (3, 30)").await;

    // Wait for the deepest layer to pick up the change
    let refreshed = db
        .wait_for_auto_refresh("ar3_l3", Duration::from_secs(60))
        .await;
    assert!(refreshed, "ar3_l3 should auto-refresh after base mutation");

    // Verify correctness at all layers
    db.assert_st_matches_query("ar3_l1", "SELECT id, val FROM ar3_src")
        .await;
    db.assert_st_matches_query("ar3_l2", "SELECT id, val * 2 AS doubled FROM ar3_src")
        .await;
    db.assert_st_matches_query("ar3_l3", "SELECT id, val * 2 + 1 AS result FROM ar3_src")
        .await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 4.2 — Diamond auto-refresh cascade
// ═══════════════════════════════════════════════════════════════════════════

/// Diamond: base → L1a + L1b → L2, all with 1s schedule.
/// Insert into base, wait for L2 to auto-refresh, verify convergence.
#[tokio::test]
async fn test_autorefresh_diamond_cascade() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute(
        "CREATE TABLE ard_src (
            id  SERIAL PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO ard_src (grp, val) VALUES ('a', 10), ('b', 20)")
        .await;

    // L1a: SUM
    db.create_st(
        "ard_l1a",
        "SELECT grp, SUM(val) AS total FROM ard_src GROUP BY grp",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // L1b: COUNT
    db.create_st(
        "ard_l1b",
        "SELECT grp, COUNT(*) AS cnt FROM ard_src GROUP BY grp",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // L2: JOIN both branches
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'ard_l2',
            $$SELECT a.grp, a.total, b.cnt
              FROM ard_l1a a JOIN ard_l1b b ON a.grp = b.grp$$,
            '1s',
            'DIFFERENTIAL'
        )",
    )
    .await;

    let l2_q = "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM ard_src GROUP BY grp";

    // Wait for initial stabilization
    wait_for_refresh_cycle(&db, "ard_l2", Duration::from_secs(30)).await;

    // Mutate
    db.execute("INSERT INTO ard_src (grp, val) VALUES ('a', 5), ('c', 30)")
        .await;

    let refreshed = db
        .wait_for_auto_refresh("ard_l2", Duration::from_secs(60))
        .await;
    assert!(refreshed, "ard_l2 should auto-refresh after base mutation");

    db.assert_st_matches_query("ard_l2", l2_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 4.3 — CALCULATED schedule
// ═══════════════════════════════════════════════════════════════════════════

/// L1 (schedule 1s) → L2 (schedule NULL = CALCULATED).
/// L2 should refresh whenever L1 has pending changes.
#[tokio::test]
async fn test_autorefresh_calculated_schedule() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE arc_src (id SERIAL PRIMARY KEY, val INT NOT NULL)")
        .await;
    db.execute("INSERT INTO arc_src VALUES (1, 100)").await;

    // L1: explicit 1s schedule
    db.create_st(
        "arc_l1",
        "SELECT id, val FROM arc_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // L2: CALCULATED schedule (NULL)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'arc_l2',
            $$SELECT id, val * 10 AS scaled FROM arc_l1$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Wait for initial stabilization
    wait_for_refresh_cycle(&db, "arc_l2", Duration::from_secs(30)).await;

    // Mutate
    db.execute("INSERT INTO arc_src VALUES (2, 200)").await;

    // Wait for L2 to auto-refresh (CALCULATED schedule should trigger it)
    let refreshed = db
        .wait_for_auto_refresh("arc_l2", Duration::from_secs(60))
        .await;
    assert!(
        refreshed,
        "arc_l2 (CALCULATED) should auto-refresh when upstream L1 changes"
    );

    db.assert_st_matches_query("arc_l2", "SELECT id, val * 10 AS scaled FROM arc_src")
        .await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 4.4 — No spurious cascades (3-layer)
// ═══════════════════════════════════════════════════════════════════════════

/// No DML → all 3 data_timestamps should remain stable across 2+
/// scheduler ticks.  Extension of the 2-layer test from
/// `e2e_cascade_regression_tests.rs`.
#[tokio::test]
async fn test_autorefresh_no_spurious_3_layer() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE arns_src (id SERIAL PRIMARY KEY, val INT NOT NULL)")
        .await;
    db.execute("INSERT INTO arns_src VALUES (1, 10), (2, 20)")
        .await;

    db.create_st(
        "arns_l1",
        "SELECT id, val FROM arns_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'arns_l2',
            $$SELECT id, val * 2 AS doubled FROM arns_l1$$,
            '1s',
            'DIFFERENTIAL'
        )",
    )
    .await;

    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'arns_l3',
            $$SELECT id, doubled + 1 AS result FROM arns_l2$$,
            '1s',
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Wait for first scheduler cycle on L3 to consume any stale buffer entries
    wait_for_refresh_cycle(&db, "arns_l3", Duration::from_secs(30)).await;

    // Record data_timestamps after first cycle
    let ts_after_first: Vec<String> = {
        let mut v = Vec::new();
        for name in &["arns_l1", "arns_l2", "arns_l3"] {
            let ts: String = db
                .query_scalar(&format!(
                    "SELECT COALESCE(data_timestamp::text, 'null') \
                     FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '{name}'"
                ))
                .await;
            v.push(ts);
        }
        v
    };

    // Wait for second scheduler cycle (no DML)
    let lr_after_first: String = db
        .query_scalar(
            "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'arns_l3'",
        )
        .await;

    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(30) {
            panic!("Timed out waiting for second scheduler cycle");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let lr: String = db
            .query_scalar(
                "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables \
                 WHERE pgt_name = 'arns_l3'",
            )
            .await;
        if lr != lr_after_first {
            break;
        }
    }

    // data_timestamps must remain stable — no spurious advance
    let names = ["arns_l1", "arns_l2", "arns_l3"];
    for (i, name) in names.iter().enumerate() {
        let ts: String = db
            .query_scalar(&format!(
                "SELECT COALESCE(data_timestamp::text, 'null') \
                 FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '{name}'"
            ))
            .await;
        assert_eq!(
            ts_after_first[i], ts,
            "data_timestamp for '{name}' must not advance on no-op scheduler ticks"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 4.5 — Staggered schedules
// ═══════════════════════════════════════════════════════════════════════════

/// L1=1s, L2=3s, L3=1s.
/// After DML, L1 refreshes quickly, L2 must wait for its 3s schedule, and
/// L3 cannot advance until L2 has caught up.  Verify eventual convergence.
#[tokio::test]
async fn test_autorefresh_staggered_schedules() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE ars_src (id SERIAL PRIMARY KEY, val INT NOT NULL)")
        .await;
    db.execute("INSERT INTO ars_src VALUES (1, 10)").await;

    // L1: fast (1s)
    db.create_st(
        "ars_l1",
        "SELECT id, val FROM ars_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // L2: slower (3s)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'ars_l2',
            $$SELECT id, val * 2 AS doubled FROM ars_l1$$,
            '3s',
            'DIFFERENTIAL'
        )",
    )
    .await;

    // L3: fast again (1s) but dependent on L2
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'ars_l3',
            $$SELECT id, doubled + 1 AS result FROM ars_l2$$,
            '1s',
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Wait for initial cycle on L3
    wait_for_refresh_cycle(&db, "ars_l3", Duration::from_secs(30)).await;

    // Now insert new data
    db.execute("INSERT INTO ars_src VALUES (2, 20)").await;

    // L3 should eventually converge — give it enough time for L2's 3s schedule
    let refreshed = db
        .wait_for_auto_refresh("ars_l3", Duration::from_secs(60))
        .await;
    assert!(
        refreshed,
        "ars_l3 should eventually auto-refresh after base mutation with staggered schedules"
    );

    // Verify final correctness
    db.assert_st_matches_query("ars_l1", "SELECT id, val FROM ars_src")
        .await;
    db.assert_st_matches_query("ars_l2", "SELECT id, val * 2 AS doubled FROM ars_src")
        .await;
    db.assert_st_matches_query("ars_l3", "SELECT id, val * 2 + 1 AS result FROM ars_src")
        .await;
}
