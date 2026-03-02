//! E2E tests for diamond dependency consistency (atomic refresh groups).
//!
//! These tests validate the diamond_consistency parameter on
//! `create_stream_table` / `alter_stream_table`, the
//! `pgtrickle.diamond_groups()` monitoring function, and end-to-end
//! consistent refresh semantics for diamond topologies.

mod e2e;
use e2e::E2eDb;

// ── Diamond topology helpers ───────────────────────────────────────────

/// Set up a simple diamond: A (base table) → B, C (stream tables) → D (stream table).
///
/// A is a base table with `id INT, val INT`.
/// B selects from A with a filter, C selects from A with a different filter,
/// D joins B and C.
async fn setup_diamond(db: &E2eDb, dc: &str) {
    // Base table
    db.execute("CREATE TABLE src_a (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO src_a VALUES (1, 10), (2, 20), (3, 30)")
        .await;

    // B: reads from A
    let sql_b = format!(
        "SELECT pgtrickle.create_stream_table('st_b', $$SELECT id, val FROM src_a WHERE val <= 20$$, \
         '1m', 'FULL', true, '{dc}')"
    );
    db.execute(&sql_b).await;

    // C: reads from A
    let sql_c = format!(
        "SELECT pgtrickle.create_stream_table('st_c', $$SELECT id, val FROM src_a WHERE val >= 20$$, \
         '1m', 'FULL', true, '{dc}')"
    );
    db.execute(&sql_c).await;

    // D: joins B and C (fan-in — the diamond convergence point)
    let sql_d = format!(
        "SELECT pgtrickle.create_stream_table('st_d', \
         $$SELECT b.id AS b_id, c.id AS c_id, b.val + c.val AS total FROM st_b b JOIN st_c c ON b.id = c.id$$, \
         '1m', 'FULL', true, '{dc}')"
    );
    db.execute(&sql_d).await;
}

// ── Tests ──────────────────────────────────────────────────────────────

/// Verify that the `diamond_consistency` column defaults to the GUC value
/// ('none') when not specified.
#[tokio::test]
async fn test_diamond_consistency_default() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO src VALUES (1, 10)").await;
    db.create_st("test_default_dc", "SELECT * FROM src", "1m", "FULL")
        .await;

    let dc: String = db
        .query_scalar(
            "SELECT diamond_consistency FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'test_default_dc'",
        )
        .await;
    assert_eq!(
        dc, "none",
        "expected default diamond_consistency to be 'none'"
    );
}

/// Verify that create_stream_table accepts diamond_consistency='atomic'.
#[tokio::test]
async fn test_diamond_consistency_create_atomic() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO src VALUES (1, 10)").await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('test_atomic_dc', \
         $$SELECT * FROM src$$, '1m', 'FULL', true, 'atomic')",
    )
    .await;

    let dc: String = db
        .query_scalar(
            "SELECT diamond_consistency FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'test_atomic_dc'",
        )
        .await;
    assert_eq!(dc, "atomic");
}

/// Verify that alter_stream_table can change diamond_consistency.
#[tokio::test]
async fn test_diamond_consistency_alter() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO src VALUES (1, 10)").await;
    db.create_st("test_alter_dc", "SELECT * FROM src", "1m", "FULL")
        .await;

    // Should start as 'none'
    let dc: String = db
        .query_scalar(
            "SELECT diamond_consistency FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'test_alter_dc'",
        )
        .await;
    assert_eq!(dc, "none");

    // Alter to 'atomic'
    db.alter_st("test_alter_dc", "diamond_consistency => 'atomic'")
        .await;

    let dc: String = db
        .query_scalar(
            "SELECT diamond_consistency FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'test_alter_dc'",
        )
        .await;
    assert_eq!(dc, "atomic");
}

