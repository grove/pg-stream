//! E2E tests for partitioned source table support.
//!
//! Validates that pg_trickle works correctly with PostgreSQL's declarative
//! table partitioning: RANGE, LIST, and HASH partitioned source tables,
//! ATTACH PARTITION detection and reinitialize, and WAL publication
//! configuration for partitioned tables.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── PT1: Partitioned source tables work end-to-end ─────────────────────

#[tokio::test]
async fn test_partition_range_full_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a RANGE-partitioned table
    db.execute(
        "CREATE TABLE orders (
            id BIGSERIAL,
            created_at DATE NOT NULL,
            total NUMERIC,
            PRIMARY KEY (id, created_at)
        ) PARTITION BY RANGE (created_at)",
    )
    .await;

    db.execute(
        "CREATE TABLE orders_2025 PARTITION OF orders
            FOR VALUES FROM ('2025-01-01') TO ('2026-01-01')",
    )
    .await;

    db.execute(
        "CREATE TABLE orders_2026 PARTITION OF orders
            FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')",
    )
    .await;

    // Insert data across partitions
    db.execute(
        "INSERT INTO orders (created_at, total) VALUES
         ('2025-06-15', 100.00),
         ('2025-12-01', 200.00),
         ('2026-03-15', 300.00)",
    )
    .await;

    // Create stream table over the partitioned source
    db.create_st(
        "order_totals",
        "SELECT created_at, total FROM orders",
        "1m",
        "FULL",
    )
    .await;

    db.refresh_st("order_totals").await;

    let count: i64 = db.count("order_totals").await;
    assert_eq!(count, 3, "All rows from all partitions should be visible");
}

#[tokio::test]
async fn test_partition_range_differential_refresh() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE diff_orders (
            id BIGSERIAL,
            month DATE NOT NULL,
            amount NUMERIC,
            PRIMARY KEY (id, month)
        ) PARTITION BY RANGE (month)",
    )
    .await;

    db.execute(
        "CREATE TABLE diff_orders_q1 PARTITION OF diff_orders
            FOR VALUES FROM ('2025-01-01') TO ('2025-04-01')",
    )
    .await;

    db.execute(
        "CREATE TABLE diff_orders_q2 PARTITION OF diff_orders
            FOR VALUES FROM ('2025-04-01') TO ('2025-07-01')",
    )
    .await;

    db.execute("INSERT INTO diff_orders (month, amount) VALUES ('2025-02-01', 50.00)")
        .await;

    // Create DIFFERENTIAL stream table
    db.create_st(
        "diff_order_st",
        "SELECT id, month, amount FROM diff_orders",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("diff_order_st").await;
    let count: i64 = db.count("diff_order_st").await;
    assert_eq!(count, 1, "Initial row should be present");

    // Insert into a different partition
    db.execute("INSERT INTO diff_orders (month, amount) VALUES ('2025-05-15', 75.00)")
        .await;

    db.refresh_st("diff_order_st").await;
    let count: i64 = db.count("diff_order_st").await;
    assert_eq!(
        count, 2,
        "Row from second partition should appear after differential refresh"
    );

    // Update across partitions
    db.execute("UPDATE diff_orders SET amount = 99.00 WHERE amount = 50.00")
        .await;

    db.refresh_st("diff_order_st").await;

    let updated: String = db
        .query_scalar("SELECT amount::text FROM diff_order_st WHERE month = '2025-02-01'")
        .await;
    assert_eq!(updated, "99.00", "Update should be reflected after refresh");

    // Delete from a partition
    db.execute("DELETE FROM diff_orders WHERE month = '2025-05-15'")
        .await;

    db.refresh_st("diff_order_st").await;
    let count: i64 = db.count("diff_order_st").await;
    assert_eq!(
        count, 1,
        "Deleted row should be removed after differential refresh"
    );
}

#[tokio::test]
async fn test_partition_list_source() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a LIST-partitioned table
    db.execute(
        "CREATE TABLE events (
            id SERIAL,
            region TEXT NOT NULL,
            payload TEXT,
            PRIMARY KEY (id, region)
        ) PARTITION BY LIST (region)",
    )
    .await;

    db.execute("CREATE TABLE events_us PARTITION OF events FOR VALUES IN ('US')")
        .await;

    db.execute("CREATE TABLE events_eu PARTITION OF events FOR VALUES IN ('EU')")
        .await;

    db.execute("INSERT INTO events (region, payload) VALUES ('US', 'click'), ('EU', 'view')")
        .await;

    db.create_st(
        "event_st",
        "SELECT region, count(*) as cnt FROM events GROUP BY region",
        "1m",
        "FULL",
    )
    .await;

    db.refresh_st("event_st").await;

    let count: i64 = db.count("event_st").await;
    assert_eq!(count, 2, "Both regions should appear in aggregated result");
}

