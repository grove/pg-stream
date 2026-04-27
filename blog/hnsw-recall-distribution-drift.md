# HNSW Recall Is a Lie

## How Distribution Drift Silently Breaks Similarity Search (and What to Do About It)

---

You built a similarity search feature. You measured recall at launch: 94% — excellent. You ship it, add it to the demo, put it on the homepage.

Six months later, a user complains that the results feel "off." You run the same recall measurement: 71%. You've been serving degraded results for months and had no idea.

This is distribution drift. It's real, it's common, and it's almost never discussed in tutorials. Here's what it is, why it happens, how to measure it, and what pg_trickle's drift-aware reindex policy does about it.

---

## What IVFFlat Actually Does (and Why That's a Problem)

IVFFlat (Inverted File with Flat quantization) is a classical approximate nearest-neighbor algorithm. It works in two phases:

**Build phase:** Train a k-means clustering on your vectors. Divide them into `lists` groups (centroids). Each vector is assigned to its nearest centroid. The index stores, for each centroid, the list of vectors in that group.

**Query phase:** Given a query vector, find the `probes` nearest centroids. Search only the vectors assigned to those centroids.

The speed comes from only searching `probes / lists` fraction of your data. The recall comes from the assumption that your query vector's nearest neighbors are in the nearest centroids.

**The problem:** The centroid assignments are computed from the data distribution *at build time*. As your data distribution changes, the centroid assignments become stale.

Imagine you're building a product recommendation system in January. Your product catalog skews toward winter items: coats, heaters, boots. You build the IVFFlat index. The centroids reflect a winter distribution.

By June, you've added thousands of summer items: swimwear, grills, sunscreen. These items get assigned to the nearest *existing* centroid at insertion time — but those centroids were trained on winter data. Your summer items end up crowded into a handful of winter centroids that happen to be geometrically nearest.

Now when someone searches for "outdoor summer activities," the query vector points toward the summer region of the embedding space. The two nearest centroids happen to be the winter outdoor centroid and the spring gardening centroid (geometrically closest to the summer space). The actual summer items are scattered across three other centroids that were *not* searched. Recall degrades.

The degradation is gradual and silent. Each new insert makes it slightly worse. Users experience it as "the search feels off" before you ever measure it.

---

## HNSW: The Tombstone Problem

HNSW (Hierarchical Navigable Small World graph) has a different problem.

HNSW is a graph-based index. Each vector is a node. Edges connect nearby nodes across multiple layers, enabling logarithmic-time traversal. Unlike IVFFlat, HNSW doesn't need retraining as data changes — new nodes are inserted by connecting them into the existing graph.

But HNSW doesn't actually *delete* nodes. When you delete a vector, the node is marked as dead (a tombstone) but its edges remain in the graph. Graph traversal still has to navigate through and around tombstones.

The consequences:

**Build slowdown:** As tombstones accumulate, inserting new nodes requires connecting through more dead nodes. Insertion time increases.

**Query slowdown:** Traversal visits tombstones and has to backtrack more often. Query latency increases.

**Index size:** The index doesn't shrink when rows are deleted. Tombstones consume space.

The pgvector documentation addresses this: "Vacuuming can take a while for HNSW indexes. Speed it up by reindexing first." But this is reactive advice — the documentation doesn't tell you *when* to reindex, how to measure when you need to, or how to automate it.

The answer for most teams is a scheduled `REINDEX CONCURRENTLY` — weekly, or monthly, or "when someone notices it's slow." This is a blunt instrument.

---

## Measuring Distribution Drift

The right metric for IVFFlat drift is recall — the fraction of true nearest neighbors returned by approximate search.

The standard measurement procedure:

```sql
-- 1. Sample query vectors
SELECT id, embedding FROM items ORDER BY random() LIMIT 1000;

-- 2. For each sample, run exact search (seq scan)
-- and approximate search (index scan), compare results
BEGIN;
SET LOCAL enable_indexscan = off;   -- force exact search
SELECT id, embedding <=> $query AS distance
FROM items ORDER BY distance LIMIT 10;
COMMIT;

-- vs.
SELECT id, embedding <=> $query AS distance
FROM items ORDER BY distance LIMIT 10;  -- uses index

-- Recall = |exact_results ∩ approx_results| / |exact_results|
```

Running this for 1,000 sample queries gives a statistically reliable recall estimate. Do it once a week. When recall drops below your target (say, 85%), rebuild the index.

The problem is that this procedure is expensive (1,000 queries × 2 modes), requires a separate monitoring job, and produces a number that you then have to act on manually.

---

## A Simpler Proxy: Row Change Rate

Exact recall measurement is the ground truth. But there's a cheaper proxy that correlates well with drift: the fraction of rows that have changed since the last index build.

If 30% of your vectors have been inserted, updated, or deleted since the last `REINDEX`, your index reflects a very different distribution than what's currently in the table. The centroid assignments are stale for 30% of the data. The tombstone fraction is significant.

This isn't a perfect predictor of recall degradation — the actual impact depends on whether the changes are uniformly distributed or concentrated in specific regions of the embedding space. But as a heuristic for "this index needs attention," it's reliable.

pg_trickle tracks this metric as `rows_changed_since_last_reindex` per stream table. The `pgtrickle.vector_status()` view (v0.38) exposes it:

```sql
SELECT
  stream_table,
  total_rows,
  rows_changed_since_reindex,
  ROUND(rows_changed_since_reindex::numeric / total_rows * 100, 1) AS drift_pct,
  last_reindex_at,
  last_refresh_at
FROM pgtrickle.vector_status();
```

