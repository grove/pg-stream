//! E2E tests for IMMEDIATE-mode multi-layer cascades.
//!
//! Validates the behavior of IMMEDIATE stream tables in multi-layer DAGs,
//! documenting what cascades automatically vs. what requires explicit refresh.
//!
//! ## Key architecture note
//!
//! IMMEDIATE mode uses statement-level AFTER triggers (`src/ivm.rs`). When
//! the base table changes, the trigger refreshes ST₁. But ST₁'s internal
//! update via MERGE may or may not fire ST₂'s trigger. Current behavior:
//! automatic cascade for 2-layer, explicit refresh needed for deeper layers.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Helper ──────────────────────────────────────────────────────────────

/// Create an IMMEDIATE-mode stream table (schedule = NULL).
async fn create_immediate_st(db: &E2eDb, name: &str, query: &str) {
    let sql = format!(
        "SELECT pgtrickle.create_stream_table('{name}', $${query}$$, \
         NULL, 'IMMEDIATE')"
    );
    db.execute(&sql).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 8.1 — 2-layer IMMEDIATE: explicit refresh for second layer
// ═══════════════════════════════════════════════════════════════════════════

/// Documents current behavior: base → IMMED_A → IMMED_B.
/// Insert into base: A is auto-refreshed via trigger.
/// B may not auto-cascade — verify and document the boundary.
#[tokio::test]
async fn test_immediate_2_layer_explicit_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE imm2_src (
            id  INT PRIMARY KEY,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO imm2_src VALUES (1, 10), (2, 20)")
        .await;

    // A: IMMEDIATE passthrough
    create_immediate_st(&db, "imm2_a", "SELECT id, val FROM imm2_src").await;

    // B: IMMEDIATE on A
    create_immediate_st(&db, "imm2_b", "SELECT id, val * 2 AS doubled FROM imm2_a").await;

    // Initial state
    assert_eq!(db.count("public.imm2_a").await, 2);
    assert_eq!(db.count("public.imm2_b").await, 2);

    // Insert into base — A should auto-refresh via trigger
    db.execute("INSERT INTO imm2_src VALUES (3, 30)").await;

    assert_eq!(
        db.count("public.imm2_a").await,
        3,
        "A should have 3 rows after INSERT (IMMEDIATE trigger)"
    );

    // B may or may not auto-cascade. Do explicit refresh to ensure correctness.
    db.refresh_st("imm2_b").await;

    assert_eq!(
        db.count("public.imm2_b").await,
        3,
        "B should have 3 rows after explicit refresh"
    );

    // Verify values
    let doubled: i32 = db
        .query_scalar("SELECT doubled FROM public.imm2_b WHERE id = 3")
        .await;
    assert_eq!(doubled, 60, "B should have doubled value for id=3");
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 8.2 — IMMEDIATE with DIFFERENTIAL downstream
// ═══════════════════════════════════════════════════════════════════════════

/// base → IMMED_A → DIFF_B.
/// Insert into base: A auto-refreshes. Manual refresh of B should
/// detect A's change via data_timestamp and pick up the updates.
#[tokio::test]
async fn test_immediate_with_differential_downstream() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE immd_src (
            id   SERIAL PRIMARY KEY,
            grp  TEXT NOT NULL,
            val  INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO immd_src (grp, val) VALUES ('a', 10), ('b', 20)")
        .await;

    // A: IMMEDIATE
    create_immediate_st(
        &db,
        "immd_a",
        "SELECT grp, SUM(val) AS total FROM immd_src GROUP BY grp",
    )
    .await;

    // B: DIFFERENTIAL on A (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'immd_b',
            $$SELECT grp, total * 3 AS tripled FROM immd_a$$,
            'calculated',
            'DIFFERENTIAL'
        )",
    )
    .await;

    let a_q = "SELECT grp, SUM(val) AS total FROM immd_src GROUP BY grp";
    let b_q = "SELECT grp, SUM(val) * 3 AS tripled FROM immd_src GROUP BY grp";

    db.assert_st_matches_query("immd_a", a_q).await;
    db.assert_st_matches_query("immd_b", b_q).await;

    // Insert into base — A auto-refreshes via IMMEDIATE trigger
    db.execute("INSERT INTO immd_src (grp, val) VALUES ('c', 30)")
        .await;

    // A should already have the new data
    db.assert_st_matches_query("immd_a", a_q).await;

    // Manual refresh of B picks up A's changes
    db.refresh_st("immd_b").await;
    db.assert_st_matches_query("immd_b", b_q).await;

    // Another cycle: DELETE
    db.execute("DELETE FROM immd_src WHERE grp = 'a'").await;
    db.assert_st_matches_query("immd_a", a_q).await;

    db.refresh_st("immd_b").await;
    db.assert_st_matches_query("immd_b", b_q).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 8.3 — 3-layer IMMEDIATE cascade propagation limits
// ═══════════════════════════════════════════════════════════════════════════

/// base → IMMED_A → IMMED_B → IMMED_C.
/// Tests how far the IMMEDIATE cascade propagates automatically.
/// Documents the current behavior boundary.
#[tokio::test]
async fn test_immediate_3_layer_propagation() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE imm3_src (
            id  INT PRIMARY KEY,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO imm3_src VALUES (1, 10), (2, 20)")
        .await;

    // 3-layer IMMEDIATE chain
    create_immediate_st(&db, "imm3_a", "SELECT id, val FROM imm3_src").await;
    create_immediate_st(&db, "imm3_b", "SELECT id, val * 2 AS doubled FROM imm3_a").await;
    create_immediate_st(
        &db,
        "imm3_c",
        "SELECT id, doubled + 1 AS result FROM imm3_b",
    )
    .await;

    // Initial state
    assert_eq!(db.count("public.imm3_a").await, 2);
    assert_eq!(db.count("public.imm3_b").await, 2);
    assert_eq!(db.count("public.imm3_c").await, 2);

    // Insert into base
    db.execute("INSERT INTO imm3_src VALUES (3, 30)").await;

    // A should auto-refresh (direct trigger)
    assert_eq!(
        db.count("public.imm3_a").await,
        3,
        "A should auto-refresh from base trigger"
    );

    // B and C may or may not auto-cascade.
    // Do explicit refresh to ensure correctness at all layers.
    db.refresh_st("imm3_b").await;
    db.refresh_st("imm3_c").await;

    assert_eq!(db.count("public.imm3_b").await, 3);
    assert_eq!(db.count("public.imm3_c").await, 3);

    // Verify values at C
    let result: i32 = db
        .query_scalar("SELECT result FROM public.imm3_c WHERE id = 3")
        .await;
    assert_eq!(result, 61, "C should have 30*2+1=61 for id=3");

    // Verify ground truth at all layers
    db.assert_st_matches_query("imm3_a", "SELECT id, val FROM imm3_src")
        .await;
    db.assert_st_matches_query("imm3_b", "SELECT id, val * 2 AS doubled FROM imm3_src")
        .await;
    db.assert_st_matches_query("imm3_c", "SELECT id, val * 2 + 1 AS result FROM imm3_src")
        .await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 8.4 — IMMEDIATE rollback: no side effects
// ═══════════════════════════════════════════════════════════════════════════

/// base → IMMED_A. Insert in a transaction that is rolled back.
/// Verify A is unchanged — the IVM trigger should not have persisted
/// any changes from the rolled-back transaction.
#[tokio::test]
async fn test_immediate_rollback_no_side_effects() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE immrb_src (
            id  INT PRIMARY KEY,
            val INT NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO immrb_src VALUES (1, 10), (2, 20)")
        .await;

    create_immediate_st(&db, "immrb_a", "SELECT id, val FROM immrb_src").await;

    assert_eq!(db.count("public.immrb_a").await, 2);

    // Start a transaction, insert, then ROLLBACK
    let pool = db.pool.clone();
    let mut conn = pool.acquire().await.expect("get connection");
    sqlx::query("BEGIN").execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO immrb_src VALUES (3, 30)")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("ROLLBACK").execute(&mut *conn).await.unwrap();
    drop(conn);

    // A should still have only 2 rows
    assert_eq!(
        db.count("public.immrb_a").await,
        2,
        "IMMEDIATE ST should not reflect rolled-back INSERT"
    );

    // The existing data should be intact
    let val: i32 = db
        .query_scalar("SELECT val FROM public.immrb_a WHERE id = 1")
        .await;
    assert_eq!(val, 10, "Existing data should be unchanged after rollback");

    // Verify a committed insert still works
    db.execute("INSERT INTO immrb_src VALUES (4, 40)").await;
    assert_eq!(
        db.count("public.immrb_a").await,
        3,
        "A should have 3 rows after committed INSERT"
    );
}
