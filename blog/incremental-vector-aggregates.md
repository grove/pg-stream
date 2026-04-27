[← Back to Blog Index](README.md)

# Incremental Vector Aggregates: Building Recommendation Engines in Pure SQL

## How `vector_avg` turns PostgreSQL into a real-time personalization engine

---

You have a recommendation system. Users interact with items. Each item has an embedding vector. You want to recommend new items based on what a user has liked before.

The standard approach: compute the average embedding of all items a user has liked (their "taste vector"), then find items closest to that vector in embedding space. This is collaborative filtering by cosine similarity. It works well. It's mathematically sound. And at scale, it falls apart.

Not because the math is wrong. Because keeping a million taste vectors up-to-date as users interact with items — thousands of times per second — is an infrastructure problem that nobody has a clean solution for.

Until now.

---

## The Batch Job You're Running Today

Here's what most recommendation systems look like on the backend:

```python
# runs nightly (or hourly if you're lucky)
for user_id in get_all_active_users():
    interactions = db.query("""
        SELECT e.embedding
        FROM user_actions ua
        JOIN item_embeddings e ON e.item_id = ua.item_id
        WHERE ua.user_id = %s AND ua.action = 'liked'
    """, user_id)

    if interactions:
        taste_vec = np.mean([r.embedding for r in interactions], axis=0)
        db.execute("""
            INSERT INTO user_taste_vectors (user_id, taste_vec, updated_at)
            VALUES (%s, %s, NOW())
            ON CONFLICT (user_id) DO UPDATE
            SET taste_vec = EXCLUDED.taste_vec,
                updated_at = EXCLUDED.updated_at
        """, user_id, taste_vec)
```

This is a full scan of every active user's interaction history. For a system with 500,000 active users averaging 200 interactions each, that's 100 million rows read, 500,000 vector means computed, and 500,000 upserts — every run.

At nightly cadence, recommendations are up to 24 hours stale. A user likes 10 items in a session. The recommendations don't reflect any of them until tomorrow.

At hourly cadence, you're processing 100 million rows per hour even if only 1,000 users actually did anything. The batch job doesn't know *who* changed — it recomputes everyone.

The smarter version tracks "users who interacted since last run" and only recomputes those. This helps, but you're still doing a full history scan per changed user. User 42 has 3,000 past interactions and just liked one new item. You read all 3,001 embeddings, compute the mean, write it back. One new interaction cost you a 3,001-row scan.

And you're maintaining all of this in application code — the change tracking, the batch scheduler, the failure handling, the monitoring. It's derived data maintenance disguised as a recommendation system.

---

## Why `AVG(vector)` Is Hard in a Traditional View

If you could just write a materialized view, you would:

```sql
CREATE MATERIALIZED VIEW user_taste AS
SELECT
  ua.user_id,
  avg(e.embedding) AS taste_vec,
  count(*) AS interaction_count
FROM user_actions ua
JOIN item_embeddings e ON e.item_id = ua.item_id
WHERE ua.action = 'liked'
GROUP BY ua.user_id;
```

The problem: `REFRESH MATERIALIZED VIEW` does a full recompute. Every time. For every user. With a `CONCURRENTLY` option if you don't want to lock out readers, which doubles the work.

This is the same scan-everything problem as the batch job, except now PostgreSQL is doing it instead of Python. It's faster per row, but it's still O(all interactions) when only O(changed users) need updating.

Standard PostgreSQL materialized views have no concept of "what changed since last time." They recompute from scratch on every refresh.

---

## The Algebraic Insight

Here's the math that makes incremental vector averaging possible.

The mean of $n$ vectors is:

$$\bar{v} = \frac{1}{n} \sum_{i=1}^{n} v_i$$

If you add one new vector $v_{n+1}$, the new mean is:

$$\bar{v}' = \frac{n \cdot \bar{v} + v_{n+1}}{n + 1}$$

If you remove vector $v_k$, the new mean is:

$$\bar{v}' = \frac{n \cdot \bar{v} - v_k}{n - 1}$$

