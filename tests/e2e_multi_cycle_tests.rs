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

#[tokio::test]
async fn test_prepared_statements_cleared_after_cache_invalidation() {
    let db = E2eDb::new().await.with_extension().await;

    let (client, connection) =
        tokio_postgres::connect(db.connection_string(), tokio_postgres::NoTls)
            .await
            .expect("Failed to open dedicated test session");
    let connection_task = tokio::spawn(async move {
        if let Err(error) = connection.await {
            panic!("Dedicated test session failed: {error}");
        }
    });

    client
        .batch_execute(
            "SET pg_trickle.use_prepared_statements = on;
             CREATE TABLE mc_prep_invalidate (id SERIAL PRIMARY KEY, grp TEXT, val INT);
             INSERT INTO mc_prep_invalidate (grp, val) VALUES ('a', 1);",
        )
        .await
        .expect("Failed to set up prepared-statement invalidation test");

    let q = "SELECT grp, SUM(val) AS total FROM mc_prep_invalidate GROUP BY grp";
    client
        .execute(
            "SELECT pgtrickle.create_stream_table($1, $2, schedule => '1m', refresh_mode => 'DIFFERENTIAL')",
            &[&"mc_prep_invalidate_st", &q],
        )
        .await
        .expect("Failed to create stream table");

    client
        .batch_execute(
            "INSERT INTO mc_prep_invalidate (grp, val) VALUES ('a', 2);
             SELECT pgtrickle.refresh_stream_table('mc_prep_invalidate_st');
             INSERT INTO mc_prep_invalidate (grp, val) VALUES ('a', 4);
             SELECT pgtrickle.refresh_stream_table('mc_prep_invalidate_st');",
        )
        .await
        .expect("Failed to warm prepared MERGE statement");

    let prepared_count_before: i64 = client
        .query_one(
            "SELECT count(*) FROM pg_prepared_statements WHERE name LIKE '__pgt_merge_%'",
            &[],
        )
        .await
        .expect("Failed to inspect prepared statements before invalidation")
        .get(0);
    assert!(
        prepared_count_before >= 1,
        "Expected prepared MERGE statement before invalidation, found {}",
        prepared_count_before
    );

    client
        .batch_execute(
            "SELECT pgtrickle.alter_stream_table('mc_prep_invalidate_st', schedule => '2m');
             INSERT INTO mc_prep_invalidate (grp, val) VALUES ('a', 3);
             SELECT pgtrickle.refresh_stream_table('mc_prep_invalidate_st');",
        )
        .await
        .expect("Failed to invalidate cache and refresh stream table");

    let st_total: i64 = client
        .query_one(
            "SELECT total FROM mc_prep_invalidate_st WHERE grp = 'a'",
            &[],
        )
        .await
        .expect("Failed to query refreshed stream table")
        .get(0);
    assert_eq!(
        st_total, 10,
        "Stream table should reflect the post-invalidation refresh"
    );

    let prepared_count_after: i64 = client
        .query_one(
            "SELECT count(*) FROM pg_prepared_statements WHERE name LIKE '__pgt_merge_%'",
            &[],
        )
        .await
        .expect("Failed to inspect prepared statements after invalidation")
        .get(0);
    assert_eq!(
        prepared_count_after, 0,
        "Prepared MERGE statements should be deallocated after cache invalidation, found {}",
        prepared_count_after
    );

    drop(client);
    connection_task
        .await
        .expect("Dedicated session task failed");
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

// ═══════════════════════════════════════════════════════════════════════
// EC-16: Function body change detection via pg_proc hash polling
// ═══════════════════════════════════════════════════════════════════════

/// EC-16: Replacing a function used in a stream table's defining query
/// triggers `needs_reinit = true` on the next differential refresh, so the
/// scheduler will force a full reinitialization with the new function logic.
#[tokio::test]
async fn test_ec16_function_body_change_marks_reinit() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a helper function: doubles the value
    db.execute(
        "CREATE FUNCTION ec16_calc(v INT) RETURNS INT LANGUAGE SQL IMMUTABLE AS $$ SELECT v * 2 $$",
    )
    .await;

    db.execute("CREATE TABLE ec16_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO ec16_src VALUES (1, 10), (2, 20)")
        .await;

    db.create_st(
        "ec16_fn_st",
        "SELECT id, ec16_calc(val) AS computed FROM ec16_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial populate should use v*2
    let sum: i64 = db
        .query_scalar("SELECT SUM(computed) FROM public.ec16_fn_st")
        .await;
    assert_eq!(sum, 60, "Initial: 10*2 + 20*2 = 60");

    // First differential refresh — establishes function hash baseline
    db.execute("INSERT INTO ec16_src VALUES (3, 5)").await;
    db.refresh_st("ec16_fn_st").await;

    let sum2: i64 = db
        .query_scalar("SELECT SUM(computed) FROM public.ec16_fn_st")
        .await;
    assert_eq!(sum2, 70, "After insert: 60 + 5*2 = 70");

    // Verify needs_reinit is false before function change
    let reinit_before: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ec16_fn_st'",
        )
        .await;
    assert!(
        !reinit_before,
        "needs_reinit should be false before function change"
    );

    // Replace the function: now triples instead of doubling
    db.execute(
        "CREATE OR REPLACE FUNCTION ec16_calc(v INT) RETURNS INT LANGUAGE SQL IMMUTABLE AS $$ SELECT v * 3 $$",
    )
    .await;

    // Insert data + refresh — should detect function hash change
    db.execute("INSERT INTO ec16_src VALUES (4, 1)").await;
    db.refresh_st("ec16_fn_st").await;

    // The refresh should have set needs_reinit = true
    let reinit_after: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ec16_fn_st'",
        )
        .await;
    assert!(
        reinit_after,
        "needs_reinit should be true after function body change (EC-16)"
    );
}