```
stream_table     | total_rows | rows_changed | drift_pct | last_reindex_at
-----------------+------------+--------------+-----------+----------------
product_corpus   | 2,400,000  | 312,000      |      13.0 | 2026-04-01
user_taste       | 850,000    |  51,000      |       6.0 | 2026-04-20
doc_embeddings   | 125,000    |  42,000      |      33.6 | 2026-03-15
```

`doc_embeddings` at 33.6% drift is the one to worry about. It was last reindexed 6 weeks ago and has had significant churn.

---

## Drift-Aware Automatic Reindexing

Manual monitoring and scheduled rebuilds are operational debt. pg_trickle v0.38 introduces a policy-based approach:

```sql
SELECT pgtrickle.alter_stream_table(
  'doc_embeddings',
  post_refresh_action     => 'reindex_if_drift',
  reindex_drift_threshold => 0.15  -- trigger at 15% drift
);
```

After each refresh cycle, the scheduler checks whether `rows_changed_since_reindex / total_rows > 0.15`. If so, it enqueues a `REINDEX CONCURRENTLY` for the vector column's index in a background worker tier with lower priority than the refresh worker.

`REINDEX CONCURRENTLY` doesn't lock the table for reads. Queries continue executing against the old index while the new index is built. When the build completes, PostgreSQL atomically swaps the indexes. The downtime window is zero.

The sequence:
1. Drift exceeds 15%.
2. pg_trickle enqueues `REINDEX CONCURRENTLY` in a low-priority tier.
3. The reindex starts. Queries use the old index.
4. The reindex completes (time proportional to table size and available parallelism).
5. Indexes are swapped atomically.
6. `rows_changed_since_reindex` resets to 0.

No oncall page. No manual `REINDEX` command. No recall degradation that goes undetected for months.

---

## Choosing the Threshold

15% is a reasonable starting point, but the right threshold depends on your data and quality requirements.

**High-recall requirements (>90%):** Use a lower threshold — 5–10%. Accept more frequent reindexes in exchange for tighter recall guarantees.

**Stable data distributions:** If your vectors come from a domain that doesn't evolve rapidly (e.g., a product catalog in a stable category), you can use a higher threshold — 20–25%.

**Volatile distributions:** If you're embedding news articles, tweets, or rapidly evolving content, drift happens fast. Use a lower threshold and a more frequent monitoring cadence.

For HNSW specifically, the threshold should also account for tombstone fraction:

```sql
SELECT pgtrickle.alter_stream_table(
  'user_taste',
  post_refresh_action     => 'reindex_if_drift',
  reindex_drift_threshold => 0.20,  -- 20% general drift
  reindex_tombstone_ratio => 0.10   -- or 10% tombstones
);
```

Either condition triggers a reindex. The tombstone ratio catches the HNSW deletion problem independently of the distribution drift.

---

## IVFFlat: Also Rebalance Your Lists

One more thing about IVFFlat that's worth knowing.

When you run `REINDEX`, the new IVFFlat index re-runs k-means clustering from scratch. It's trained on the *current* data distribution. The cluster assignments are fresh.

How many lists should you use? The pgvector README recommends:
- Up to 1M rows: `lists = rows / 1000`
- Over 1M rows: `lists = sqrt(rows)`

But these are starting points. If your data has changed significantly in structure — you've moved from a single domain to multiple domains, for example — the optimal `lists` count may have changed too.

pg_trickle doesn't (yet) auto-tune `lists`. You'll need to specify it in your index definition:

```sql
CREATE INDEX CONCURRENTLY product_corpus_ivf_idx
ON product_corpus USING ivfflat (embedding vector_cosine_ops)
WITH (lists = 500);
```

After a reindex triggered by drift, check recall measurement again. If recall improved less than expected, try a different `lists` value.

---

## The Combined Problem

In practice, both problems — IVFFlat distribution drift and HNSW tombstone accumulation — occur together. A production embedding table:
1. Grows as new content is added (HNSW: new insertions, IVFFlat: new insertions to stale centroids)
2. Is updated as content changes (HNSW: delete old node, insert new node = tombstone + new node)
3. Has documents deleted as content is removed (HNSW: tombstones, IVFFlat: removed from centroid without rebalancing)

Without automated reindexing, this produces a slow, steady recall degradation that's invisible until a user reports it. With drift-aware reindexing, you set the policy once and let pg_trickle maintain the health of your vector indexes alongside the freshness of your data.

---

## What This Doesn't Solve

Drift-aware reindexing helps. It doesn't solve everything.

**Very large tables:** A `REINDEX CONCURRENTLY` on a 100M-row vector table takes hours. Even at a 15% drift threshold, you might be doing this every few weeks. At some scale, you need a sharding strategy (partitioned tables with per-partition indexes, or a dedicated vector database) rather than monolithic reindexing.

**Embed-model changes:** If you change your embedding model (e.g., from OpenAI text-embedding-3-small to text-embedding-3-large), *all* your vectors need to be recomputed. This isn't a drift problem — it's a full migration. pg_trickle's reindexing helps after the re-embedding is done, but it doesn't drive the re-embedding itself.

**Recall monitoring:** Drift percentage is a proxy. It doesn't tell you whether recall has actually degraded. If you need hard recall SLAs, you need recall measurement in addition to drift monitoring. pg_trickle's `vector_status()` view is meant to sit alongside, not replace, periodic recall sampling.

The message is: distribution drift is a real production problem, most teams discover it too late, and the tooling to manage it automatically now exists. Use it.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
