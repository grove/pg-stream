//! E2E tests for multi-cycle refresh correctness (F24: G8.2).
//!
//! Validates that multiple DML → refresh cycles produce correct cumulative
//! results for aggregate, join, and window queries, and that prepared
//! statement caching survives across cycles.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// Multi-cycle aggregate
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_multi_cycle_aggregate_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE mc_agg (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute("INSERT INTO mc_agg (grp, val) VALUES ('a', 10), ('b', 20)")
        .await;

    let q = "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM mc_agg GROUP BY grp";
    db.create_st("mc_agg_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("mc_agg_st", q).await;

    // Cycle 1: inserts
    db.execute("INSERT INTO mc_agg (grp, val) VALUES ('a', 5), ('c', 30)")
        .await;
    db.refresh_st("mc_agg_st").await;
    db.assert_st_matches_query("mc_agg_st", q).await;

    // Cycle 2: updates
    db.execute("UPDATE mc_agg SET val = val * 2 WHERE grp = 'b'")
        .await;
    db.refresh_st("mc_agg_st").await;
    db.assert_st_matches_query("mc_agg_st", q).await;

    // Cycle 3: deletes
    db.execute("DELETE FROM mc_agg WHERE grp = 'c'").await;
    db.refresh_st("mc_agg_st").await;
    db.assert_st_matches_query("mc_agg_st", q).await;

    // Cycle 4: mixed
    db.execute("INSERT INTO mc_agg (grp, val) VALUES ('a', 100)")
        .await;
    db.execute("UPDATE mc_agg SET grp = 'b' WHERE grp = 'a' AND val = 5")
        .await;
    db.execute("DELETE FROM mc_agg WHERE grp = 'b' AND val = 40")
        .await;
    db.refresh_st("mc_agg_st").await;
    db.assert_st_matches_query("mc_agg_st", q).await;

    // Cycle 5: no changes (idempotent refresh)
    db.refresh_st("mc_agg_st").await;
    db.assert_st_matches_query("mc_agg_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-cycle JOIN
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_multi_cycle_join_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE mc_left (id SERIAL PRIMARY KEY, key INT, lval TEXT)")
        .await;
    db.execute("CREATE TABLE mc_right (id SERIAL PRIMARY KEY, key INT, rval TEXT)")
        .await;
    db.execute("INSERT INTO mc_left (key, lval) VALUES (1, 'a'), (2, 'b')")
        .await;
    db.execute("INSERT INTO mc_right (key, rval) VALUES (1, 'x'), (3, 'z')")
        .await;

    let q = "SELECT l.key, l.lval, r.rval \
             FROM mc_left l JOIN mc_right r ON l.key = r.key";
    db.create_st("mc_join_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("mc_join_st", q).await;

    // Cycle 1
    db.execute("INSERT INTO mc_right (key, rval) VALUES (2, 'y')")
        .await;
    db.refresh_st("mc_join_st").await;
    db.assert_st_matches_query("mc_join_st", q).await;

    // Cycle 2
    db.execute("UPDATE mc_left SET key = 3 WHERE lval = 'a'")
        .await;
    db.refresh_st("mc_join_st").await;
    db.assert_st_matches_query("mc_join_st", q).await;

    // Cycle 3
    db.execute("DELETE FROM mc_right WHERE key = 2").await;
    db.refresh_st("mc_join_st").await;
    db.assert_st_matches_query("mc_join_st", q).await;

    // Cycle 4
    db.execute("INSERT INTO mc_left (key, lval) VALUES (3, 'c')")
        .await;
    db.execute("INSERT INTO mc_right (key, rval) VALUES (3, 'w')")
        .await;
    db.refresh_st("mc_join_st").await;
    db.assert_st_matches_query("mc_join_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-cycle WINDOW
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "DVM: window function differential produces incorrect results across refresh cycles (ROADMAP)"]
async fn test_multi_cycle_window_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE mc_win (id SERIAL PRIMARY KEY, dept TEXT, salary INT)")
        .await;
    db.execute(
        "INSERT INTO mc_win (dept, salary) VALUES \
         ('eng', 100), ('eng', 200), ('sales', 150)",
    )
    .await;

    let q = "SELECT dept, salary, \
             ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn \
             FROM mc_win";
    db.create_st("mc_win_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("mc_win_st", q).await;

    for i in 0..5 {
        db.execute(&format!(
            "INSERT INTO mc_win (dept, salary) VALUES ('eng', {})",
            300 + i * 10
        ))
        .await;
        db.refresh_st("mc_win_st").await;
        db.assert_st_matches_query("mc_win_st", q).await;
    }

    // Delete three rows across cycles
    db.execute("DELETE FROM mc_win WHERE salary = 100").await;
    db.refresh_st("mc_win_st").await;
    db.assert_st_matches_query("mc_win_st", q).await;

    db.execute("DELETE FROM mc_win WHERE salary = 200").await;
    db.refresh_st("mc_win_st").await;
    db.assert_st_matches_query("mc_win_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-cycle with prepared statements (cache survival)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_multi_cycle_prepared_statement_cache() {
    let db = E2eDb::new().await.with_extension().await;
    // Ensure prepared statements are on
    db.execute("SET pg_trickle.use_prepared_statements = on")
        .await;
    db.execute("CREATE TABLE mc_prep (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute("INSERT INTO mc_prep (grp, val) VALUES ('a', 1)")
        .await;

    let q = "SELECT grp, SUM(val) AS total FROM mc_prep GROUP BY grp";
    db.create_st("mc_prep_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("mc_prep_st", q).await;

    // Run enough cycles to trigger generic plan (typically ~5+ executions)
    for i in 2..=8 {
        db.execute(&format!(
            "INSERT INTO mc_prep (grp, val) VALUES ('a', {})",
            i
        ))
        .await;
        db.refresh_st("mc_prep_st").await;
        db.assert_st_matches_query("mc_prep_st", q).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-cycle: group elimination and revival
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_multi_cycle_group_elimination_revival() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE mc_grp (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute("INSERT INTO mc_grp (grp, val) VALUES ('a', 10), ('b', 20)")
        .await;

    let q = "SELECT grp, SUM(val) AS total FROM mc_grp GROUP BY grp";
    db.create_st("mc_grp_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("mc_grp_st", q).await;

    // Eliminate group 'a'
    db.execute("DELETE FROM mc_grp WHERE grp = 'a'").await;
    db.refresh_st("mc_grp_st").await;
    db.assert_st_matches_query("mc_grp_st", q).await;

    // Revive group 'a'
    db.execute("INSERT INTO mc_grp (grp, val) VALUES ('a', 50)")
        .await;
    db.refresh_st("mc_grp_st").await;
    db.assert_st_matches_query("mc_grp_st", q).await;

    // Eliminate again and add new group
    db.execute("DELETE FROM mc_grp WHERE grp = 'a'").await;
    db.execute("INSERT INTO mc_grp (grp, val) VALUES ('c', 99)")
        .await;
    db.refresh_st("mc_grp_st").await;
    db.assert_st_matches_query("mc_grp_st", q).await;
}
