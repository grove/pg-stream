//! E2E tests for Bootstrap Source Gating (v0.5.0, Phase 3).
//!
//! Validates:
//! - BOOT-2: `gate_source()` inserts into `pgt_source_gates`
//! - BOOT-3: `ungate_source()` marks gated=false; `source_gates()` returns current status
//! - BOOT-4: Scheduler logs SKIP in `pgt_refresh_history` for gated sources;
//!   manual refresh is NOT blocked by gates
//! - Edge cases: idempotent gating, re-gating after ungate, non-existent table
//!
//! Prerequisites: `just build-e2e-image` (for scheduler tests)

mod e2e;

use e2e::E2eDb;
use std::time::Duration;

// ── Catalog API tests (light E2E) ──────────────────────────────────────────

/// BOOT-2: gate_source() inserts a row into pgt_source_gates with gated=true.
#[tokio::test]
async fn test_gate_source_inserts_gate_record() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE gated_src (id INT PRIMARY KEY, val TEXT)")
        .await;

    db.execute("SELECT pgtrickle.gate_source('gated_src')")
        .await;

    let gated: bool = db
        .query_scalar(
            "SELECT g.gated \
             FROM pgtrickle.pgt_source_gates g \
             JOIN pg_class c ON c.oid = g.source_relid \
             WHERE c.relname = 'gated_src'",
        )
        .await;

    assert!(
        gated,
        "gate_source() should set gated=true in pgt_source_gates"
    );
}

/// BOOT-3: source_gates() returns rows with the correct fields after gating.
#[tokio::test]
async fn test_source_gates_returns_gated_source() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sg_src (id INT PRIMARY KEY)").await;
    db.execute("SELECT pgtrickle.gate_source('sg_src')").await;

    let gated: bool = db
        .query_scalar(
            "SELECT gated FROM pgtrickle.source_gates() \
             WHERE source_table = 'sg_src'",
        )
        .await;

    assert!(gated, "source_gates() should report the source as gated");
}

/// BOOT-3: ungate_source() sets gated=false and records ungated_at.
#[tokio::test]
async fn test_ungate_source_clears_gate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ug_src (id INT PRIMARY KEY)").await;
    db.execute("SELECT pgtrickle.gate_source('ug_src')").await;
    db.execute("SELECT pgtrickle.ungate_source('ug_src')").await;

    let gated: bool = db
        .query_scalar(
            "SELECT gated FROM pgtrickle.source_gates() \
             WHERE source_table = 'ug_src'",
        )
        .await;

    let ungated_at_not_null: bool = db
        .query_scalar(
            "SELECT ungated_at IS NOT NULL \
             FROM pgtrickle.source_gates() \
             WHERE source_table = 'ug_src'",
        )
        .await;

    assert!(!gated, "ungate_source() should set gated=false");
    assert!(ungated_at_not_null, "ungate_source() should set ungated_at");
}

/// Idempotent gating: calling gate_source() twice is safe.
#[tokio::test]
async fn test_gate_source_is_idempotent() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE idem_src (id INT PRIMARY KEY)")
        .await;
    db.execute("SELECT pgtrickle.gate_source('idem_src')").await;
    // Second call must not error.
    db.execute("SELECT pgtrickle.gate_source('idem_src')").await;

    let count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgtrickle.pgt_source_gates g \
             JOIN pg_class c ON c.oid = g.source_relid \
             WHERE c.relname = 'idem_src'",
        )
        .await;

    assert_eq!(
        count, 1,
        "idempotent gate_source() must produce exactly one row"
    );
}

/// Re-gating after ungate works correctly.
#[tokio::test]
async fn test_regate_after_ungate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rg_src (id INT PRIMARY KEY)").await;
    db.execute("SELECT pgtrickle.gate_source('rg_src')").await;
    db.execute("SELECT pgtrickle.ungate_source('rg_src')").await;
    // Re-gate: should set gated=true and clear ungated_at.
    db.execute("SELECT pgtrickle.gate_source('rg_src')").await;

    let gated: bool = db
        .query_scalar(
            "SELECT gated FROM pgtrickle.source_gates() \
             WHERE source_table = 'rg_src'",
        )
        .await;

    assert!(gated, "re-gating after ungate should set gated=true again");
}

/// gate_source() on a non-existent relation returns an error.
#[tokio::test]
async fn test_gate_source_nonexistent_table_errors() {
    let db = E2eDb::new().await.with_extension().await;

    let result = db
        .try_execute("SELECT pgtrickle.gate_source('does_not_exist_table_xyz')")
        .await;

    assert!(
        result.is_err(),
        "gate_source() on a non-existent table should return an error"
    );
}

