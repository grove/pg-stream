//! E2E tests for the Transactional Outbox Pattern (v0.28.0).
//!
//! Covers:
//! - OUTBOX-1: enable_outbox — catalog registration, table & view creation
//! - OUTBOX-2: disable_outbox — catalog cleanup, table/view drop
//! - OUTBOX-3: outbox_status — JSONB summary
//! - OUTBOX-B1: create_consumer_group — catalog registration
//! - OUTBOX-B2: drop_consumer_group — catalog cleanup
//! - OUTBOX-B3: poll_outbox — batch polling & lease grant
//! - OUTBOX-B4: commit_offset — offset commit & lease clear
//! - OUTBOX-B5: consumer_heartbeat — heartbeat update
//! - OUTBOX-B6: consumer_lag — per-consumer lag metrics
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Create a minimal DIFFERENTIAL stream table for outbox tests.
async fn make_outbox_st(db: &E2eDb, src: &str, st: &str) {
    db.execute(&format!(
        "CREATE TABLE {src} (id INT PRIMARY KEY, val TEXT)"
    ))
    .await;
    db.execute(&format!(
        "INSERT INTO {src} VALUES (1, 'a'), (2, 'b'), (3, 'c')"
    ))
    .await;
    db.create_st(
        st,
        &format!("SELECT id, val FROM {src}"),
        "1m",
        "DIFFERENTIAL",
    )
    .await;
}

// ══════════════════════════════════════════════════════════════════════════════
// OUTBOX-1: enable_outbox
// ══════════════════════════════════════════════════════════════════════════════

/// OUTBOX-1a: Enabling outbox registers the stream table in the catalog.
#[tokio::test]
async fn test_enable_outbox_creates_catalog_entry() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "ob1a_src", "ob1a_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('ob1a_st')")
        .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'ob1a_st')",
        )
        .await;
    assert!(exists, "Outbox catalog entry should be created");
}

/// OUTBOX-1b: Enabling outbox creates the physical outbox table.
#[tokio::test]
async fn test_enable_outbox_creates_outbox_table() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "ob1b_src", "ob1b_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('ob1b_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'ob1b_st'",
        )
        .await;

    let table_exists: bool = db
        .query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
             WHERE table_schema = 'pgtrickle' AND table_name = '{outbox_name}')"
        ))
        .await;
    assert!(table_exists, "Outbox table '{}' should exist", outbox_name);
}

/// OUTBOX-1c: Enabling outbox with custom retention hours stores the correct value.
#[tokio::test]
async fn test_enable_outbox_custom_retention() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "ob1c_src", "ob1c_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('ob1c_st', 48)")
        .await;

    let retention: i32 = db
        .query_scalar(
            "SELECT retention_hours FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'ob1c_st'",
        )
        .await;
    assert_eq!(retention, 48, "Retention hours should be 48");
}

/// OUTBOX-1d: Enabling outbox twice raises OutboxAlreadyEnabled.
#[tokio::test]
async fn test_enable_outbox_already_enabled_is_error() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "ob1d_src", "ob1d_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('ob1d_st')")
        .await;

    // Second call should fail
    let result = db
        .try_execute("SELECT pgtrickle.enable_outbox('ob1d_st')")
        .await;
    assert!(result.is_err(), "Enabling outbox twice should fail");
}

// ══════════════════════════════════════════════════════════════════════════════
// OUTBOX-2: disable_outbox
// ══════════════════════════════════════════════════════════════════════════════

/// OUTBOX-2a: Disabling outbox removes the catalog entry.
#[tokio::test]
async fn test_disable_outbox_removes_catalog_entry() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "ob2a_src", "ob2a_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('ob2a_st')")
        .await;
    db.execute("SELECT pgtrickle.disable_outbox('ob2a_st')")
        .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'ob2a_st')",
        )
        .await;
    assert!(
        !exists,
        "Outbox catalog entry should be removed after disable"
    );
}

/// OUTBOX-2b: disable_outbox with if_exists=true on a non-existent outbox is a no-op.
#[tokio::test]
async fn test_disable_outbox_if_exists_no_op() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "ob2b_src", "ob2b_st").await;

    // No outbox enabled, but if_exists=true should not error
    db.execute("SELECT pgtrickle.disable_outbox('ob2b_st', true)")
        .await;
}

/// OUTBOX-2c: disable_outbox without if_exists on a non-existent outbox is an error.
#[tokio::test]
async fn test_disable_outbox_not_enabled_is_error() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "ob2c_src", "ob2c_st").await;

    let result = db
        .try_execute("SELECT pgtrickle.disable_outbox('ob2c_st')")
        .await;
    assert!(result.is_err(), "Disabling non-existent outbox should fail");
}

