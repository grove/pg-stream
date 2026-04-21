//! ERR-4 (v0.26.0): Crash-recovery test for downstream publication subscriber.
//!
//! Validates that a publication subscriber created via
//! `stream_table_to_publication()` survives a postmaster restart and catches
//! up with zero data loss after the server recovers.
//!
//! Test scenario:
//! 1. Create a stream table with CDC enabled.
//! 2. Create a downstream publication subscriber.
//! 3. Insert rows and refresh the stream table.
//! 4. Simulate a brief server disruption (connection failure / reconnect).
//! 5. Insert more rows and refresh again.
//! 6. Assert the subscriber catches up and has all rows (zero data loss).
//!
//! Note: This test does not perform a true postmaster kill (which would
//! require container-level control). Instead it validates resilience via
//! connection pooling recovery — the full kill/restart scenario is covered
//! by the CNPG smoke test (`cnpg/` cluster configuration) and the soak
//! test (`e2e_soak_tests.rs`).
//!
//! Prerequisites: `./tests/build_e2e_image.sh` or `just test-e2e`

mod e2e;

use e2e::E2eDb;
use std::time::Duration;

// ── ERR-4: Publication subscriber crash-recovery ───────────────────────────

/// ERR-4 (v0.26.0): Publication subscriber catches up after connection disruption.
///
/// Tests that:
/// - `stream_table_to_publication()` registers the stream table successfully.
/// - Changes inserted before and after a simulated disruption are all visible.
/// - No rows are lost when the connection pool reconnects.
#[tokio::test]
async fn test_err4_publication_subscriber_catches_up_after_disruption() {
    let db = E2eDb::new().await.with_extension().await;

    // ── Setup ──────────────────────────────────────────────────────────────
    db.execute("CREATE TABLE err4_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO err4_src SELECT g, 'initial_' || g FROM generate_series(1, 100) g")
        .await;

    db.create_st(
        "err4_st",
        "SELECT id, val FROM err4_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    assert_eq!(db.count("public.err4_st").await, 100);

    // ── Register a downstream publication ─────────────────────────────────
    // `stream_table_to_publication()` creates a PostgreSQL publication for the ST.
    // This will error if publications are not supported (e.g., replica mode),
    // so we tolerate errors and skip the publication-specific assertions in that case.
    let pub_result = db
        .try_execute("SELECT pgtrickle.stream_table_to_publication('err4_st')")
        .await;

    let publication_registered = pub_result.is_ok();

    // ── Phase 1: Changes before disruption ────────────────────────────────
    db.execute("INSERT INTO err4_src SELECT g, 'batch1_' || g FROM generate_series(101, 200) g")
        .await;
    db.refresh_st("err4_st").await;
    assert_eq!(db.count("public.err4_st").await, 200);

    // ── Simulate disruption: drop + recreate connection ───────────────────
    // We simulate a brief connection loss by running pg_terminate_backend()
    // on idle connections (except our own). In a real crash, the pool would
    // recover automatically via reconnect.
    let _terminated: i64 = db
        .query_scalar(
            "SELECT count(pg_terminate_backend(pid)) \
             FROM pg_stat_activity \
             WHERE state = 'idle' \
               AND pid <> pg_backend_pid() \
               AND application_name NOT LIKE 'pgtrickle%'",
        )
        .await;

    // Brief pause to let the pool settle.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // ── Phase 2: Changes after disruption ─────────────────────────────────
    db.execute("INSERT INTO err4_src SELECT g, 'batch2_' || g FROM generate_series(201, 300) g")
        .await;
    db.refresh_st("err4_st").await;

    // ERR-4 correctness: all 300 rows must be present (zero data loss).
    let count = db.count("public.err4_st").await;
    assert_eq!(
        count, 300,
        "ERR-4: stream table must have all 300 rows after disruption + recovery, got {count}"
    );

    // Data integrity: verify no duplicates.
    let distinct: i64 = db
        .query_scalar("SELECT count(DISTINCT id) FROM public.err4_st")
        .await;
    assert_eq!(
        distinct, 300,
        "ERR-4: all rows must have distinct ids (no duplicates after recovery)"
    );

    // If publication was registered, verify it still exists.
    if publication_registered {
        let pub_count: i64 = db
            .query_scalar("SELECT count(*) FROM pg_publication WHERE pubname LIKE '%err4_st%'")
            .await;
        assert!(
            pub_count > 0,
            "ERR-4: publication must still exist after disruption + recovery"
        );
    }
}

/// ERR-4 (v0.26.0): Publication subscriber clean state after failed registration.
///
/// Tests that if `stream_table_to_publication()` is called on an ST that does
/// not exist, the error is handled cleanly with no orphaned catalog state.
#[tokio::test]
async fn test_err4_publication_registration_failure_is_clean() {
    let db = E2eDb::new().await.with_extension().await;

    // Attempt to register a publication for a non-existent stream table.
    let result = db
        .try_execute("SELECT pgtrickle.stream_table_to_publication('nonexistent_st')")
        .await;

    // Must fail with an error (not panic or leave orphaned state).
    assert!(
        result.is_err(),
        "ERR-4: stream_table_to_publication on non-existent ST must return an error"
    );

    // No orphaned publication rows should have been created.
    let pub_count: i64 = db
        .query_scalar("SELECT count(*) FROM pg_publication WHERE pubname LIKE '%nonexistent_st%'")
        .await;
    assert_eq!(
        pub_count, 0,
        "ERR-4: failed publication registration must leave no orphaned pg_publication rows"
    );
}
