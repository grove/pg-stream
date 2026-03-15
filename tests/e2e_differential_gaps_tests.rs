//! E2E tests for the last differential mode gaps (G1: UDAs, G3: nested OR+sublinks).
//!
//! G1: User-defined aggregates use the group-rescan strategy — any change
//! in a group triggers full re-aggregation from source data.
//!
//! G3: Multiple OR+sublink conjuncts in AND are expanded to a cartesian
//! product of UNION branches. De Morgan normalization (NOT(AND/OR) →
//! OR/AND of negated arms) exposes hidden OR+sublink patterns for the
//! UNION rewrite.
//!
//! G2 (nested window expressions) was already implemented in a prior release.

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// G1: User-Defined Aggregates (UDAs)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_uda_simple_differential() {
    let db = E2eDb::new().await.with_extension().await;

    // Create a simple custom aggregate: concatenates text values.
    db.execute(
        "CREATE FUNCTION concat_sfunc(state TEXT, val TEXT) \
         RETURNS TEXT LANGUAGE sql IMMUTABLE AS $$ \
         SELECT CASE WHEN state IS NULL THEN val ELSE state || ',' || val END $$",
    )
    .await;
    db.execute(
        "CREATE AGGREGATE custom_concat(TEXT) ( \
         SFUNC = concat_sfunc, STYPE = TEXT)",
    )
    .await;

    db.execute("CREATE TABLE uda_src (id SERIAL PRIMARY KEY, grp TEXT, val TEXT)")
        .await;
    db.execute("INSERT INTO uda_src (grp, val) VALUES ('a', 'x'), ('a', 'y'), ('b', 'z')")
        .await;

    let q = "SELECT grp, custom_concat(val) AS combined FROM uda_src GROUP BY grp";
    db.create_st("uda_simple_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("uda_simple_st", q).await;

    // Verify mode is DIFFERENTIAL (not FULL fallback).
    let (_, mode, _, _) = db.pgt_status("uda_simple_st").await;
    assert_eq!(mode, "DIFFERENTIAL", "UDA should use DIFFERENTIAL mode");

    // INSERT
    db.execute("INSERT INTO uda_src (grp, val) VALUES ('a', 'w')")
        .await;
    db.refresh_st("uda_simple_st").await;
    db.assert_st_matches_query("uda_simple_st", q).await;

    // DELETE
    db.execute("DELETE FROM uda_src WHERE val = 'y'").await;
    db.refresh_st("uda_simple_st").await;
    db.assert_st_matches_query("uda_simple_st", q).await;

    // UPDATE
    db.execute("UPDATE uda_src SET val = 'Z' WHERE val = 'z'")
        .await;
    db.refresh_st("uda_simple_st").await;
    db.assert_st_matches_query("uda_simple_st", q).await;
}

#[tokio::test]
async fn test_uda_combined_with_builtin() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE FUNCTION sum2_sfunc(state NUMERIC, val NUMERIC) \
         RETURNS NUMERIC LANGUAGE sql IMMUTABLE AS $$ SELECT COALESCE(state, 0) + val $$",
    )
    .await;
    db.execute(
        "CREATE AGGREGATE custom_sum(NUMERIC) ( \
         SFUNC = sum2_sfunc, STYPE = NUMERIC, INITCOND = '0')",
    )
    .await;

    db.execute("CREATE TABLE uda_mix (id SERIAL PRIMARY KEY, grp TEXT, val NUMERIC)")
        .await;
    db.execute("INSERT INTO uda_mix (grp, val) VALUES ('a', 10), ('a', 20), ('b', 30)")
        .await;

    // Mix UDA with built-in aggregates.
    let q = "SELECT grp, custom_sum(val) AS cs, COUNT(*) AS cnt, SUM(val) AS total \
             FROM uda_mix GROUP BY grp";
    db.create_st("uda_mix_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("uda_mix_st", q).await;

    let (_, mode, _, _) = db.pgt_status("uda_mix_st").await;
    assert_eq!(mode, "DIFFERENTIAL");

    db.execute("INSERT INTO uda_mix (grp, val) VALUES ('a', 5)")
        .await;
    db.refresh_st("uda_mix_st").await;
    db.assert_st_matches_query("uda_mix_st", q).await;

    db.execute("DELETE FROM uda_mix WHERE grp = 'b'").await;
    db.refresh_st("uda_mix_st").await;
    db.assert_st_matches_query("uda_mix_st", q).await;
}