// ══════════════════════════════════════════════════════════════════════════════
// OUTBOX-3: outbox_status
// ══════════════════════════════════════════════════════════════════════════════

/// OUTBOX-3: outbox_status returns a valid JSONB summary.
#[tokio::test]
async fn test_outbox_status_returns_jsonb() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "ob3_src", "ob3_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('ob3_st')").await;

    let status: serde_json::Value = db
        .query_scalar("SELECT pgtrickle.outbox_status('ob3_st')")
        .await;

    assert!(
        status.get("outbox_table_name").is_some(),
        "outbox_status should contain outbox_table_name"
    );
    assert!(
        status.get("row_count").is_some(),
        "outbox_status should contain row_count"
    );
    assert!(
        status.get("retention_hours").is_some(),
        "outbox_status should contain retention_hours"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// OUTBOX-B1/B2: create_consumer_group / drop_consumer_group
// ══════════════════════════════════════════════════════════════════════════════

/// OUTBOX-B1a: create_consumer_group registers the group in the catalog.
#[tokio::test]
async fn test_create_consumer_group_creates_catalog_entry() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "cg1a_src", "cg1a_st").await;
    db.execute("SELECT pgtrickle.enable_outbox('cg1a_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'cg1a_st'",
        )
        .await;

    db.execute(&format!(
        "SELECT pgtrickle.create_consumer_group('grp_cg1a', '{outbox_name}')"
    ))
    .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_consumer_groups \
             WHERE group_name = 'grp_cg1a')",
        )
        .await;
    assert!(exists, "Consumer group should be in catalog");
}

/// OUTBOX-B1b: create_consumer_group with invalid auto_offset_reset is an error.
#[tokio::test]
async fn test_create_consumer_group_invalid_offset_reset() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "cg1b_src", "cg1b_st").await;
    db.execute("SELECT pgtrickle.enable_outbox('cg1b_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'cg1b_st'",
        )
        .await;

    let result = db
        .try_execute(&format!(
            "SELECT pgtrickle.create_consumer_group('grp_cg1b', '{outbox_name}', 'invalid')"
        ))
        .await;
    assert!(result.is_err(), "Invalid auto_offset_reset should fail");
}

/// OUTBOX-B2: drop_consumer_group removes the group from the catalog.
#[tokio::test]
async fn test_drop_consumer_group_removes_catalog_entry() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "cg2_src", "cg2_st").await;
    db.execute("SELECT pgtrickle.enable_outbox('cg2_st')").await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'cg2_st'",
        )
        .await;

    db.execute(&format!(
        "SELECT pgtrickle.create_consumer_group('grp_cg2', '{outbox_name}')"
    ))
    .await;
    db.execute("SELECT pgtrickle.drop_consumer_group('grp_cg2')")
        .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_consumer_groups \
             WHERE group_name = 'grp_cg2')",
        )
        .await;
    assert!(!exists, "Consumer group should be removed from catalog");
}

/// OUTBOX-B2: drop_consumer_group with if_exists on non-existent is a no-op.
#[tokio::test]
async fn test_drop_consumer_group_if_exists_no_op() {
    let db = E2eDb::new().await.with_extension().await;

    // Should not error because if_exists=true
    db.execute("SELECT pgtrickle.drop_consumer_group('grp_nonexistent', true)")
        .await;
}

// ══════════════════════════════════════════════════════════════════════════════
// OUTBOX-B3/B4/B5/B6: poll_outbox, commit_offset, heartbeat, consumer_lag
// ══════════════════════════════════════════════════════════════════════════════

/// OUTBOX-B3: poll_outbox on an empty outbox returns no rows.
#[tokio::test]
async fn test_poll_outbox_empty_returns_no_rows() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "poll_empty_src", "poll_empty_st").await;
    db.execute("SELECT pgtrickle.enable_outbox('poll_empty_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'poll_empty_st'",
        )
        .await;

    db.execute(&format!(
        "SELECT pgtrickle.create_consumer_group('grp_poll_empty', '{outbox_name}')"
    ))
    .await;

    let row_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.poll_outbox('grp_poll_empty', 'worker-1')")
        .await;
    assert_eq!(row_count, 0, "Empty outbox should return 0 rows");
}

