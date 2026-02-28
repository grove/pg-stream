//! E2E tests for full lifecycle scenarios.
//!
//! Validates end-to-end workflows: create → mutate → refresh → drop,
//! multiple STs on same source, chained STs, edge cases with data types,
//! wide tables, and NULLs.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Full Lifecycle ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_full_lifecycle_create_mutate_refresh_drop() {
    let db = E2eDb::new().await.with_extension().await;

    // Create source and ST
    db.execute("CREATE TABLE lc_src (id INT PRIMARY KEY, name TEXT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO lc_src VALUES (1, 'Alice', 100), (2, 'Bob', 200)")
        .await;

    db.create_st(
        "lc_st",
        "SELECT id, name, amount FROM lc_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Verify initial data
    assert_eq!(db.count("public.lc_st").await, 2);

    // Mutate source
    db.execute("INSERT INTO lc_src VALUES (3, 'Charlie', 300)")
        .await;
    db.execute("UPDATE lc_src SET amount = 150 WHERE id = 2")
        .await;
    db.execute("DELETE FROM lc_src WHERE id = 1").await;

    // Refresh
    db.refresh_st("lc_st").await;

    // Verify updated data
    assert_eq!(db.count("public.lc_st").await, 2);
    db.assert_st_matches_query("public.lc_st", "SELECT id, name, amount FROM lc_src")
        .await;

    // Drop
    db.drop_st("lc_st").await;

    // Verify cleanup
    assert!(!db.table_exists("public", "lc_st").await);
    let cat_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE pgt_name = 'lc_st'")
        .await;
    assert_eq!(cat_count, 0);
}

#[tokio::test]
async fn test_multiple_st_on_same_source() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ms_src (id INT PRIMARY KEY, region TEXT, amount INT)")
        .await;
    db.execute(
        "INSERT INTO ms_src VALUES \
         (1, 'US', 100), (2, 'EU', 200), (3, 'US', 300)",
    )
    .await;

    // Two STs on same source with different queries
    db.create_st(
        "ms_all",
        "SELECT id, region, amount FROM ms_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    db.create_st(
        "ms_us_only",
        "SELECT id, amount FROM ms_src WHERE region = 'US'",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.ms_all").await, 3);
    assert_eq!(db.count("public.ms_us_only").await, 2);

    // Modify source
    db.execute("INSERT INTO ms_src VALUES (4, 'EU', 400)").await;
    db.execute("INSERT INTO ms_src VALUES (5, 'US', 500)").await;

    // Refresh both
    db.refresh_st("ms_all").await;
    db.refresh_st("ms_us_only").await;

    assert_eq!(db.count("public.ms_all").await, 5);
    assert_eq!(db.count("public.ms_us_only").await, 3);

    // Drop one, verify the other still works
    db.drop_st("ms_all").await;
    assert!(!db.table_exists("public", "ms_all").await);

    // ms_us_only should still work fine
    db.execute("INSERT INTO ms_src VALUES (6, 'US', 600)").await;
    db.refresh_st("ms_us_only").await;
    assert_eq!(db.count("public.ms_us_only").await, 4);
}

