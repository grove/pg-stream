[← Back to Blog Index](README.md)

# Deploying RAG at Scale: pg_trickle as Your Embedding Infrastructure

## Post-refresh hooks, drift-aware reindexing, and the operational reality of production vector search

---

You've built a RAG application. The embeddings are in PostgreSQL. pgvector is handling the approximate nearest-neighbor search. pg_trickle is keeping your stream tables fresh. It works in development. It works with 10,000 documents.

Now you need it to work with 50 million documents, 200 tenants, a 99.9% uptime SLA, and an on-call engineer who has never heard of IVFFlat.

This post is about the operational side of running pgvector at scale with pg_trickle — the things that don't come up in tutorials but determine whether your system survives its first Black Friday.

---

## The Three Problems Nobody Warns You About

Once you've solved embedding freshness (stream tables keep vectors current), three new problems emerge:

1. **Index drift.** Your HNSW or IVFFlat index gets less accurate over time as data changes, but nothing tells you when to rebuild it.
2. **Operational blindness.** You have no visibility into embedding lag, index health, or which stream tables are falling behind.
3. **Multi-tenant isolation.** Shared ANN indexes are fast but leak performance characteristics across tenants. Separate indexes per tenant are correct but expensive to manage.

These aren't pgvector problems. They're operational problems that appear at scale with any vector infrastructure. The question is whether your tooling helps you manage them or whether you're building monitoring dashboards from scratch.

---

## Problem 1: Index Drift Is Silent and Cumulative

Here's what happens to an HNSW index over time.

When you first create the index, it builds a navigable small-world graph over your current data. Every vector is a node. Edges connect nearby nodes across multiple graph layers. Query traversal starts from a fixed entry point and greedily hops toward the query vector. The graph structure guarantees logarithmic traversal time and high recall.

Then your data changes. Stream tables are refreshed. New embeddings are inserted. Old embeddings are updated or deleted.

HNSW handles inserts well — new nodes are connected into the existing graph. But it handles deletions by marking nodes as tombstones. The edges to and from deleted nodes remain. Graph traversal still visits these dead nodes, backtracks, and tries other paths.

After 50,000 deletions and 50,000 insertions (a net-zero change in table size), your index has 50,000 tombstones — nodes that add latency and reduce recall without contributing results.

IVFFlat has a different problem. Its cluster centroids were computed from the data at build time. As the data distribution shifts — new topics, new product categories, seasonal changes — the centroids become stale. Vectors are assigned to the nearest existing centroid, even if that centroid no longer represents the region well. Recall degrades gradually.

Both degradation modes are silent. Your queries still return results. The results just get slowly worse. Users experience it as "the search feels off" before anyone measures it.

### The Old Approach: Scheduled Rebuilds

Most teams handle this with a cron job:

```bash
# Every Sunday at 3am
0 3 * * 0 psql -c "REINDEX INDEX CONCURRENTLY idx_docs_embedding;"
```

This is a blunt instrument. You rebuild every week whether you need to or not. You don't rebuild mid-week even if a massive data import just invalidated half your index. The rebuild is expensive — for a 50-million-row table with 1536-dimensional vectors, `REINDEX CONCURRENTLY` takes 30–60 minutes and doubles your storage temporarily.

### The pg_trickle Approach: Drift-Aware Reindexing

pg_trickle v0.38.0 introduces `post_refresh_action` — a per-stream-table option that runs after each refresh cycle:

```sql
SELECT pgtrickle.alter_stream_table(
  'docs_embedded',
  post_refresh_action     => 'reindex_if_drift',
  reindex_drift_threshold => 0.10  -- 10% of rows changed since last reindex
);
```

After every refresh, the scheduler checks:

$$\text{drift} = \frac{\text{rows\_changed\_since\_last\_reindex}}{\text{total\_rows}}$$

If drift exceeds the threshold (10% in this example), the scheduler enqueues a `REINDEX CONCURRENTLY` job in a lower-priority tier. The reindex runs asynchronously — it doesn't block the next refresh cycle or delay search queries.

The key details:

