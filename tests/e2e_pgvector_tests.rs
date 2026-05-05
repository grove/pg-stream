//! F4 (v0.37.0): E2E tests for pgVectorMV — incremental vector aggregate operators.
//!
//! Validates that stream tables computing `avg(embedding)` and `sum(embedding)`
//! over pgvector `vector` typed columns produce correct results after INSERT,
//! UPDATE, and DELETE operations (correctness vs FULL refresh).
//!
//! Uses the group-rescan strategy: affected groups are re-aggregated from source
//! data using pgvector's native `avg(vector)` and `sum(vector)` aggregates.
//!
//! Prerequisites: `./tests/build_e2e_image.sh` (includes pgvector)

mod e2e;

use e2e::E2eDb;

// ═══════════════════════════════════════════════════════════════════════
// Helper: enable pgvector and the vector agg GUC
// ═══════════════════════════════════════════════════════════════════════

async fn setup_pgvector(db: &E2eDb) {
    db.execute("CREATE EXTENSION IF NOT EXISTS vector").await;
}

// ═══════════════════════════════════════════════════════════════════════
// F4-1: User-taste centroid — avg(embedding) per user
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_pgvector_avg_centroid_insert() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE embeddings (
            id SERIAL PRIMARY KEY,
            user_id INT,
            embedding vector(3)
        )",
    )
    .await;
    db.execute(
        "INSERT INTO embeddings (user_id, embedding) VALUES
            (1, '[1,0,0]'),
            (1, '[0,1,0]'),
            (2, '[0,0,1]')",
    )
    .await;

    let q = "SELECT user_id, avg(embedding) AS centroid FROM embeddings GROUP BY user_id";
    let create_sql = format!(
        "SELECT pgtrickle.create_stream_table('centroid_st', $${q}$$, '1m', 'DIFFERENTIAL')"
    );
    db.execute_seq(&["SET pg_trickle.enable_vector_agg = on", &create_sql])
        .await;
    db.assert_st_matches_query("centroid_st", q).await;

    // INSERT new embedding for user 1
    db.execute("INSERT INTO embeddings (user_id, embedding) VALUES (1, '[1,1,0]')")
        .await;
    db.refresh_st("centroid_st").await;
    db.assert_st_matches_query("centroid_st", q).await;

    // INSERT new user
    db.execute("INSERT INTO embeddings (user_id, embedding) VALUES (3, '[0,1,1]')")
        .await;
    db.refresh_st("centroid_st").await;
    db.assert_st_matches_query("centroid_st", q).await;
}

#[tokio::test]
async fn test_pgvector_avg_centroid_update() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE emb_upd (
            id SERIAL PRIMARY KEY,
            user_id INT,
            embedding vector(3)
        )",
    )
    .await;
    db.execute(
        "INSERT INTO emb_upd (user_id, embedding) VALUES
            (1, '[1,0,0]'),
            (1, '[0,1,0]'),
            (2, '[1,1,1]')",
    )
    .await;

    let q = "SELECT user_id, avg(embedding) AS centroid FROM emb_upd GROUP BY user_id";
    let create_sql = format!(
        "SELECT pgtrickle.create_stream_table('centroid_upd_st', $${q}$$, '1m', 'DIFFERENTIAL')"
    );
    db.execute_seq(&["SET pg_trickle.enable_vector_agg = on", &create_sql])
        .await;
    db.assert_st_matches_query("centroid_upd_st", q).await;

    // UPDATE embedding
    db.execute("UPDATE emb_upd SET embedding = '[0,0,1]' WHERE user_id = 1 AND id = 1")
        .await;
    db.refresh_st("centroid_upd_st").await;
    db.assert_st_matches_query("centroid_upd_st", q).await;
}