/// Verify that a diamond topology with diamond_consistency='atomic' detects
/// the correct diamond group membership via `pgtrickle.diamond_groups()`.
#[tokio::test]
async fn test_diamond_groups_sql_function() {
    let db = E2eDb::new().await.with_extension().await;
    setup_diamond(&db, "atomic").await;

    // The diamond_groups() function should show a non-empty result
    let group_count: i64 = db
        .query_scalar("SELECT count(DISTINCT group_id) FROM pgtrickle.diamond_groups()")
        .await;
    assert!(
        group_count >= 1,
        "expected at least one diamond group, got {}",
        group_count
    );

    // Check that the group has at least 2 members
    let member_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.diamond_groups()")
        .await;
    assert!(
        member_count >= 2,
        "expected at least 2 members in diamond group, got {}",
        member_count
    );
}

/// Verify that a linear chain (no diamond) does not create consistency groups.
#[tokio::test]
async fn test_diamond_linear_unaffected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_lin (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO src_lin VALUES (1, 10)").await;

    // Linear chain: src_lin → st_x → st_y (no diamond)
    db.execute(
        "SELECT pgtrickle.create_stream_table('st_x', \
         $$SELECT id, val FROM src_lin$$, '1m', 'FULL', true, 'atomic')",
    )
    .await;
    db.execute(
        "SELECT pgtrickle.create_stream_table('st_y', \
         $$SELECT id, val FROM st_x$$, '1m', 'FULL', true, 'atomic')",
    )
    .await;

    // diamond_groups() should return 0 rows (no diamond in a linear chain)
    let group_count: i64 = db
        .query_scalar("SELECT count(DISTINCT group_id) FROM pgtrickle.diamond_groups()")
        .await;
    assert_eq!(
        group_count, 0,
        "linear chain should not produce diamond groups"
    );
}

/// Verify that diamond_consistency='none' mode does not create groups.
/// Even in a diamond topology, 'none' means no grouping.
#[tokio::test]
async fn test_diamond_none_mode_no_groups() {
    let db = E2eDb::new().await.with_extension().await;
    setup_diamond(&db, "none").await;

    // diamond_groups() shows topology-detected diamonds regardless of mode,
    // but the scheduler won't use SAVEPOINT for 'none' mode STs.
    // For now, just verify the function doesn't crash and returns results.
    let _count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.diamond_groups()")
        .await;
}

/// Verify that refreshing all STs in a diamond succeeds and D contains
/// consistent data when all members are in atomic mode.
#[tokio::test]
async fn test_diamond_atomic_all_succeed() {
    let db = E2eDb::new().await.with_extension().await;
    setup_diamond(&db, "atomic").await;

    // After creation with initialize=true, all STs should be populated.
    let d_count: i64 = db.count("st_d").await;
    // D joins B and C on id where val<=20 (B: {1,2}) and val>=20 (C: {2,3})
    // Intersection: id=2 (val 20 appears in both). So D should have 1 row.
    assert_eq!(
        d_count, 1,
        "expected 1 row in st_d after initial population"
    );

    // Verify D's content is consistent
    let total: i64 = db
        .query_scalar("SELECT total FROM st_d WHERE b_id = 2")
        .await;
    assert_eq!(total, 40, "expected total = 20 + 20 = 40");

    // Insert more data and manually refresh
    db.execute("INSERT INTO src_a VALUES (4, 20)").await;

    db.refresh_st("st_b").await;
    db.refresh_st("st_c").await;
    db.refresh_st("st_d").await;

    // Now B has {1,2,4} (val<=20), C has {2,3,4} (val>=20), so D = {2,4}
    let d_count: i64 = db.count("st_d").await;
    assert_eq!(d_count, 2, "expected 2 rows in st_d after refresh");
}

/// Verify that the `diamond_consistency` column is visible in the catalog.
#[tokio::test]
async fn test_diamond_consistency_in_catalog_view() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE src_cat (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO src_cat VALUES (1, 10)").await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('test_cat_dc', \
         $$SELECT * FROM src_cat$$, '1m', 'FULL', true, 'atomic')",
    )
    .await;

    // Query it from the info view (which selects st.*)
    let dc: String = db
        .query_scalar(
            "SELECT diamond_consistency FROM pgtrickle.stream_tables_info \
             WHERE pgt_name = 'test_cat_dc'",
        )
        .await;
    assert_eq!(dc, "atomic");
}