- **`rows_changed_since_last_reindex`** is tracked in pg_trickle's catalog. Every refresh increments it by the number of rows merged (inserts + updates + deletes). A `REINDEX` resets it to zero.
- **`REINDEX CONCURRENTLY`** builds a new index alongside the old one, then swaps atomically. No downtime. Readers continue using the old index until the new one is ready.
- **The lower-priority tier** means the reindex doesn't compete with your refresh cycles. If your system is under load, the reindex waits. Your data freshness is never sacrificed for index maintenance.
- **The threshold is tunable.** For a search-critical application, set it to 0.05 (5%). For a background analytics corpus, 0.20 (20%) is fine.

There are also two simpler options:

```sql
-- Always ANALYZE after refresh (updates planner statistics)
post_refresh_action => 'analyze'

-- Always REINDEX after refresh (aggressive, for small tables)
post_refresh_action => 'reindex'
```

For most production systems, `reindex_if_drift` is the right choice. It rebuilds when necessary and skips when the index is still healthy.

---

## Problem 2: You Can't Fix What You Can't See

A stream table maintaining an embedding corpus has several dimensions you need to monitor:

- **Embedding lag.** How far behind is the stream table from the source data? If your 10-second schedule is consistently taking 12 seconds, you're falling behind.
- **Index age.** When was the HNSW index last rebuilt? How much has the data changed since then?
- **Drift percentage.** What fraction of the current index reflects stale centroid assignments or tombstone accumulation?
- **Refresh cost.** How long does each differential refresh take? Is it trending up?

Without monitoring, you discover problems when users complain. With monitoring, you discover them in a Grafana dashboard at 9am.

### `pgtrickle.vector_status()`

v0.38.0 adds a dedicated monitoring view for vector stream tables:

```sql
SELECT * FROM pgtrickle.vector_status();
```

```
 stream_table    | embedding_col | index_type | total_rows | rows_changed | drift_pct | last_refresh        | refresh_lag_ms | last_reindex        | index_age_hours | post_action
-----------------+---------------+------------+------------+--------------+-----------+---------------------+----------------+---------------------+-----------------+------------------
 docs_embedded   | embedding     | hnsw       |  2,340,891 |      128,445 |      5.49 | 2026-04-27 14:30:02 |          3,241 | 2026-04-25 03:00:00 |           59.50 | reindex_if_drift
 user_taste      | taste_vec     | hnsw       |    892,103 |       14,209 |      1.59 | 2026-04-27 14:30:05 |          1,892 | 2026-04-27 08:15:00 |            6.25 | reindex_if_drift
 product_search  | search_vec    | ivfflat    | 12,089,442 |    1,450,281 |     12.00 | 2026-04-27 14:29:58 |          8,102 | 2026-04-20 03:00:00 |          179.50 | reindex_if_drift
```

At a glance: `product_search` has 12% drift and hasn't been reindexed in a week. If the threshold is 10%, the next refresh will trigger a rebuild.

The view integrates with pg_trickle's self-monitoring system (v0.20+). If you're already scraping pg_trickle metrics with Prometheus, the vector-specific metrics are included automatically.

### Prometheus Metrics

The same data is exposed as Prometheus metrics:

```
# HELP pgtrickle_vector_drift_ratio Fraction of rows changed since last reindex
# TYPE pgtrickle_vector_drift_ratio gauge
pgtrickle_vector_drift_ratio{stream_table="docs_embedded"} 0.0549
pgtrickle_vector_drift_ratio{stream_table="user_taste"} 0.0159
pgtrickle_vector_drift_ratio{stream_table="product_search"} 0.1200

# HELP pgtrickle_vector_index_age_seconds Seconds since last REINDEX
# TYPE pgtrickle_vector_index_age_seconds gauge
pgtrickle_vector_index_age_seconds{stream_table="docs_embedded"} 214200
pgtrickle_vector_index_age_seconds{stream_table="user_taste"} 22500
pgtrickle_vector_index_age_seconds{stream_table="product_search"} 646200

# HELP pgtrickle_vector_refresh_lag_ms Milliseconds since last successful refresh
# TYPE pgtrickle_vector_refresh_lag_ms gauge
pgtrickle_vector_refresh_lag_ms{stream_table="docs_embedded"} 3241
```