#[tokio::test]
async fn test_chained_stream_tables() {
    let db = E2eDb::new().await.with_extension().await;

    // Base table → ST1 → ST2
    db.execute("CREATE TABLE chain_base (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO chain_base VALUES (1, 10), (2, 20), (3, 30)")
        .await;

    db.create_st(
        "chain_st1",
        "SELECT id, val FROM chain_base WHERE val >= 20",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.chain_st1").await, 2);

    // ST on top of ST (chained)
    db.create_st(
        "chain_st2",
        "SELECT id, val FROM public.chain_st1 WHERE val >= 30",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.chain_st2").await, 1);

    // Insert into base
    db.execute("INSERT INTO chain_base VALUES (4, 40)").await;

    // Refresh chain
    db.refresh_st("chain_st1").await;
    db.refresh_st("chain_st2").await;

    assert_eq!(db.count("public.chain_st1").await, 3); // val >= 20: 20, 30, 40
    assert_eq!(db.count("public.chain_st2").await, 2); // val >= 30: 30, 40
}

#[tokio::test]
async fn test_recreate_after_drop() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rc_src (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO rc_src VALUES (1), (2)").await;

    db.create_st("rc_st", "SELECT id FROM rc_src", "1m", "FULL")
        .await;
    assert_eq!(db.count("public.rc_st").await, 2);

    db.drop_st("rc_st").await;
    assert!(!db.table_exists("public", "rc_st").await);

    // Recreate with same name
    db.execute("INSERT INTO rc_src VALUES (3)").await;
    db.create_st("rc_st", "SELECT id FROM rc_src", "1m", "FULL")
        .await;

    assert_eq!(db.count("public.rc_st").await, 3);
}

#[tokio::test]
async fn test_high_frequency_mutations() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE hf_src (id INT PRIMARY KEY, counter INT)")
        .await;
    db.execute("INSERT INTO hf_src VALUES (1, 0)").await;

    db.create_st(
        "hf_st",
        "SELECT id, counter FROM hf_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Rapid mutations: 100 updates
    for i in 1..=100 {
        db.execute(&format!("UPDATE hf_src SET counter = {} WHERE id = 1", i))
            .await;
    }

    db.refresh_st("hf_st").await;

    // Final state should match
    let counter: i32 = db
        .query_scalar("SELECT counter FROM public.hf_st WHERE id = 1")
        .await;
    assert_eq!(counter, 100, "Should reflect the final mutation value");
}

#[tokio::test]
async fn test_empty_source_table() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE empty_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    // No inserts — source table is empty

    db.create_st("empty_st", "SELECT id, val FROM empty_src", "1m", "FULL")
        .await;

    assert_eq!(db.count("public.empty_st").await, 0);

    let (status, _, populated, _) = db.pgt_status("empty_st").await;
    assert_eq!(status, "ACTIVE");
    assert!(populated, "is_populated should be true even with 0 rows");
}

#[tokio::test]
async fn test_source_with_nulls() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE null_src (id INT PRIMARY KEY, name TEXT, score INT, notes TEXT)")
        .await;
    db.execute(
        "INSERT INTO null_src VALUES \
         (1, 'Alice', 100, 'good'), \
         (2, NULL, NULL, 'missing data'), \
         (3, 'Charlie', 90, NULL)",
    )
    .await;

    db.create_st(
        "null_st",
        "SELECT id, name, score, notes FROM null_src",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.null_st").await, 3);

    // Verify NULLs are preserved
    let name_null: bool = db
        .query_scalar("SELECT name IS NULL FROM public.null_st WHERE id = 2")
        .await;
    assert!(name_null, "NULL name should be preserved");

    let score_null: bool = db
        .query_scalar("SELECT score IS NULL FROM public.null_st WHERE id = 2")
        .await;
    assert!(score_null, "NULL score should be preserved");

    let notes_null: bool = db
        .query_scalar("SELECT notes IS NULL FROM public.null_st WHERE id = 3")
        .await;
    assert!(notes_null, "NULL notes should be preserved");
}

#[tokio::test]
async fn test_wide_table() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a table with 50+ columns
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    let mut col_names = Vec::new();
    for i in 1..=50 {
        cols.push(format!("col_{} INT", i));
        vals.push(format!("{}", i));
        col_names.push(format!("col_{}", i));
    }

    let create_sql = format!(
        "CREATE TABLE wide_src (id INT PRIMARY KEY, {})",
        cols.join(", ")
    );
    db.execute(&create_sql).await;

    let insert_sql = format!("INSERT INTO wide_src VALUES (1, {})", vals.join(", "));
    db.execute(&insert_sql).await;

    let select_cols = format!("id, {}", col_names.join(", "));
    db.create_st(
        "wide_st",
        &format!("SELECT {} FROM wide_src", select_cols),
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.wide_st").await, 1);

    // Verify a few columns
    let col_1: i32 = db
        .query_scalar("SELECT col_1 FROM public.wide_st WHERE id = 1")
        .await;
    assert_eq!(col_1, 1);

    let col_50: i32 = db
        .query_scalar("SELECT col_50 FROM public.wide_st WHERE id = 1")
        .await;
    assert_eq!(col_50, 50);
}

#[tokio::test]
async fn test_various_data_types() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE types_src ( \
            id INT PRIMARY KEY, \
            big BIGINT, \
            num NUMERIC(10,2), \
            txt TEXT, \
            flag BOOLEAN, \
            ts TIMESTAMPTZ, \
            j JSONB, \
            uid UUID, \
            raw BYTEA, \
            arr INT[] \
        )",
    )
    .await;

    db.execute(
        "INSERT INTO types_src VALUES ( \
            1, 9999999999, 12345.67, 'hello world', true, \
            '2025-01-15T10:30:00Z'::timestamptz, \
            '{\"key\": \"value\"}'::jsonb, \
            'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid, \
            E'\\\\xDEADBEEF'::bytea, \
            ARRAY[1,2,3]::int[] \
        )",
    )
    .await;

    db.create_st(
        "types_st",
        "SELECT id, big, num, txt, flag, ts, j, uid, raw, arr FROM types_src",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.types_st").await, 1);

    // Spot checks on types
    let big: i64 = db
        .query_scalar("SELECT big FROM public.types_st WHERE id = 1")
        .await;
    assert_eq!(big, 9999999999i64);

    let txt: String = db
        .query_scalar("SELECT txt FROM public.types_st WHERE id = 1")
        .await;
    assert_eq!(txt, "hello world");

    let flag: bool = db
        .query_scalar("SELECT flag FROM public.types_st WHERE id = 1")
        .await;
    assert!(flag);

    let json_val: String = db
        .query_scalar("SELECT j->>'key' FROM public.types_st WHERE id = 1")
        .await;
    assert_eq!(json_val, "value");

    let uid: String = db
        .query_scalar("SELECT uid::text FROM public.types_st WHERE id = 1")
        .await;
    assert_eq!(uid, "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11");
}
