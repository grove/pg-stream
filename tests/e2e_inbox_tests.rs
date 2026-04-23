//! E2E tests for the Transactional Inbox Pattern (v0.28.0).
//!
//! Covers:
//! - INBOX-1: create_inbox — managed inbox creation with stream tables
//! - INBOX-2: drop_inbox — catalog cleanup and stream table removal
//! - INBOX-3: enable_inbox_tracking — BYOT inbox mode
//! - INBOX-4: inbox_health — JSONB health summary
//! - INBOX-5: inbox_status — table summary of one or all inboxes
//! - INBOX-6: replay_inbox_messages — reset messages for replay
//! - INBOX-B1: enable/disable_inbox_ordering — per-aggregate ordering
//! - INBOX-B2: enable/disable_inbox_priority — priority-tier processing
//! - INBOX-B3: inbox_ordering_gaps — sequence gap detection
//! - INBOX-B4: inbox_is_my_partition — consistent-hash partition check
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-1: create_inbox
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-1a: create_inbox registers the inbox in the catalog.
#[tokio::test]
async fn test_create_inbox_registers_catalog_entry() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib1a_orders')")
        .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_config \
             WHERE inbox_name = 'ib1a_orders')",
        )
        .await;
    assert!(exists, "Inbox catalog entry should be created");
}

/// INBOX-1b: create_inbox creates the inbox table.
#[tokio::test]
async fn test_create_inbox_creates_inbox_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib1b_events')")
        .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
             WHERE table_schema = 'pgtrickle' AND table_name = 'ib1b_events')",
        )
        .await;
    assert!(exists, "Inbox table should be created in pgtrickle schema");
}

/// INBOX-1c: create_inbox creates the pending stream table.
#[tokio::test]
async fn test_create_inbox_creates_pending_stream_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib1c_msgs')")
        .await;

    let st_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'ib1c_msgs_pending')",
        )
        .await;
    assert!(st_exists, "Pending stream table should be created");
}

/// INBOX-1d: create_inbox with with_dead_letter=true creates the DLQ stream table.
#[tokio::test]
async fn test_create_inbox_with_dead_letter_creates_dlq() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib1d_dlq', with_dead_letter => true)")
        .await;

    let dlq_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'ib1d_dlq_dlq')",
        )
        .await;
    assert!(dlq_exists, "DLQ stream table should be created");
}

/// INBOX-1e: create_inbox with with_stats=true creates the stats stream table.
#[tokio::test]
async fn test_create_inbox_with_stats_creates_stats_st() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib1e_stats', with_stats => true)")
        .await;

    let stats_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'ib1e_stats_stats')",
        )
        .await;
    assert!(stats_exists, "Stats stream table should be created");
}

/// INBOX-1f: Creating the same inbox twice raises InboxAlreadyExists.
#[tokio::test]
async fn test_create_inbox_already_exists_is_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib1f_dup')")
        .await;

    let result = db
        .try_execute("SELECT pgtrickle.create_inbox('ib1f_dup')")
        .await;
    assert!(result.is_err(), "Creating inbox twice should fail");
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-2: drop_inbox
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-2a: drop_inbox removes the catalog entry.
#[tokio::test]
async fn test_drop_inbox_removes_catalog_entry() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib2a_drop')")
        .await;
    db.execute("SELECT pgtrickle.drop_inbox('ib2a_drop')").await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_config \
             WHERE inbox_name = 'ib2a_drop')",
        )
        .await;
    assert!(!exists, "Inbox catalog entry should be removed");
}

/// INBOX-2b: drop_inbox removes the associated stream tables.
#[tokio::test]
async fn test_drop_inbox_removes_stream_tables() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib2b_drop')")
        .await;
    db.execute("SELECT pgtrickle.drop_inbox('ib2b_drop')").await;

    let pending_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'ib2b_drop_pending')",
        )
        .await;
    assert!(!pending_exists, "Pending stream table should be removed");
}

/// INBOX-2c: drop_inbox with if_exists=true on a non-existent inbox is a no-op.
#[tokio::test]
async fn test_drop_inbox_if_exists_no_op() {
    let db = E2eDb::new().await.with_extension().await;

    // Should not fail
    db.execute("SELECT pgtrickle.drop_inbox('nonexistent_inbox', true)")
        .await;
}