#[tokio::test]
async fn test_partition_hash_source() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a HASH-partitioned table
    db.execute(
        "CREATE TABLE hash_data (
            id INT PRIMARY KEY,
            val TEXT
        ) PARTITION BY HASH (id)",
    )
    .await;

    db.execute(
        "CREATE TABLE hash_data_0 PARTITION OF hash_data
            FOR VALUES WITH (MODULUS 2, REMAINDER 0)",
    )
    .await;

    db.execute(
        "CREATE TABLE hash_data_1 PARTITION OF hash_data
            FOR VALUES WITH (MODULUS 2, REMAINDER 1)",
    )
    .await;

    db.execute("INSERT INTO hash_data VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')")
        .await;

    db.create_st("hash_st", "SELECT id, val FROM hash_data", "1m", "FULL")
        .await;

    db.refresh_st("hash_st").await;

    let count: i64 = db.count("hash_st").await;
    assert_eq!(
        count, 4,
        "All rows across hash partitions should be visible"
    );
}

// ── PT2: ATTACH PARTITION detection ────────────────────────────────────

#[tokio::test]
async fn test_partition_attach_triggers_reinit() {
    let db = E2eDb::new().await.with_extension().await;

    // Create partitioned source with one partition
    db.execute(
        "CREATE TABLE attach_orders (
            id BIGSERIAL,
            created_at DATE NOT NULL,
            total NUMERIC,
            PRIMARY KEY (id, created_at)
        ) PARTITION BY RANGE (created_at)",
    )
    .await;

    db.execute(
        "CREATE TABLE attach_orders_2025 PARTITION OF attach_orders
            FOR VALUES FROM ('2025-01-01') TO ('2026-01-01')",
    )
    .await;

    db.execute("INSERT INTO attach_orders (created_at, total) VALUES ('2025-06-01', 100.00)")
        .await;

    db.create_st(
        "attach_st",
        "SELECT id, created_at, total FROM attach_orders",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("attach_st").await;
    let count: i64 = db.count("attach_st").await;
    assert_eq!(count, 1, "Initial partition data should be present");

    // Create a standalone table with pre-existing data, then attach it
    db.execute(
        "CREATE TABLE attach_orders_2026 (
            id BIGSERIAL,
            created_at DATE NOT NULL,
            total NUMERIC,
            PRIMARY KEY (id, created_at)
        )",
    )
    .await;

    db.execute(
        "INSERT INTO attach_orders_2026 (created_at, total) VALUES
         ('2026-03-01', 200.00),
         ('2026-06-01', 300.00)",
    )
    .await;

    // ATTACH PARTITION — this should trigger reinit detection
    db.execute(
        "ALTER TABLE attach_orders ATTACH PARTITION attach_orders_2026
            FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')",
    )
    .await;

    // Check that the stream table is marked for reinit
    let needs_reinit: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'attach_st'",
        )
        .await;
    assert!(
        needs_reinit,
        "ATTACH PARTITION should mark dependent stream table for reinitialize"
    );

    // Refresh should pick up all data including newly attached partition
    db.refresh_st("attach_st").await;

    let count: i64 = db.count("attach_st").await;
    assert_eq!(
        count, 3,
        "After reinit, all rows including attached partition data should be visible"
    );
}

#[tokio::test]
async fn test_partition_detach_triggers_reinit() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE detach_orders (
            id BIGSERIAL,
            created_at DATE NOT NULL,
            total NUMERIC,
            PRIMARY KEY (id, created_at)
        ) PARTITION BY RANGE (created_at)",
    )
    .await;

    db.execute(
        "CREATE TABLE detach_orders_2025 PARTITION OF detach_orders
            FOR VALUES FROM ('2025-01-01') TO ('2026-01-01')",
    )
    .await;

    db.execute(
        "CREATE TABLE detach_orders_2026 PARTITION OF detach_orders
            FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')",
    )
    .await;

    db.execute(
        "INSERT INTO detach_orders (created_at, total) VALUES
         ('2025-06-01', 100.00),
         ('2026-06-01', 200.00)",
    )
    .await;

    db.create_st(
        "detach_st",
        "SELECT id, created_at, total FROM detach_orders",
        "1m",
        "FULL",
    )
    .await;

    db.refresh_st("detach_st").await;
    let count: i64 = db.count("detach_st").await;
    assert_eq!(count, 2, "Both partitions should contribute data");

    // DETACH a partition
    db.execute("ALTER TABLE detach_orders DETACH PARTITION detach_orders_2026")
        .await;

    // Stream table should be marked for reinit
    let needs_reinit: bool = db
        .query_scalar(
            "SELECT needs_reinit FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'detach_st'",
        )
        .await;
    assert!(
        needs_reinit,
        "DETACH PARTITION should mark dependent stream table for reinitialize"
    );

    // After refresh, only the remaining partition's data should be visible
    db.refresh_st("detach_st").await;

    let count: i64 = db.count("detach_st").await;
    assert_eq!(
        count, 1,
        "After reinit, only remaining partition data should be visible"
    );
}

// ── PT4: Foreign table source restriction ──────────────────────────────

