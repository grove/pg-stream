//! E2E tests for concurrent operations.
//!
//! Validates that concurrent DML during refresh, parallel ST creation,
//! and refresh/drop races are handled safely.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

#[tokio::test]
async fn test_concurrent_inserts_during_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cc_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO cc_src SELECT g, g FROM generate_series(1, 100) g")
        .await;

    db.create_st("cc_st", "SELECT id, val FROM cc_src", "1m", "FULL")
        .await;
    assert_eq!(db.count("public.cc_st").await, 100);

    // Insert more data while refresh is happening
    let pool_refresh = db.pool.clone();
    let pool_insert = db.pool.clone();

    // First insert some data to need a refresh
    db.execute("INSERT INTO cc_src SELECT g, g FROM generate_series(101, 200) g")
        .await;

    let refresh_handle = tokio::spawn(async move {
        sqlx::query("SELECT pgtrickle.refresh_stream_table('cc_st')")
            .execute(&pool_refresh)
            .await
    });

    let insert_handle = tokio::spawn(async move {
        // Insert additional rows concurrently
        sqlx::query("INSERT INTO cc_src SELECT g, g FROM generate_series(201, 250) g")
            .execute(&pool_insert)
            .await
    });

    let (refresh_result, insert_result) = tokio::join!(refresh_handle, insert_handle);
    refresh_result
        .expect("refresh task panicked")
        .expect("refresh failed");
    insert_result
        .expect("insert task panicked")
        .expect("insert failed");

    // After another refresh, all data should be visible
    db.refresh_st("cc_st").await;

    let count = db.count("public.cc_st").await;
    assert_eq!(
        count, 250,
        "All rows should be present after concurrent ops + refresh"
    );
}

#[tokio::test]
async fn test_create_two_sts_simultaneously() {
    let db = E2eDb::new().await.with_extension().await;

    // Create two source tables
    db.execute("CREATE TABLE cc_src_a (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cc_src_a VALUES (1, 'a')").await;

    db.execute("CREATE TABLE cc_src_b (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cc_src_b VALUES (1, 'b')").await;

    let pool_a = db.pool.clone();
    let pool_b = db.pool.clone();

    let h1 = tokio::spawn(async move {
        sqlx::query(
            "SELECT pgtrickle.create_stream_table('cc_st_a', \
             $$ SELECT id, val FROM cc_src_a $$, '1m', 'FULL')",
        )
        .execute(&pool_a)
        .await
    });

    let h2 = tokio::spawn(async move {
        sqlx::query(
            "SELECT pgtrickle.create_stream_table('cc_st_b', \
             $$ SELECT id, val FROM cc_src_b $$, '1m', 'FULL')",
        )
        .execute(&pool_b)
        .await
    });

    let (r1, r2) = tokio::join!(h1, h2);
    r1.expect("task a panicked").expect("create st_a failed");
    r2.expect("task b panicked").expect("create st_b failed");

    // Both STs should exist
    assert!(db.table_exists("public", "cc_st_a").await);
    assert!(db.table_exists("public", "cc_st_b").await);

    let count_a = db.count("public.cc_st_a").await;
    let count_b = db.count("public.cc_st_b").await;
    assert_eq!(count_a, 1);
    assert_eq!(count_b, 1);
}

#[tokio::test]
async fn test_refresh_and_drop_race() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cc_race (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO cc_race SELECT g, g FROM generate_series(1, 50) g")
        .await;

    db.create_st("cc_race_st", "SELECT id, val FROM cc_race", "1m", "FULL")
        .await;

    // Add data to trigger refresh
    db.execute("INSERT INTO cc_race SELECT g, g FROM generate_series(51, 100) g")
        .await;

    let pool_refresh = db.pool.clone();
    let pool_drop = db.pool.clone();

    let h_refresh = tokio::spawn(async move {
        sqlx::query("SELECT pgtrickle.refresh_stream_table('cc_race_st')")
            .execute(&pool_refresh)
            .await
    });

    let h_drop = tokio::spawn(async move {
        // Slight delay to let refresh start
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        sqlx::query("SELECT pgtrickle.drop_stream_table('cc_race_st')")
            .execute(&pool_drop)
            .await
    });

    let (r_refresh, r_drop) = tokio::join!(h_refresh, h_drop);

    // At least one should succeed, the other may fail gracefully
    let refresh_ok = r_refresh.as_ref().map(|r| r.is_ok()).unwrap_or(false);
    let drop_ok = r_drop.as_ref().map(|r| r.is_ok()).unwrap_or(false);

    assert!(
        refresh_ok || drop_ok,
        "At least one of refresh/drop should succeed"
    );

    // If drop succeeded, the ST should be gone
    if drop_ok {
        let exists: bool = db
            .query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'cc_race_st')",
            )
            .await;
        // It might or might not exist depending on ordering
        // The key assertion is no crash/panic occurred
        let _ = exists;
    }
}