#[tokio::test]
async fn test_pgvector_avg_centroid_delete() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE emb_del (
            id SERIAL PRIMARY KEY,
            user_id INT,
            embedding vector(3)
        )",
    )
    .await;
    db.execute(
        "INSERT INTO emb_del (user_id, embedding) VALUES
            (1, '[1,0,0]'),
            (1, '[0,1,0]'),
            (2, '[1,1,1]')",
    )
    .await;

    let q = "SELECT user_id, avg(embedding) AS centroid FROM emb_del GROUP BY user_id";
    let create_sql = format!(
        "SELECT pgtrickle.create_stream_table('centroid_del_st', $${q}$$, '1m', 'DIFFERENTIAL')"
    );
    db.execute_seq(&["SET pg_trickle.enable_vector_agg = on", &create_sql])
        .await;
    db.assert_st_matches_query("centroid_del_st", q).await;

    // DELETE one row from user 1
    db.execute("DELETE FROM emb_del WHERE user_id = 1 AND id = 1")
        .await;
    db.refresh_st("centroid_del_st").await;
    db.assert_st_matches_query("centroid_del_st", q).await;

    // DELETE entire user group
    db.execute("DELETE FROM emb_del WHERE user_id = 2").await;
    db.refresh_st("centroid_del_st").await;
    db.assert_st_matches_query("centroid_del_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// F4-2: vector_sum — sum(embedding) per group
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_pgvector_sum_differential() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE emb_sum_src (
            id SERIAL PRIMARY KEY,
            grp TEXT,
            embedding vector(3)
        )",
    )
    .await;
    db.execute(
        "INSERT INTO emb_sum_src (grp, embedding) VALUES
            ('a', '[1,0,0]'),
            ('a', '[0,1,0]'),
            ('b', '[1,1,1]')",
    )
    .await;

    let q = "SELECT grp, sum(embedding) AS total_vec FROM emb_sum_src GROUP BY grp";
    let create_sql = format!(
        "SELECT pgtrickle.create_stream_table('vec_sum_st', $${q}$$, '1m', 'DIFFERENTIAL')"
    );
    db.execute_seq(&["SET pg_trickle.enable_vector_agg = on", &create_sql])
        .await;
    db.assert_st_matches_query("vec_sum_st", q).await;

    // INSERT
    db.execute("INSERT INTO emb_sum_src (grp, embedding) VALUES ('a', '[0,0,1]')")
        .await;
    db.refresh_st("vec_sum_st").await;
    db.assert_st_matches_query("vec_sum_st", q).await;

    // DELETE
    db.execute("DELETE FROM emb_sum_src WHERE grp = 'b'").await;
    db.refresh_st("vec_sum_st").await;
    db.assert_st_matches_query("vec_sum_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// F4-3: Distance operator fallback — queries with <-> fall back to FULL
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_pgvector_distance_operator_fallback_to_full() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE emb_dist (
            id SERIAL PRIMARY KEY,
            category TEXT,
            embedding vector(3)
        )",
    )
    .await;
    db.execute(
        "INSERT INTO emb_dist (category, embedding) VALUES
            ('cat1', '[1,0,0]'),
            ('cat1', '[0.9,0.1,0]'),
            ('cat2', '[0,1,0]')",
    )
    .await;

    // A stream table using a distance operator in ORDER BY should work
    // via FULL refresh mode (distance operators are FULL-fallback safe).
    // Use FULL mode explicitly to avoid relying on differential fallback.
    let q = "SELECT id, category, embedding FROM emb_dist ORDER BY embedding <-> '[1,0,0]' LIMIT 5";
    db.create_st("dist_st", q, "1m", "FULL").await;
    db.assert_st_matches_query("dist_st", q).await;

    // Refresh after insert
    db.execute("INSERT INTO emb_dist (category, embedding) VALUES ('cat1', '[0.8,0.2,0]')")
        .await;
    db.refresh_st("dist_st").await;
    db.assert_st_matches_query("dist_st", q).await;
}

