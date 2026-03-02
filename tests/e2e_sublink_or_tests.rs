//! E2E tests for EXISTS/IN with OR in WHERE clause (F21: G3.3).
//!
//! Validates sublink expressions (EXISTS, NOT EXISTS, IN) combined with OR
//! under differential refresh.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// EXISTS ... OR col = value
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_exists_or_column_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE eo_orders (id SERIAL PRIMARY KEY, customer TEXT, total INT)")
        .await;
    db.execute("CREATE TABLE eo_vip (id SERIAL PRIMARY KEY, customer TEXT)")
        .await;
    db.execute("INSERT INTO eo_orders (customer, total) VALUES ('a', 100), ('b', 200), ('c', 50)")
        .await;
    db.execute("INSERT INTO eo_vip (customer) VALUES ('a')")
        .await;

    let q = "SELECT o.customer, o.total FROM eo_orders o \
             WHERE EXISTS (SELECT 1 FROM eo_vip v WHERE v.customer = o.customer) \
             OR o.total > 150";
    db.create_st("eo_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("eo_st", q).await;

    // Add to VIP — row should now qualify via EXISTS path
    db.execute("INSERT INTO eo_vip (customer) VALUES ('c')")
        .await;
    db.refresh_st("eo_st").await;
    db.assert_st_matches_query("eo_st", q).await;

    // Remove from VIP — row 'c' now only qualifies if total > 150 (it doesn't => gone)
    db.execute("DELETE FROM eo_vip WHERE customer = 'c'").await;
    db.refresh_st("eo_st").await;
    db.assert_st_matches_query("eo_st", q).await;

    // Update total to qualify via OR
    db.execute("UPDATE eo_orders SET total = 999 WHERE customer = 'c'")
        .await;
    db.refresh_st("eo_st").await;
    db.assert_st_matches_query("eo_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// IN ... OR col = value
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_in_or_column_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE io_items (id SERIAL PRIMARY KEY, cat TEXT, price INT)")
        .await;
    db.execute("CREATE TABLE io_promo (id SERIAL PRIMARY KEY, cat TEXT)")
        .await;
    db.execute("INSERT INTO io_items (cat, price) VALUES ('a', 10), ('b', 50), ('c', 100)")
        .await;
    db.execute("INSERT INTO io_promo (cat) VALUES ('a')").await;

    let q = "SELECT i.cat, i.price FROM io_items i \
             WHERE i.cat IN (SELECT p.cat FROM io_promo p) \
             OR i.price >= 100";
    db.create_st("io_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("io_st", q).await;

    // Add promo for 'b'
    db.execute("INSERT INTO io_promo (cat) VALUES ('b')").await;
    db.refresh_st("io_st").await;
    db.assert_st_matches_query("io_st", q).await;

    // Remove promo for 'a'
    db.execute("DELETE FROM io_promo WHERE cat = 'a'").await;
    db.refresh_st("io_st").await;
    db.assert_st_matches_query("io_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// NOT EXISTS ... OR EXISTS
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_not_exists_or_exists_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE ne_main (id SERIAL PRIMARY KEY, code TEXT, val INT)")
        .await;
    db.execute("CREATE TABLE ne_block (id SERIAL PRIMARY KEY, code TEXT)")
        .await;
    db.execute("CREATE TABLE ne_fast (id SERIAL PRIMARY KEY, code TEXT)")
        .await;
    db.execute("INSERT INTO ne_main (code, val) VALUES ('a', 1), ('b', 2), ('c', 3)")
        .await;
    db.execute("INSERT INTO ne_block (code) VALUES ('a')").await;
    db.execute("INSERT INTO ne_fast (code) VALUES ('c')").await;

    let q = "SELECT m.code, m.val FROM ne_main m \
             WHERE NOT EXISTS (SELECT 1 FROM ne_block b WHERE b.code = m.code) \
             OR EXISTS (SELECT 1 FROM ne_fast f WHERE f.code = m.code)";
    db.create_st("ne_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("ne_st", q).await;

    // Unblock 'a'
    db.execute("DELETE FROM ne_block WHERE code = 'a'").await;
    db.refresh_st("ne_st").await;
    db.assert_st_matches_query("ne_st", q).await;

    // Block 'b' and remove from fast
    db.execute("INSERT INTO ne_block (code) VALUES ('b')").await;
    db.execute("DELETE FROM ne_fast WHERE code = 'c'").await;
    db.refresh_st("ne_st").await;
    db.assert_st_matches_query("ne_st", q).await;

    // Add fast-track for 'b' → qualifies via EXISTS even though blocked
    db.execute("INSERT INTO ne_fast (code) VALUES ('b')").await;
    db.refresh_st("ne_st").await;
    db.assert_st_matches_query("ne_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// EXISTS with aggregate in subquery
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "DVM: correlated EXISTS with HAVING uses recomputation diff that loses aggregate state (ROADMAP)"]
async fn test_exists_with_having_in_subquery_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE eh_cust (id SERIAL PRIMARY KEY, name TEXT)")
        .await;
    db.execute("CREATE TABLE eh_ord (id SERIAL PRIMARY KEY, cust_id INT, amount INT)")
        .await;
    db.execute("INSERT INTO eh_cust (id, name) VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')")
        .await;
    db.execute("INSERT INTO eh_ord (cust_id, amount) VALUES (1, 100), (1, 200), (2, 50)")
        .await;

    let q = "SELECT c.name FROM eh_cust c \
             WHERE EXISTS ( \
                 SELECT 1 FROM eh_ord o WHERE o.cust_id = c.id \
                 GROUP BY o.cust_id HAVING SUM(o.amount) > 100 \
             )";
    db.create_st("eh_st", q, "1m", "DIFFERENTIAL").await;
    db.assert_st_matches_query("eh_st", q).await;

    // Bob's total crosses threshold
    db.execute("INSERT INTO eh_ord (cust_id, amount) VALUES (2, 200)")
        .await;
    db.refresh_st("eh_st").await;
    db.assert_st_matches_query("eh_st", q).await;

    // Alice's total drops below
    db.execute("DELETE FROM eh_ord WHERE cust_id = 1 AND amount = 200")
        .await;
    db.refresh_st("eh_st").await;
    db.assert_st_matches_query("eh_st", q).await;
}
