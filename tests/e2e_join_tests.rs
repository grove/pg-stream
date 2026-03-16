mod common;
use crate::common::*;
#[tokio::test]
async fn test_full_join_multi_row_unmatched() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE fj_multi_left (id INT, val TEXT)")
        .await;
    db.execute("CREATE TABLE fj_multi_right (id INT, val TEXT)")
        .await;

    db.execute("INSERT INTO fj_multi_left VALUES (1, 'L1'), (2, 'L2')")
        .await;
    db.execute("INSERT INTO fj_multi_right VALUES (3, 'R3'), (4, 'R4'), (5, 'R5')")
        .await;

    let q = "SELECT l.id as lid, r.id as rid FROM fj_multi_left l FULL JOIN fj_multi_right r ON l.id = r.id";

    db.create_st("fj_multi_st", q, "1m", "DIFFERENTIAL").await;

    db.assert_st_matches_query("fj_multi_st", q).await;

    // Add match
    db.execute("INSERT INTO fj_multi_left VALUES (3, 'L3')")
        .await;
    db.refresh_st("fj_multi_st").await;

    db.assert_st_matches_query("fj_multi_st", q).await;
}
