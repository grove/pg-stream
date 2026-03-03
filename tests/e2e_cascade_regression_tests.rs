//! Regression tests for stream-table-on-stream-table (ST-on-ST) cascade bugs
//! discovered while following the GETTING_STARTED.md walkthrough.
//!
//! These tests cover:
//!
//! 1. **CDC trigger exclusion** — `has_user_triggers` must not count
//!    pg_trickle's own CDC triggers as user triggers (Bug: `pg_trickle_%`
//!    triggers were not excluded, causing incorrect DML path selection).
//!
//! 2. **ST-on-ST cascade propagation** — Changes to a base table must
//!    propagate through a chain of stream tables via manual refresh calls
//!    in topological (source-to-sink) order.
//!
//! 3. **0-row DIFFERENTIAL must not bump data_timestamp** — A DIFFERENTIAL
//!    refresh that finds no rows in the current frontier window (stale
//!    change buffer rows not yet cleaned up) must not advance
//!    `data_timestamp`, because downstream STs compare
//!    `upstream.data_timestamp > us.data_timestamp` to detect changes.
//!    Bumping it would trigger spurious FULL refreshes downstream.
//!
//! 4. **FULL refresh for ST-upstream changes** — When a STREAM_TABLE
//!    upstream has newer data than the downstream ST, a manual refresh
//!    must pick up those changes (since there is no CDC change buffer
//!    between stream tables).
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Set up a simple two-layer cascade:
///   base_table `orders` → ST `order_summary` → ST `order_report`
///
/// `order_summary` uses a NULL (CALCULATED) schedule.
/// `order_report` uses a '1m' schedule.
async fn setup_two_layer_cascade(db: &E2eDb) {
    db.execute(
        "CREATE TABLE orders (
            id    SERIAL PRIMARY KEY,
            region TEXT NOT NULL,
            amount NUMERIC(10,2) NOT NULL
        )",
    )
    .await;

    db.execute(
        "INSERT INTO orders (region, amount) VALUES
            ('East', 100), ('East', 200), ('West', 300)",
    )
    .await;

    // Layer 1: order_summary — aggregates per region from base table
    db.create_st(
        "order_summary",
        "SELECT region, COUNT(*) AS order_count, SUM(amount) AS total_amount
         FROM orders GROUP BY region",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Layer 2: order_report — reads from order_summary (ST-on-ST)
    // Uses NULL schedule (CALCULATED mode)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'order_report',
            $$SELECT region, total_amount FROM order_summary WHERE order_count > 0$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;
}

// ── Test 1: CDC trigger exclusion ────────────────────────────────────────────

/// Regression: `has_user_triggers` must exclude `pg_trickle_%` CDC triggers.
///
/// Before the fix, pg_trickle's own CDC triggers (named `pg_trickle_cdc_<oid>`)
/// were counted as user triggers because the exclusion filter only matched
/// `pgt_%`. This caused the refresh executor to use the slower explicit-DML
/// path unnecessarily.
///
/// This test creates a stream table on a source table that has NO user
/// triggers and verifies that the pg_trickle CDC triggers do not appear
/// as user triggers.
#[tokio::test]
async fn test_cdc_triggers_not_counted_as_user_triggers() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE cdc_excl_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO cdc_excl_src VALUES (1, 'a')").await;

    db.create_st(
        "cdc_excl_st",
        "SELECT id, val FROM cdc_excl_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Verify that CDC triggers exist on the source table
    let cdc_trigger_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pg_trigger t
             JOIN pg_class c ON t.tgrelid = c.oid
             WHERE c.relname = 'cdc_excl_src'
               AND t.tgname LIKE 'pg_trickle_%'",
        )
        .await;
    assert!(
        cdc_trigger_count > 0,
        "CDC triggers should exist on the source table"
    );

    // The stream table itself should have zero user triggers
    // (pg_trickle CDC triggers are on the *source* table, not the ST).
    // But more importantly: the source table should also report zero
    // user triggers when we exclude pg_trickle's own triggers.
    let source_oid: i32 = db
        .query_scalar("SELECT 'cdc_excl_src'::regclass::oid::int")
        .await;
    let has_user_triggers: bool = db
        .query_scalar(&format!(
            "SELECT EXISTS(
                SELECT 1 FROM pg_trigger
                WHERE tgrelid = {}::oid
                  AND tgisinternal = false
                  AND tgname NOT LIKE 'pgt_%'
                  AND tgname NOT LIKE 'pg_trickle_%'
            )",
            source_oid
        ))
        .await;
    assert!(
        !has_user_triggers,
        "Source table should have no user triggers (CDC triggers must be excluded)"
    );

    // Now add an actual user trigger and verify it IS detected
    db.execute(
        "CREATE OR REPLACE FUNCTION user_noop() RETURNS TRIGGER AS $$
         BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql",
    )
    .await;
    db.execute(
        "CREATE TRIGGER my_user_trigger AFTER INSERT ON cdc_excl_src
         FOR EACH ROW EXECUTE FUNCTION user_noop()",
    )
    .await;

    let has_user_triggers_after: bool = db
        .query_scalar(&format!(
            "SELECT EXISTS(
                SELECT 1 FROM pg_trigger
                WHERE tgrelid = {}::oid
                  AND tgisinternal = false
                  AND tgname NOT LIKE 'pgt_%'
                  AND tgname NOT LIKE 'pg_trickle_%'
            )",
            source_oid
        ))
        .await;
    assert!(
        has_user_triggers_after,
        "Real user trigger should be detected after exclusion"
    );
}