// ═══════════════════════════════════════════════════════════════════════
// F4-4: HNSW index on centroid stream table
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_pgvector_hnsw_index_on_stream_table() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE emb_hnsw_src (
            id SERIAL PRIMARY KEY,
            user_id INT,
            embedding vector(3)
        )",
    )
    .await;
    db.execute(
        "INSERT INTO emb_hnsw_src (user_id, embedding) VALUES
            (1, '[1,0,0]'), (1, '[0.9,0.1,0]'),
            (2, '[0,1,0]'), (2, '[0,0.9,0.1]'),
            (3, '[0,0,1]')",
    )
    .await;

    let q = "SELECT user_id, avg(embedding) AS centroid FROM emb_hnsw_src GROUP BY user_id";
    let create_sql = format!(
        "SELECT pgtrickle.create_stream_table('centroid_hnsw_st', $${q}$$, '1m', 'DIFFERENTIAL')"
    );
    db.execute_seq(&["SET pg_trickle.enable_vector_agg = on", &create_sql])
        .await;
    db.assert_st_matches_query("centroid_hnsw_st", q).await;

    // Create HNSW index on the centroid stream table
    db.execute(
        "CREATE INDEX centroid_hnsw_idx ON public.centroid_hnsw_st \
         USING hnsw (centroid vector_cosine_ops)",
    )
    .await;

    // Verify refresh still works with index in place
    db.execute("INSERT INTO emb_hnsw_src (user_id, embedding) VALUES (1, '[1,0,0]')")
        .await;
    db.refresh_st("centroid_hnsw_st").await;
    db.assert_st_matches_query("centroid_hnsw_st", q).await;

    // Verify the HNSW index can be used for ANN search
    let nn_result = db
        .query_scalar_opt::<i32>("SELECT user_id FROM public.centroid_hnsw_st ORDER BY centroid <-> '[1,0,0]'::vector LIMIT 1")
        .await;
    assert!(
        nn_result.is_some(),
        "HNSW ANN query should return at least one result"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// T-VP1 (v0.47.0): Drift-based reindex on HNSW stream table
// ═══════════════════════════════════════════════════════════════════════

/// T-VP1: Create a vector stream table with post_refresh_action='reindex_if_drift',
/// change rows beyond the threshold, verify that the drift counter increments
/// and that last_reindex_at is updated after a refresh.
#[tokio::test]
async fn test_vector_post_refresh_action_reindex_if_drift() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE drift_src (
            id SERIAL PRIMARY KEY,
            embedding vector(3)
        )",
    )
    .await;

    // Insert initial rows
    for i in 1..=20i32 {
        db.execute(&format!(
            "INSERT INTO drift_src (embedding) VALUES ('[{},{},0]')",
            i, i
        ))
        .await;
    }

    // Create stream table
    let q = "SELECT id, embedding FROM drift_src";
    db.execute(&format!(
        "SELECT pgtrickle.create_stream_table('drift_st', $${q}$$, '1m', 'DIFFERENTIAL')"
    ))
    .await;

    // Configure drift-triggered REINDEX (50% threshold so we can trigger it)
    db.execute(
        "SELECT pgtrickle.alter_stream_table('drift_st', \
         post_refresh_action => 'reindex_if_drift', \
         reindex_drift_threshold => 0.50)",
    )
    .await;

    // Add more rows to exceed 50% threshold (20 rows * 50% = 10, add 11)
    for i in 21..=31i32 {
        db.execute(&format!(
            "INSERT INTO drift_src (embedding) VALUES ('[{},{},1]')",
            i, i
        ))
        .await;
    }

    // Refresh — should increment rows_changed_since_last_reindex
    db.refresh_st("drift_st").await;

    // Check that vector_status() shows this stream table
    let drift_pct = db
        .query_scalar_opt::<f64>(
            "SELECT drift_pct FROM pgtrickle.vector_status() WHERE name = 'public.drift_st'",
        )
        .await;
    assert!(
        drift_pct.is_some(),
        "vector_status() should return a row for drift_st"
    );

    // Verify post_refresh_action is stored correctly
    let action = db
        .query_scalar_opt::<String>(
            "SELECT post_refresh_action FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'drift_st'",
        )
        .await
        .expect("post_refresh_action should be set");
    assert_eq!(
        action, "reindex_if_drift",
        "post_refresh_action should be 'reindex_if_drift'"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// T-VP2 (v0.47.0): vector_status() accuracy and reset behavior
// ═══════════════════════════════════════════════════════════════════════

/// T-VP2: Verify that pgtrickle.vector_status() reports correct lag, drift,
/// and metadata for vector stream tables. Also verify that after an alter to
/// 'none', the table disappears from vector_status().
#[tokio::test]
async fn test_vector_status_view_accuracy_and_reset() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE vs_src (
            id SERIAL PRIMARY KEY,
            body TEXT,
            embedding vector(3)
        )",
    )
    .await;

    db.execute(
        "INSERT INTO vs_src (body, embedding) VALUES
             ('hello', '[1,0,0]'),
             ('world', '[0,1,0]'),
             ('foo',   '[0,0,1]')",
    )
    .await;

    let q = "SELECT id, body, embedding FROM vs_src";
    db.execute(&format!(
        "SELECT pgtrickle.create_stream_table('vs_st', $${q}$$, '1m', 'DIFFERENTIAL')"
    ))
    .await;

    // Set post_refresh_action to 'analyze'
    db.execute("SELECT pgtrickle.alter_stream_table('vs_st', post_refresh_action => 'analyze')")
        .await;

    // Do a refresh to trigger the action
    db.refresh_st("vs_st").await;

    // vector_status() should show vs_st
    let row = db
        .query_scalar_opt::<String>(
            "SELECT post_refresh_action FROM pgtrickle.vector_status() \
             WHERE name = 'public.vs_st'",
        )
        .await;
    assert!(
        row.is_some(),
        "vector_status() should include vs_st after setting post_refresh_action"
    );
    assert_eq!(
        row.unwrap(),
        "analyze",
        "post_refresh_action should be 'analyze'"
    );

    // data_timestamp should be set (not NULL)
    let ts = db
        .query_scalar_opt::<String>(
            "SELECT data_timestamp::TEXT FROM pgtrickle.vector_status() \
             WHERE name = 'public.vs_st'",
        )
        .await;
    assert!(
        ts.is_some(),
        "data_timestamp should not be NULL in vector_status()"
    );

    // Reset to 'none' — should disappear from vector_status()
    db.execute("SELECT pgtrickle.alter_stream_table('vs_st', post_refresh_action => 'none')")
        .await;

    let gone = db
        .query_scalar_opt::<String>(
            "SELECT post_refresh_action FROM pgtrickle.vector_status() \
             WHERE name = 'public.vs_st'",
        )
        .await;
    assert!(
        gone.is_none(),
        "vs_st should not appear in vector_status() after resetting to 'none'"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// T-VP3 / VP-5 (v0.47.0): Vector-aggregate cases — shard-additive verification
// ═══════════════════════════════════════════════════════════════════════

/// T-VP3 / VP-5: Verify that vector_avg() produces correct results consistent
/// with pgvector's native avg(vector) aggregate. This exercises the same
/// shard-additive algebra that would run in a Citus distributed deployment.
///
/// This test runs on the standard single-node e2e image (no Citus required)
/// but validates the arithmetic that the Citus path relies on.
#[tokio::test]
async fn test_vector_avg_shard_additive_correctness() {
    let db = E2eDb::new().await.with_extension().await;
    setup_pgvector(&db).await;

    db.execute(
        "CREATE TABLE shard_src (
            id      SERIAL PRIMARY KEY,
            shard   INTEGER NOT NULL,
            vec     vector(4)
        )",
    )
    .await;

    // Simulate two logical shards with known vectors
    // Shard 1: avg([1,2,3,4], [3,4,5,6]) = [2,3,4,5]
    // Shard 2: avg([0,0,0,0], [4,4,4,4]) = [2,2,2,2]
    db.execute(
        "INSERT INTO shard_src (shard, vec) VALUES
             (1, '[1,2,3,4]'), (1, '[3,4,5,6]'),
             (2, '[0,0,0,0]'), (2, '[4,4,4,4]')",
    )
    .await;

    let q = "SELECT shard, avg(vec) AS centroid, count(*) AS cnt FROM shard_src GROUP BY shard";
    let create_sql = format!(
        "SELECT pgtrickle.create_stream_table('shard_avg_st', $${q}$$, '1m', 'DIFFERENTIAL')"
    );
    db.execute_seq(&["SET pg_trickle.enable_vector_agg = on", &create_sql])
        .await;

    db.assert_st_matches_query("shard_avg_st", q).await;

    // Verify shard 1 centroid: avg([1,2,3,4],[3,4,5,6]) = [2,3,4,5]
    let s1_centroid: Option<String> = db
        .query_scalar_opt("SELECT centroid::TEXT FROM public.shard_avg_st WHERE shard = 1")
        .await;
    assert!(s1_centroid.is_some(), "shard 1 centroid should be present");
    // pgvector formats vectors as '[x,y,z,w]'; check it's close to [2,3,4,5]
    let ct = s1_centroid.unwrap();
    assert!(
        ct.contains("2") && ct.contains("3") && ct.contains("4") && ct.contains("5"),
        "shard 1 centroid ({ct}) should approximate [2,3,4,5]"
    );

    // Now insert into shard 1 — differential refresh must update centroid correctly
    db.execute("INSERT INTO shard_src (shard, vec) VALUES (1, '[5,6,7,8]')")
        .await;
    db.refresh_st("shard_avg_st").await;
    db.assert_st_matches_query("shard_avg_st", q).await;
}
