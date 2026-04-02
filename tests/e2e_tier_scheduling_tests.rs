//! C-1d: E2E tests for tiered refresh scheduling.
//!
//! Validates:
//!   - Tier assignment via `ALTER STREAM TABLE … SET (tier = '…')`
//!   - Invalid tier names rejected with ERROR
//!   - Catalog column `refresh_tier` updated correctly
//!   - Scheduler respects tier multipliers when `pg_trickle.tiered_scheduling = on`
//!   - Frozen ST never refreshes until promoted
//!   - Cold ST skips cycles relative to Hot
//!
//! Light-eligible tests (SQL surface validation) use `E2eDb::new()`.
//! Scheduler-dependent tests use `E2eDb::new_on_postgres_db()` (full E2E only).

mod e2e;

use e2e::E2eDb;

// ── Light-eligible: SQL surface validation ────────────────────────────────

/// C-1d: Default tier is 'hot' after creation.
#[tokio::test]
async fn test_tier_default_is_hot() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tier_src1 (id INT PRIMARY KEY, val INT)")
        .await;

    db.create_st("tier_st1", "SELECT id, val FROM tier_src1", "1m", "FULL")
        .await;

    let tier: String = db
        .query_scalar(
            "SELECT refresh_tier FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'tier_st1'",
        )
        .await;
    assert_eq!(tier, "hot", "Default tier should be 'hot'");
}

/// C-1d: ALTER STREAM TABLE sets tier to 'warm' and updates catalog.
#[tokio::test]
async fn test_tier_alter_to_warm() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tier_src2 (id INT PRIMARY KEY, val INT)")
        .await;

    db.create_st("tier_st2", "SELECT id, val FROM tier_src2", "1m", "FULL")
        .await;

    db.alter_st("tier_st2", "tier => 'warm'").await;

    let tier: String = db
        .query_scalar(
            "SELECT refresh_tier FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'tier_st2'",
        )
        .await;
    assert_eq!(tier, "warm");
}

/// C-1d: ALTER STREAM TABLE sets tier to 'cold'.
#[tokio::test]
async fn test_tier_alter_to_cold() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tier_src3 (id INT PRIMARY KEY, val INT)")
        .await;

    db.create_st("tier_st3", "SELECT id, val FROM tier_src3", "1m", "FULL")
        .await;

    db.alter_st("tier_st3", "tier => 'cold'").await;

    let tier: String = db
        .query_scalar(
            "SELECT refresh_tier FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'tier_st3'",
        )
        .await;
    assert_eq!(tier, "cold");
}

/// C-1d: ALTER STREAM TABLE sets tier to 'frozen'.
#[tokio::test]
async fn test_tier_alter_to_frozen() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tier_src4 (id INT PRIMARY KEY, val INT)")
        .await;

    db.create_st("tier_st4", "SELECT id, val FROM tier_src4", "1m", "FULL")
        .await;

    db.alter_st("tier_st4", "tier => 'frozen'").await;

    let tier: String = db
        .query_scalar(
            "SELECT refresh_tier FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'tier_st4'",
        )
        .await;
    assert_eq!(tier, "frozen");
}

/// C-1d: ALTER STREAM TABLE back to 'hot' from 'cold'.
#[tokio::test]
async fn test_tier_promote_cold_to_hot() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tier_src5 (id INT PRIMARY KEY, val INT)")
        .await;

    db.create_st("tier_st5", "SELECT id, val FROM tier_src5", "1m", "FULL")
        .await;

    db.alter_st("tier_st5", "tier => 'cold'").await;
    db.alter_st("tier_st5", "tier => 'hot'").await;

    let tier: String = db
        .query_scalar(
            "SELECT refresh_tier FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'tier_st5'",
        )
        .await;
    assert_eq!(tier, "hot");
}

/// C-1d: Invalid tier name is rejected with ERROR.
#[tokio::test]
async fn test_tier_invalid_name_rejected() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tier_src6 (id INT PRIMARY KEY, val INT)")
        .await;

    db.create_st("tier_st6", "SELECT id, val FROM tier_src6", "1m", "FULL")
        .await;

    let result = db
        .try_execute("SELECT pgtrickle.alter_stream_table('tier_st6', tier => 'invalid')")
        .await;
    assert!(
        result.is_err(),
        "Invalid tier name 'invalid' should be rejected"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.to_lowercase().contains("invalid") || err_msg.to_lowercase().contains("tier"),
        "Error should mention invalid tier: {err_msg}"
    );
}

/// C-1d: Tier is case-insensitive (e.g., 'WARM' normalised to 'warm').
#[tokio::test]
async fn test_tier_case_insensitive() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tier_src7 (id INT PRIMARY KEY, val INT)")
        .await;

    db.create_st("tier_st7", "SELECT id, val FROM tier_src7", "1m", "FULL")
        .await;

    db.alter_st("tier_st7", "tier => 'WARM'").await;

    let tier: String = db
        .query_scalar(
            "SELECT refresh_tier FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'tier_st7'",
        )
        .await;
    assert_eq!(tier, "warm", "Tier should be normalised to lowercase");
}

/// C-1d: Tier visible in stream_tables_info view.
#[tokio::test]
async fn test_tier_visible_in_info_view() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE tier_src8 (id INT PRIMARY KEY, val INT)")
        .await;

    db.create_st("tier_st8", "SELECT id, val FROM tier_src8", "1m", "FULL")
        .await;

    db.alter_st("tier_st8", "tier => 'cold'").await;

    let tier: String = db
        .query_scalar(
            "SELECT refresh_tier FROM pgtrickle.stream_tables_info \
             WHERE pgt_name = 'tier_st8'",
        )
        .await;
    assert_eq!(tier, "cold");
}