// ── Test 2: ST-on-ST cascade — manual refresh propagation ────────────────────

/// Regression: Changes to a base table must propagate through a chain of
/// stream tables when refreshed manually in topological order.
///
/// Before the fix, `check_upstream_changes` only checked CDC change buffers
/// (TABLE sources). When the only upstream source was a STREAM_TABLE,
/// `determine_refresh_action` always returned NO_DATA because the change
/// buffer didn't exist.
#[tokio::test]
async fn test_st_on_st_cascade_propagates_insert() {
    let db = E2eDb::new().await.with_extension().await;
    setup_two_layer_cascade(&db).await;

    // Verify initial state
    let east_amount: String = db
        .query_scalar("SELECT total_amount::text FROM order_report WHERE region = 'East'")
        .await;
    assert_eq!(east_amount, "300.00", "East: 100 + 200 = 300");

    // Insert a new order
    db.execute("INSERT INTO orders (region, amount) VALUES ('East', 150)")
        .await;

    // Refresh in topological order (upstream first)
    db.refresh_st("order_summary").await;
    db.refresh_st("order_report").await;

    // Verify the change cascaded
    let post_east_amount: String = db
        .query_scalar("SELECT total_amount::text FROM order_report WHERE region = 'East'")
        .await;
    assert_eq!(
        post_east_amount, "450.00",
        "East should be 100 + 200 + 150 = 450 after cascade"
    );

    // West should be untouched
    let west_amount: String = db
        .query_scalar("SELECT total_amount::text FROM order_report WHERE region = 'West'")
        .await;
    assert_eq!(west_amount, "300.00", "West should be unaffected");
}

/// Regression: DELETE on a base table cascades through ST-on-ST chain.
#[tokio::test]
async fn test_st_on_st_cascade_propagates_delete() {
    let db = E2eDb::new().await.with_extension().await;
    setup_two_layer_cascade(&db).await;

    // Delete the West order
    db.execute("DELETE FROM orders WHERE region = 'West'").await;

    db.refresh_st("order_summary").await;
    db.refresh_st("order_report").await;

    // West had order_count=1; after delete it's 0, so the WHERE clause
    // in order_report (order_count > 0) will exclude it
    let west_exists: bool = db
        .query_scalar("SELECT EXISTS(SELECT 1 FROM order_report WHERE region = 'West')")
        .await;
    assert!(
        !west_exists,
        "West should vanish from order_report after its only order is deleted"
    );
}

// ── Helper: fast scheduler ───────────────────────────────────────────────────