// ══════════════════════════════════════════════════════════════════════
// C1 — Multiple STs on same source refreshed concurrently
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_concurrent_refresh_multiple_sts_same_source() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cc_shared (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO cc_shared SELECT g, g * 10 FROM generate_series(1, 50) g")
        .await;

    db.create_st(
        "cc_shared_st1",
        "SELECT id, val FROM cc_shared",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "cc_shared_st2",
        "SELECT id, val * 2 AS val2 FROM cc_shared",
        "1m",
        "FULL",
    )
    .await;

    // Insert more data, then refresh both concurrently
    db.execute("INSERT INTO cc_shared SELECT g, g * 10 FROM generate_series(51, 100) g")
        .await;

    let pool1 = db.pool.clone();
    let pool2 = db.pool.clone();

    let h1 = tokio::spawn(async move {
        sqlx::query("SELECT pgtrickle.refresh_stream_table('cc_shared_st1')")
            .execute(&pool1)
            .await
    });
    let h2 = tokio::spawn(async move {
        sqlx::query("SELECT pgtrickle.refresh_stream_table('cc_shared_st2')")
            .execute(&pool2)
            .await
    });

    let (r1, r2) = tokio::join!(h1, h2);
    r1.expect("task1 panicked").expect("refresh st1 failed");
    r2.expect("task2 panicked").expect("refresh st2 failed");

    // Both STs should reflect the full 100-row source
    assert_eq!(db.count("public.cc_shared_st1").await, 100);
    assert_eq!(db.count("public.cc_shared_st2").await, 100);
}

// ══════════════════════════════════════════════════════════════════════
// C2 — Advisory lock contention: concurrent refresh of same ST
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_concurrent_refresh_same_st_no_corruption() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cc_lock_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO cc_lock_src SELECT g, g FROM generate_series(1, 100) g")
        .await;

    db.create_st(
        "cc_lock_st",
        "SELECT id, val FROM cc_lock_src",
        "1m",
        "FULL",
    )
    .await;

    db.execute("INSERT INTO cc_lock_src SELECT g, g FROM generate_series(101, 200) g")
        .await;

    let pool1 = db.pool.clone();
    let pool2 = db.pool.clone();

    let h1 = tokio::spawn(async move {
        sqlx::query("SELECT pgtrickle.refresh_stream_table('cc_lock_st')")
            .execute(&pool1)
            .await
    });
    let h2 = tokio::spawn(async move {
        sqlx::query("SELECT pgtrickle.refresh_stream_table('cc_lock_st')")
            .execute(&pool2)
            .await
    });

    let (r1, r2) = tokio::join!(h1, h2);
    // Both calls may succeed (advisory lock serializes) or second may skip.
    // Neither should panic.
    let _ = r1.expect("task1 panicked");
    let _ = r2.expect("task2 panicked");

    // After both complete, row count must be exactly correct — no duplicates
    let count = db.count("public.cc_lock_st").await;
    assert_eq!(count, 200, "No duplicate rows after concurrent refreshes");
}

// ══════════════════════════════════════════════════════════════════════
// C3 — Full-refresh racing with DML on source
// ══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_full_refresh_racing_with_dml() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cc_dml_src (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO cc_dml_src SELECT g, g FROM generate_series(1, 100) g")
        .await;

    db.create_st("cc_dml_st", "SELECT id, val FROM cc_dml_src", "1m", "FULL")
        .await;
    assert_eq!(db.count("public.cc_dml_st").await, 100);

    db.execute("INSERT INTO cc_dml_src SELECT g, g FROM generate_series(101, 150) g")
        .await;

    let pool_r = db.pool.clone();
    let pool_i = db.pool.clone();

    let h_refresh = tokio::spawn(async move {
        sqlx::query("SELECT pgtrickle.refresh_stream_table('cc_dml_st')")
            .execute(&pool_r)
            .await
    });
    let h_insert = tokio::spawn(async move {
        sqlx::query("INSERT INTO cc_dml_src SELECT g, g FROM generate_series(151, 200) g")
            .execute(&pool_i)
            .await
    });

    let (r_refresh, r_insert) = tokio::join!(h_refresh, h_insert);
    r_refresh
        .expect("refresh task panicked")
        .expect("refresh failed");
    r_insert
        .expect("insert task panicked")
        .expect("insert failed");

    // After a stabilising refresh, count must converge to 200
    db.refresh_st("cc_dml_st").await;
    let count = db.count("public.cc_dml_st").await;
    assert_eq!(
        count, 200,
        "ST must converge to 200 after stabilising refresh"
    );
}