You don't need to re-scan the history. You need the current sum (or mean + count), the delta, and one arithmetic operation. This is why `AVG` is called an *algebraic aggregate* — it can be decomposed into sub-aggregates (`SUM` and `COUNT`) that support incremental updates.

pg_trickle's DVM engine has maintained algebraic aggregates since v0.9 for scalar types: `AVG(price)`, `SUM(revenue)`, `COUNT(*)`. The internal representation keeps a running `(sum, count)` pair per group, and each delta is applied as a simple addition or subtraction.

What v0.37.0 adds is that this same machinery now works for `vector` types from pgvector. Element-wise. A 1536-dimensional vector sum is 1536 independent sums maintained in parallel. The incremental cost is O(dimensions), not O(history length).

---

## `vector_avg` and `vector_sum` in Practice

### User Taste Vectors

The canonical example. A user's taste is the average embedding of items they've liked:

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'user_taste',
  query        => $$
    SELECT
      ua.user_id,
      vector_avg(e.embedding) AS taste_vec,
      count(*)                AS interaction_count,
      max(ua.created_at)      AS last_interaction
    FROM user_actions ua
    JOIN item_embeddings e ON e.item_id = ua.item_id
    WHERE ua.action = 'liked'
    GROUP BY ua.user_id
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

CREATE INDEX ON user_taste USING hnsw (taste_vec vector_cosine_ops);
```

When user 42 likes a new item:

1. The `INSERT INTO user_actions` fires pg_trickle's CDC trigger. The change is buffered in microseconds.
2. Within 5 seconds, the scheduler wakes up and processes the change buffer.
3. The DVM engine sees that user 42's group is affected. It looks up user 42's current running state: `(sum_vector, count)`.
4. It fetches the new item's embedding from `item_embeddings` (a single row lookup via the join).
5. It computes: `new_sum = old_sum + new_embedding`, `new_count = old_count + 1`, `new_avg = new_sum / new_count`.
6. It applies a `MERGE` to the `user_taste` stream table: one row updated for user 42.
7. The HNSW index on `taste_vec` receives one update.

Total work: one row read from the change buffer, one row lookup in the join, one vector addition, one division, one row merge. Regardless of whether user 42 has 10 or 10,000 past interactions.

### Category Centroids

Product categories have an "average" embedding that represents the category's semantic center. Useful for hierarchical navigation, category-to-category similarity, and cold-start recommendations.

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'category_centroids',
  query        => $$
    SELECT
      p.category_id,
      c.name              AS category_name,
      vector_avg(p.embedding) AS centroid,
      count(*)            AS product_count
    FROM products p
    JOIN categories c ON c.id = p.category_id
    WHERE p.active = true
    GROUP BY p.category_id, c.name
  $$,
  schedule     => '10 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

When a product is added to a category, only that category's centroid updates. When a product is reassigned from "Electronics" to "Smart Home," two centroids update — one gains a vector, one loses one. The other 500 categories are untouched.

### Document Cluster Representatives

If you're building a RAG system with document clustering (for better retrieval, for deduplication, for topic modeling), you need cluster representatives:

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'cluster_centroids',
  query        => $$
    SELECT
      dc.cluster_id,
      vector_avg(d.embedding) AS centroid,
      count(*)                AS doc_count
    FROM document_clusters dc
    JOIN documents d ON d.id = dc.doc_id
    GROUP BY dc.cluster_id
  $$,
  schedule     => '15 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

A new document is assigned to a cluster. The cluster's centroid shifts to include it. The shift is exact — not a stale approximation from the last batch run.

### `vector_sum` for Weighted Aggregation

Sometimes you want weighted aggregation — items viewed more recently should contribute more to the taste vector:

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'user_weighted_taste',
  query        => $$
    SELECT
      ua.user_id,
      vector_sum(e.embedding * ua.weight) AS weighted_sum,
      sum(ua.weight)                       AS total_weight
    FROM user_actions ua
    JOIN item_embeddings e ON e.item_id = ua.item_id
    WHERE ua.action IN ('liked', 'viewed', 'purchased')
    GROUP BY ua.user_id
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

-- Application divides at query time:
-- taste_vec = weighted_sum / total_weight
```

