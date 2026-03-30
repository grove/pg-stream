//! E2E tests for A-2: Columnar Change Tracking.
//!
//! Validates that the changed_cols VARBIT bitmask correctly filters delta
//! rows where no referenced column changed, reducing delta volume for
//! wide-table UPDATE workloads.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── A-2: Wide table — update non-referenced column is skipped ──────────

/// Wide table (20 columns), ST references only 2. UPDATE to a non-referenced
/// column should be skipped entirely by the changed_cols bitmask filter,
/// producing zero delta rows and no change to the ST.
#[tokio::test]
async fn test_a2_wide_table_unreferenced_update_skipped() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a wide source table with 20 columns.
    db.execute(
        "CREATE TABLE wide_src (
            id SERIAL PRIMARY KEY,
            region TEXT NOT NULL,
            amount NUMERIC NOT NULL,
            col3 TEXT, col4 TEXT, col5 TEXT, col6 TEXT, col7 TEXT,
            col8 TEXT, col9 TEXT, col10 TEXT, col11 TEXT, col12 TEXT,
            col13 TEXT, col14 TEXT, col15 TEXT, col16 TEXT, col17 TEXT,
            col18 TEXT, col19 TEXT, col20 TEXT
        )",
    )
    .await;

    db.execute(
        "INSERT INTO wide_src (region, amount) VALUES
         ('US', 100), ('EU', 200), ('APAC', 300)",
    )
    .await;

    // ST references only region + amount (2 of 20 columns).
    db.create_st(
        "wide_st",
        "SELECT region, SUM(amount) AS total FROM wide_src GROUP BY region",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("wide_st").await;

    // Verify initial data.
    let count: i64 = db.count("wide_st").await;
    assert_eq!(count, 3, "initial refresh should have 3 regions");

    // UPDATE a non-referenced column (col3) — should be skipped by bitmask.
    db.execute("UPDATE wide_src SET col3 = 'irrelevant' WHERE region = 'US'")
        .await;

    db.refresh_st("wide_st").await;

    // ST should be unchanged — the update was to an irrelevant column.
    db.assert_st_matches_query(
        "wide_st",
        "SELECT region, SUM(amount) AS total FROM wide_src GROUP BY region",
    )
    .await;
}

/// Wide table — update to a referenced value column (amount) should be
/// correctly reflected in the ST after differential refresh.
#[tokio::test]
async fn test_a2_wide_table_referenced_value_update_applied() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wide_val_src (
            id SERIAL PRIMARY KEY,
            region TEXT NOT NULL,
            amount NUMERIC NOT NULL,
            col3 TEXT, col4 TEXT, col5 TEXT, col6 TEXT, col7 TEXT,
            col8 TEXT, col9 TEXT, col10 TEXT
        )",
    )
    .await;

    db.execute(
        "INSERT INTO wide_val_src (region, amount) VALUES
         ('US', 100), ('EU', 200), ('APAC', 300)",
    )
    .await;

    db.create_st(
        "wide_val_st",
        "SELECT region, SUM(amount) AS total FROM wide_val_src GROUP BY region",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("wide_val_st").await;

    // UPDATE a referenced value column (amount) — should be applied.
    db.execute("UPDATE wide_val_src SET amount = 999 WHERE region = 'US'")
        .await;

    db.refresh_st("wide_val_st").await;

    // Verify the update was applied correctly.
    db.assert_st_matches_query(
        "wide_val_st",
        "SELECT region, SUM(amount) AS total FROM wide_val_src GROUP BY region",
    )
    .await;

    // Specific value check: US should now be 999.
    let us_total: String = db
        .query_scalar("SELECT total::text FROM wide_val_st WHERE region = 'US'")
        .await;
    assert_eq!(
        us_total, "999",
        "US total should be 999 after value-column update"
    );
}

/// Wide table — update to a key column (region / GROUP BY) should correctly
/// move the row to a different group in the ST.
#[tokio::test]
async fn test_a2_wide_table_key_column_update_moves_group() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE wide_key_src (
            id SERIAL PRIMARY KEY,
            region TEXT NOT NULL,
            amount NUMERIC NOT NULL,
            col3 TEXT, col4 TEXT, col5 TEXT, col6 TEXT, col7 TEXT
        )",
    )
    .await;

    db.execute(
        "INSERT INTO wide_key_src (region, amount) VALUES
         ('US', 100), ('EU', 200), ('APAC', 300)",
    )
    .await;

    db.create_st(
        "wide_key_st",
        "SELECT region, SUM(amount) AS total FROM wide_key_src GROUP BY region",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("wide_key_st").await;
    assert_eq!(db.count("wide_key_st").await, 3i64);

    // UPDATE the GROUP BY key column — row should move groups.
    db.execute("UPDATE wide_key_src SET region = 'EU' WHERE region = 'APAC'")
        .await;

    db.refresh_st("wide_key_st").await;

    // Now US=100, EU=200+300=500, APAC gone.
    let count: i64 = db.count("wide_key_st").await;
    assert_eq!(count, 2, "APAC should be merged into EU");

    db.assert_st_matches_query(
        "wide_key_st",
        "SELECT region, SUM(amount) AS total FROM wide_key_src GROUP BY region",
    )
    .await;
}

/// Mixed scenario: both unreferenced and referenced columns updated in same
/// transaction. Only the referenced-column changes should affect the ST.
#[tokio::test]
async fn test_a2_mixed_referenced_unreferenced_updates() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE mix_src (
            id SERIAL PRIMARY KEY,
            category TEXT NOT NULL,
            val INT NOT NULL,
            notes TEXT, extra1 TEXT, extra2 TEXT
        )",
    )
    .await;

    db.execute("INSERT INTO mix_src (category, val) VALUES ('A', 10), ('B', 20), ('C', 30)")
        .await;

    db.create_st(
        "mix_st",
        "SELECT category, SUM(val) AS total FROM mix_src GROUP BY category",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("mix_st").await;

    // Batch: update notes (unreferenced), update val (referenced), insert new
    db.execute("UPDATE mix_src SET notes = 'foo' WHERE category = 'A'")
        .await;
    db.execute("UPDATE mix_src SET val = 99 WHERE category = 'B'")
        .await;
    db.execute("INSERT INTO mix_src (category, val) VALUES ('D', 40)")
        .await;

    db.refresh_st("mix_st").await;

    db.assert_st_matches_query(
        "mix_st",
        "SELECT category, SUM(val) AS total FROM mix_src GROUP BY category",
    )
    .await;

    let count: i64 = db.count("mix_st").await;
    assert_eq!(count, 4, "should have 4 categories after insert");
}
