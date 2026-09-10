//! E2E tests for INTERSECT / EXCEPT FULL-refresh correctness (F19: G2.4).
//!
//! Validates set operations (INTERSECT, INTERSECT ALL, EXCEPT, EXCEPT ALL).
//! Differential maintenance of set operations remains FULL-only. The REL-101
//! tests at the end verify the durable private multiplicity state that a FULL
//! refresh builds for every set-operation stream table: exact per-branch
//! counts, privacy (no counter/invisible-row leakage into the visible ST), and
//! cleanup on drop.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// INTERSECT
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_intersect_basic_full() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE isect_a (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE isect_b (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO isect_a (val) VALUES (1), (2), (3)")
        .await;
    db.execute("INSERT INTO isect_b (val) VALUES (2), (3), (4)")
        .await;

    let q = "SELECT val FROM isect_a INTERSECT SELECT val FROM isect_b";
    db.create_st("isect_st", q, "1m", "FULL").await;
    db.assert_st_matches_query("isect_st", q).await;

    // Add value to both → appears in intersection
    db.execute("INSERT INTO isect_a (val) VALUES (4)").await;
    db.refresh_st("isect_st").await;
    db.assert_st_matches_query("isect_st", q).await;

    // Remove shared value from one side
    db.execute("DELETE FROM isect_b WHERE val = 2").await;
    db.refresh_st("isect_st").await;
    db.assert_st_matches_query("isect_st", q).await;
}

#[tokio::test]
async fn test_intersect_all_full() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE isect_all_a (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE isect_all_b (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO isect_all_a (val) VALUES (1), (1), (2), (3)")
        .await;
    db.execute("INSERT INTO isect_all_b (val) VALUES (1), (2), (2), (3)")
        .await;

    let q = "SELECT val FROM isect_all_a INTERSECT ALL SELECT val FROM isect_all_b";
    db.create_st("isect_all_st", q, "1m", "FULL").await;
    db.assert_st_matches_query("isect_all_st", q).await;

    // Add duplicate in A
    db.execute("INSERT INTO isect_all_a (val) VALUES (2)").await;
    db.refresh_st("isect_all_st").await;
    db.assert_st_matches_query("isect_all_st", q).await;

    // Remove from B
    db.execute("DELETE FROM isect_all_b WHERE val = 1").await;
    db.refresh_st("isect_all_st").await;
    db.assert_st_matches_query("isect_all_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// EXCEPT
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_except_basic_full() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE exc_a (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE exc_b (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO exc_a (val) VALUES (1), (2), (3)")
        .await;
    db.execute("INSERT INTO exc_b (val) VALUES (2), (4)").await;

    let q = "SELECT val FROM exc_a EXCEPT SELECT val FROM exc_b";
    db.create_st("exc_st", q, "1m", "FULL").await;
    db.assert_st_matches_query("exc_st", q).await;

    // Add to B a value that exists in A → shrinks result
    db.execute("INSERT INTO exc_b (val) VALUES (1)").await;
    db.refresh_st("exc_st").await;
    db.assert_st_matches_query("exc_st", q).await;

    // Add to A a new value
    db.execute("INSERT INTO exc_a (val) VALUES (5)").await;
    db.refresh_st("exc_st").await;
    db.assert_st_matches_query("exc_st", q).await;

    // Remove from B → re-exposes value in A
    db.execute("DELETE FROM exc_b WHERE val = 2").await;
    db.refresh_st("exc_st").await;
    db.assert_st_matches_query("exc_st", q).await;
}

#[tokio::test]
async fn test_except_all_full() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE exc_all_a (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE exc_all_b (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO exc_all_a (val) VALUES (1), (1), (1), (2)")
        .await;
    db.execute("INSERT INTO exc_all_b (val) VALUES (1), (2)")
        .await;

    let q = "SELECT val FROM exc_all_a EXCEPT ALL SELECT val FROM exc_all_b";
    db.create_st("exc_all_st", q, "1m", "FULL").await;
    db.assert_st_matches_query("exc_all_st", q).await;

    // Remove duplicate from A
    db.execute("DELETE FROM exc_all_a WHERE id = (SELECT MIN(id) FROM exc_all_a WHERE val = 1)")
        .await;
    db.refresh_st("exc_all_st").await;
    db.assert_st_matches_query("exc_all_st", q).await;

    // Add to B → more subtractions
    db.execute("INSERT INTO exc_all_b (val) VALUES (1)").await;
    db.refresh_st("exc_all_st").await;
    db.assert_st_matches_query("exc_all_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-way chain (A UNION ALL B EXCEPT C)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_set_ops_chain_full() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE so_a (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE so_b (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE so_c (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO so_a (val) VALUES (1), (2)").await;
    db.execute("INSERT INTO so_b (val) VALUES (3), (4)").await;
    db.execute("INSERT INTO so_c (val) VALUES (2), (3)").await;

    let q = "(SELECT val FROM so_a UNION ALL SELECT val FROM so_b) \
             EXCEPT SELECT val FROM so_c";
    db.create_st("so_chain_st", q, "1m", "FULL").await;
    db.assert_st_matches_query("so_chain_st", q).await;

    db.execute("INSERT INTO so_c (val) VALUES (1)").await;
    db.refresh_st("so_chain_st").await;
    db.assert_st_matches_query("so_chain_st", q).await;

    db.execute("DELETE FROM so_b WHERE val = 4").await;
    db.refresh_st("so_chain_st").await;
    db.assert_st_matches_query("so_chain_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-column set operations
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_intersect_multi_column_full() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE isect_mc_a (id SERIAL PRIMARY KEY, x INT, y TEXT)")
        .await;
    db.execute("CREATE TABLE isect_mc_b (id SERIAL PRIMARY KEY, x INT, y TEXT)")
        .await;
    db.execute("INSERT INTO isect_mc_a (x, y) VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .await;
    db.execute("INSERT INTO isect_mc_b (x, y) VALUES (1, 'a'), (2, 'z'), (4, 'd')")
        .await;

    let q = "SELECT x, y FROM isect_mc_a INTERSECT SELECT x, y FROM isect_mc_b";
    db.create_st("isect_mc_st", q, "1m", "FULL").await;
    db.assert_st_matches_query("isect_mc_st", q).await;

    // Make (2,'b') match by updating B
    db.execute("UPDATE isect_mc_b SET y = 'b' WHERE x = 2")
        .await;
    db.refresh_st("isect_mc_st").await;
    db.assert_st_matches_query("isect_mc_st", q).await;

    // Remove matching row from A
    db.execute("DELETE FROM isect_mc_a WHERE x = 1").await;
    db.refresh_st("isect_mc_st").await;
    db.assert_st_matches_query("isect_mc_st", q).await;
}

#[tokio::test]
async fn test_set_operation_with_nulls() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE set_null_a (id INT, val TEXT)")
        .await;
    db.execute("CREATE TABLE set_null_b (id INT, val TEXT)")
        .await;

    db.execute("INSERT INTO set_null_a VALUES (1, NULL), (NULL, 'A')")
        .await;
    db.execute("INSERT INTO set_null_b VALUES (1, NULL), (NULL, 'B')")
        .await;

    let q = "SELECT id, val FROM set_null_a UNION ALL SELECT id, val FROM set_null_b";

    db.create_st("set_null_st", q, "1m", "DIFFERENTIAL").await;

    db.assert_st_matches_query("set_null_st", q).await;

    db.execute("INSERT INTO set_null_a VALUES (NULL, NULL)")
        .await;
    db.refresh_st("set_null_st").await;

    db.assert_st_matches_query("set_null_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// REL-101-1 / REL-101-3: durable, private per-branch multiplicity state
//
// A FULL refresh of a set-operation stream table always builds a private,
// extension-owned state relation holding the exact left/right branch
// multiplicities keyed by semantic row identity. The state must never leak
// counters or invisible rows into the user-visible stream table, and must be
// cleaned up on drop.
// ═══════════════════════════════════════════════════════════════════════

async fn setop_pgt_id(db: &E2eDb, name: &str) -> i64 {
    db.query_scalar::<i64>(&format!(
        "SELECT pgt_id FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '{name}'"
    ))
    .await
}

#[tokio::test]
async fn test_setop_private_state_intersect_counts_and_privacy() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE ps_a (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE ps_b (id SERIAL PRIMARY KEY, val INT)")
        .await;
    // val=2 appears twice on the left, once on the right.
    db.execute("INSERT INTO ps_a (val) VALUES (1), (2), (2), (3)")
        .await;
    db.execute("INSERT INTO ps_b (val) VALUES (2), (3), (3), (4)")
        .await;

    let q = "SELECT val FROM ps_a INTERSECT SELECT val FROM ps_b";
    db.create_st("ps_isect", q, "1m", "FULL").await;

    // User-visible results are exactly the query result (multiset equality).
    db.assert_st_matches_query("ps_isect", q).await;
    let pgt_id = setop_pgt_id(&db, "ps_isect").await;
    let state_rel = format!("pgtrickle.\"__pgt_setop_state_{pgt_id}\"");

    // The private state relation exists and is catalogued.
    let exists: bool = db
        .query_scalar(&format!("SELECT to_regclass('{state_rel}') IS NOT NULL"))
        .await;
    assert!(exists, "private set-operation state relation must exist");
    let catalogued: i64 = db
        .query_scalar(&format!(
            "SELECT count(*) FROM pgtrickle.pgt_set_operation_states \
             WHERE pgt_id = {pgt_id} AND operation = 'INTERSECT' AND is_all = false"
        ))
        .await;
    assert_eq!(
        catalogued, 1,
        "catalog row must describe the state relation"
    );

    // Counters never leak into the user-visible stream table.
    let leaked: i64 = db
        .query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_name = 'ps_isect' AND column_name LIKE '\\_\\_pgt\\_count%'",
        )
        .await;
    assert_eq!(leaked, 0, "multiplicity counters must not leak into the ST");

    // The private state holds the exact per-branch multiplicities.
    let count_l: i64 = db
        .query_scalar(&format!(
            "SELECT __pgt_count_l FROM {state_rel} WHERE val = 2"
        ))
        .await;
    let count_r: i64 = db
        .query_scalar(&format!(
            "SELECT __pgt_count_r FROM {state_rel} WHERE val = 2"
        ))
        .await;
    assert_eq!((count_l, count_r), (2, 1), "val=2 has 2 left, 1 right");

    // Rebuild after a source change keeps the state exact.
    db.execute("INSERT INTO ps_b (val) VALUES (2)").await;
    db.refresh_st("ps_isect").await;
    db.assert_st_matches_query("ps_isect", q).await;
    let count_r2: i64 = db
        .query_scalar(&format!(
            "SELECT __pgt_count_r FROM {state_rel} WHERE val = 2"
        ))
        .await;
    assert_eq!(count_r2, 2, "right count for val=2 must rebuild to 2");

    // DROP removes the private relation and its catalog rows.
    db.drop_st("ps_isect").await;
    let exists_after: bool = db
        .query_scalar(&format!("SELECT to_regclass('{state_rel}') IS NOT NULL"))
        .await;
    assert!(
        !exists_after,
        "private state relation must be dropped with ST"
    );
    let catalogued_after: i64 = db
        .query_scalar(&format!(
            "SELECT count(*) FROM pgtrickle.pgt_set_operation_states WHERE pgt_id = {pgt_id}"
        ))
        .await;
    assert_eq!(catalogued_after, 0, "catalog rows must be cleaned up");
}

#[tokio::test]
async fn test_setop_private_state_except_tracks_invisible_rows() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE pe_a (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE pe_b (id SERIAL PRIMARY KEY, val INT)")
        .await;
    // val=1 only on the left (visible), val=2 on both (invisible under EXCEPT).
    db.execute("INSERT INTO pe_a (val) VALUES (1), (2), (2)")
        .await;
    db.execute("INSERT INTO pe_b (val) VALUES (2)").await;

    let q = "SELECT val FROM pe_a EXCEPT SELECT val FROM pe_b";
    db.create_st("pe_exc", q, "1m", "FULL").await;

    // The visible ST contains only visible rows (val=1) — never the invisible
    // row val=2.
    db.assert_st_matches_query("pe_exc", q).await;
    let visible_has_2: i64 = db
        .query_scalar("SELECT count(*) FROM pe_exc WHERE val = 2")
        .await;
    assert_eq!(
        visible_has_2, 0,
        "invisible EXCEPT rows must not appear in ST"
    );

    let pgt_id = setop_pgt_id(&db, "pe_exc").await;
    let state_rel = format!("pgtrickle.\"__pgt_setop_state_{pgt_id}\"");

    // The private state DOES track the invisible row's per-branch counts so a
    // future differential path can restore it if the right branch shrinks.
    let l: i64 = db
        .query_scalar(&format!(
            "SELECT __pgt_count_l FROM {state_rel} WHERE val = 2"
        ))
        .await;
    let r: i64 = db
        .query_scalar(&format!(
            "SELECT __pgt_count_r FROM {state_rel} WHERE val = 2"
        ))
        .await;
    assert_eq!((l, r), (2, 1), "val=2 tracked with 2 left, 1 right");

    db.drop_st("pe_exc").await;
    let exists_after: bool = db
        .query_scalar(&format!("SELECT to_regclass('{state_rel}') IS NOT NULL"))
        .await;
    assert!(
        !exists_after,
        "private state relation must be dropped with ST"
    );
}

#[tokio::test]
async fn test_setop_private_state_built_for_all_forms() {
    // Private multiplicity state is built for every set-operation form on FULL
    // refresh, without any configuration. The visible ST stays exact; the
    // counters live only in the private state relation.
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE pd_a (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE pd_b (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO pd_a (val) VALUES (1), (2), (2)")
        .await;
    db.execute("INSERT INTO pd_b (val) VALUES (2), (3)").await;

    let forms = [
        ("pd_isect", "INTERSECT", "INTERSECT", false),
        ("pd_isect_all", "INTERSECT ALL", "INTERSECT", true),
        ("pd_exc", "EXCEPT", "EXCEPT", false),
        ("pd_exc_all", "EXCEPT ALL", "EXCEPT", true),
    ];

    for (name, op_sql, op_catalog, is_all) in forms {
        let q = format!("SELECT val FROM pd_a {op_sql} SELECT val FROM pd_b");
        db.create_st(name, &q, "1m", "FULL").await;
        // Visible result stays exactly correct.
        db.assert_st_matches_query(name, &q).await;

        let pgt_id = setop_pgt_id(&db, name).await;
        let state_rel = format!("pgtrickle.\"__pgt_setop_state_{pgt_id}\"");
        let exists: bool = db
            .query_scalar(&format!("SELECT to_regclass('{state_rel}') IS NOT NULL"))
            .await;
        assert!(exists, "{name}: private state relation must be built");

        // Catalog row records the operation/is_all classification.
        let catalogued: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM pgtrickle.pgt_set_operation_states \
                 WHERE pgt_id = {pgt_id} AND operation = '{op_catalog}' AND is_all = {is_all}"
            ))
            .await;
        assert_eq!(catalogued, 1, "{name}: catalog row must classify the form");

        // Counters never leak into the visible stream table.
        let leaked: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM information_schema.columns \
                 WHERE table_name = '{name}' AND column_name LIKE '\\_\\_pgt\\_count%'"
            ))
            .await;
        assert_eq!(leaked, 0, "{name}: counters must not leak into the ST");

        // val=2 has multiplicity 2 on the left and 1 on the right in every form.
        let count_l: i64 = db
            .query_scalar(&format!(
                "SELECT __pgt_count_l FROM {state_rel} WHERE val = 2"
            ))
            .await;
        let count_r: i64 = db
            .query_scalar(&format!(
                "SELECT __pgt_count_r FROM {state_rel} WHERE val = 2"
            ))
            .await;
        assert_eq!(
            (count_l, count_r),
            (2, 1),
            "{name}: val=2 tracked with 2 left, 1 right"
        );

        db.drop_st(name).await;
        let exists_after: bool = db
            .query_scalar(&format!("SELECT to_regclass('{state_rel}') IS NOT NULL"))
            .await;
        assert!(
            !exists_after,
            "{name}: private state must be dropped with ST"
        );
    }
}