The weight can be anything: a recency decay, an action-type multiplier (purchase = 3, like = 2, view = 1), or a learned user-specific weight. The DVM engine doesn't care — it maintains `vector_sum` and `sum` as independent algebraic aggregates.

---

## The Numbers

Let's be concrete about what this costs vs. the batch approach.

**Scenario:** 1 million users, 200 interactions each on average, 1536-dimensional embeddings. 5,000 new interactions per minute across all users during peak hours.

### Batch approach (hourly)

- Rows scanned per batch: depends on change tracking quality. Optimistic case: ~300,000 (60 minutes × 5,000/min). But you scan each changed user's full history, not just the new interaction. Average 200 rows per user. So if 50,000 unique users changed, you scan 50,000 × 200 = 10 million rows.
- Vector operations: 50,000 full mean computations (each averaging 200 vectors of dimension 1536).
- Writes: 50,000 upserts.
- Duration: depends on hardware, but 10M row reads + 50K vector means + 50K upserts is minutes, not seconds.
- Staleness: up to 60 minutes.

### pg_trickle differential (5-second schedule)

- Changes per cycle: ~417 (5,000/min × 5s/60s).
- Rows read from change buffer: 417.
- Join lookups (item embedding): 417 single-row index lookups.
- Vector operations per cycle: 417 element-wise additions + 417 scalar increments + 417 divisions.
- Writes: ≤417 row merges (fewer if multiple interactions per user in the same cycle — they're batched by group key).
- Duration: milliseconds.
- Staleness: ≤5 seconds.

The batch job does O(changed_users × avg_history_length) work. The differential approach does O(new_interactions) work. As history grows, the batch gets slower. The differential doesn't care about history length at all.

For a user with 10,000 past interactions who likes one new item, the batch scans 10,001 rows and computes a full vector mean. pg_trickle does one addition and one division.

---

## What About Deletions?

Deletions work the same way, in reverse.

If user 42 unlikes an item, pg_trickle's CDC trigger captures the `DELETE` from `user_actions`. The DVM engine computes:

$$\text{new\_sum} = \text{old\_sum} - \text{removed\_embedding}$$
$$\text{new\_count} = \text{old\_count} - 1$$
$$\text{new\_avg} = \frac{\text{new\_sum}}{\text{new\_count}}$$

One subtraction, one decrement, one division. The taste vector adjusts instantly.

For updates — the user changes a rating, or an item's embedding is recomputed with a new model — the DVM applies the update as a delete of the old value plus an insert of the new value. Two vector operations per affected group.

This is a fundamental property of algebraic aggregates: they support all three DML operations (INSERT, UPDATE, DELETE) with constant-time deltas per group. There's no special case for "what if we need to remove a vector from the average?" It's just subtraction.

---

## Combining with Nearest-Neighbor Search

The real power of maintaining taste vectors as a stream table is that you can index them with HNSW and use them for fast approximate nearest-neighbor search.

**"Users like me" queries:**

```sql
-- Find users with similar taste to user 42
SELECT user_id,
       taste_vec <=> (SELECT taste_vec FROM user_taste WHERE user_id = 42) AS distance
FROM user_taste
WHERE user_id != 42
ORDER BY distance
LIMIT 20;
```

This returns the 20 users whose taste vectors are closest to user 42's (lowest cosine distance) — a user-similarity search over HNSW. The index makes it fast. The stream table makes it fresh.

**"Items for me" queries:**

```sql
-- Find items closest to user 42's taste
SELECT i.id, i.name,
       i.embedding <=> ut.taste_vec AS distance
FROM items i, user_taste ut
WHERE ut.user_id = 42
ORDER BY i.embedding <=> ut.taste_vec
LIMIT 20;
```

This is standard pgvector ANN search, but the query vector is the user's live taste vector rather than a stale batch-computed one.

**"Users who would like this item" queries:**

```sql
-- Given a new item, find users whose taste is closest
SELECT user_id, interaction_count,
       taste_vec <=> $new_item_embedding AS affinity
FROM user_taste
ORDER BY taste_vec <=> $new_item_embedding
LIMIT 1000;
```

This is the push-recommendation pattern: when a new item is added, find the users most likely to want it. With an HNSW index on `taste_vec`, this runs in milliseconds regardless of user count.

All three queries return results that reflect every interaction up to 5 seconds ago. Not up to the last batch run. Not up to yesterday.

---

## The Consistency Guarantee That Matters

There's a subtle but important property of maintaining taste vectors inside PostgreSQL via pg_trickle rather than in an external system.

When user 42 likes an item and then immediately runs a "show me my recommendations" query, the taste vector is guaranteed to be at least as fresh as the most recent committed write. With a 5-second schedule, the maximum staleness is one refresh cycle. With IMMEDIATE mode, the staleness is zero — the taste vector updates in the same transaction as the like.

Compare this with an external pipeline: the like writes to PostgreSQL, a message is published to a queue, a worker picks it up, computes the new vector, writes it back. The user might see stale recommendations for seconds or minutes, depending on queue depth, worker concurrency, and retry logic. Under load, this gap widens. During a worker outage, it becomes infinite.

pg_trickle's guarantee is simpler: the stream table is maintained by the same database engine that stores the source data. The scheduler runs inside PostgreSQL's background worker framework. There's no message queue, no external service, no network hop. The latency bound is the refresh schedule — a configuration parameter, not an operational variable.

---

## Beyond Taste Vectors: Other Uses for `vector_avg`

The taste-vector pattern is the most common, but `vector_avg` is a general-purpose primitive. Some other applications:

**Content moderation:** Maintain the average embedding of flagged content per user. When a new post's embedding is too close to the user's "flagged content centroid," auto-flag for review.

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'user_flagged_centroid',
  query => $$
    SELECT user_id, vector_avg(embedding) AS flagged_centroid
    FROM posts
    WHERE moderation_status = 'flagged'
    GROUP BY user_id
  $$
);
```

**Search quality monitoring:** Track the average embedding of queries that returned zero results. When this centroid shifts significantly, your content coverage has a gap.

**Anomaly detection:** The average embedding of a time window of events. When the current window's centroid diverges from the historical average, something changed.

**Cold-start mitigation:** For new users with no interaction history, use the centroid of their demographic cohort or signup-intent cluster as a starting taste vector.

These all share the same property: they're running averages over groups that change incrementally. The DVM engine doesn't know or care whether the vector represents a user preference, a content category, or an anomaly signal. It maintains the algebraic aggregate. You decide what it means.

---

## What v0.37.0 Actually Ships

Let's be specific about what's built and what's planned.

**Shipping in v0.37.0:**

- `vector_avg(vector)` — element-wise mean with running `(sum_vector, count)` state. Compatible with pgvector 0.7+.
- `vector_sum(vector)` — element-wise sum. For weighted aggregation patterns where the application divides at query time.
- Both aggregates work in DIFFERENTIAL mode with INSERT, UPDATE, and DELETE deltas.
- Both work with HNSW and IVFFlat indexes on the output column.
- Criterion benchmark baseline: microseconds per vector for the reducer, establishing a regression gate for future releases.
- pgvector added to the E2E test Docker image, with integration tests for all aggregate patterns.

**Not in v0.37.0 (coming in v0.39.0):**

- `halfvec_avg` / `halfvec_sum` — half-precision aggregates for storage-tiered pipelines.
- `sparsevec_avg` / `sparsevec_sum` — sparse vector aggregates for SPLADE and learned sparse models.

**Not in v0.37.0 (coming in v0.38.0):**

- Drift-aware HNSW reindexing (`post_refresh_action => 'reindex_if_drift'`).
- `pgtrickle.vector_status()` monitoring view.

The v0.37.0 scope is intentionally focused: get the core algebraic aggregate right, ship it with tests and benchmarks, and build the operational features on top in the next release. The aggregate math has to be bulletproof before you build monitoring and automation around it.

---

## Compared to the Alternatives

**Batch recomputation (Python/Celery/Airflow):** Works. Scales poorly. The compute cost grows with history length, not change volume. Staleness is bounded by your batch interval. Failure modes are operational nightmares (stale queue, silent worker death, ordering bugs).

**Real-time feature stores (Feast, Tecton):** These are designed for exactly this use case. They maintain running aggregates over event streams and serve them at query time. The problem: they're separate infrastructure. You need a message bus (Kafka), a compute layer (Spark/Flink), a serving layer, and a consistency model between all of them. For teams already running PostgreSQL, this is a large commitment.

**Application-level incremental updates:** You can do the math yourself in application code. When user 42 likes an item, read their current `(sum_vec, count)`, add the new embedding, divide, write back. This works for simple cases. It breaks when you need atomic updates across multiple groups, when you need to handle concurrent writes correctly, or when the aggregation involves JOINs (e.g., the item embedding lives in a different table from the user action).

**pg_trickle:** The aggregation is defined in SQL. The incremental math is handled by the DVM engine. The consistency is guaranteed by PostgreSQL's ACID semantics. The scheduling is a configuration parameter. There's no external infrastructure. It's one extension.

The trade-off: you're committing to PostgreSQL as your compute layer. If you're already running PostgreSQL for your operational data (which you probably are), this isn't a trade-off. It's a simplification.

---

## Getting Started

```sql
-- Prerequisites
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trickle;

