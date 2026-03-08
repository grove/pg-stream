//! E2E tests for WAL-based CDC (Change Data Capture) via logical replication.
//!
//! Validates:
//! - W1: Full WAL CDC lifecycle (trigger → transitioning → WAL)
//! - W1: INSERT, UPDATE, DELETE correctness through WAL CDC
//! - W1: Transition timeout and fallback to triggers
//! - W2: Automatic fallback on persistent poll errors (slot dropped)
//! - W2: Health check detects missing prerequisites
//! - W3: `auto` is the default cdc_mode (no explicit config needed)
//!
//! Prerequisites:
//! - `./tests/build_e2e_image.sh` (Docker image with wal_level=logical)
//! - Docker with `wal_level = logical` and `max_replication_slots = 10`

mod e2e;

use e2e::E2eDb;
use std::time::Duration;

/// Helper: query the CDC mode for a source table's dependency.
async fn get_cdc_mode(db: &E2eDb, source_table: &str) -> String {
    let oid = db.table_oid(source_table).await;
    db.query_scalar(&format!(
        "SELECT d.cdc_mode FROM pgtrickle.pgt_dependencies d \
         WHERE d.source_relid = {oid} LIMIT 1"
    ))
    .await
}

/// Helper: check if a replication slot exists for a source table.
async fn slot_exists(db: &E2eDb, source_table: &str) -> bool {
    let oid = db.table_oid(source_table).await;
    let slot_name = format!("pgtrickle_{}", oid);
    db.query_scalar::<bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = '{slot_name}')"
    ))
    .await
}

/// Helper: check if a publication exists for a source table.
async fn publication_exists(db: &E2eDb, source_table: &str) -> bool {
    let oid = db.table_oid(source_table).await;
    let pub_name = format!("pgtrickle_cdc_{}", oid);
    db.query_scalar::<bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM pg_publication WHERE pubname = '{pub_name}')"
    ))
    .await
}

