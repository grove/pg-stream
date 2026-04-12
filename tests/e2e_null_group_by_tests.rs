//! CORR-3: NULL-safe GROUP BY elimination under deletes.
//!
//! Validates that groups keyed by NULL values are correctly removed from the
//! stream table when all member rows are deleted, and that differential
//! refresh produces correct results for NULL-keyed groups under
//! INSERT/UPDATE/DELETE workloads.

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// Full group deletion with NULL group key
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_null_group_by_full_delete() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ng_del (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute("INSERT INTO ng_del (grp, val) VALUES (NULL, 10), (NULL, 20), ('a', 30)")
        .await;

    let q = "SELECT grp, SUM(val) AS total FROM ng_del GROUP BY grp";
    db.create_st("ng_del_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ng_del_st", q).await;

    // Delete all NULL-group rows → NULL group must disappear
    db.execute("DELETE FROM ng_del WHERE grp IS NULL").await;
    db.refresh_st("ng_del_st").await;
    db.assert_st_matches_query("ng_del_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Partial delete then full delete of NULL group
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_null_group_by_partial_then_full_delete() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ng_part (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute(
        "INSERT INTO ng_part (grp, val) VALUES \
         (NULL, 10), (NULL, 20), (NULL, 30), ('b', 50)",
    )
    .await;

    let q = "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM ng_part GROUP BY grp";
    db.create_st("ng_part_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ng_part_st", q).await;

    // Partial delete: remove one NULL-group row
    db.execute("DELETE FROM ng_part WHERE grp IS NULL AND val = 10")
        .await;
    db.refresh_st("ng_part_st").await;
    db.assert_st_matches_query("ng_part_st", q).await;

    // Full delete: remove remaining NULL-group rows
    db.execute("DELETE FROM ng_part WHERE grp IS NULL").await;
    db.refresh_st("ng_part_st").await;
    db.assert_st_matches_query("ng_part_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// NULL group key with INSERT after full delete
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_null_group_by_reappear_after_delete() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ng_reap (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute("INSERT INTO ng_reap (grp, val) VALUES (NULL, 100), ('a', 50)")
        .await;

    let q = "SELECT grp, SUM(val) AS total FROM ng_reap GROUP BY grp";
    db.create_st("ng_reap_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ng_reap_st", q).await;

    // Delete all NULL rows
    db.execute("DELETE FROM ng_reap WHERE grp IS NULL").await;
    db.refresh_st("ng_reap_st").await;
    db.assert_st_matches_query("ng_reap_st", q).await;

    // Re-add NULL group
    db.execute("INSERT INTO ng_reap (grp, val) VALUES (NULL, 999)")
        .await;
    db.refresh_st("ng_reap_st").await;
    db.assert_st_matches_query("ng_reap_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-column GROUP BY with mixed NULLs
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_multi_column_null_group_by_delete() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ng_multi (id SERIAL PRIMARY KEY, g1 TEXT, g2 TEXT, val INT)")
        .await;
    db.execute(
        "INSERT INTO ng_multi (g1, g2, val) VALUES \
         (NULL, NULL, 10), (NULL, NULL, 20), \
         (NULL, 'x', 30), ('a', NULL, 40), ('a', 'x', 50)",
    )
    .await;

    let q = "SELECT g1, g2, SUM(val) AS total FROM ng_multi GROUP BY g1, g2";
    db.create_st("ng_multi_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ng_multi_st", q).await;

    // Delete all (NULL, NULL) rows
    db.execute("DELETE FROM ng_multi WHERE g1 IS NULL AND g2 IS NULL")
        .await;
    db.refresh_st("ng_multi_st").await;
    db.assert_st_matches_query("ng_multi_st", q).await;

    // Delete (NULL, 'x') rows
    db.execute("DELETE FROM ng_multi WHERE g1 IS NULL AND g2 = 'x'")
        .await;
    db.refresh_st("ng_multi_st").await;
    db.assert_st_matches_query("ng_multi_st", q).await;

    // Delete ('a', NULL) rows
    db.execute("DELETE FROM ng_multi WHERE g1 = 'a' AND g2 IS NULL")
        .await;
    db.refresh_st("ng_multi_st").await;
    db.assert_st_matches_query("ng_multi_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// NULL GROUP BY with HAVING (CORR-3 + CORR-5 interaction)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_null_group_by_having_threshold_down() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ng_hav (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute(
        "INSERT INTO ng_hav (grp, val) VALUES \
         (NULL, 30), (NULL, 20), ('a', 100)",
    )
    .await;

    let q = "SELECT grp, SUM(val) AS total FROM ng_hav GROUP BY grp HAVING SUM(val) > 25";
    db.create_st("ng_hav_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ng_hav_st", q).await;

    // Delete from NULL group so sum drops below threshold (30→20)
    db.execute("DELETE FROM ng_hav WHERE grp IS NULL AND val = 30")
        .await;
    db.refresh_st("ng_hav_st").await;
    db.assert_st_matches_query("ng_hav_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// UPDATE moves row from non-NULL group into NULL group
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_null_group_by_update_into_null() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ng_upd (id SERIAL PRIMARY KEY, grp TEXT, val INT)")
        .await;
    db.execute("INSERT INTO ng_upd (grp, val) VALUES ('a', 10), ('a', 20), ('b', 30)")
        .await;

    let q = "SELECT grp, SUM(val) AS total FROM ng_upd GROUP BY grp";
    db.create_st("ng_upd_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ng_upd_st", q).await;

    // Move a row into NULL group
    db.execute("UPDATE ng_upd SET grp = NULL WHERE val = 10")
        .await;
    db.refresh_st("ng_upd_st").await;
    db.assert_st_matches_query("ng_upd_st", q).await;

    // Move all 'a' rows to NULL
    db.execute("UPDATE ng_upd SET grp = NULL WHERE grp = 'a'")
        .await;
    db.refresh_st("ng_upd_st").await;
    db.assert_st_matches_query("ng_upd_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// COUNT(*) with NULL group key (no SUM involved)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_null_group_by_count_only() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ng_cnt (id SERIAL PRIMARY KEY, grp TEXT, label TEXT)")
        .await;
    db.execute(
        "INSERT INTO ng_cnt (grp, label) VALUES \
         (NULL, 'x'), (NULL, 'y'), ('a', 'z')",
    )
    .await;

    let q = "SELECT grp, COUNT(*) AS cnt FROM ng_cnt GROUP BY grp";
    db.create_st("ng_cnt_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ng_cnt_st", q).await;

    // Delete one from NULL group
    db.execute("DELETE FROM ng_cnt WHERE grp IS NULL AND label = 'x'")
        .await;
    db.refresh_st("ng_cnt_st").await;
    db.assert_st_matches_query("ng_cnt_st", q).await;

    // Delete all from NULL group
    db.execute("DELETE FROM ng_cnt WHERE grp IS NULL").await;
    db.refresh_st("ng_cnt_st").await;
    db.assert_st_matches_query("ng_cnt_st", q).await;
}