/// INBOX-2d: drop_inbox without if_exists on a non-existent inbox is an error.
#[tokio::test]
async fn test_drop_inbox_not_found_is_error() {
    let db = E2eDb::new().await.with_extension().await;

    let result = db
        .try_execute("SELECT pgtrickle.drop_inbox('nonexistent_inbox')")
        .await;
    assert!(
        result.is_err(),
        "drop_inbox on non-existent inbox should fail"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-3: enable_inbox_tracking (BYOT mode)
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-3: enable_inbox_tracking registers BYOT inbox in the catalog.
#[tokio::test]
async fn test_enable_inbox_tracking_registers_catalog_entry() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a user-managed table with required columns
    db.execute(
        "CREATE TABLE ib3_byot_msgs (
            event_id      TEXT PRIMARY KEY,
            event_type    TEXT,
            payload       JSONB,
            processed_at  TIMESTAMPTZ,
            retry_count   INT NOT NULL DEFAULT 0,
            error         TEXT,
            received_at   TIMESTAMPTZ NOT NULL DEFAULT now()
        )",
    )
    .await;

    db.execute("SELECT pgtrickle.enable_inbox_tracking('ib3_byot', 'public.ib3_byot_msgs')")
        .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_config \
             WHERE inbox_name = 'ib3_byot')",
        )
        .await;
    assert!(exists, "BYOT inbox catalog entry should be created");

    let is_managed: bool = db
        .query_scalar(
            "SELECT is_managed FROM pgtrickle.pgt_inbox_config \
             WHERE inbox_name = 'ib3_byot'",
        )
        .await;
    assert!(!is_managed, "BYOT inbox should have is_managed = false");
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-4: inbox_health
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-4: inbox_health returns a valid JSONB summary.
#[tokio::test]
async fn test_inbox_health_returns_jsonb() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib4_health')")
        .await;

    let health: serde_json::Value = db
        .query_scalar("SELECT pgtrickle.inbox_health('ib4_health')")
        .await;

    assert!(
        health.get("inbox_name").is_some(),
        "inbox_health should contain inbox_name"
    );
    assert!(
        health.get("status").is_some(),
        "inbox_health should contain status"
    );
    assert!(
        health.get("total_messages").is_some(),
        "inbox_health should contain total_messages"
    );
    assert!(
        health.get("pending_messages").is_some(),
        "inbox_health should contain pending_messages"
    );
}

/// INBOX-4: inbox_health reflects message counts correctly.
#[tokio::test]
async fn test_inbox_health_message_counts() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib4b_counts')")
        .await;

    // Insert 2 pending messages
    db.execute(
        "INSERT INTO pgtrickle.ib4b_counts (event_id, event_type, payload) \
         VALUES ('evt-1', 'order.created', '{\"id\": 1}'), \
                ('evt-2', 'order.shipped', '{\"id\": 2}')",
    )
    .await;

    let health: serde_json::Value = db
        .query_scalar("SELECT pgtrickle.inbox_health('ib4b_counts')")
        .await;

    let total = health["total_messages"].as_i64().unwrap_or(-1);
    let pending = health["pending_messages"].as_i64().unwrap_or(-1);
    assert_eq!(total, 2, "Should have 2 total messages");
    assert_eq!(pending, 2, "Should have 2 pending messages");
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-5: inbox_status
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-5a: inbox_status for a specific inbox returns 1 row.
#[tokio::test]
async fn test_inbox_status_single_inbox() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib5a_status')")
        .await;

    let count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.inbox_status('ib5a_status')")
        .await;
    assert_eq!(count, 1, "Should return 1 row for the specified inbox");
}

/// INBOX-5b: inbox_status with NULL returns all inboxes.
#[tokio::test]
async fn test_inbox_status_all_inboxes() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib5b_a')").await;
    db.execute("SELECT pgtrickle.create_inbox('ib5b_b')").await;
    db.execute("SELECT pgtrickle.create_inbox('ib5b_c')").await;

    let count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.inbox_status()")
        .await;
    assert!(count >= 3, "Should return at least 3 inbox rows");
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-6: replay_inbox_messages
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-6: replay_inbox_messages resets processed messages.
#[tokio::test]
async fn test_replay_inbox_messages_resets_state() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib6_replay')")
        .await;

    // Insert 2 messages and mark them as processed
    db.execute(
        "INSERT INTO pgtrickle.ib6_replay (event_id, event_type, payload) \
         VALUES ('evt-r1', 'test.event', '{}'), \
                ('evt-r2', 'test.event', '{}')",
    )
    .await;
    db.execute(
        "UPDATE pgtrickle.ib6_replay \
         SET processed_at = now(), retry_count = 1, error = 'test' \
         WHERE event_id IN ('evt-r1', 'evt-r2')",
    )
    .await;

    // Replay both messages
    let replayed: i64 = db
        .query_scalar(
            "SELECT pgtrickle.replay_inbox_messages('ib6_replay', \
             ARRAY['evt-r1', 'evt-r2'])",
        )
        .await;
    assert_eq!(replayed, 2, "Should report 2 messages replayed");

    // Verify state is reset
    let processed_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.ib6_replay WHERE processed_at IS NOT NULL")
        .await;
    assert_eq!(processed_count, 0, "All messages should be un-processed");

    let retry_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.ib6_replay WHERE retry_count > 0")
        .await;
    assert_eq!(retry_count, 0, "All retry counts should be reset to 0");
}