Standard Grafana alerting rules:

```yaml
- alert: VectorIndexDriftHigh
  expr: pgtrickle_vector_drift_ratio > 0.15
  for: 10m
  labels:
    severity: warning
  annotations:
    summary: "Vector index drift > 15% on {{ $labels.stream_table }}"
    description: "Consider lowering reindex_drift_threshold or investigating write volume."

- alert: VectorRefreshLagHigh
  expr: pgtrickle_vector_refresh_lag_ms > 30000
  for: 5m
  labels:
    severity: critical
  annotations:
    summary: "Embedding corpus {{ $labels.stream_table }} is >30s behind source data"
```

This is the same observability pattern pg_trickle uses for all stream tables, extended with vector-specific dimensions. The on-call engineer doesn't need to know about IVFFlat centroids. They need to know "this metric is above threshold" and have a runbook that says "pg_trickle handles it automatically, but if drift stays high after reindex, investigate write volume."

---

## Problem 3: Multi-Tenant Vector Search

Multi-tenant RAG is where most vector search architectures break down.

The naive approach: one shared table, one shared HNSW index, filter by `tenant_id` at query time. This works until it doesn't:

- **Cross-tenant recall interference.** A large tenant's embeddings dominate the HNSW graph. Small tenants' queries traverse through the large tenant's nodes to reach their own data. Recall varies by tenant size.
- **Over-fetching.** HNSW returns the `k` approximate nearest neighbors globally, then filters to the tenant. If a tenant has 0.1% of the data, you need to retrieve ~1000× candidates to find 10 results. Latency is unpredictable.
- **Data isolation.** A WHERE clause is only as reliable as the developer who writes it. One missing filter and you've leaked data across tenants.

### Tiered Tenancy

The right architecture depends on tenant size. pg_trickle supports three patterns:

**Large tenants (>1M embeddings): Dedicated stream table per tenant.**

```sql
-- For tenant "acme" with millions of documents
SELECT pgtrickle.create_stream_table(
  name  => 'search_corpus_acme',
  query => $$
    SELECT d.id, d.body, d.embedding, d.metadata
    FROM documents d
    WHERE d.tenant_id = 'acme'
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

CREATE INDEX ON search_corpus_acme USING hnsw (embedding vector_cosine_ops);
```

Each large tenant gets a dedicated HNSW index with no cross-tenant interference. The index is smaller, so queries are faster. Drift-aware reindexing operates per-tenant, rebuilding only the indexes that need it.

**Medium tenants (10K–1M embeddings): Partitioned stream table with partial indexes.**

```sql
-- Shared stream table with tenant partitioning
SELECT pgtrickle.create_stream_table(
  name  => 'search_corpus_medium',
  query => $$
    SELECT d.id, d.body, d.embedding, d.tenant_id, d.metadata
    FROM documents d
    WHERE d.tenant_id IN (SELECT tenant_id FROM tenants WHERE tier = 'medium')
  $$,
  schedule     => '10 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

-- Per-tenant partial indexes
CREATE INDEX ON search_corpus_medium USING hnsw (embedding vector_cosine_ops)
  WHERE tenant_id = 'tenant_a';
CREATE INDEX ON search_corpus_medium USING hnsw (embedding vector_cosine_ops)
  WHERE tenant_id = 'tenant_b';
-- ... generated dynamically per tenant
```

Partial HNSW indexes scope the graph to one tenant's data. The planner picks the right partial index when the query includes `WHERE tenant_id = ?`. No cross-tenant interference, no over-fetching.

**Small tenants (<10K embeddings): Shared table with RLS.**

```sql
-- For hundreds of small tenants sharing one table
SELECT pgtrickle.create_stream_table(
  name  => 'search_corpus_shared',
  query => $$
    SELECT d.id, d.body, d.embedding, d.tenant_id
    FROM documents d
    WHERE d.tenant_id IN (SELECT tenant_id FROM tenants WHERE tier = 'small')
  $$,
  schedule     => '15 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

-- RLS enforces isolation at the database level
ALTER TABLE search_corpus_shared ENABLE ROW LEVEL SECURITY;
ALTER TABLE search_corpus_shared FORCE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation ON search_corpus_shared
  AS RESTRICTIVE FOR ALL
  USING (tenant_id = current_setting('app.current_tenant_id')::text);

CREATE INDEX ON search_corpus_shared USING hnsw (embedding vector_cosine_ops);
```

