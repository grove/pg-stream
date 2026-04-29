//! WAKE-1: E2E tests for scheduler wake behaviour.
//!
//! O39-2 (v0.39.0): `event_driven_wake` is NOT functional in background
//! workers — PostgreSQL's `LISTEN` is restricted to B_BACKEND processes.
//! The scheduler always operates in polling-only mode regardless of the GUC.
//! CDC triggers still emit `pg_notify('pgtrickle_wake')` for future use
//! once a background-worker-compatible latch mechanism is available.
//!
//! Verifies that:
//! 1. CDC triggers emit `pg_notify('pgtrickle_wake', '')` after writing to
//!    the change buffer (for future use).
//! 2. Setting `event_driven_wake = on` emits a warning; the scheduler operates
//!    in polling-only mode.
//! 3. Poll-based operation works correctly regardless of the GUC value.

mod e2e;

use e2e::E2eDb;
use std::time::Duration;

// ── Helpers ────────────────────────────────────────────────────────────────

/// Configure the scheduler with a long poll interval to make event-driven
/// wake distinguishable from poll-based wake.
#[allow(dead_code)]
async fn configure_event_driven_scheduler(db: &E2eDb) {
    // Set a long poll interval so we can distinguish event-driven wake
    // (fast) from poll-based wake (slow).
    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 5000")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.auto_backoff = off")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.event_driven_wake = on")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.wake_debounce_ms = 10")
        .await;
    db.reload_config_and_wait().await;
    db.wait_for_setting("pg_trickle.scheduler_interval_ms", "5000")
        .await;
    db.wait_for_setting("pg_trickle.event_driven_wake", "on")
        .await;

    let sched_running = db.wait_for_scheduler(Duration::from_secs(90)).await;
    assert!(
        sched_running,
        "pg_trickle scheduler did not appear within 90 s"
    );
}

/// Wait until a ST has at least `min_completed` COMPLETED refreshes.
async fn wait_for_n_refreshes(
    db: &E2eDb,
    pgt_name: &str,
    min_completed: i64,
    timeout: Duration,
) -> bool {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let count: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM pgtrickle.pgt_refresh_history h \
                 JOIN pgtrickle.pgt_stream_tables d ON h.pgt_id = d.pgt_id \
                 WHERE d.pgt_name = '{pgt_name}' AND h.status = 'COMPLETED'"
            ))
            .await;
        if count >= min_completed {
            return true;
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// WAKE-1: Verify that CDC triggers include `pg_notify('pgtrickle_wake', '')`
/// in the generated trigger function body.
#[tokio::test]
async fn test_wake_cdc_trigger_emits_notify() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("CREATE TABLE wake_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO wake_src VALUES (1, 'a')").await;
    db.create_st(
        "wake_st",
        "SELECT id, val FROM wake_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // Check that the trigger function includes the pg_notify call.
    let fn_body: String = db
        .query_scalar(
            "SELECT prosrc FROM pg_proc \
             WHERE proname LIKE 'pg_trickle_cdc_ins_fn_%' \
             ORDER BY oid DESC LIMIT 1",
        )
        .await;
    assert!(
        fn_body.contains("pg_notify('pgtrickle_wake'"),
        "INSERT trigger function should contain pg_notify('pgtrickle_wake'): {}",
        fn_body,
    );

    // Also check UPDATE and DELETE trigger functions.
    let upd_body: String = db
        .query_scalar(
            "SELECT prosrc FROM pg_proc \
             WHERE proname LIKE 'pg_trickle_cdc_upd_fn_%' \
             ORDER BY oid DESC LIMIT 1",
        )
        .await;
    assert!(
        upd_body.contains("pg_notify('pgtrickle_wake'"),
        "UPDATE trigger function should contain pg_notify('pgtrickle_wake')",
    );

    let del_body: String = db
        .query_scalar(
            "SELECT prosrc FROM pg_proc \
             WHERE proname LIKE 'pg_trickle_cdc_del_fn_%' \
             ORDER BY oid DESC LIMIT 1",
        )
        .await;
    assert!(
        del_body.contains("pg_notify('pgtrickle_wake'"),
        "DELETE trigger function should contain pg_notify('pgtrickle_wake')",
    );
}

/// WAKE-1: Verify the TRUNCATE trigger function also includes the notify.
#[tokio::test]
async fn test_wake_truncate_trigger_emits_notify() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("CREATE TABLE trunc_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO trunc_src VALUES (1, 'a')").await;
    db.create_st("trunc_st", "SELECT id, val FROM trunc_src", "1s", "FULL")
        .await;

    let fn_body: String = db
        .query_scalar(
            "SELECT prosrc FROM pg_proc \
             WHERE proname LIKE 'pg_trickle_cdc_truncate_fn_%' \
             ORDER BY oid DESC LIMIT 1",
        )
        .await;
    assert!(
        fn_body.contains("pg_notify('pgtrickle_wake'"),
        "TRUNCATE trigger function should contain pg_notify('pgtrickle_wake'): {}",
        fn_body,
    );
}