/// INBOX-6: replay_inbox_messages with empty array returns 0.
#[tokio::test]
async fn test_replay_inbox_messages_empty_array() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib6b_empty')")
        .await;

    let replayed: i64 = db
        .query_scalar("SELECT pgtrickle.replay_inbox_messages('ib6b_empty', ARRAY[]::text[])")
        .await;
    assert_eq!(replayed, 0, "Empty array should replay 0 messages");
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-B1: enable_inbox_ordering / disable_inbox_ordering
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-B1a: enable_inbox_ordering creates the next_<inbox> stream table.
#[tokio::test]
async fn test_enable_inbox_ordering_creates_next_stream_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib_ord_a')")
        .await;

    // Add seq_num ordering column (aggregate_id is already part of the inbox table schema)
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE pgtrickle.ib_ord_a ADD COLUMN seq_num BIGINT",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;

    db.execute("SELECT pgtrickle.enable_inbox_ordering('ib_ord_a', 'aggregate_id', 'seq_num')")
        .await;

    let next_st_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'next_ib_ord_a')",
        )
        .await;
    assert!(
        next_st_exists,
        "next_<inbox> stream table should be created"
    );

    let config_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_ordering_config \
             WHERE inbox_name = 'ib_ord_a')",
        )
        .await;
    assert!(config_exists, "Ordering config should be registered");
}

/// INBOX-B1b: disable_inbox_ordering removes the next_<inbox> stream table.
#[tokio::test]
async fn test_disable_inbox_ordering_removes_next_stream_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib_ord_b')")
        .await;
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE pgtrickle.ib_ord_b ADD COLUMN seq_num BIGINT",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;
    db.execute("SELECT pgtrickle.enable_inbox_ordering('ib_ord_b', 'aggregate_id', 'seq_num')")
        .await;
    db.execute("SELECT pgtrickle.disable_inbox_ordering('ib_ord_b')")
        .await;

    let next_st_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'next_ib_ord_b')",
        )
        .await;
    assert!(
        !next_st_exists,
        "next_<inbox> stream table should be removed after disable"
    );

    let config_exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_ordering_config \
             WHERE inbox_name = 'ib_ord_b')",
        )
        .await;
    assert!(!config_exists, "Ordering config should be removed");
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-B2: enable_inbox_priority / disable_inbox_priority
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-B2a: enable_inbox_priority registers the priority config.
#[tokio::test]
async fn test_enable_inbox_priority_registers_config() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib_pri_a')")
        .await;
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE pgtrickle.ib_pri_a ADD COLUMN priority INT NOT NULL DEFAULT 5",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;

    db.execute("SELECT pgtrickle.enable_inbox_priority('ib_pri_a', 'priority')")
        .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_priority_config \
             WHERE inbox_name = 'ib_pri_a')",
        )
        .await;
    assert!(exists, "Priority config should be registered");
}

/// INBOX-B2b: disable_inbox_priority removes the priority config.
#[tokio::test]
async fn test_disable_inbox_priority_removes_config() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib_pri_b')")
        .await;
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE pgtrickle.ib_pri_b ADD COLUMN priority INT NOT NULL DEFAULT 5",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;
    db.execute("SELECT pgtrickle.enable_inbox_priority('ib_pri_b', 'priority')")
        .await;
    db.execute("SELECT pgtrickle.disable_inbox_priority('ib_pri_b')")
        .await;

    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_priority_config \
             WHERE inbox_name = 'ib_pri_b')",
        )
        .await;
    assert!(!exists, "Priority config should be removed after disable");
}

/// INBOX-B2c: Enabling both ordering and priority on the same inbox is an error.
#[tokio::test]
async fn test_enable_priority_conflicts_with_ordering() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib_conflict')")
        .await;
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE pgtrickle.ib_conflict ADD COLUMN seq_num BIGINT, ADD COLUMN priority INT NOT NULL DEFAULT 5",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;

    db.execute("SELECT pgtrickle.enable_inbox_ordering('ib_conflict', 'aggregate_id', 'seq_num')")
        .await;

    let result = db
        .try_execute("SELECT pgtrickle.enable_inbox_priority('ib_conflict', 'priority')")
        .await;
    assert!(
        result.is_err(),
        "Enabling priority after ordering should fail with conflict error"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-B3: inbox_ordering_gaps
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-B3: inbox_ordering_gaps returns no gaps for sequential messages.
#[tokio::test]
async fn test_inbox_ordering_gaps_no_gaps_sequential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib_gaps_a')")
        .await;
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE pgtrickle.ib_gaps_a ADD COLUMN seq_num BIGINT",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;

    db.execute("SELECT pgtrickle.enable_inbox_ordering('ib_gaps_a', 'aggregate_id', 'seq_num')")
        .await;

    // Insert sequential messages for one aggregate
    db.execute(
        "INSERT INTO pgtrickle.ib_gaps_a \
         (event_id, aggregate_id, seq_num) \
         VALUES ('e1', 'agg-1', 1), ('e2', 'agg-1', 2), ('e3', 'agg-1', 3)",
    )
    .await;

    let gap_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.inbox_ordering_gaps('ib_gaps_a')")
        .await;
    assert_eq!(gap_count, 0, "Sequential messages should have no gaps");
}