For small tenants, the over-fetching problem is less severe (they have fewer rows in the index to skip). RLS guarantees data isolation regardless of application bugs. The shared index means less infrastructure per tenant.

### Monitoring Per Tenant

`pgtrickle.vector_status()` reports per stream table. With the tiered pattern above, large tenants have dedicated entries. Medium tenants share a stream table but have per-partition drift tracking. Small tenants share everything — but their data volume is small enough that drift accumulates slowly.

---

## The Docker Image: One Command to RAG

For teams evaluating pg_trickle + pgvector or building a proof of concept, v0.38.0 ships a Docker image with everything pre-installed:

```bash
docker run -d \
  -e POSTGRES_PASSWORD=secret \
  -p 5432:5432 \
  ghcr.io/grove/pg-trickle-rag:latest
```

The image includes:
- PostgreSQL 18
- pgvector 0.8+
- pg_trickle (latest release)
- pgai (if available) for in-database embedding generation

```sql
-- Everything is ready
CREATE EXTENSION vector;
CREATE EXTENSION pg_trickle;

-- Start building
SELECT pgtrickle.create_stream_table(...);
```

No Docker Compose files to manage. No extension compilation. No shared_preload_libraries configuration. One container, one port, ready.

---

## The `embedding_stream_table()` API (v0.40.0)

For v0.37–v0.38, creating a vector stream table requires writing the full denormalization query by hand. This is powerful but verbose. For the common case — "I have a documents table, I want a searchable, indexed, denormalized corpus" — there's a lot of boilerplate.

v0.40.0 introduces a higher-level API:

```sql
SELECT pgtrickle.embedding_stream_table(
  name              => 'docs_search',
  source_table      => 'documents',
  embedding_columns => ARRAY['embedding'],
  denormalize_from  => ARRAY[
    ROW('doc_tags',     'JOIN doc_tags ON doc_tags.doc_id = documents.id',     'tags'),
    ROW('doc_metadata', 'LEFT JOIN doc_metadata ON doc_metadata.doc_id = documents.id', 'metadata')
  ],
  schedule          => '10 seconds',
  index_type        => 'hnsw',
  post_refresh_action => 'reindex_if_drift'
);
```

The function:
1. Auto-generates the denormalization query from the source table and join specifications.
2. Creates the stream table with the generated query.
3. Creates the HNSW (or IVFFlat) index on the embedding column(s).
4. Configures drift-aware reindexing with sensible defaults.
5. Returns a summary of what was created.

For expert users who need to inspect or customize the generated SQL, a dry-run mode returns the query without executing it:

```sql
SELECT pgtrickle.embedding_stream_table(
  name         => 'docs_search',
  source_table => 'documents',
  embedding_columns => ARRAY['embedding'],
  dry_run      => true
);
-- Returns: the generated CREATE STREAM TABLE SQL, the index DDL, and the configuration
```

This is syntactic sugar over existing primitives. Everything it does, you can do with `create_stream_table()` and `CREATE INDEX`. The value is removing boilerplate for the 80% use case.

---

## Sparse and Half-Precision Vectors (v0.39.0)

Production RAG systems often use tiered storage for embeddings. Full-precision `vector(1536)` for the canonical representation. Half-precision `halfvec(1536)` for indexed search (half the storage, nearly identical recall). Sparse vectors (`sparsevec`) for SPLADE or learned sparse models used in re-ranking.

v0.39.0 extends the algebraic aggregate support to these types:

```sql
-- Half-precision centroid for storage-efficient search
SELECT pgtrickle.create_stream_table(
  name  => 'category_centroids_half',
  query => $$
    SELECT category_id,
           halfvec_avg(embedding::halfvec(1536)) AS centroid
    FROM products
    GROUP BY category_id
  $$,
  refresh_mode => 'DIFFERENTIAL'
);

-- Sparse vector aggregate for SPLADE re-ranking
SELECT pgtrickle.create_stream_table(
  name  => 'topic_sparse_centroids',
  query => $$
    SELECT topic_id,
           sparsevec_avg(sparse_embedding) AS centroid
    FROM documents
    GROUP BY topic_id
  $$,
  refresh_mode => 'DIFFERENTIAL'
);
```