-- Your existing tables
CREATE TABLE items (
  id        SERIAL PRIMARY KEY,
  name      TEXT,
  embedding vector(1536)
);

CREATE TABLE user_actions (
  id         SERIAL PRIMARY KEY,
  user_id    INT NOT NULL,
  item_id    INT NOT NULL REFERENCES items(id),
  action     TEXT NOT NULL,
  created_at TIMESTAMPTZ DEFAULT now()
);

-- One statement: live taste vectors for all users
SELECT pgtrickle.create_stream_table(
  name         => 'user_taste',
  query        => $$
    SELECT
      ua.user_id,
      vector_avg(i.embedding)  AS taste_vec,
      count(*)                 AS interactions
    FROM user_actions ua
    JOIN items i ON i.id = ua.item_id
    WHERE ua.action IN ('liked', 'purchased')
    GROUP BY ua.user_id
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

-- Index for fast ANN queries
CREATE INDEX ON user_taste USING hnsw (taste_vec vector_cosine_ops);

-- Query: items for user 42
SELECT i.id, i.name,
       i.embedding <=> ut.taste_vec AS distance
FROM items i, user_taste ut
WHERE ut.user_id = 42
ORDER BY i.embedding <=> ut.taste_vec
LIMIT 10;
```

That's the entire recommendation backend. Two extensions, one stream table, one index, one query. The taste vectors stay fresh as users interact. The HNSW index stays current. No workers, no queues, no batch jobs.

---

## Conclusion

The gap between "we have user interaction data and item embeddings" and "we have a working recommendation system" has always been an infrastructure gap, not an algorithmic one. The math is simple — it's averaging vectors. The hard part is maintaining those averages at scale, in real time, correctly, without building a distributed system.

`vector_avg` in pg_trickle v0.37.0 closes that gap. It takes the same algebraic-aggregate machinery that's been maintaining `SUM`, `COUNT`, and `AVG` over scalar values since v0.9, and extends it to vector types. The incremental cost is proportional to the number of *new* interactions, not the size of the history. The consistency is ACID. The infrastructure is one PostgreSQL extension.

Your recommendation engine doesn't need Kafka, Flink, Redis, or a feature store. It needs a running average that stays up-to-date. That's what `vector_avg` is.

---

*pg_trickle is an open-source PostgreSQL extension. Source code, documentation, and installation instructions are at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle). Vector aggregate support is detailed in the [v0.37.0 roadmap](https://github.com/grove/pg-trickle/blob/main/roadmap/v0.37.0.md).*