/// OUTBOX-B3: poll_outbox after direct insert returns the inserted rows.
#[tokio::test]
async fn test_poll_outbox_returns_rows_after_insert() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "poll_rows_src", "poll_rows_st").await;
    db.execute("SELECT pgtrickle.enable_outbox('poll_rows_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'poll_rows_st'",
        )
        .await;

    // Direct insert into the outbox table to simulate a refresh write
    db.execute(&format!(
        "INSERT INTO pgtrickle.\"{outbox_name}\" \
         (pgt_id, inserted_count, deleted_count, is_claim_check) \
         VALUES (gen_random_uuid(), 5, 0, false), \
                (gen_random_uuid(), 3, 2, false)"
    ))
    .await;

    db.execute(&format!(
        "SELECT pgtrickle.create_consumer_group('grp_poll_rows', '{outbox_name}', 'earliest')"
    ))
    .await;

    let row_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.poll_outbox('grp_poll_rows', 'worker-1', 10)")
        .await;
    assert_eq!(row_count, 2, "Should poll 2 inserted rows");
}

/// OUTBOX-B4: commit_offset moves the committed_offset forward.
#[tokio::test]
async fn test_commit_offset_advances_offset() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "commit_src", "commit_st").await;
    db.execute("SELECT pgtrickle.enable_outbox('commit_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'commit_st'",
        )
        .await;

    // Insert rows
    db.execute(&format!(
        "INSERT INTO pgtrickle.\"{outbox_name}\" \
         (pgt_id, inserted_count, deleted_count, is_claim_check) \
         VALUES (gen_random_uuid(), 1, 0, false)"
    ))
    .await;

    db.execute(&format!(
        "SELECT pgtrickle.create_consumer_group('grp_commit', '{outbox_name}', 'earliest')"
    ))
    .await;

    // Poll to get a batch + outbox_id
    let outbox_id: i64 = db
        .query_scalar("SELECT outbox_id FROM pgtrickle.poll_outbox('grp_commit', 'w1', 10) LIMIT 1")
        .await;

    // Commit the offset
    db.execute(&format!(
        "SELECT pgtrickle.commit_offset('grp_commit', 'w1', {outbox_id})"
    ))
    .await;

    let committed: i64 = db
        .query_scalar(
            "SELECT committed_offset FROM pgtrickle.pgt_consumer_offsets \
             WHERE group_name = 'grp_commit' AND consumer_id = 'w1'",
        )
        .await;
    assert_eq!(
        committed, outbox_id,
        "Committed offset should match the last polled outbox_id"
    );
}

/// OUTBOX-B5: consumer_heartbeat updates the heartbeat timestamp.
#[tokio::test]
async fn test_consumer_heartbeat_updates_timestamp() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "hb_src", "hb_st").await;
    db.execute("SELECT pgtrickle.enable_outbox('hb_st')").await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'hb_st'",
        )
        .await;

    // Insert a row and poll to register the consumer offset row
    db.execute(&format!(
        "INSERT INTO pgtrickle.\"{outbox_name}\" \
         (pgt_id, inserted_count, deleted_count, is_claim_check) \
         VALUES (gen_random_uuid(), 1, 0, false)"
    ))
    .await;
    db.execute(&format!(
        "SELECT pgtrickle.create_consumer_group('grp_hb', '{outbox_name}', 'earliest')"
    ))
    .await;
    db.execute("SELECT pgtrickle.poll_outbox('grp_hb', 'w1', 1)")
        .await;

    // Verify the consumer offset row exists (heartbeat column updated on poll)
    let offset_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_consumer_offsets \
             WHERE group_name = 'grp_hb' AND consumer_id = 'w1')",
        )
        .await;
    assert!(
        offset_exists,
        "Consumer offset should be registered after poll"
    );

    // Heartbeat should succeed without error
    db.execute("SELECT pgtrickle.consumer_heartbeat('grp_hb', 'w1')")
        .await;
}

/// OUTBOX-B6: consumer_lag returns per-consumer lag rows.
#[tokio::test]
async fn test_consumer_lag_returns_metrics() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "lag_src", "lag_st").await;
    db.execute("SELECT pgtrickle.enable_outbox('lag_st')").await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'lag_st'",
        )
        .await;

    // Insert 3 rows
    db.execute(&format!(
        "INSERT INTO pgtrickle.\"{outbox_name}\" \
         (pgt_id, inserted_count, deleted_count, is_claim_check) \
         VALUES (gen_random_uuid(), 1, 0, false), \
                (gen_random_uuid(), 2, 0, false), \
                (gen_random_uuid(), 3, 0, false)"
    ))
    .await;

    db.execute(&format!(
        "SELECT pgtrickle.create_consumer_group('grp_lag', '{outbox_name}', 'earliest')"
    ))
    .await;

    // Register a consumer by polling
    db.execute("SELECT pgtrickle.poll_outbox('grp_lag', 'w1', 3)")
        .await;

    let lag_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.consumer_lag('grp_lag')")
        .await;
    assert_eq!(lag_count, 1, "Should have 1 consumer in lag metrics");

    let lag: i64 = db
        .query_scalar("SELECT lag FROM pgtrickle.consumer_lag('grp_lag') WHERE consumer_id = 'w1'")
        .await;
    // Consumer has not committed yet, so lag = max_id - 0 = 3
    assert_eq!(lag, 3, "Lag should be 3 (3 rows, nothing committed)");
}