`halfvec_avg` maintains running state upcast to full-precision `vector(d)` internally (to avoid rounding accumulation), then casts back to `halfvec(d)` on read. `sparsevec_avg` computes element-wise means over the union of sparse dimensions, treating absent entries as zero.

This matters for storage-tiered architectures. You can maintain a pipeline:

```
raw documents → vector(1536) embeddings
             → halfvec(1536) search corpus (indexed, 50% storage)
             → sparsevec(1536) re-ranking corpus (indexed, ~10% storage)
```

Each layer is a stream table. Each is incrementally maintained. Each has its own index and drift monitoring.

---

## Reactive Distance Subscriptions (v0.39.0)

Traditional monitoring is pull-based: you query a dashboard. Reactive subscriptions are push-based: the database tells you when something happens.

v0.39.0 extends reactive subscriptions to vector-distance predicates:

```sql
LISTEN fraud_alert;

SELECT pgtrickle.create_reactive_subscription(
  'fraud_alert',
  $$
    SELECT t.id, t.amount, t.merchant
    FROM transactions_embedded t
    JOIN known_fraud_patterns k
      ON t.embedding <=> k.embedding < 0.05
  $$
);
```

This fires a `NOTIFY fraud_alert` whenever a new transaction's embedding enters within cosine distance 0.05 of a known fraud pattern. The subscription is differential — it only fires for *newly matched* rows, not on every refresh cycle.

Other applications:

- **Content moderation:** Alert when a new post is semantically similar to previously flagged content.
- **Competitive intelligence:** Notify when a new product listing appears close to your product's embedding.
- **SLA monitoring:** Alert when the average query embedding drifts far from the training distribution (distribution shift detection).

```sql
LISTEN content_review;

SELECT pgtrickle.create_reactive_subscription(
  'content_review',
  $$
    SELECT p.id, p.author_id, p.body
    FROM posts_embedded p
    JOIN flagged_content_centroids fc
      ON p.embedding <=> fc.centroid < 0.08
    WHERE p.created_at > now() - interval '1 hour'
  $$
);
```

The subscription is maintained by the DVM engine like any stream table, but instead of materializing results into a table, it emits `NOTIFY` events for new matches. The application listens on the PostgreSQL connection and receives events in real time.

---

## The Operational Playbook

Here's a concrete operational guide for running pgvector + pg_trickle in production.

### Initial Setup

```sql
-- 1. Install extensions
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trickle;

-- 2. Create your embedding corpus as a stream table
SELECT pgtrickle.create_stream_table(
  name  => 'search_corpus',
  query => $$ ... your denormalization query ... $$,
  schedule     => '10 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

-- 3. Create the ANN index
CREATE INDEX ON search_corpus USING hnsw (embedding vector_cosine_ops)
  WITH (m = 16, ef_construction = 200);

-- 4. Enable drift-aware reindexing
SELECT pgtrickle.alter_stream_table(
  'search_corpus',
  post_refresh_action     => 'reindex_if_drift',
  reindex_drift_threshold => 0.10
);
```

### Tuning Guidelines

| Parameter | Conservative | Balanced | Aggressive |
|-----------|-------------|----------|------------|
| `schedule` | 30 seconds | 10 seconds | 3 seconds |
| `reindex_drift_threshold` | 0.05 (5%) | 0.10 (10%) | 0.20 (20%) |
| `post_refresh_action` | `reindex_if_drift` | `reindex_if_drift` | `analyze` |
| HNSW `m` | 32 | 16 | 8 |
| HNSW `ef_construction` | 400 | 200 | 100 |

- **Conservative:** High recall priority. More frequent reindexing, larger graph degree. For search-critical applications.
- **Balanced:** Good recall with reasonable maintenance overhead. The default for most systems.
- **Aggressive:** Maximize throughput, tolerate moderate recall fluctuation. For analytics or non-user-facing search.