/// EC-16: After function change detection marks needs_reinit, a subsequent
/// full refresh produces correct results using the new function logic.
#[tokio::test]
async fn test_ec16_function_change_full_refresh_recovery() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a helper function: adds 100
    db.execute(
        "CREATE FUNCTION ec16r_calc(v INT) RETURNS INT LANGUAGE SQL IMMUTABLE AS $$ SELECT v + 100 $$",
    )
    .await;

    db.execute("CREATE TABLE ec16r_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO ec16r_src VALUES (1, 5), (2, 10)")
        .await;

    db.create_st(
        "ec16r_fn_st",
        "SELECT id, ec16r_calc(val) AS computed FROM ec16r_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial: 5+100=105, 10+100=110
    let sum_initial: i64 = db
        .query_scalar("SELECT SUM(computed) FROM public.ec16r_fn_st")
        .await;
    assert_eq!(sum_initial, 215, "Initial: (5+100) + (10+100) = 215");

    // Establish function hash baseline via a differential refresh
    db.execute("INSERT INTO ec16r_src VALUES (3, 20)").await;
    db.refresh_st("ec16r_fn_st").await;

    // Replace function: now adds 200 instead of 100
    db.execute(
        "CREATE OR REPLACE FUNCTION ec16r_calc(v INT) RETURNS INT LANGUAGE SQL IMMUTABLE AS $$ SELECT v + 200 $$",
    )
    .await;

    // Trigger differential refresh — detects hash change, marks needs_reinit
    db.execute("INSERT INTO ec16r_src VALUES (4, 1)").await;
    db.refresh_st("ec16r_fn_st").await;

    let reinit: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ec16r_fn_st'",
        )
        .await;
    assert!(reinit, "needs_reinit should be true after function change");

    // Simulate what the scheduler would do: alter to FULL mode + refresh
    // (In production, the scheduler calls execute_full_refresh which clears
    // needs_reinit. For testing, we do a manual full_refresh_stream_table.)
    db.execute("SELECT pgtrickle.alter_stream_table('ec16r_fn_st', refresh_mode => 'FULL')")
        .await;
    db.refresh_st("ec16r_fn_st").await;

    // After full refresh with new function (v+200):
    // id=1: 5+200=205, id=2: 10+200=210, id=3: 20+200=220, id=4: 1+200=201
    let sum_after: i64 = db
        .query_scalar("SELECT SUM(computed) FROM public.ec16r_fn_st")
        .await;
    assert_eq!(
        sum_after, 836,
        "After full refresh with new function: 205+210+220+201 = 836"
    );
}

/// EC-16: A stream table whose defining query uses no user-defined functions
/// should not be affected by the hash polling mechanism.
#[tokio::test]
async fn test_ec16_no_functions_unaffected() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ec16n_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO ec16n_src VALUES (1, 10)").await;

    db.create_st(
        "ec16n_st",
        "SELECT id, val FROM ec16n_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Multiple refresh cycles — should never trigger needs_reinit
    db.execute("INSERT INTO ec16n_src VALUES (2, 20)").await;
    db.refresh_st("ec16n_st").await;

    db.execute("INSERT INTO ec16n_src VALUES (3, 30)").await;
    db.refresh_st("ec16n_st").await;

    let reinit: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'ec16n_st'",
        )
        .await;
    assert!(
        !reinit,
        "Stream table without functions should never have needs_reinit set by hash polling"
    );
    assert_eq!(db.count("public.ec16n_st").await, 3);
}