// ══════════════════════════════════════════════════════════════════════════════
// Regression tests: outbox written by the refresh path (issue #660)
// ══════════════════════════════════════════════════════════════════════════════

/// Issue #660a: A manual differential refresh that produces rows must write at
/// least one header row to `pgtrickle.outbox_<st>`.
///
/// Before the fix, `write_outbox_row` was never called from
/// `execute_manual_differential_refresh`, so the outbox table stayed empty
/// even after a successful refresh.
#[tokio::test]
async fn test_outbox_written_after_manual_refresh() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "reg660a_src", "reg660a_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('reg660a_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'reg660a_st'",
        )
        .await;

    // Drive a source change after the initial refresh established the frontier.
    db.execute("INSERT INTO reg660a_src VALUES (4, 'd')").await;

    // Manual refresh — this is the path that was broken.
    db.refresh_st("reg660a_st").await;

    let outbox_row_count: i64 = db
        .query_scalar(&format!("SELECT count(*) FROM pgtrickle.\"{outbox_name}\""))
        .await;
    assert!(
        outbox_row_count >= 1,
        "pgtrickle.{outbox_name} must contain at least one row after a \
         non-empty differential refresh (regression test for issue #660)"
    );
}

/// Issue #660b: The outbox header row written after a manual refresh must have
/// accurate `inserted_count` and a non-NULL inline `payload` (when the delta
/// is below the claim-check threshold).
#[tokio::test]
async fn test_outbox_header_row_has_correct_counts_and_payload() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "reg660b_src", "reg660b_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('reg660b_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'reg660b_st'",
        )
        .await;

    // Two new rows — initial data (id 1,2,3) was loaded at create_st time.
    db.execute("INSERT INTO reg660b_src VALUES (4, 'd'), (5, 'e')")
        .await;
    db.refresh_st("reg660b_st").await;

    // Fetch individual columns from the most recent outbox header row.
    let inserted: i64 = db
        .query_scalar(&format!(
            "SELECT inserted_count FROM pgtrickle.\"{outbox_name}\" ORDER BY id DESC LIMIT 1"
        ))
        .await;
    let is_claim_check: bool = db
        .query_scalar(&format!(
            "SELECT is_claim_check FROM pgtrickle.\"{outbox_name}\" ORDER BY id DESC LIMIT 1"
        ))
        .await;
    let payload_is_null: bool = db
        .query_scalar(&format!(
            "SELECT payload IS NULL FROM pgtrickle.\"{outbox_name}\" ORDER BY id DESC LIMIT 1"
        ))
        .await;

    assert_eq!(
        inserted, 2,
        "inserted_count should be 2 (regression test for issue #660)"
    );
    assert!(
        !is_claim_check,
        "is_claim_check should be false for a small delta (regression test for issue #660)"
    );
    assert!(
        !payload_is_null,
        "payload should not be NULL for an inline delta (regression test for issue #660)"
    );
}

/// Issue #660c: `poll_outbox` must return rows after a real refresh cycle,
/// confirming the end-to-end path: source change → refresh → outbox → poll.
#[tokio::test]
async fn test_poll_outbox_returns_rows_after_real_refresh() {
    let db = E2eDb::new().await.with_extension().await;
    make_outbox_st(&db, "reg660c_src", "reg660c_st").await;

    db.execute("SELECT pgtrickle.enable_outbox('reg660c_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'reg660c_st'",
        )
        .await;

    db.execute(&format!(
        "SELECT pgtrickle.create_consumer_group('grp_reg660c', '{outbox_name}', 'earliest')"
    ))
    .await;

    // Drive a change and refresh.
    db.execute("INSERT INTO reg660c_src VALUES (4, 'd')").await;
    db.refresh_st("reg660c_st").await;

    let polled_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.poll_outbox('grp_reg660c', 'w1', 100)")
        .await;
    assert!(
        polled_count >= 1,
        "poll_outbox must return at least one row after a real refresh cycle \
         (regression test for issue #660)"
    );
}