### Monitoring Checklist

1. **Embedding lag** (`pgtrickle_vector_refresh_lag_ms`): Should be below 2× your schedule interval. If consistently above, your refresh is too expensive for the cycle time.
2. **Drift ratio** (`pgtrickle_vector_drift_ratio`): Steady state should stay below your threshold. If it keeps hitting the threshold, your write rate is high enough to warrant a lower schedule interval.
3. **Reindex frequency**: Check `last_reindex` in `vector_status()`. If reindexing every day, consider whether the drift threshold is too low or the write volume is genuinely high.
4. **Refresh duration trend**: If refresh time is trending up, check whether the delta size is growing (more changes per cycle) or the stream table is getting larger (more rows to merge into).

### Failure Modes and Recovery

**Refresh consistently late.** The differential refresh takes longer than the schedule interval. Solution: increase the schedule interval, reduce query complexity, or check for missing indexes on source tables.

**Drift never decreases after reindex.** The write rate is so high that by the time the reindex finishes, enough new changes have accumulated to exceed the threshold again. Solution: increase the threshold, or accept that for very high-write-rate tables, the index will always have some drift.

**REINDEX CONCURRENTLY fails.** PostgreSQL can fail concurrent reindex under certain conditions (e.g., unique constraint violations from concurrent writes to the underlying table). pg_trickle retries once; on second failure, it logs a warning and skips until the next threshold crossing. The old index continues to serve queries — the failure is not catastrophic.

---

## What's Built, What's Coming

To be honest about the timeline:

| Feature | Status | Version |
|---------|--------|---------|
| Vector columns in stream tables | Working today | — |
| FULL refresh with pgvector expressions | Working today | — |
| Denormalized corpus pattern (multi-JOIN) | Working today | — |
| `vector_avg` / `vector_sum` aggregates | Shipping | v0.37.0 |
| `post_refresh_action` (analyze, reindex, reindex_if_drift) | Planned | v0.38.0 |
| `pgtrickle.vector_status()` monitoring view | Planned | v0.38.0 |
| RAG Docker image (pg_trickle + pgvector) | Planned | v0.38.0 |
| `halfvec_avg` / `sparsevec_avg` aggregates | Planned | v0.39.0 |
| Reactive distance subscriptions | Planned | v0.39.0 |
| `embedding_stream_table()` API | Planned | v0.40.0 |
| Per-tenant ANN patterns (docs + examples) | Planned | v0.40.0 |

If you're running pgvector today and want to start using pg_trickle, the denormalized-corpus and FULL-refresh patterns work right now. `vector_avg` arrives in the next release. The operational tooling (monitoring, drift-aware reindex) follows immediately after.

---

## The Bigger Picture

The AI infrastructure ecosystem is fragmented. Embeddings are generated by one service, stored in another, indexed by a third, and served by a fourth. Each boundary is a consistency gap. Each service is a failure domain. Each integration is a maintenance burden.

pg_trickle's position is that most of this fragmentation is unnecessary — at least for the PostgreSQL half of the stack. If your transactional data lives in PostgreSQL (and it probably does), there's no fundamental reason your derived embedding data should live somewhere else.

pgvector stores and indexes vectors in PostgreSQL. pg_trickle keeps derived data synchronized with source data in PostgreSQL. Together, they handle the full pipeline from "source data changed" to "search index is current" — inside one database, one process, one ACID transaction boundary.

The remaining external dependency is the embedding model itself. You still need to call an API (or run a local model) to generate embeddings from text. That's a real boundary — neural network inference is expensive and doesn't belong in a database transaction. But everything *after* the embedding is written — maintaining corpora, computing aggregates, rebuilding indexes, monitoring freshness — that's data infrastructure. And PostgreSQL is very good at data infrastructure.

---

*pg_trickle is an open-source PostgreSQL extension. Source code, documentation, and installation instructions are at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle). The pgvector integration roadmap spans [v0.37.0](https://github.com/grove/pg-trickle/blob/main/roadmap/v0.37.0.md) through [v0.40.0](https://github.com/grove/pg-trickle/blob/main/roadmap/v0.40.0.md).*