#[tokio::test]
async fn test_uda_auto_mode_resolves_to_differential() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE FUNCTION mymin_sfunc(state INT, val INT) \
         RETURNS INT LANGUAGE sql IMMUTABLE AS $$ \
         SELECT CASE WHEN state IS NULL OR val < state THEN val ELSE state END $$",
    )
    .await;
    db.execute(
        "CREATE AGGREGATE my_min(INT) ( \
         SFUNC = mymin_sfunc, STYPE = INT)",
    )
    .await;

    db.execute("CREATE TABLE uda_auto (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute("INSERT INTO uda_auto (grp, val) VALUES ('a', 10), ('a', 5), ('b', 20)")
        .await;

    let q = "SELECT grp, my_min(val) AS m FROM uda_auto GROUP BY grp";
    // Use AUTO mode — should resolve to DIFFERENTIAL, not fall back to FULL.
    db.create_st("uda_auto_st", q, "1m", "AUTO").await;
    db.assert_st_matches_query("uda_auto_st", q).await;

    let (_, mode, _, _) = db.pgt_status("uda_auto_st").await;
    assert_eq!(
        mode, "DIFFERENTIAL",
        "AUTO should resolve to DIFFERENTIAL for UDA"
    );
}

#[tokio::test]
async fn test_uda_multiple_in_same_query() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE FUNCTION cat_sfunc(state TEXT, val TEXT) \
         RETURNS TEXT LANGUAGE sql IMMUTABLE AS $$ \
         SELECT CASE WHEN state IS NULL THEN val ELSE state || ',' || val END $$",
    )
    .await;
    db.execute("CREATE AGGREGATE cat_agg(TEXT) (SFUNC = cat_sfunc, STYPE = TEXT)")
        .await;
    db.execute(
        "CREATE FUNCTION cat2_sfunc(state TEXT, val TEXT) \
         RETURNS TEXT LANGUAGE sql IMMUTABLE AS $$ \
         SELECT CASE WHEN state IS NULL THEN val ELSE state || ';' || val END $$",
    )
    .await;
    db.execute("CREATE AGGREGATE cat2_agg(TEXT) (SFUNC = cat2_sfunc, STYPE = TEXT)")
        .await;

    db.execute("CREATE TABLE uda_multi (id SERIAL PRIMARY KEY, grp TEXT, a TEXT, b TEXT)")
        .await;
    db.execute(
        "INSERT INTO uda_multi (grp, a, b) VALUES \
         ('x', 'a1', 'b1'), ('x', 'a2', 'b2'), ('y', 'a3', 'b3')",
    )
    .await;

    let q = "SELECT grp, cat_agg(a) AS ca, cat2_agg(b) AS cb FROM uda_multi GROUP BY grp";
    db.create_st("uda_multi_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("uda_multi_st", q).await;

    db.execute("INSERT INTO uda_multi (grp, a, b) VALUES ('x', 'a4', 'b4')")
        .await;
    db.refresh_st("uda_multi_st").await;
    db.assert_st_matches_query("uda_multi_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// G3: Sublinks in Deeply Nested OR
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_nested_or_two_exists() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE no_main (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE no_vip (id INT PRIMARY KEY)").await;
    db.execute("CREATE TABLE no_premium (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO no_main (val) VALUES (1), (2), (3), (4), (5)")
        .await;
    db.execute("INSERT INTO no_vip VALUES (1), (3)").await;
    db.execute("INSERT INTO no_premium VALUES (2), (4)").await;

    // Two OR+sublink conjuncts in AND:
    // (val > 3 OR EXISTS(vip)) AND (val < 2 OR EXISTS(premium))
    let q = "SELECT no_main.id, no_main.val FROM no_main \
             WHERE (no_main.val > 3 OR EXISTS (SELECT 1 FROM no_vip WHERE no_vip.id = no_main.id)) \
             AND (no_main.val < 2 OR EXISTS (SELECT 1 FROM no_premium WHERE no_premium.id = no_main.id))";
    db.create_st("no_two_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("no_two_st", q).await;

    let (_, mode, _, _) = db.pgt_status("no_two_st").await;
    assert_eq!(mode, "DIFFERENTIAL");

    // INSERT into main
    db.execute("INSERT INTO no_main (val) VALUES (6)").await;
    db.refresh_st("no_two_st").await;
    db.assert_st_matches_query("no_two_st", q).await;

    // INSERT into vip (changes EXISTS result)
    db.execute("INSERT INTO no_vip VALUES (5)").await;
    db.refresh_st("no_two_st").await;
    db.assert_st_matches_query("no_two_st", q).await;

    // DELETE from premium
    db.execute("DELETE FROM no_premium WHERE id = 2").await;
    db.refresh_st("no_two_st").await;
    db.assert_st_matches_query("no_two_st", q).await;
}

#[tokio::test]
async fn test_nested_or_mixed_and_or_under_or() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE nom_src (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE nom_ref (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO nom_src (val) VALUES (1), (2), (3), (4)")
        .await;
    db.execute("INSERT INTO nom_ref VALUES (2)").await;

    // Top-level OR: a OR (b AND EXISTS(...))
    let q = "SELECT nom_src.id, nom_src.val FROM nom_src \
             WHERE nom_src.val > 3 OR (nom_src.val < 2 AND EXISTS (SELECT 1 FROM nom_ref WHERE nom_ref.id = nom_src.id))";
    db.create_st("nom_mixed_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("nom_mixed_st", q).await;

    let (_, mode, _, _) = db.pgt_status("nom_mixed_st").await;
    assert_eq!(mode, "DIFFERENTIAL");

    db.execute("INSERT INTO nom_src (val) VALUES (5)").await;
    db.refresh_st("nom_mixed_st").await;
    db.assert_st_matches_query("nom_mixed_st", q).await;
}

#[tokio::test]
async fn test_nested_or_cdc_cycle() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE noc_orders (id SERIAL PRIMARY KEY, customer_id INT, amount INT)")
        .await;
    db.execute("CREATE TABLE noc_vips (customer_id INT PRIMARY KEY)")
        .await;
    db.execute("CREATE TABLE noc_promos (customer_id INT PRIMARY KEY)")
        .await;

    db.execute(
        "INSERT INTO noc_orders (customer_id, amount) VALUES \
         (1, 100), (2, 200), (3, 50), (4, 300)",
    )
    .await;
    db.execute("INSERT INTO noc_vips VALUES (1), (3)").await;
    db.execute("INSERT INTO noc_promos VALUES (2), (4)").await;

    // Complex: two OR+EXISTS in AND
    let q = "SELECT noc_orders.id, noc_orders.amount FROM noc_orders \
             WHERE (noc_orders.amount > 150 OR EXISTS (SELECT 1 FROM noc_vips WHERE noc_vips.customer_id = noc_orders.customer_id)) \
             AND (noc_orders.amount < 250 OR EXISTS (SELECT 1 FROM noc_promos WHERE noc_promos.customer_id = noc_orders.customer_id))";
    db.create_st("noc_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("noc_st", q).await;

    // Full CDC cycle: insert, update, delete
    db.execute("INSERT INTO noc_orders (customer_id, amount) VALUES (5, 175)")
        .await;
    db.refresh_st("noc_st").await;
    db.assert_st_matches_query("noc_st", q).await;

    db.execute("UPDATE noc_orders SET amount = 500 WHERE customer_id = 3")
        .await;
    db.refresh_st("noc_st").await;
    db.assert_st_matches_query("noc_st", q).await;

    db.execute("DELETE FROM noc_orders WHERE customer_id = 1")
        .await;
    db.refresh_st("noc_st").await;
    db.assert_st_matches_query("noc_st", q).await;
}

#[tokio::test]
async fn test_nested_or_demorgan_not_and() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dm_main (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE dm_ref (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO dm_main (val) VALUES (1), (2), (3), (4), (5)")
        .await;
    db.execute("INSERT INTO dm_ref VALUES (2), (4)").await;

    // NOT (a AND NOT EXISTS(s))  →  De Morgan  →  (NOT a) OR EXISTS(s)
    let q = "SELECT dm_main.id, dm_main.val FROM dm_main \
             WHERE NOT (dm_main.val > 3 AND NOT EXISTS (SELECT 1 FROM dm_ref WHERE dm_ref.id = dm_main.id))";
    db.create_st("dm_not_and_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("dm_not_and_st", q).await;

    let (_, mode, _, _) = db.pgt_status("dm_not_and_st").await;
    assert_eq!(
        mode, "DIFFERENTIAL",
        "De Morgan pattern should be DIFFERENTIAL"
    );

    // INSERT
    db.execute("INSERT INTO dm_main (val) VALUES (6)").await;
    db.refresh_st("dm_not_and_st").await;
    db.assert_st_matches_query("dm_not_and_st", q).await;

    // Change EXISTS result
    db.execute("INSERT INTO dm_ref VALUES (5)").await;
    db.refresh_st("dm_not_and_st").await;
    db.assert_st_matches_query("dm_not_and_st", q).await;

    // DELETE
    db.execute("DELETE FROM dm_ref WHERE id = 2").await;
    db.refresh_st("dm_not_and_st").await;
    db.assert_st_matches_query("dm_not_and_st", q).await;
}

#[tokio::test]
async fn test_nested_or_demorgan_and_prefix() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE dm2_src (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE dm2_ref (id INT PRIMARY KEY)")
        .await;
    db.execute("INSERT INTO dm2_src (val) VALUES (1), (2), (3), (4), (5)")
        .await;
    db.execute("INSERT INTO dm2_ref VALUES (1), (3)").await;

    // AND prefix + NOT(AND+sublink):
    // val < 5 AND NOT (val > 2 AND NOT EXISTS (...))
    let q = "SELECT dm2_src.id, dm2_src.val FROM dm2_src \
             WHERE dm2_src.val < 5 AND NOT (dm2_src.val > 2 AND NOT EXISTS (SELECT 1 FROM dm2_ref WHERE dm2_ref.id = dm2_src.id))";
    db.create_st("dm2_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("dm2_st", q).await;

    let (_, mode, _, _) = db.pgt_status("dm2_st").await;
    assert_eq!(mode, "DIFFERENTIAL");

    db.execute("INSERT INTO dm2_src (val) VALUES (2)").await;
    db.refresh_st("dm2_st").await;
    db.assert_st_matches_query("dm2_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// G1 additional: UDA edge cases
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_uda_with_filter_clause() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE FUNCTION fcat_sfunc(state TEXT, val TEXT) \
         RETURNS TEXT LANGUAGE sql IMMUTABLE AS $$ \
         SELECT CASE WHEN state IS NULL THEN val ELSE state || ',' || val END $$",
    )
    .await;
    db.execute("CREATE AGGREGATE fcat_agg(TEXT) (SFUNC = fcat_sfunc, STYPE = TEXT)")
        .await;

    db.execute("CREATE TABLE uda_filt (id SERIAL PRIMARY KEY, grp TEXT, val TEXT, active BOOL)")
        .await;
    db.execute(
        "INSERT INTO uda_filt (grp, val, active) VALUES \
         ('a', 'x', true), ('a', 'y', false), ('a', 'z', true), ('b', 'w', true)",
    )
    .await;

    let q = "SELECT grp, fcat_agg(val) FILTER (WHERE active) AS filtered \
             FROM uda_filt GROUP BY grp";
    db.create_st("uda_filt_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("uda_filt_st", q).await;

    let (_, mode, _, _) = db.pgt_status("uda_filt_st").await;
    assert_eq!(mode, "DIFFERENTIAL");

    db.execute("INSERT INTO uda_filt (grp, val, active) VALUES ('a', 'v', true)")
        .await;
    db.refresh_st("uda_filt_st").await;
    db.assert_st_matches_query("uda_filt_st", q).await;

    db.execute("UPDATE uda_filt SET active = false WHERE val = 'z'")
        .await;
    db.refresh_st("uda_filt_st").await;
    db.assert_st_matches_query("uda_filt_st", q).await;
}

#[tokio::test]
async fn test_uda_with_order_by_in_agg() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE FUNCTION ocat_sfunc(state TEXT, val TEXT) \
         RETURNS TEXT LANGUAGE sql IMMUTABLE AS $$ \
         SELECT CASE WHEN state IS NULL THEN val ELSE state || ',' || val END $$",
    )
    .await;
    db.execute("CREATE AGGREGATE ocat_agg(TEXT) (SFUNC = ocat_sfunc, STYPE = TEXT)")
        .await;

    db.execute("CREATE TABLE uda_ord (id SERIAL PRIMARY KEY, grp TEXT, val TEXT, priority INT)")
        .await;
    db.execute(
        "INSERT INTO uda_ord (grp, val, priority) VALUES \
         ('a', 'x', 3), ('a', 'y', 1), ('a', 'z', 2), ('b', 'w', 1)",
    )
    .await;

    let q = "SELECT grp, ocat_agg(val ORDER BY priority) AS ordered \
             FROM uda_ord GROUP BY grp";
    db.create_st("uda_ord_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("uda_ord_st", q).await;

    let (_, mode, _, _) = db.pgt_status("uda_ord_st").await;
    assert_eq!(mode, "DIFFERENTIAL");

    db.execute("INSERT INTO uda_ord (grp, val, priority) VALUES ('a', 'v', 0)")
        .await;
    db.refresh_st("uda_ord_st").await;
    db.assert_st_matches_query("uda_ord_st", q).await;
}

#[tokio::test]
async fn test_uda_schema_qualified() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE SCHEMA uda_ns").await;
    db.execute(
        "CREATE FUNCTION uda_ns.mysum_sfunc(state NUMERIC, val NUMERIC) \
         RETURNS NUMERIC LANGUAGE sql IMMUTABLE AS $$ SELECT COALESCE(state, 0) + val $$",
    )
    .await;
    db.execute(
        "CREATE AGGREGATE uda_ns.mysum(NUMERIC) ( \
         SFUNC = uda_ns.mysum_sfunc, STYPE = NUMERIC, INITCOND = '0')",
    )
    .await;

    db.execute("CREATE TABLE uda_sq (id SERIAL PRIMARY KEY, grp TEXT, val NUMERIC)")
        .await;
    db.execute("INSERT INTO uda_sq (grp, val) VALUES ('a', 10), ('a', 20), ('b', 5)")
        .await;

    let q = "SELECT grp, uda_ns.mysum(val) AS total FROM uda_sq GROUP BY grp";
    db.create_st("uda_sq_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("uda_sq_st", q).await;

    let (_, mode, _, _) = db.pgt_status("uda_sq_st").await;
    assert_eq!(mode, "DIFFERENTIAL");

    db.execute("INSERT INTO uda_sq (grp, val) VALUES ('a', 30)")
        .await;
    db.refresh_st("uda_sq_st").await;
    db.assert_st_matches_query("uda_sq_st", q).await;
}

#[tokio::test]
async fn test_uda_insert_delete_update_full_cycle() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE FUNCTION cmin_sfunc(state INT, val INT) \
         RETURNS INT LANGUAGE sql IMMUTABLE AS $$ \
         SELECT CASE WHEN state IS NULL OR val < state THEN val ELSE state END $$",
    )
    .await;
    db.execute("CREATE AGGREGATE cust_min(INT) (SFUNC = cmin_sfunc, STYPE = INT)")
        .await;

    db.execute("CREATE TABLE uda_crud (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute(
        "INSERT INTO uda_crud (grp, val) VALUES \
         ('a', 10), ('a', 20), ('a', 5), ('b', 30), ('b', 15)",
    )
    .await;

    let q = "SELECT grp, cust_min(val) AS m, COUNT(*) AS n FROM uda_crud GROUP BY grp";
    db.create_st("uda_crud_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("uda_crud_st", q).await;

    // INSERT — adds new minimum
    db.execute("INSERT INTO uda_crud (grp, val) VALUES ('a', 1)")
        .await;
    db.refresh_st("uda_crud_st").await;
    db.assert_st_matches_query("uda_crud_st", q).await;

    // UPDATE — changes existing minimum
    db.execute("UPDATE uda_crud SET val = 100 WHERE grp = 'a' AND val = 1")
        .await;
    db.refresh_st("uda_crud_st").await;
    db.assert_st_matches_query("uda_crud_st", q).await;

    // DELETE — removes a row
    db.execute("DELETE FROM uda_crud WHERE grp = 'b' AND val = 15")
        .await;
    db.refresh_st("uda_crud_st").await;
    db.assert_st_matches_query("uda_crud_st", q).await;

    // DELETE all in group — group disappears
    db.execute("DELETE FROM uda_crud WHERE grp = 'b'").await;
    db.refresh_st("uda_crud_st").await;
    db.assert_st_matches_query("uda_crud_st", q).await;

    // INSERT into empty group — group reappears
    db.execute("INSERT INTO uda_crud (grp, val) VALUES ('b', 42)")
        .await;
    db.refresh_st("uda_crud_st").await;
    db.assert_st_matches_query("uda_crud_st", q).await;
}
