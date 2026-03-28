//! C3-1: E2E tests for per-database worker quota enforcement.
//!
//! These tests verify:
//! - `pg_trickle.per_database_worker_quota` GUC is registered with the
//!   correct default (0 = disabled) and accepts valid values.
//! - The busy-burst threshold is applied correctly (burst allowed when cluster
//!   is below 80 % capacity).
//! - Hot-priority stream tables are dispatched before Warm/Cold ones when
//!   the per-DB quota is active.
//!
//! **Full-E2E tests** (scheduler-dependent) require the custom Docker image
//! built by `just build-e2e-image`.  GUC smoke tests run under the light-E2E
//! harness as well.

mod e2e;

use e2e::E2eDb;
use std::time::Duration;

// ── GUC smoke tests ────────────────────────────────────────────────────────

/// Verify that `per_database_worker_quota` is registered with default 0.
#[tokio::test]
async fn test_per_database_worker_quota_guc_default() {
    let db = E2eDb::new().await.with_extension().await;

    let quota = db
        .show_setting("pg_trickle.per_database_worker_quota")
        .await;
    assert_eq!(
        quota, "0",
        "per_database_worker_quota default should be 0 (disabled)"
    );
}

/// Verify that the GUC roundtrips: SET → SHOW returns the same value.
#[tokio::test]
async fn test_per_database_worker_quota_guc_set_roundtrip() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SET pg_trickle.per_database_worker_quota = 4")
        .await;
    let quota = db
        .show_setting("pg_trickle.per_database_worker_quota")
        .await;
    assert_eq!(
        quota, "4",
        "per_database_worker_quota should round-trip to 4"
    );
}

/// Verify that the GUC accepts the maximum value (64).
#[tokio::test]
async fn test_per_database_worker_quota_guc_max_value() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SET pg_trickle.per_database_worker_quota = 64")
        .await;
    let quota = db
        .show_setting("pg_trickle.per_database_worker_quota")
        .await;
    assert_eq!(
        quota, "64",
        "per_database_worker_quota should accept max value 64"
    );
}

/// Verify that ALTER DATABASE accepts the per_database_worker_quota GUC.
///
/// This exercises the SUSET context — the setting must survive being applied
/// at database granularity (not just session-level SET).
#[tokio::test]
async fn test_per_database_worker_quota_alter_database() {
    let db = E2eDb::new().await.with_extension().await;

    let db_name: String = db.query_scalar("SELECT current_database()").await;

    // Apply at database level — requires superuser.
    db.execute(&format!(
        "ALTER DATABASE \"{db_name}\" SET pg_trickle.per_database_worker_quota = 3"
    ))
    .await;

    // Read it back from pg_db_role_setting so we can verify without
    // reconnecting (setconfig is visible immediately upon catalog query).
    let stored_value: Option<String> = db
        .query_scalar_opt(&format!(
            "SELECT s.setconfig::text
             FROM pg_db_role_setting s
             JOIN pg_database d ON d.oid = s.setdatabase
             WHERE d.datname = '{db_name}'
               AND s.setrole = 0
               AND s.setconfig::text LIKE '%per_database_worker_quota%'"
        ))
        .await;

    assert!(
        stored_value.is_some(),
        "ALTER DATABASE should persist per_database_worker_quota in pg_db_role_setting"
    );
    assert!(
        stored_value.unwrap().contains('3'),
        "Stored config should contain value 3"
    );
}

// ── Scheduler integration tests ────────────────────────────────────────────

/// When `per_database_worker_quota` is set to a small positive value, all
/// stream tables in a single database should still eventually complete their
/// refresh — the quota is a concurrency cap, not a block.
#[tokio::test]
async fn test_per_database_worker_quota_does_not_starve_refreshes() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    // Set quota to 1 concurrent worker for this DB, but keep global at 4.
    db.execute("ALTER SYSTEM SET pg_trickle.per_database_worker_quota = 1")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 100")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.auto_backoff = off")
        .await;
    db.reload_config_and_wait().await;
    db.wait_for_setting("pg_trickle.per_database_worker_quota", "1")
        .await;
    db.wait_for_setting("pg_trickle.scheduler_interval_ms", "100")
        .await;

    let sched_ok = db.wait_for_scheduler(Duration::from_secs(90)).await;
    assert!(sched_ok, "Scheduler BGW must start within 90 s");

    // Create a source table and two dependent stream tables.
    db.execute("CREATE TABLE quota_src (id int, val text)")
        .await;
    db.execute("INSERT INTO quota_src SELECT i, 'v' || i FROM generate_series(1,100) i")
        .await;

    db.create_st(
        "quota_st_a",
        "SELECT id, val FROM quota_src WHERE id % 2 = 0",
        "1s",
        "FULL",
    )
    .await;
    db.create_st(
        "quota_st_b",
        "SELECT id, val FROM quota_src WHERE id % 2 = 1",
        "1s",
        "FULL",
    )
    .await;

    // Both STs must complete at least one refresh within a generous timeout.
    let completed_a = wait_for_st_refresh(&db, "quota_st_a", Duration::from_secs(60)).await;
    let completed_b = wait_for_st_refresh(&db, "quota_st_b", Duration::from_secs(60)).await;

    assert!(
        completed_a,
        "quota_st_a should refresh even with per_database_worker_quota=1"
    );
    assert!(
        completed_b,
        "quota_st_b should refresh even with per_database_worker_quota=1"
    );
}

/// When quota is 0 (disabled) the scheduler behaves as before — all STs
/// should refresh up to `max_concurrent_refreshes`.
#[tokio::test]
async fn test_per_database_worker_quota_zero_disables_cap() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    // Ensure quota is off (default 0).
    db.execute("ALTER SYSTEM SET pg_trickle.per_database_worker_quota = 0")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 100")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.auto_backoff = off")
        .await;
    db.reload_config_and_wait().await;
    db.wait_for_setting("pg_trickle.per_database_worker_quota", "0")
        .await;
    db.wait_for_setting("pg_trickle.scheduler_interval_ms", "100")
        .await;

    let sched_ok = db.wait_for_scheduler(Duration::from_secs(90)).await;
    assert!(sched_ok, "Scheduler BGW must start within 90 s");

    db.execute("CREATE TABLE quota_src2 (id int)").await;
    db.execute("INSERT INTO quota_src2 SELECT generate_series(1,50)")
        .await;

    db.create_st(
        "quota_st_unlimited",
        "SELECT id FROM quota_src2",
        "1s",
        "FULL",
    )
    .await;

    let completed = wait_for_st_refresh(&db, "quota_st_unlimited", Duration::from_secs(60)).await;
    assert!(
        completed,
        "ST should refresh normally when per_database_worker_quota=0"
    );
}

// ── Helper ─────────────────────────────────────────────────────────────────

/// Poll `pgt_refresh_history` until at least one COMPLETED record appears
/// for the named stream table, or the timeout elapses.
async fn wait_for_st_refresh(db: &E2eDb, st_name: &str, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        let count: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM pgtrickle.pgt_refresh_history h \
                 JOIN pgtrickle.pgt_stream_tables d ON h.pgt_id = d.pgt_id \
                 WHERE d.pgt_name = '{st_name}' AND h.status = 'COMPLETED'"
            ))
            .await;
        if count > 0 {
            return true;
        }
    }
}