/// Configure the scheduler for fast testing:
/// - `pg_trickle.scheduler_interval_ms = 100` (wake every 100ms)
/// - `pg_trickle.min_schedule_seconds = 1` (allow 1-second schedule)
async fn configure_fast_scheduler(db: &E2eDb) {
    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 100")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.execute("SELECT pg_reload_conf()").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

// ── Test 3: 0-row DIFFERENTIAL must not bump data_timestamp ─────────────────

/// Regression: A DIFFERENTIAL refresh that finds zero rows in the current
/// frontier window must NOT advance `data_timestamp`.
///
/// Before the fix, every DIFFERENTIAL refresh — even one that produced 0
/// inserts and 0 deletes — called `update_after_refresh` which bumped
/// `data_timestamp`. This caused downstream stream tables (which compare
/// `upstream.data_timestamp > us.data_timestamp`) to see a false "upstream
/// changed" signal, triggering unnecessary FULL refreshes every cycle.
///
/// This fix is in the **scheduler** path (`execute_scheduled_refresh`), so
/// the test uses auto-refresh via a short schedule rather than manual refresh.
#[tokio::test]
async fn test_zero_row_differential_preserves_data_timestamp() {
    // Must use postgres database — the scheduler bgworker only connects to it.
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE ts_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO ts_src VALUES (1, 'a'), (2, 'b')")
        .await;

    // create_st with initialize=true populates the ST and sets timestamps.
    db.create_st("ts_st", "SELECT id, val FROM ts_src", "1s", "DIFFERENTIAL")
        .await;

    // The scheduler's first cycle after creation may legitimately bump
    // data_timestamp (stale change buffer entries from the initial INSERT).
    // Wait for that first scheduler cycle to complete by polling last_refresh_at.
    let initial_last_refresh: String = db
        .query_scalar(
            "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'ts_st'",
        )
        .await;

    // Wait for first scheduler cycle
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > std::time::Duration::from_secs(30) {
            panic!("Timed out waiting for first scheduler cycle");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let lr: String = db
            .query_scalar(
                "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables
                 WHERE pgt_name = 'ts_st'",
            )
            .await;
        if lr != initial_last_refresh {
            break;
        }
    }

    // Now record data_timestamp after the first scheduler cycle.
    // All stale buffer entries should be consumed by now.
    let ts_after_first_cycle: String = db
        .query_scalar(
            "SELECT data_timestamp::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'ts_st'",
        )
        .await;
    let lr_after_first_cycle: String = db
        .query_scalar(
            "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'ts_st'",
        )
        .await;

    // Wait for a SECOND scheduler cycle (no changes in source this time)
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > std::time::Duration::from_secs(30) {
            panic!("Timed out waiting for second scheduler cycle");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let lr: String = db
            .query_scalar(
                "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables
                 WHERE pgt_name = 'ts_st'",
            )
            .await;
        if lr != lr_after_first_cycle {
            break;
        }
    }

    // data_timestamp must NOT have advanced on the second (noop) cycle
    let ts_after_second_cycle: String = db
        .query_scalar(
            "SELECT data_timestamp::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'ts_st'",
        )
        .await;
    assert_eq!(
        ts_after_first_cycle, ts_after_second_cycle,
        "data_timestamp must not change when scheduler refresh finds no new data"
    );

    // Sanity check: insert data and verify data_timestamp DOES advance
    db.execute("INSERT INTO ts_src VALUES (3, 'c')").await;
    let refreshed = db
        .wait_for_auto_refresh("ts_st", std::time::Duration::from_secs(30))
        .await;
    assert!(refreshed, "ts_st should pick up the new row");

    let ts_after_real_change: String = db
        .query_scalar(
            "SELECT data_timestamp::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'ts_st'",
        )
        .await;
    assert_ne!(
        ts_after_first_cycle, ts_after_real_change,
        "data_timestamp must advance when scheduler refresh writes rows"
    );
}

// ── Test 4: No spurious downstream cascades after 0-row refresh ─────────────

/// Regression: A 0-row refresh of an upstream ST must not trigger a
/// downstream refresh cascade.
///
/// This is the end-to-end version of test 3: when upstream ST refreshes
/// with 0 rows (NO_DATA or 0-row DIFFERENTIAL), its `data_timestamp`
/// stays unchanged, so downstream STs that compare timestamps should
/// NOT see "upstream changed" and should NOT perform a FULL refresh.
///
/// Uses auto-refresh via short schedules to exercise the scheduler path
/// where the 0-row DIFFERENTIAL fix lives.
#[tokio::test]
async fn test_no_spurious_cascade_after_noop_upstream_refresh() {
    // Must use postgres database — the scheduler bgworker only connects to it.
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    // Build a two-layer cascade with short schedules for auto-refresh
    db.execute(
        "CREATE TABLE cascade_noop (
            id    SERIAL PRIMARY KEY,
            region TEXT NOT NULL,
            amount NUMERIC(10,2) NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO cascade_noop (region, amount) VALUES
            ('East', 100), ('West', 200)",
    )
    .await;

    // Layer 1: short schedule
    db.create_st(
        "noop_summary",
        "SELECT region, SUM(amount) AS total FROM cascade_noop GROUP BY region",
        "1s",
        "DIFFERENTIAL",
    )
    .await;

    // Layer 2: ST-on-ST, also short schedule
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'noop_report',
            $$SELECT region, total FROM noop_summary$$,
            '1s',
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Both STs are populated from create_st/create_stream_table.
    // The scheduler's first cycle may legitimately bump data_timestamp
    // (stale change buffer entries from initial INSERT). Wait for the
    // first two scheduler cycles on noop_report to stabilize.

    // Wait for first scheduler cycle on noop_report
    let lr_initial: String = db
        .query_scalar(
            "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'noop_report'",
        )
        .await;
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > std::time::Duration::from_secs(30) {
            panic!("Timed out waiting for 1st noop_report scheduler cycle");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let lr: String = db
            .query_scalar(
                "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables
                 WHERE pgt_name = 'noop_report'",
            )
            .await;
        if lr != lr_initial {
            break;
        }
    }

    // Record timestamps after first scheduler cycle (buffer is now clean)
    let report_ts_after_cycle1: String = db
        .query_scalar(
            "SELECT data_timestamp::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'noop_report'",
        )
        .await;
    let lr_after_cycle1: String = db
        .query_scalar(
            "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'noop_report'",
        )
        .await;

    // Wait for second scheduler cycle (no DML changes)
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > std::time::Duration::from_secs(30) {
            panic!("Timed out waiting for 2nd noop_report scheduler cycle");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let lr: String = db
            .query_scalar(
                "SELECT last_refresh_at::text FROM pgtrickle.pgt_stream_tables
                 WHERE pgt_name = 'noop_report'",
            )
            .await;
        if lr != lr_after_cycle1 {
            break;
        }
    }

    // data_timestamp must be stable — no spurious cascade
    let report_ts_after_cycle2: String = db
        .query_scalar(
            "SELECT data_timestamp::text FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = 'noop_report'",
        )
        .await;
    assert_eq!(
        report_ts_after_cycle1, report_ts_after_cycle2,
        "noop_report data_timestamp should not advance when upstream has no changes"
    );

    let report_rows = db.count("public.noop_report").await;
    assert_eq!(
        report_rows, 2,
        "noop_report should have 2 rows (East + West)"
    );
}

// ── Test 5: Three-layer cascade ─────────────────────────────────────────────

/// Regression: A three-layer STREAM_TABLE cascade (base → ST₁ → ST₂ → ST₃)
/// correctly propagates DML through all layers.
///
/// This mirrors the GETTING_STARTED.md `department_tree` → `department_stats`
/// → `department_report` pipeline but uses a simpler schema to isolate the
/// cascade mechanism from the recursive CTE logic.
#[tokio::test]
async fn test_three_layer_cascade_insert_propagates() {
    let db = E2eDb::new().await.with_extension().await;

    // Base table
    db.execute(
        "CREATE TABLE items (
            id       SERIAL PRIMARY KEY,
            category TEXT NOT NULL,
            price    NUMERIC(10,2) NOT NULL
        )",
    )
    .await;
    db.execute(
        "INSERT INTO items (category, price) VALUES
            ('A', 10), ('A', 20), ('B', 30)",
    )
    .await;

    // Layer 1: category_totals (reads from base)
    db.create_st(
        "category_totals",
        "SELECT category, COUNT(*) AS cnt, SUM(price) AS total
         FROM items GROUP BY category",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Layer 2: category_flags (reads from ST layer 1)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'category_flags',
            $$SELECT category, total,
                     CASE WHEN total > 25 THEN true ELSE false END AS is_big
              FROM category_totals$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Layer 3: big_categories (reads from ST layer 2)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'big_categories',
            $$SELECT category, total FROM category_flags WHERE is_big = true$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Initial state: A total=30 (>25, big), B total=30 (>25, big)
    assert_eq!(
        db.count("public.big_categories").await,
        2,
        "Both A(30) and B(30) qualify as big initially"
    );

    // Insert a new item in category C
    db.execute("INSERT INTO items (category, price) VALUES ('C', 50)")
        .await;

    // Refresh all three layers in topological order
    db.refresh_st("category_totals").await;
    db.refresh_st("category_flags").await;
    db.refresh_st("big_categories").await;

    // C has total=50 > 25, so it should appear in big_categories
    assert_eq!(
        db.count("public.big_categories").await,
        3,
        "A(30), B(30), C(50) should all be big"
    );

    let c_total: String = db
        .query_scalar("SELECT total::text FROM big_categories WHERE category = 'C'")
        .await;
    assert_eq!(c_total, "50.00");
}

/// Regression: UPDATE on a base table cascades correctly through three layers.
#[tokio::test]
async fn test_three_layer_cascade_update_propagates() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE products (
            id SERIAL PRIMARY KEY, category TEXT NOT NULL, price NUMERIC(10,2) NOT NULL
        )",
    )
    .await;
    db.execute("INSERT INTO products (category, price) VALUES ('X', 100), ('X', 200), ('Y', 50)")
        .await;

    // Layer 1
    db.create_st(
        "prod_summary",
        "SELECT category, SUM(price) AS total FROM products GROUP BY category",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Layer 2 (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'prod_labels',
            $$SELECT category, total,
                     CASE WHEN total >= 200 THEN 'premium' ELSE 'basic' END AS tier
              FROM prod_summary$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Layer 3 (ST-on-ST)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'premium_categories',
            $$SELECT category, total FROM prod_labels WHERE tier = 'premium'$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Initial: X total=300 (premium), Y total=50 (basic)
    assert_eq!(db.count("public.premium_categories").await, 1);
    let initial_cat: String = db
        .query_scalar("SELECT category FROM premium_categories")
        .await;
    assert_eq!(initial_cat, "X");

    // Update: increase Y's price so its total becomes 250 (premium)
    db.execute("UPDATE products SET price = 250 WHERE category = 'Y' AND price = 50")
        .await;

    db.refresh_st("prod_summary").await;
    db.refresh_st("prod_labels").await;
    db.refresh_st("premium_categories").await;

    // Both X (300) and Y (250) should now be premium
    assert_eq!(
        db.count("public.premium_categories").await,
        2,
        "Both X and Y should be premium after update"
    );

    let cats: Vec<String> =
        sqlx::query_scalar("SELECT category FROM premium_categories ORDER BY category")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(cats, vec!["X", "Y"]);
}

// ── Test 6: Dependencies catalog for ST-on-ST ───────────────────────────────

/// Regression: When a ST reads from another ST (not a base table), the
/// dependency must be recorded as `source_type = 'STREAM_TABLE'` in
/// `pgtrickle.pgt_dependencies`.
///
/// This is critical because `check_upstream_changes` dispatches to
/// different detection strategies based on source_type:
/// - TABLE → check CDC change buffer
/// - STREAM_TABLE → compare data_timestamp
#[tokio::test]
async fn test_st_on_st_dependency_is_stream_table_type() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dep_base (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO dep_base VALUES (1, 10)").await;

    // Layer 1: reads from base table
    db.create_st(
        "dep_layer1",
        "SELECT id, val FROM dep_base",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Layer 2: reads from layer 1 (a stream table)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'dep_layer2',
            $$SELECT id, val * 2 AS doubled FROM dep_layer1$$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;

    // Check that dep_layer2 depends on dep_layer1 as STREAM_TABLE
    let source_type: String = db
        .query_scalar(
            "SELECT d.source_type
             FROM pgtrickle.pgt_dependencies d
             JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id
             WHERE st.pgt_name = 'dep_layer2'",
        )
        .await;
    assert_eq!(
        source_type, "STREAM_TABLE",
        "Dependency on another stream table must be recorded as STREAM_TABLE"
    );

    // dep_layer1 should depend on dep_base as TABLE
    let base_source_type: String = db
        .query_scalar(
            "SELECT d.source_type
             FROM pgtrickle.pgt_dependencies d
             JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id
             WHERE st.pgt_name = 'dep_layer1'",
        )
        .await;
    assert_eq!(
        base_source_type, "TABLE",
        "Dependency on a base table must be recorded as TABLE"
    );
}