/// Helper: wait until the CDC mode for a source transitions to the given value,
/// or timeout. Returns the final CDC mode.
async fn wait_for_cdc_mode(
    db: &E2eDb,
    source_table: &str,
    target: &str,
    timeout: Duration,
) -> String {
    let start = std::time::Instant::now();
    loop {
        let mode = get_cdc_mode(db, source_table).await;
        if mode == target {
            return mode;
        }
        if start.elapsed() > timeout {
            return mode;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ── W3: Auto is the default CDC mode ──────────────────────────────────

#[tokio::test]
async fn test_wal_auto_is_default_cdc_mode() {
    let db = E2eDb::new().await.with_extension().await;

    let cdc_mode = db.show_setting("pg_trickle.cdc_mode").await;
    assert_eq!(cdc_mode, "auto", "Default cdc_mode should be 'auto'");
}

#[tokio::test]
async fn test_wal_level_is_logical() {
    let db = E2eDb::new().await.with_extension().await;

    let wal_level: String = db.query_scalar("SHOW wal_level").await;
    assert_eq!(
        wal_level, "logical",
        "E2E container should have wal_level = logical"
    );
}

// ── W1: WAL Transition Lifecycle ──────────────────────────────────────

/// Test the full TRIGGER → TRANSITIONING → WAL lifecycle.
///
/// With `cdc_mode = 'auto'` (default) and `wal_level = logical`, the
/// scheduler should automatically start the transition and complete it
/// once the WAL decoder catches up.
#[tokio::test]
async fn test_wal_transition_lifecycle() {
    // Use postgres db so the scheduler bgworker picks up the STs
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("CREATE TABLE wal_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("ALTER TABLE wal_src REPLICA IDENTITY FULL")
        .await;
    db.execute("INSERT INTO wal_src VALUES (1, 'initial')")
        .await;

    db.create_st(
        "wal_lifecycle_st",
        "SELECT id, val FROM wal_src",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // Initial state should be TRIGGER (transition hasn't started yet)
    let initial_mode = get_cdc_mode(&db, "wal_src").await;
    assert_eq!(initial_mode, "TRIGGER", "Should start in TRIGGER mode");

    // Wait for the scheduler to transition to WAL
    // The scheduler runs every 1s and the transition needs to:
    // 1. Start transition (create slot + publication)
    // 2. Poll WAL to catch up
    // 3. Complete transition (verify lag < 64KB)
    let final_mode = wait_for_cdc_mode(&db, "wal_src", "WAL", Duration::from_secs(30)).await;
    assert_eq!(
        final_mode, "WAL",
        "Should transition to WAL mode (got: {final_mode})"
    );

    // Verify infrastructure was created
    assert!(
        slot_exists(&db, "wal_src").await,
        "Replication slot should exist"
    );
    assert!(
        publication_exists(&db, "wal_src").await,
        "Publication should exist"
    );
}

/// Test that INSERTs are captured correctly through WAL-based CDC.
#[tokio::test]
async fn test_wal_cdc_captures_insert() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("CREATE TABLE wal_ins (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("ALTER TABLE wal_ins REPLICA IDENTITY FULL")
        .await;
    db.execute("INSERT INTO wal_ins VALUES (1, 'a')").await;

    db.create_st(
        "wal_ins_st",
        "SELECT id, val FROM wal_ins",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.wal_ins_st").await, 1);

    // Wait for WAL transition
    let mode = wait_for_cdc_mode(&db, "wal_ins", "WAL", Duration::from_secs(30)).await;
    assert_eq!(mode, "WAL", "Should transition to WAL mode");

    // Insert new rows — WAL decoder should capture them
    db.execute("INSERT INTO wal_ins VALUES (2, 'b'), (3, 'c')")
        .await;

    // Wait for the scheduler to do a refresh
    let refreshed = db
        .wait_for_auto_refresh("wal_ins_st", Duration::from_secs(15))
        .await;
    assert!(refreshed, "Scheduler should trigger a refresh");

    assert_eq!(
        db.count("public.wal_ins_st").await,
        3,
        "WAL CDC should capture all INSERTs"
    );
}

/// Test that UPDATEs are captured correctly through WAL-based CDC.
#[tokio::test]
async fn test_wal_cdc_captures_update() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("CREATE TABLE wal_upd (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("ALTER TABLE wal_upd REPLICA IDENTITY FULL")
        .await;
    db.execute("INSERT INTO wal_upd VALUES (1, 'old')").await;

    db.create_st(
        "wal_upd_st",
        "SELECT id, val FROM wal_upd",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    let mode = wait_for_cdc_mode(&db, "wal_upd", "WAL", Duration::from_secs(30)).await;
    assert_eq!(mode, "WAL", "Should transition to WAL mode");

    db.execute("UPDATE wal_upd SET val = 'new' WHERE id = 1")
        .await;

    let refreshed = db
        .wait_for_auto_refresh("wal_upd_st", Duration::from_secs(15))
        .await;
    assert!(refreshed, "Scheduler should trigger a refresh");

    let val: String = db
        .query_scalar("SELECT val FROM public.wal_upd_st WHERE id = 1")
        .await;
    assert_eq!(val, "new", "UPDATE should be reflected via WAL CDC");
}

/// Test that DELETEs are captured correctly through WAL-based CDC.
#[tokio::test]
async fn test_wal_cdc_captures_delete() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("CREATE TABLE wal_del (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("ALTER TABLE wal_del REPLICA IDENTITY FULL")
        .await;
    db.execute("INSERT INTO wal_del VALUES (1, 'keep'), (2, 'remove')")
        .await;

    db.create_st(
        "wal_del_st",
        "SELECT id, val FROM wal_del",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.wal_del_st").await, 2);

    let mode = wait_for_cdc_mode(&db, "wal_del", "WAL", Duration::from_secs(30)).await;
    assert_eq!(mode, "WAL", "Should transition to WAL mode");

    db.execute("DELETE FROM wal_del WHERE id = 2").await;

    let refreshed = db
        .wait_for_auto_refresh("wal_del_st", Duration::from_secs(15))
        .await;
    assert!(refreshed, "Scheduler should trigger a refresh");

    assert_eq!(
        db.count("public.wal_del_st").await,
        1,
        "DELETE should be reflected via WAL CDC"
    );
}

// ── W1: Transition with trigger-only fallback ─────────────────────────

/// When cdc_mode = 'trigger', no WAL transition should occur even if
/// wal_level = logical.
#[tokio::test]
async fn test_trigger_mode_no_wal_transition() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    let default_cdc_mode = db.show_setting("pg_trickle.cdc_mode").await;

    // Force trigger-only mode
    db.alter_system_set_and_wait("pg_trickle.cdc_mode", "'trigger'", "trigger")
        .await;

    db.execute("CREATE TABLE trig_only (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO trig_only VALUES (1, 'a')").await;

    db.create_st(
        "trig_only_st",
        "SELECT id, val FROM trig_only",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // Wait a few seconds — should stay in TRIGGER mode
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mode = get_cdc_mode(&db, "trig_only").await;
    assert_eq!(
        mode, "TRIGGER",
        "cdc_mode='trigger' should prevent WAL transition"
    );

    // No replication slot should exist
    assert!(
        !slot_exists(&db, "trig_only").await,
        "No slot should be created in trigger-only mode"
    );

    // Reset for other tests
    db.alter_system_reset_and_wait("pg_trickle.cdc_mode", &default_cdc_mode)
        .await;
}

// ── W2: Fallback hardening ────────────────────────────────────────────

/// When a replication slot is externally dropped while in WAL mode,
/// the health check should detect it and fall back to triggers.
#[tokio::test]
async fn test_wal_fallback_on_missing_slot() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("CREATE TABLE wal_fb (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("ALTER TABLE wal_fb REPLICA IDENTITY FULL").await;
    db.execute("INSERT INTO wal_fb VALUES (1, 'x')").await;

    db.create_st(
        "wal_fb_st",
        "SELECT id, val FROM wal_fb",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // Wait for WAL transition to complete
    let mode = wait_for_cdc_mode(&db, "wal_fb", "WAL", Duration::from_secs(30)).await;
    assert_eq!(mode, "WAL", "Should be in WAL mode before test");

    // Externally drop the replication slot to simulate infrastructure failure
    let oid = db.table_oid("wal_fb").await;
    let slot_name = format!("pgtrickle_{}", oid);
    db.execute(&format!("SELECT pg_drop_replication_slot('{slot_name}')"))
        .await;

    // Wait for the health check / poll error to trigger fallback
    let fallback_mode = wait_for_cdc_mode(&db, "wal_fb", "TRIGGER", Duration::from_secs(30)).await;
    assert_eq!(
        fallback_mode, "TRIGGER",
        "Should fall back to TRIGGER after slot is dropped"
    );

    // Verify the stream table still works — insert data and refresh
    db.execute("INSERT INTO wal_fb VALUES (2, 'y')").await;

    let refreshed = db
        .wait_for_auto_refresh("wal_fb_st", Duration::from_secs(15))
        .await;
    assert!(refreshed, "Trigger-based CDC should resume after fallback");
    assert_eq!(db.count("public.wal_fb_st").await, 2);
}

/// Cleanup on DROP: dropping a stream table in WAL mode should clean up
/// the replication slot and publication.
#[tokio::test]
async fn test_wal_cleanup_on_drop() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    db.execute("CREATE TABLE wal_drop (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("ALTER TABLE wal_drop REPLICA IDENTITY FULL")
        .await;
    db.execute("INSERT INTO wal_drop VALUES (1, 'a')").await;

    db.create_st(
        "wal_drop_st",
        "SELECT id, val FROM wal_drop",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    wait_for_cdc_mode(&db, "wal_drop", "WAL", Duration::from_secs(30)).await;

    let oid = db.table_oid("wal_drop").await;
    let slot_name = format!("pgtrickle_{}", oid);
    let pub_name = format!("pgtrickle_cdc_{}", oid);

    // Verify slot + publication exist before drop
    assert!(slot_exists(&db, "wal_drop").await);
    assert!(publication_exists(&db, "wal_drop").await);

    // Drop the stream table
    db.drop_st("wal_drop_st").await;

    // Verify slot and publication were cleaned up
    let slot_gone: bool = db
        .query_scalar(&format!(
            "SELECT NOT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = '{slot_name}')"
        ))
        .await;
    let pub_gone: bool = db
        .query_scalar(&format!(
            "SELECT NOT EXISTS(SELECT 1 FROM pg_publication WHERE pubname = '{pub_name}')"
        ))
        .await;
    assert!(slot_gone, "Replication slot should be dropped on ST drop");
    assert!(pub_gone, "Publication should be dropped on ST drop");
}

/// Keyless tables should stay on triggers (WAL mode requires PK for pk_hash).
#[tokio::test]
async fn test_wal_keyless_table_stays_on_triggers() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;

    // Table without primary key — need REPLICA IDENTITY FULL for cdc_mode='auto'
    db.execute("CREATE TABLE wal_keyless (val TEXT)").await;
    db.execute("ALTER TABLE wal_keyless REPLICA IDENTITY FULL")
        .await;
    db.execute("INSERT INTO wal_keyless VALUES ('a'), ('b')")
        .await;

    db.create_st(
        "wal_keyless_st",
        "SELECT val FROM wal_keyless",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // Wait a few seconds — should stay on TRIGGER because no PK
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mode = get_cdc_mode(&db, "wal_keyless").await;
    assert_eq!(
        mode, "TRIGGER",
        "Keyless table should stay on TRIGGER mode (WAL requires PK)"
    );
}