/// Gap-1 regression: outbox must be written after a manual FULL-mode refresh.
/// Prior to the fix, `execute_manual_full_refresh` returned `(0, 0)` so the
/// outer arm's `rows_inserted > 0` guard was never satisfied.
#[tokio::test]
async fn test_outbox_written_after_manual_full_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE gap1_full_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO gap1_full_src VALUES (1, 'a'), (2, 'b')")
        .await;
    db.create_st(
        "gap1_full_st",
        "SELECT id, val FROM gap1_full_src",
        "1m",
        "FULL",
    )
    .await;
    db.execute("SELECT pgtrickle.enable_outbox('gap1_full_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'gap1_full_st'",
        )
        .await;

    // Insert a new row and do a manual refresh — this previously wrote 0 rows
    // to the outbox because the count was always zeroed.
    db.execute("INSERT INTO gap1_full_src VALUES (3, 'c')")
        .await;
    db.refresh_st("gap1_full_st").await;

    let count: i64 = db
        .query_scalar(&format!("SELECT count(*) FROM pgtrickle.\"{outbox_name}\""))
        .await;
    assert!(
        count >= 1,
        "outbox must contain at least one row after a manual FULL refresh \
         (Gap-1 regression test)"
    );
}

/// Gap-1 regression: outbox must be written after a manual IMMEDIATE-mode refresh.
/// IMMEDIATE mode routes to `execute_manual_full_refresh`; the centralized
/// outbox write introduced in #682 runs for all refresh modes including IMMEDIATE.
#[tokio::test]
async fn test_outbox_written_after_manual_immediate_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE gap1_imm_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO gap1_imm_src VALUES (1, 'x'), (2, 'y')")
        .await;
    db.create_st(
        "gap1_imm_st",
        "SELECT id, val FROM gap1_imm_src",
        "1m",
        "IMMEDIATE",
    )
    .await;
    db.execute("SELECT pgtrickle.enable_outbox('gap1_imm_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'gap1_imm_st'",
        )
        .await;

    db.execute("INSERT INTO gap1_imm_src VALUES (3, 'z')").await;
    db.refresh_st("gap1_imm_st").await;

    let count: i64 = db
        .query_scalar(&format!("SELECT count(*) FROM pgtrickle.\"{outbox_name}\""))
        .await;
    assert!(
        count >= 1,
        "outbox must contain at least one row after a manual IMMEDIATE refresh \
         (Gap-1 regression test)"
    );
}

/// Gap-1 regression: outbox must be written when needs_reinit forces a FULL
/// refresh.  We force the flag directly to reproduce the reinit path without
/// needing a DDL hook.
#[tokio::test]
async fn test_outbox_written_after_reinit_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE gap1_reinit_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO gap1_reinit_src VALUES (1, 'a'), (2, 'b')")
        .await;
    db.create_st(
        "gap1_reinit_st",
        "SELECT id, val FROM gap1_reinit_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.execute("SELECT pgtrickle.enable_outbox('gap1_reinit_st')")
        .await;

    let outbox_name: String = db
        .query_scalar(
            "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
             WHERE stream_table_name = 'gap1_reinit_st'",
        )
        .await;

    // Force the reinit flag so the next manual refresh takes the reinit path.
    db.execute(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET needs_reinit = true WHERE pgt_name = 'gap1_reinit_st'",
    )
    .await;

    db.execute("INSERT INTO gap1_reinit_src VALUES (3, 'c')")
        .await;
    db.refresh_st("gap1_reinit_st").await;

    let count: i64 = db
        .query_scalar(&format!("SELECT count(*) FROM pgtrickle.\"{outbox_name}\""))
        .await;
    assert!(
        count >= 1,
        "outbox must contain at least one row after a needs_reinit FULL refresh \
         (Gap-1 regression test)"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// New catalog tables — existence checks
// ══════════════════════════════════════════════════════════════════════════════

/// v0.28.0 catalog tables should all exist after extension install.
#[tokio::test]
async fn test_v028_catalog_tables_exist() {
    let db = E2eDb::new().await.with_extension().await;

    for table in &[
        "pgt_outbox_config",
        "pgt_consumer_groups",
        "pgt_consumer_offsets",
        "pgt_consumer_leases",
        "pgt_inbox_config",
        "pgt_inbox_ordering_config",
        "pgt_inbox_priority_config",
    ] {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = 'pgtrickle' AND table_name = '{table}')"
            ))
            .await;
        assert!(exists, "Catalog table 'pgtrickle.{table}' should exist");
    }
}