/// source_gates() returns an empty result set when no gates have been registered.
#[tokio::test]
async fn test_source_gates_empty_by_default() {
    let db = E2eDb::new().await.with_extension().await;

    let count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.source_gates()")
        .await;

    assert_eq!(
        count, 0,
        "source_gates() should be empty when nothing is gated"
    );
}

/// Multiple sources can be gated simultaneously.
#[tokio::test]
async fn test_multiple_sources_gated() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE multi_src1 (id INT PRIMARY KEY)")
        .await;
    db.execute("CREATE TABLE multi_src2 (id INT PRIMARY KEY)")
        .await;
    db.execute("SELECT pgtrickle.gate_source('multi_src1')")
        .await;
    db.execute("SELECT pgtrickle.gate_source('multi_src2')")
        .await;

    let gated_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.source_gates() WHERE gated = true")
        .await;

    assert_eq!(gated_count, 2, "both sources should appear as gated");
}

// ── Manual refresh not blocked (BOOT-4 boundary) ──────────────────────────

/// BOOT-4 boundary: manual refresh_stream_table() is NOT blocked by gates.
///
/// Gates only suppress scheduler-initiated refreshes.  Manual refreshes
/// must always succeed so operators can unblock out-of-band.
#[tokio::test]
async fn test_manual_refresh_not_blocked_by_gate() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE man_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO man_src VALUES (1, 'a'), (2, 'b')")
        .await;

    db.create_st("man_st", "SELECT id, val FROM man_src", "5m", "FULL")
        .await;

    // Gate the source table.
    db.execute("SELECT pgtrickle.gate_source('man_src')").await;

    // Manual refresh must succeed even though the source is gated.
    db.refresh_st("man_st").await;

    let count: i64 = db.count("pgtrickle.man_st").await;
    assert_eq!(
        count, 2,
        "manual refresh must succeed even when source is gated"
    );
}

// ── Scheduler SKIP tests (full E2E — requires bgworker) ───────────────────

/// BOOT-4: When a source is gated the scheduler logs SKIP+SKIPPED in history.
///
/// Procedure:
/// 1. Configure fast scheduler.
/// 2. Create source + stream table.
/// 3. Wait for at least one successful COMPLETED refresh to confirm the
///    scheduler is working.
/// 4. Gate the source.
/// 5. Insert more rows (so the scheduler would normally fire again).
/// 6. Wait for a SKIPPED record to appear in pgt_refresh_history.
#[tokio::test]
async fn test_scheduler_logs_skip_when_source_gated() {
    let db = E2eDb::new_on_postgres_db().await;

    // Lower scheduler cadence to speed up the test.
    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 200")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.reload_config_and_wait().await;

    let sched_ok = db.wait_for_scheduler(Duration::from_secs(90)).await;
    assert!(sched_ok, "pg_trickle scheduler must be running");

    db.execute("CREATE TABLE sched_gate_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO sched_gate_src VALUES (1, 'hello')")
        .await;
    db.create_st(
        "sched_gate_st",
        "SELECT id, val FROM sched_gate_src",
        "1s",
        "FULL",
    )
    .await;

    // Wait for at least one COMPLETED scheduler refresh.
    let refreshed = db
        .wait_for_auto_refresh("sched_gate_st", Duration::from_secs(60))
        .await;
    assert!(
        refreshed,
        "scheduler should auto-refresh sched_gate_st before gate is set"
    );

    // Now gate the source.
    db.execute("SELECT pgtrickle.gate_source('sched_gate_src')")
        .await;

    // Insert a row so the scheduler has a reason to fire (change detected).
    db.execute("INSERT INTO sched_gate_src VALUES (2, 'world')")
        .await;

    // Wait up to 30 s for a SKIPPED record in pgt_refresh_history.
    let pgt_id: i64 = db
        .query_scalar(
            "SELECT pgt_id FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'sched_gate_st'",
        )
        .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut got_skip = false;
    while std::time::Instant::now() < deadline {
        let skip_count: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM pgtrickle.pgt_refresh_history \
                 WHERE pgt_id = {pgt_id} AND status = 'SKIPPED' AND action = 'SKIP'"
            ))
            .await;
        if skip_count > 0 {
            got_skip = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    assert!(
        got_skip,
        "scheduler should write a SKIPPED/SKIP record to pgt_refresh_history when source is gated"
    );

    // After ungating the source, the scheduler should resume normal refreshes.
    db.execute("SELECT pgtrickle.ungate_source('sched_gate_src')")
        .await;

    let resumed = db
        .wait_for_auto_refresh("sched_gate_st", Duration::from_secs(60))
        .await;
    assert!(
        resumed,
        "scheduler should resume refreshes after ungate_source()"
    );
}