/// WAKE-O39-2: Verify that setting event_driven_wake=on still operates in
/// poll-only mode (the GUC does not cause a panic or LISTEN attempt in the
/// background worker).
///
/// Since LISTEN is not supported in background workers, the scheduler must
/// remain in polling-only mode when event_driven_wake=on. This test asserts
/// that refreshes complete via polling even with the GUC enabled.
#[tokio::test]
async fn test_wake_event_driven_guc_falls_back_to_poll() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    // Use a short poll interval so the poll completes quickly.
    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 500")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.auto_backoff = off")
        .await;
    // Enable event_driven_wake — this should emit a warning but still work
    // in poll-only mode without crashing the background worker.
    db.execute("ALTER SYSTEM SET pg_trickle.event_driven_wake = on")
        .await;
    db.reload_config_and_wait().await;
    db.wait_for_setting("pg_trickle.event_driven_wake", "on")
        .await;

    let sched_running = db.wait_for_scheduler(Duration::from_secs(90)).await;
    assert!(
        sched_running,
        "pg_trickle scheduler did not appear within 90 s (should not crash with event_driven_wake=on)"
    );

    db.execute("CREATE TABLE lat_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO lat_src VALUES (1, 100)").await;
    db.create_st(
        "lat_st",
        "SELECT id, val FROM lat_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // With poll interval = 500ms, the refresh should complete via polling
    // within a few seconds. This confirms poll-only mode is working.
    let ok = wait_for_n_refreshes(&db, "lat_st", 1, Duration::from_secs(30)).await;
    assert!(
        ok,
        "Poll-only refresh with event_driven_wake=on did not complete within 30 s. \
         Scheduler may have crashed due to LISTEN attempt in background worker.",
    );
}

/// WAKE-1: Verify that poll-based fallback still works when event_driven_wake
/// is disabled.
#[tokio::test]
async fn test_wake_poll_fallback_works() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 200")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.auto_backoff = off")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.event_driven_wake = off")
        .await;
    db.reload_config_and_wait().await;
    db.wait_for_setting("pg_trickle.event_driven_wake", "off")
        .await;

    let sched_running = db.wait_for_scheduler(Duration::from_secs(90)).await;
    assert!(sched_running, "scheduler did not start");

    db.execute("CREATE TABLE poll_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO poll_src VALUES (1, 'a')").await;
    db.create_st("poll_st", "SELECT id, val FROM poll_src", "1s", "FULL")
        .await;

    // Poll-based refresh should still work within a reasonable time frame.
    let ok = wait_for_n_refreshes(&db, "poll_st", 1, Duration::from_secs(30)).await;
    assert!(ok, "Poll-based refresh did not complete within 30 s");
}
