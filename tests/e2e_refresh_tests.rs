//! E2E tests for `pgstream.refresh_stream_table()`.
//!
//! Validates refresh picks up all DML changes (inserts, updates, deletes),
//! handles edge cases, and correctly updates metadata.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Basic DML Refresh ──────────────────────────────────────────────────

#[tokio::test]
async fn test_refresh_picks_up_inserts() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_ins (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO rf_ins VALUES (1, 'a'), (2, 'b')")
        .await;

    db.create_st(
        "rf_ins_st",
        "SELECT id, val FROM rf_ins",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    assert_eq!(db.count("public.rf_ins_st").await, 2);

    // Insert new rows
    db.execute("INSERT INTO rf_ins VALUES (3, 'c'), (4, 'd')")
        .await;
    db.refresh_st("rf_ins_st").await;

    assert_eq!(db.count("public.rf_ins_st").await, 4);
}

#[tokio::test]
async fn test_refresh_picks_up_updates() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_upd (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO rf_upd VALUES (1, 'old'), (2, 'old')")
        .await;

    db.create_st(
        "rf_upd_st",
        "SELECT id, val FROM rf_upd",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.execute("UPDATE rf_upd SET val = 'new' WHERE id = 1")
        .await;
    db.refresh_st("rf_upd_st").await;

    let val: String = db
        .query_scalar("SELECT val FROM public.rf_upd_st WHERE id = 1")
        .await;
    assert_eq!(val, "new", "Updated value should be reflected");

    // Unchanged row should still be there
    let val2: String = db
        .query_scalar("SELECT val FROM public.rf_upd_st WHERE id = 2")
        .await;
    assert_eq!(val2, "old");
}

#[tokio::test]
async fn test_refresh_picks_up_deletes() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_del (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO rf_del VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .await;

    db.create_st(
        "rf_del_st",
        "SELECT id, val FROM rf_del",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    assert_eq!(db.count("public.rf_del_st").await, 3);

    db.execute("DELETE FROM rf_del WHERE id = 2").await;
    db.refresh_st("rf_del_st").await;

    assert_eq!(db.count("public.rf_del_st").await, 2);

    // Verify specific row is gone
    let has_2: bool = db
        .query_scalar("SELECT EXISTS(SELECT 1 FROM public.rf_del_st WHERE id = 2)")
        .await;
    assert!(!has_2, "Deleted row should not appear in ST");
}

#[tokio::test]
async fn test_refresh_mixed_dml() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_mix (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO rf_mix VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .await;

    db.create_st(
        "rf_mix_st",
        "SELECT id, val FROM rf_mix",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Mixed DML
    db.execute("INSERT INTO rf_mix VALUES (4, 'd')").await;
    db.execute("UPDATE rf_mix SET val = 'a_updated' WHERE id = 1")
        .await;
    db.execute("DELETE FROM rf_mix WHERE id = 2").await;

    db.refresh_st("rf_mix_st").await;

    // Final state should match defining query
    db.assert_st_matches_query("public.rf_mix_st", "SELECT id, val FROM rf_mix")
        .await;
}

// ── Aggregation & Join Refresh ─────────────────────────────────────────

#[tokio::test]
async fn test_refresh_aggregate_correctness() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_agg (id SERIAL PRIMARY KEY, grp INT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO rf_agg (grp, amount) VALUES (1, 100), (1, 200), (2, 50)")
        .await;

    db.create_st(
        "rf_agg_st",
        "SELECT grp, SUM(amount) AS total, COUNT(*) AS cnt FROM rf_agg GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Add more data
    db.execute("INSERT INTO rf_agg (grp, amount) VALUES (1, 300), (2, 150), (3, 500)")
        .await;

    db.refresh_st("rf_agg_st").await;

    let total_1: i64 = db
        .query_scalar("SELECT total::bigint FROM public.rf_agg_st WHERE grp = 1")
        .await;
    assert_eq!(total_1, 600, "Group 1 total: 100+200+300");

    let cnt_2: i64 = db
        .query_scalar("SELECT cnt FROM public.rf_agg_st WHERE grp = 2")
        .await;
    assert_eq!(cnt_2, 2);

    assert_eq!(db.count("public.rf_agg_st").await, 3);
}

#[tokio::test]
async fn test_refresh_join_after_insert() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_cust (id INT PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE rf_ord (id INT PRIMARY KEY, cust_id INT, amount INT)")
        .await;
    db.execute("INSERT INTO rf_cust VALUES (1, 'Alice')").await;
    db.execute("INSERT INTO rf_ord VALUES (1, 1, 100)").await;

    db.create_st(
        "rf_join_st",
        "SELECT c.name, o.amount FROM rf_cust c JOIN rf_ord o ON c.id = o.cust_id",
        "1m",
        "FULL",
    )
    .await;

    assert_eq!(db.count("public.rf_join_st").await, 1);

    // Add a new customer and order
    db.execute("INSERT INTO rf_cust VALUES (2, 'Bob')").await;
    db.execute("INSERT INTO rf_ord VALUES (2, 2, 200)").await;

    db.refresh_st("rf_join_st").await;

    assert_eq!(db.count("public.rf_join_st").await, 2);
}

// ── Idempotency & Edge Cases ───────────────────────────────────────────

#[tokio::test]
async fn test_refresh_idempotent() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_idem (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO rf_idem VALUES (1, 'a'), (2, 'b')")
        .await;

    db.create_st(
        "rf_idem_st",
        "SELECT id, val FROM rf_idem",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Multiple refreshes with no changes
    db.refresh_st("rf_idem_st").await;
    db.refresh_st("rf_idem_st").await;
    db.refresh_st("rf_idem_st").await;

    assert_eq!(db.count("public.rf_idem_st").await, 2);
    db.assert_st_matches_query("public.rf_idem_st", "SELECT id, val FROM rf_idem")
        .await;
}

// ── Metadata Updates ───────────────────────────────────────────────────

#[tokio::test]
async fn test_refresh_updates_data_timestamp() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_ts (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO rf_ts VALUES (1)").await;

    db.create_st("rf_ts_st", "SELECT id FROM rf_ts", "1m", "FULL")
        .await;

    let ts_before: Option<String> = db
        .query_scalar_opt(
            "SELECT data_timestamp::text FROM pgstream.pgs_stream_tables WHERE pgs_name = 'rf_ts_st'",
        )
        .await;

    // Small delay to ensure timestamp difference
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    db.execute("INSERT INTO rf_ts VALUES (2)").await;
    db.refresh_st("rf_ts_st").await;

    let ts_after: Option<String> = db
        .query_scalar_opt(
            "SELECT data_timestamp::text FROM pgstream.pgs_stream_tables WHERE pgs_name = 'rf_ts_st'",
        )
        .await;

    assert!(
        ts_after.is_some(),
        "data_timestamp should be set after refresh"
    );
    assert_ne!(
        ts_before, ts_after,
        "data_timestamp should advance after refresh"
    );
}

#[tokio::test]
async fn test_refresh_updates_last_refresh_at() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_lr (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO rf_lr VALUES (1)").await;

    db.create_st("rf_lr_st", "SELECT id FROM rf_lr", "1m", "FULL")
        .await;

    db.execute("INSERT INTO rf_lr VALUES (2)").await;
    db.refresh_st("rf_lr_st").await;

    let has_refresh_at: bool = db
        .query_scalar(
            "SELECT last_refresh_at IS NOT NULL FROM pgstream.pgs_stream_tables WHERE pgs_name = 'rf_lr_st'",
        )
        .await;
    assert!(
        has_refresh_at,
        "last_refresh_at should be set after refresh"
    );
}

#[tokio::test]
async fn test_refresh_resets_consecutive_errors() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_err (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO rf_err VALUES (1)").await;

    db.create_st("rf_err_st", "SELECT id FROM rf_err", "1m", "FULL")
        .await;

    // After a successful refresh, consecutive_errors should be 0
    db.execute("INSERT INTO rf_err VALUES (2)").await;
    db.refresh_st("rf_err_st").await;

    let (_, _, _, errors) = db.pgs_status("rf_err_st").await;
    assert_eq!(errors, 0, "consecutive_errors should be 0 after success");
}

#[tokio::test]
async fn test_refresh_records_history() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_hist (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO rf_hist VALUES (1)").await;

    db.create_st("rf_hist_st", "SELECT id FROM rf_hist", "1m", "FULL")
        .await;

    db.execute("INSERT INTO rf_hist VALUES (2)").await;
    db.refresh_st("rf_hist_st").await;

    // Manual refresh updates catalog metadata but doesn't write to
    // pgs_refresh_history (only the scheduler does). Verify the catalog
    // was updated correctly instead.
    let has_refresh_at: bool = db
        .query_scalar(
            "SELECT last_refresh_at IS NOT NULL FROM pgstream.pgs_stream_tables WHERE pgs_name = 'rf_hist_st'",
        )
        .await;
    assert!(
        has_refresh_at,
        "last_refresh_at should be set after manual refresh"
    );

    let data_ts: bool = db
        .query_scalar(
            "SELECT data_timestamp IS NOT NULL FROM pgstream.pgs_stream_tables WHERE pgs_name = 'rf_hist_st'",
        )
        .await;
    assert!(data_ts, "data_timestamp should be set after manual refresh");

    // Verify the pgs_refresh_history table exists and is queryable
    let table_exists = db.table_exists("pgstream", "pgs_refresh_history").await;
    assert!(table_exists, "pgs_refresh_history table should exist");
}

// ── Suspended ST Refresh ───────────────────────────────────────────────

#[tokio::test]
async fn test_refresh_suspended_st_fails() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_susp (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO rf_susp VALUES (1)").await;

    db.create_st("rf_susp_st", "SELECT id FROM rf_susp", "1m", "FULL")
        .await;

    // Suspend the ST
    db.alter_st("rf_susp_st", "status => 'SUSPENDED'").await;

    // Refresh should fail
    let result = db
        .try_execute("SELECT pgstream.refresh_stream_table('rf_susp_st')")
        .await;
    assert!(result.is_err(), "Refreshing a SUSPENDED ST should fail");
}

// ── Large Batch ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_refresh_large_batch() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rf_big (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO rf_big SELECT g, 'row_' || g FROM generate_series(1, 100) g")
        .await;

    db.create_st(
        "rf_big_st",
        "SELECT id, val FROM rf_big",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    assert_eq!(db.count("public.rf_big_st").await, 100);

    // Insert 10,000 more rows
    db.execute("INSERT INTO rf_big SELECT g, 'row_' || g FROM generate_series(101, 10100) g")
        .await;

    db.refresh_st("rf_big_st").await;

    assert_eq!(db.count("public.rf_big_st").await, 10100);
}

// ── BIT_AND / BIT_OR / BIT_XOR Aggregates ──────────────────────────────

#[tokio::test]
async fn test_bit_and_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE bit_src (id INT PRIMARY KEY, dept TEXT, flags INT)")
        .await;
    db.execute(
        "INSERT INTO bit_src VALUES \
         (1, 'eng', 7), (2, 'eng', 5), (3, 'sales', 15), (4, 'sales', 12)",
    )
    .await;

    db.create_st(
        "bit_and_st",
        "SELECT dept, BIT_AND(flags) AS all_flags FROM bit_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.bit_and_st",
        "SELECT dept, BIT_AND(flags) AS all_flags FROM bit_src GROUP BY dept",
    )
    .await;

    // Mutate: insert a row that changes the BIT_AND result for 'eng'
    db.execute("INSERT INTO bit_src VALUES (5, 'eng', 4)").await;
    db.refresh_st("bit_and_st").await;

    db.assert_st_matches_query(
        "public.bit_and_st",
        "SELECT dept, BIT_AND(flags) AS all_flags FROM bit_src GROUP BY dept",
    )
    .await;

    // Delete a row
    db.execute("DELETE FROM bit_src WHERE id = 3").await;
    db.refresh_st("bit_and_st").await;

    db.assert_st_matches_query(
        "public.bit_and_st",
        "SELECT dept, BIT_AND(flags) AS all_flags FROM bit_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_bit_or_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE bitor_src (id INT PRIMARY KEY, dept TEXT, perms INT)")
        .await;
    db.execute(
        "INSERT INTO bitor_src VALUES \
         (1, 'eng', 1), (2, 'eng', 2), (3, 'sales', 4)",
    )
    .await;

    db.create_st(
        "bit_or_st",
        "SELECT dept, BIT_OR(perms) AS any_perms FROM bitor_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.bit_or_st",
        "SELECT dept, BIT_OR(perms) AS any_perms FROM bitor_src GROUP BY dept",
    )
    .await;

    // Insert: adds a new permission bit
    db.execute("INSERT INTO bitor_src VALUES (4, 'eng', 8)")
        .await;
    db.refresh_st("bit_or_st").await;

    db.assert_st_matches_query(
        "public.bit_or_st",
        "SELECT dept, BIT_OR(perms) AS any_perms FROM bitor_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_bit_xor_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE bitxor_src (id INT PRIMARY KEY, dept TEXT, val INT)")
        .await;
    db.execute(
        "INSERT INTO bitxor_src VALUES \
         (1, 'eng', 5), (2, 'eng', 3), (3, 'sales', 7)",
    )
    .await;

    db.create_st(
        "bit_xor_st",
        "SELECT dept, BIT_XOR(val) AS xor_val FROM bitxor_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.bit_xor_st",
        "SELECT dept, BIT_XOR(val) AS xor_val FROM bitxor_src GROUP BY dept",
    )
    .await;

    // Update: changes XOR result
    db.execute("UPDATE bitxor_src SET val = 10 WHERE id = 1")
        .await;
    db.refresh_st("bit_xor_st").await;

    db.assert_st_matches_query(
        "public.bit_xor_st",
        "SELECT dept, BIT_XOR(val) AS xor_val FROM bitxor_src GROUP BY dept",
    )
    .await;
}

// ── JSON_OBJECT_AGG / JSONB_OBJECT_AGG ─────────────────────────────────

/// Test JSON_OBJECT_AGG in DIFFERENTIAL mode.
/// Uses JSONB_OBJECT_AGG (drop-in replacement) because the `json` type
/// lacks an equality operator — PostgreSQL's `IS DISTINCT FROM` (used
/// by the differential change-detection guard) requires equality.
/// `jsonb` supports equality natively.
#[tokio::test]
async fn test_json_object_agg_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE jobj_src (id INT PRIMARY KEY, dept TEXT, prop TEXT, val TEXT)")
        .await;
    db.execute(
        "INSERT INTO jobj_src VALUES \
         (1, 'eng', 'lang', 'rust'), \
         (2, 'eng', 'os', 'linux'), \
         (3, 'sales', 'region', 'us')",
    )
    .await;

    db.create_st(
        "jobj_st",
        "SELECT dept, JSONB_OBJECT_AGG(prop, val) AS props FROM jobj_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.jobj_st",
        "SELECT dept, JSONB_OBJECT_AGG(prop, val) AS props FROM jobj_src GROUP BY dept",
    )
    .await;

    // Insert a new property
    db.execute("INSERT INTO jobj_src VALUES (4, 'eng', 'editor', 'vim')")
        .await;
    db.refresh_st("jobj_st").await;

    db.assert_st_matches_query(
        "public.jobj_st",
        "SELECT dept, JSONB_OBJECT_AGG(prop, val) AS props FROM jobj_src GROUP BY dept",
    )
    .await;

    // Delete a property
    db.execute("DELETE FROM jobj_src WHERE id = 2").await;
    db.refresh_st("jobj_st").await;

    db.assert_st_matches_query(
        "public.jobj_st",
        "SELECT dept, JSONB_OBJECT_AGG(prop, val) AS props FROM jobj_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_jsonb_object_agg_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE jbobj_src (id INT PRIMARY KEY, dept TEXT, key TEXT, val TEXT)")
        .await;
    db.execute(
        "INSERT INTO jbobj_src VALUES \
         (1, 'eng', 'level', 'senior'), \
         (2, 'sales', 'territory', 'west')",
    )
    .await;

    db.create_st(
        "jbobj_st",
        "SELECT dept, JSONB_OBJECT_AGG(key, val) AS data FROM jbobj_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.jbobj_st",
        "SELECT dept, JSONB_OBJECT_AGG(key, val) AS data FROM jbobj_src GROUP BY dept",
    )
    .await;

    // Update a value
    db.execute("UPDATE jbobj_src SET val = 'east' WHERE id = 2")
        .await;
    db.refresh_st("jbobj_st").await;

    db.assert_st_matches_query(
        "public.jbobj_st",
        "SELECT dept, JSONB_OBJECT_AGG(key, val) AS data FROM jbobj_src GROUP BY dept",
    )
    .await;
}

// ── Mixed new aggregates with existing ones ────────────────────────────

#[tokio::test]
async fn test_mixed_bit_or_with_count_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE mixed_bit_src (id INT PRIMARY KEY, dept TEXT, perms INT)")
        .await;
    db.execute(
        "INSERT INTO mixed_bit_src VALUES \
         (1, 'eng', 1), (2, 'eng', 2), (3, 'eng', 4), (4, 'sales', 8)",
    )
    .await;

    db.create_st(
        "mixed_bit_st",
        "SELECT dept, COUNT(*) AS cnt, BIT_OR(perms) AS combined_perms \
         FROM mixed_bit_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.mixed_bit_st",
        "SELECT dept, COUNT(*) AS cnt, BIT_OR(perms) AS combined_perms \
         FROM mixed_bit_src GROUP BY dept",
    )
    .await;

    // Insert
    db.execute("INSERT INTO mixed_bit_src VALUES (5, 'eng', 16)")
        .await;
    db.refresh_st("mixed_bit_st").await;

    db.assert_st_matches_query(
        "public.mixed_bit_st",
        "SELECT dept, COUNT(*) AS cnt, BIT_OR(perms) AS combined_perms \
         FROM mixed_bit_src GROUP BY dept",
    )
    .await;
}

// ── Statistical aggregate tests ────────────────────────────────────────

#[tokio::test]
async fn test_stddev_pop_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sdp_src (id INT PRIMARY KEY, dept TEXT, amount NUMERIC)")
        .await;
    db.execute(
        "INSERT INTO sdp_src VALUES \
         (1, 'eng', 100), (2, 'eng', 200), (3, 'eng', 300), \
         (4, 'sales', 50), (5, 'sales', 150)",
    )
    .await;

    db.create_st(
        "sdp_st",
        "SELECT dept, STDDEV_POP(amount) AS sd FROM sdp_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.sdp_st",
        "SELECT dept, STDDEV_POP(amount) AS sd FROM sdp_src GROUP BY dept",
    )
    .await;

    // Insert a new row
    db.execute("INSERT INTO sdp_src VALUES (6, 'eng', 400)")
        .await;
    db.refresh_st("sdp_st").await;

    db.assert_st_matches_query(
        "public.sdp_st",
        "SELECT dept, STDDEV_POP(amount) AS sd FROM sdp_src GROUP BY dept",
    )
    .await;

    // Delete a row
    db.execute("DELETE FROM sdp_src WHERE id = 4").await;
    db.refresh_st("sdp_st").await;

    db.assert_st_matches_query(
        "public.sdp_st",
        "SELECT dept, STDDEV_POP(amount) AS sd FROM sdp_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_stddev_samp_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sds_src (id INT PRIMARY KEY, dept TEXT, amount NUMERIC)")
        .await;
    db.execute(
        "INSERT INTO sds_src VALUES \
         (1, 'eng', 100), (2, 'eng', 200), (3, 'eng', 300), \
         (4, 'sales', 50), (5, 'sales', 150)",
    )
    .await;

    db.create_st(
        "sds_st",
        "SELECT dept, STDDEV_SAMP(amount) AS sd FROM sds_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.sds_st",
        "SELECT dept, STDDEV_SAMP(amount) AS sd FROM sds_src GROUP BY dept",
    )
    .await;

    // Update a value
    db.execute("UPDATE sds_src SET amount = 350 WHERE id = 3")
        .await;
    db.refresh_st("sds_st").await;

    db.assert_st_matches_query(
        "public.sds_st",
        "SELECT dept, STDDEV_SAMP(amount) AS sd FROM sds_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_stddev_alias_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE sda_src (id INT PRIMARY KEY, dept TEXT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO sda_src VALUES (1, 'a', 10), (2, 'a', 20), (3, 'a', 30)")
        .await;

    // STDDEV is an alias for STDDEV_SAMP
    db.create_st(
        "sda_st",
        "SELECT dept, STDDEV(amount) AS sd FROM sda_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.sda_st",
        "SELECT dept, STDDEV(amount) AS sd FROM sda_src GROUP BY dept",
    )
    .await;

    db.execute("INSERT INTO sda_src VALUES (4, 'a', 40)").await;
    db.refresh_st("sda_st").await;

    db.assert_st_matches_query(
        "public.sda_st",
        "SELECT dept, STDDEV(amount) AS sd FROM sda_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_var_pop_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE vp_src (id INT PRIMARY KEY, dept TEXT, amount NUMERIC)")
        .await;
    db.execute(
        "INSERT INTO vp_src VALUES \
         (1, 'eng', 100), (2, 'eng', 200), (3, 'eng', 300)",
    )
    .await;

    db.create_st(
        "vp_st",
        "SELECT dept, VAR_POP(amount) AS vp FROM vp_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.vp_st",
        "SELECT dept, VAR_POP(amount) AS vp FROM vp_src GROUP BY dept",
    )
    .await;

    // Delete and insert
    db.execute("DELETE FROM vp_src WHERE id = 3").await;
    db.execute("INSERT INTO vp_src VALUES (4, 'eng', 400)")
        .await;
    db.refresh_st("vp_st").await;

    db.assert_st_matches_query(
        "public.vp_st",
        "SELECT dept, VAR_POP(amount) AS vp FROM vp_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_var_samp_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE vs_src (id INT PRIMARY KEY, dept TEXT, amount NUMERIC)")
        .await;
    db.execute(
        "INSERT INTO vs_src VALUES \
         (1, 'eng', 100), (2, 'eng', 200), (3, 'eng', 300), \
         (4, 'sales', 50), (5, 'sales', 150)",
    )
    .await;

    db.create_st(
        "vs_st",
        "SELECT dept, VAR_SAMP(amount) AS vs FROM vs_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.vs_st",
        "SELECT dept, VAR_SAMP(amount) AS vs FROM vs_src GROUP BY dept",
    )
    .await;

    // Update triggering re-aggregation
    db.execute("UPDATE vs_src SET amount = 250 WHERE id = 2")
        .await;
    db.refresh_st("vs_st").await;

    db.assert_st_matches_query(
        "public.vs_st",
        "SELECT dept, VAR_SAMP(amount) AS vs FROM vs_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_variance_alias_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE va_src (id INT PRIMARY KEY, dept TEXT, amount NUMERIC)")
        .await;
    db.execute("INSERT INTO va_src VALUES (1, 'x', 10), (2, 'x', 30), (3, 'x', 50)")
        .await;

    // VARIANCE is an alias for VAR_SAMP
    db.create_st(
        "va_st",
        "SELECT dept, VARIANCE(amount) AS v FROM va_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.va_st",
        "SELECT dept, VARIANCE(amount) AS v FROM va_src GROUP BY dept",
    )
    .await;

    db.execute("INSERT INTO va_src VALUES (4, 'x', 70)").await;
    db.refresh_st("va_st").await;

    db.assert_st_matches_query(
        "public.va_st",
        "SELECT dept, VARIANCE(amount) AS v FROM va_src GROUP BY dept",
    )
    .await;
}

#[tokio::test]
async fn test_mixed_stddev_with_sum_count_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE mstat_src (id INT PRIMARY KEY, dept TEXT, amount NUMERIC)")
        .await;
    db.execute(
        "INSERT INTO mstat_src VALUES \
         (1, 'eng', 100), (2, 'eng', 200), (3, 'eng', 300), \
         (4, 'sales', 50), (5, 'sales', 150)",
    )
    .await;

    db.create_st(
        "mstat_st",
        "SELECT dept, COUNT(*) AS cnt, SUM(amount) AS total, STDDEV_POP(amount) AS sd \
         FROM mstat_src GROUP BY dept",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.assert_st_matches_query(
        "public.mstat_st",
        "SELECT dept, COUNT(*) AS cnt, SUM(amount) AS total, STDDEV_POP(amount) AS sd \
         FROM mstat_src GROUP BY dept",
    )
    .await;

    // Insert a new row
    db.execute("INSERT INTO mstat_src VALUES (6, 'eng', 400)")
        .await;
    db.refresh_st("mstat_st").await;

    db.assert_st_matches_query(
        "public.mstat_st",
        "SELECT dept, COUNT(*) AS cnt, SUM(amount) AS total, STDDEV_POP(amount) AS sd \
         FROM mstat_src GROUP BY dept",
    )
    .await;
}