/// INBOX-B3: inbox_ordering_gaps detects a gap in the sequence.
#[tokio::test]
async fn test_inbox_ordering_gaps_detects_gap() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("SELECT pgtrickle.create_inbox('ib_gaps_b')")
        .await;
    db.execute_seq(&[
        "SET pg_trickle.block_source_ddl = false",
        "ALTER TABLE pgtrickle.ib_gaps_b ADD COLUMN seq_num BIGINT",
        "SET pg_trickle.block_source_ddl = true",
    ])
    .await;

    db.execute("SELECT pgtrickle.enable_inbox_ordering('ib_gaps_b', 'aggregate_id', 'seq_num')")
        .await;

    // Insert messages with gap at seq 3 (1, 2, 4)
    db.execute(
        "INSERT INTO pgtrickle.ib_gaps_b \
         (event_id, aggregate_id, seq_num) \
         VALUES ('e1', 'agg-1', 1), ('e2', 'agg-1', 2), ('e4', 'agg-1', 4)",
    )
    .await;

    let gap_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.inbox_ordering_gaps('ib_gaps_b')")
        .await;
    assert!(gap_count >= 1, "Should detect at least 1 gap");
}

// ══════════════════════════════════════════════════════════════════════════════
// INBOX-B4: inbox_is_my_partition
// ══════════════════════════════════════════════════════════════════════════════

/// INBOX-B4: inbox_is_my_partition distributes deterministically.
#[tokio::test]
async fn test_inbox_is_my_partition_deterministic() {
    let db = E2eDb::new().await.with_extension().await;

    // The same aggregate_id should always land on the same worker
    let result1: bool = db
        .query_scalar("SELECT pgtrickle.inbox_is_my_partition('agg-123', 0, 3)")
        .await;
    let result2: bool = db
        .query_scalar("SELECT pgtrickle.inbox_is_my_partition('agg-123', 0, 3)")
        .await;
    assert_eq!(
        result1, result2,
        "Same input must produce same output (deterministic)"
    );
}

/// INBOX-B4: inbox_is_my_partition exactly one worker owns each aggregate.
#[tokio::test]
async fn test_inbox_is_my_partition_exactly_one_owner() {
    let db = E2eDb::new().await.with_extension().await;

    // With 3 workers, exactly one worker should own any given aggregate_id
    let w0: bool = db
        .query_scalar("SELECT pgtrickle.inbox_is_my_partition('agg-test-xyz', 0, 3)")
        .await;
    let w1: bool = db
        .query_scalar("SELECT pgtrickle.inbox_is_my_partition('agg-test-xyz', 1, 3)")
        .await;
    let w2: bool = db
        .query_scalar("SELECT pgtrickle.inbox_is_my_partition('agg-test-xyz', 2, 3)")
        .await;

    let owners = [w0, w1, w2].iter().filter(|&&x| x).count();
    assert_eq!(
        owners, 1,
        "Exactly one worker should own 'agg-test-xyz', but {owners} workers claim ownership"
    );
}

/// INBOX-B4: inbox_is_my_partition with total_workers=1 always returns true.
#[tokio::test]
async fn test_inbox_is_my_partition_single_worker() {
    let db = E2eDb::new().await.with_extension().await;

    let result: bool = db
        .query_scalar("SELECT pgtrickle.inbox_is_my_partition('any-aggregate', 0, 1)")
        .await;
    assert!(result, "Single worker should own all partitions");
}

/// INBOX-B4: inbox_is_my_partition with total_workers=0 returns true (degenerate).
#[tokio::test]
async fn test_inbox_is_my_partition_degenerate_zero_workers() {
    let db = E2eDb::new().await.with_extension().await;

    let result: bool = db
        .query_scalar("SELECT pgtrickle.inbox_is_my_partition('any-aggregate', 0, 0)")
        .await;
    assert!(
        result,
        "Degenerate case (0 workers) should return true for all"
    );
}