#[tokio::test]
async fn test_foreign_table_full_refresh_works() {
    let db = E2eDb::new().await.with_extension().await;

    // Set up a foreign data wrapper pointing to the same database
    db.execute("CREATE EXTENSION IF NOT EXISTS postgres_fdw")
        .await;

    // Build server options — use 127.0.0.1 for loopback (inet_server_addr()
    // returns the container's external IP which may not be reachable from
    // within the same container).  `user` is a USER MAPPING option, not a
    // SERVER option in postgres_fdw.
    let port: String = db.query_scalar("SELECT inet_server_port()::text").await;
    let dbname: String = db.query_scalar("SELECT current_database()").await;

    db.execute(&format!(
        "CREATE SERVER IF NOT EXISTS loopback FOREIGN DATA WRAPPER postgres_fdw \
         OPTIONS (host '127.0.0.1', port '{port}', dbname '{dbname}')",
    ))
    .await;

    db.execute(&format!(
        "CREATE USER MAPPING IF NOT EXISTS FOR CURRENT_USER SERVER loopback OPTIONS (user '{}')",
        db.query_scalar::<String>("SELECT current_user").await
    ))
    .await;

    // Create a local table, then a foreign table pointing to it
    db.execute("CREATE TABLE fdw_source (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO fdw_source VALUES (1, 'hello'), (2, 'world')")
        .await;

    db.execute(
        "CREATE FOREIGN TABLE fdw_remote (id INT, val TEXT)
         SERVER loopback OPTIONS (table_name 'fdw_source')",
    )
    .await;

    // FULL refresh should work with foreign tables
    db.create_st("fdw_st", "SELECT id, val FROM fdw_remote", "1m", "FULL")
        .await;

    db.refresh_st("fdw_st").await;

    let count: i64 = db.count("fdw_st").await;
    assert_eq!(
        count, 2,
        "Foreign table should be readable via FULL refresh"
    );
}

// ── Partitioned table with aggregation ─────────────────────────────────

#[tokio::test]
async fn test_partition_with_aggregation() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE agg_sales (
            id SERIAL,
            sale_date DATE NOT NULL,
            category TEXT NOT NULL,
            amount NUMERIC,
            PRIMARY KEY (id, sale_date)
        ) PARTITION BY RANGE (sale_date)",
    )
    .await;

    db.execute(
        "CREATE TABLE agg_sales_h1 PARTITION OF agg_sales
            FOR VALUES FROM ('2025-01-01') TO ('2025-07-01')",
    )
    .await;

    db.execute(
        "CREATE TABLE agg_sales_h2 PARTITION OF agg_sales
            FOR VALUES FROM ('2025-07-01') TO ('2026-01-01')",
    )
    .await;

    db.execute(
        "INSERT INTO agg_sales (sale_date, category, amount) VALUES
         ('2025-02-01', 'A', 100), ('2025-03-01', 'A', 200),
         ('2025-08-01', 'B', 300), ('2025-09-01', 'A', 150)",
    )
    .await;

    db.create_st(
        "sales_agg_st",
        "SELECT category, SUM(amount) AS total, COUNT(*) AS cnt FROM agg_sales GROUP BY category",
        "1m",
        "FULL",
    )
    .await;

    db.refresh_st("sales_agg_st").await;

    let a_total: String = db
        .query_scalar("SELECT total::text FROM sales_agg_st WHERE category = 'A'")
        .await;
    assert_eq!(
        a_total, "450",
        "Category A total should span both partitions"
    );

    let b_total: String = db
        .query_scalar("SELECT total::text FROM sales_agg_st WHERE category = 'B'")
        .await;
    assert_eq!(
        b_total, "300",
        "Category B total should be from H2 partition"
    );
}

#[tokio::test]
async fn test_partition_differential_with_aggregation() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE diff_agg (
            id SERIAL,
            month DATE NOT NULL,
            dept TEXT NOT NULL,
            revenue NUMERIC,
            PRIMARY KEY (id, month)
        ) PARTITION BY RANGE (month)",
    )
    .await;

    db.execute(
        "CREATE TABLE diff_agg_q1 PARTITION OF diff_agg
            FOR VALUES FROM ('2025-01-01') TO ('2025-04-01')",
    )
    .await;

    db.execute(
        "CREATE TABLE diff_agg_q2 PARTITION OF diff_agg
            FOR VALUES FROM ('2025-04-01') TO ('2025-07-01')",
    )
    .await;

    db.execute(
        "INSERT INTO diff_agg (month, dept, revenue) VALUES
         ('2025-01-15', 'eng', 1000),
         ('2025-02-15', 'eng', 2000)",
    )
    .await;

    db.create_st(
        "diff_agg_st",
        "SELECT dept, SUM(revenue) AS total FROM diff_agg GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("diff_agg_st").await;

    let eng_total: String = db
        .query_scalar("SELECT total::text FROM diff_agg_st WHERE dept = 'eng'")
        .await;
    assert_eq!(eng_total, "3000");

    // Insert into second partition
    db.execute("INSERT INTO diff_agg (month, dept, revenue) VALUES ('2025-05-15', 'eng', 500)")
        .await;

    db.refresh_st("diff_agg_st").await;

    let eng_total: String = db
        .query_scalar("SELECT total::text FROM diff_agg_st WHERE dept = 'eng'")
        .await;
    assert_eq!(
        eng_total, "3500",
        "Aggregate should include row from second partition"
    );
}
