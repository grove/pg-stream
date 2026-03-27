//! G12-SQL-IN: Multi-column `IN (subquery)` correctness tests.
//!
//! Verifies that:
//! 1. Single-column IN (subquery) works correctly with DIFFERENTIAL mode
//! 2. Multi-column IN (subquery) is detected and returns a structured error
//!    with a rewrite suggestion to EXISTS

mod e2e;

use e2e::E2eDb;

// ── Single-column IN: positive correctness test ────────────────────────

#[tokio::test]
async fn test_single_column_in_subquery_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sc_orders (id INT PRIMARY KEY, cust_id INT NOT NULL, amount INT)")
        .await;
    db.execute("CREATE TABLE sc_vip_customers (id INT PRIMARY KEY)")
        .await;

    db.execute("INSERT INTO sc_orders VALUES (1, 10, 100), (2, 20, 200), (3, 30, 300)")
        .await;
    db.execute("INSERT INTO sc_vip_customers VALUES (10), (30)")
        .await;

    // Single-column IN subquery — should work
    db.execute(
        "SELECT pgtrickle.create_stream_table(\
            'sc_in_st',\
            $$SELECT id, cust_id, amount FROM sc_orders \
              WHERE cust_id IN (SELECT id FROM sc_vip_customers)$$,\
            '24h', 'DIFFERENTIAL'\
         )",
    )
    .await;

    // Should contain orders for VIP customers only
    let count: i64 = db.count("public.sc_in_st").await;
    assert_eq!(count, 2, "Expected 2 VIP orders");

    // Insert a new VIP customer
    db.execute("INSERT INTO sc_vip_customers VALUES (20)").await;
    db.execute("SELECT pgtrickle.refresh_stream_table('sc_in_st')")
        .await;

    let count: i64 = db.count("public.sc_in_st").await;
    assert_eq!(count, 3, "Expected 3 VIP orders after adding customer 20");

    // Delete a VIP customer
    db.execute("DELETE FROM sc_vip_customers WHERE id = 10")
        .await;
    db.execute("SELECT pgtrickle.refresh_stream_table('sc_in_st')")
        .await;

    let count: i64 = db.count("public.sc_in_st").await;
    assert_eq!(count, 2, "Expected 2 VIP orders after removing customer 10");

    // Verify multiset equality
    db.assert_st_matches_query(
        "public.sc_in_st",
        "SELECT id, cust_id, amount FROM sc_orders WHERE cust_id IN (SELECT id FROM sc_vip_customers)",
    )
    .await;
}

// ── Multi-column IN: structured error test ─────────────────────────────

#[tokio::test]
async fn test_multi_column_in_subquery_returns_error() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE mc_orders (\
            id INT PRIMARY KEY, \
            cust_id INT NOT NULL, \
            region_id INT NOT NULL, \
            amount INT\
         )",
    )
    .await;
    db.execute(
        "CREATE TABLE mc_regions (\
            cust_id INT NOT NULL, \
            region_id INT NOT NULL, \
            PRIMARY KEY (cust_id, region_id)\
         )",
    )
    .await;

    db.execute("INSERT INTO mc_orders VALUES (1, 10, 1, 100), (2, 20, 2, 200)")
        .await;
    db.execute("INSERT INTO mc_regions VALUES (10, 1), (20, 2)")
        .await;

    // Multi-column IN subquery — should be rejected with a clear error
    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table(\
                'mc_in_st',\
                $$SELECT id, amount FROM mc_orders \
                  WHERE (cust_id, region_id) IN \
                        (SELECT cust_id, region_id FROM mc_regions)$$,\
                '24h', 'DIFFERENTIAL'\
             )",
        )
        .await;

    let err = result
        .expect_err("Multi-column IN should be rejected")
        .to_string();
    assert!(
        err.contains("multi-column IN") || err.contains("not supported"),
        "Expected structured error about multi-column IN, got: {err}"
    );
    assert!(
        err.contains("EXISTS"),
        "Error should suggest EXISTS rewrite, got: {err}"
    );
}

// ── Multi-column IN rewritten as EXISTS: positive test ─────────────────

#[tokio::test]
async fn test_multi_column_rewritten_as_exists_works() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE ex_orders (\
            id INT PRIMARY KEY, \
            cust_id INT NOT NULL, \
            region_id INT NOT NULL, \
            amount INT\
         )",
    )
    .await;
    db.execute(
        "CREATE TABLE ex_regions (\
            cust_id INT NOT NULL, \
            region_id INT NOT NULL, \
            PRIMARY KEY (cust_id, region_id)\
         )",
    )
    .await;

    db.execute("INSERT INTO ex_orders VALUES (1, 10, 1, 100), (2, 20, 2, 200), (3, 30, 3, 300)")
        .await;
    db.execute("INSERT INTO ex_regions VALUES (10, 1), (30, 3)")
        .await;

    // The recommended rewrite using EXISTS works
    db.execute(
        "SELECT pgtrickle.create_stream_table(\
            'ex_in_st',\
            $$SELECT id, amount FROM ex_orders o \
              WHERE EXISTS (\
                  SELECT 1 FROM ex_regions r \
                  WHERE r.cust_id = o.cust_id AND r.region_id = o.region_id\
              )$$,\
            '24h', 'DIFFERENTIAL'\
         )",
    )
    .await;

    let count: i64 = db.count("public.ex_in_st").await;
    assert_eq!(count, 2, "Expected 2 matching orders");

    // Add a new region match
    db.execute("INSERT INTO ex_regions VALUES (20, 2)").await;
    db.execute("SELECT pgtrickle.refresh_stream_table('ex_in_st')")
        .await;

    let count: i64 = db.count("public.ex_in_st").await;
    assert_eq!(count, 3, "Expected 3 matching orders after adding region");

    db.assert_st_matches_query(
        "public.ex_in_st",
        "SELECT id, amount FROM ex_orders o \
         WHERE EXISTS (SELECT 1 FROM ex_regions r WHERE r.cust_id = o.cust_id AND r.region_id = o.region_id)",
    )
    .await;
}
