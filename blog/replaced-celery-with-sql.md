# How We Replaced a Celery Pipeline with 3 SQL Statements

## A before/after story about async Python workers and differential view maintenance

---

This is a story about a pipeline we built, how it grew, and what we eventually replaced it with.

The names are generic. The failure modes are real. If you're running async Python workers to maintain derived data in PostgreSQL, some of this will be familiar.

---

## The Original Problem

We had an e-commerce platform. PostgreSQL for the operational data. A Elasticsearch for the search index. An event-driven architecture for "everything important should trigger something."

After 18 months, we had a serious denormalization problem. Our product search index needed data from seven tables: `products`, `categories`, `brands`, `suppliers`, `tags`, `inventory`, and `price_history`. The Elasticsearch document was a denormalized flat record of all of them. Keeping it in sync required knowing when any of the seven tables changed and what to reindex.

We built a Celery pipeline to handle it.

---

## The Pipeline (Version 1)

```
PostgreSQL row change
    → row-level trigger writes to outbox table
    → Celery Beat polls outbox every 30 seconds
    → Celery worker resolves the change to a product_id
    → Celery worker fetches full denormalized record from PostgreSQL
    → Celery worker indexes into Elasticsearch
    → outbox entry marked processed
```

At launch, this worked fine. Products updated within 60 seconds. Acceptable.

**Stats at launch:**
- Pipeline components: 3 (PostgreSQL trigger, Celery Beat, Celery worker)
- Lines of code in pipeline: ~400
- Deployment steps: 2 (app deploy, worker deploy)
- Average latency, source change → indexed: 35 seconds
- Throughput: easily handled our 50–100 product changes/day

---

## Version 2: Scale

We expanded to 200,000 products. Daily product data sync from supplier APIs: 15,000 updates per night. The 30-second polling interval, combined with a worker pool of 4, created a backlog that took 4 hours to drain.

**Solutions attempted:**
1. Increase worker concurrency to 16. Memory on the worker instances ballooned. We hit PostgreSQL connection limits because each worker opened its own connection pool.
2. Switch from polling to Redis Pub/Sub notifications. Reduced latency to 5 seconds on average. Added Redis as a dependency. Added a Redis sentinel deployment for HA.
3. Add a `priority` field to the outbox — high-traffic items process first. Added a manager task to compute priorities. Added a monitoring dashboard for queue depth by priority.

**Stats at Version 2:**
- Pipeline components: 5 (trigger, Beat, worker, Redis, priority manager)
- Lines of code in pipeline: ~1,200
- Deployment steps: 4
- Average latency: 5 seconds
- P99 latency: 180 seconds (during supplier sync)
- Incidents per month: ~2

---

## Version 3: Correctness

During a supplier sync, we discovered that the order of Celery task execution didn't match the order of changes. Product 12345 was updated twice in 3 seconds by the supplier sync. Two tasks were enqueued. The tasks ran out of order. Elasticsearch ended up with the *older* version of the product.

The fix was to add a `version` field to products and the outbox, and have the worker skip indexing if the task's version was lower than the current version. This required:
- A `version` column on `products` (auto-incremented on every UPDATE)
- The outbox to store the version at change time
- The worker to re-fetch the product and check versions before indexing
- A migration for all existing products

We also discovered that the outbox table had grown to 8 million rows because the cleanup job had silently failed two months earlier. The cleanup job now ran on a schedule and sent an alert when the backlog exceeded 100k rows.

**Stats at Version 3:**
- Pipeline components: 6 (trigger, Beat, worker, Redis, priority manager, cleanup job)
- Lines of code in pipeline: ~1,800
- Deployment steps: 5
- Average latency: 5 seconds
- P99 latency: 180 seconds
- On-call runbook length: 6 pages
- Incidents per month: ~1.5

---

## What We Actually Needed

At this point we had an honest conversation about what the pipeline was doing.

The Elasticsearch index was serving product search. The product search was our primary user-facing feature. Latency above 5 seconds was causing user complaints (products added to a campaign weren't appearing in searches fast enough).

The Elasticsearch requirement was actually an assumption, not a hard constraint. We had chosen Elasticsearch initially because we needed full-text search with fast faceting and we'd assumed PostgreSQL couldn't do it. By version 3, we were running PostgreSQL 18 with much better full-text support, pgvector for semantic search, and partial indexes for faceting.

We decided to run the experiment: what would it take to replace the Elasticsearch index with a denormalized PostgreSQL table and keep it fresh with IVM?

---

## The Replacement: 3 SQL Statements

### Statement 1: Create the stream table

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'product_search',
  query        => $$
    SELECT
      p.id            AS product_id,
      p.name          AS product_name,
      p.description,
      p.sku,
      b.name          AS brand_name,
      b.id            AS brand_id,
      c.name          AS category_name,
      c.id            AS category_id,
      c.path          AS category_path,
      s.name          AS supplier_name,
      s.country       AS supplier_country,
      i.qty           AS stock_qty,
      i.qty > 0       AS in_stock,
      ph.current_price,
      ph.original_price,
      ROUND(
        (ph.original_price - ph.current_price) / ph.original_price * 100, 1
      )               AS discount_pct,
      array_agg(t.name ORDER BY t.name)
                      AS tags,
      p.embedding     AS search_vec,
      p.created_at,
      p.updated_at
    FROM products p
    JOIN brands b ON b.id = p.brand_id
    JOIN categories c ON c.id = p.category_id
    JOIN suppliers s ON s.id = p.supplier_id
    LEFT JOIN inventory i ON i.product_id = p.id
    LEFT JOIN current_prices ph ON ph.product_id = p.id
    LEFT JOIN product_tags pt ON pt.product_id = p.id
    LEFT JOIN tags t ON t.id = pt.tag_id
    WHERE p.active = true
    GROUP BY p.id, b.name, b.id, c.name, c.id, c.path,
             s.name, s.country, i.qty, ph.current_price,
             ph.original_price, p.embedding, p.created_at, p.updated_at
  $$,
  schedule     => '3 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

### Statement 2: Create the full-text search index

```sql
-- GIN index for full-text search over product name + description + tags
CREATE INDEX product_search_fts_idx ON product_search
USING GIN (
  to_tsvector('english',
    product_name || ' ' || COALESCE(description, '') || ' ' ||
    brand_name || ' ' || category_name || ' ' ||
    COALESCE(array_to_string(tags, ' '), ''))
);
```

### Statement 3: Create the vector search index

```sql
-- HNSW index for semantic search
CREATE INDEX product_search_vec_idx ON product_search
USING hnsw (search_vec vector_cosine_ops)
WITH (m = 16, ef_construction = 64);
```

That's it. No Redis. No Celery. No outbox table. No cleanup jobs. No version fields.

---

## How the Search Query Changed

**Before:**
```python
# Elasticsearch query
results = es.search(
    index="products",
    query={
        "bool": {
            "must": [{"multi_match": {"query": q, "fields": ["product_name^3", "description", "brand_name"]}}],
            "filter": [
                {"term": {"in_stock": True}},
                {"range": {"discount_pct": {"gte": min_discount}}}
            ]
        }
    },
    size=20
)
product_ids = [r['_id'] for r in results['hits']['hits']]
# Then a second round-trip to PostgreSQL to get fresh data
products = db.query("SELECT * FROM products WHERE id = ANY(%s)", [product_ids])
```

**After:**
```sql
-- Hybrid full-text + semantic search, all in PostgreSQL
WITH semantic AS (
  SELECT product_id, search_vec <=> $query_vec AS distance
  FROM product_search
  WHERE in_stock = true
  AND discount_pct >= $min_discount
  ORDER BY search_vec <=> $query_vec
  LIMIT 50
),
fulltext AS (
  SELECT product_id,
         ts_rank_cd(
           to_tsvector('english', product_name || ' ' || COALESCE(description,'') || ' ' || brand_name),
           plainto_tsquery('english', $query_text)
         ) AS rank
  FROM product_search
  WHERE in_stock = true
  AND to_tsvector('english', product_name || ' ' || COALESCE(description,'') || ' ' || brand_name)
      @@ plainto_tsquery('english', $query_text)
),
rrf AS (
  SELECT COALESCE(s.product_id, f.product_id) AS product_id,
         COALESCE(1.0 / (60 + ROW_NUMBER() OVER (ORDER BY s.distance)), 0) +
         COALESCE(1.0 / (60 + ROW_NUMBER() OVER (ORDER BY f.rank DESC)), 0) AS rrf_score
  FROM semantic s
  FULL OUTER JOIN fulltext f ON f.product_id = s.product_id
)
SELECT ps.*
FROM rrf
JOIN product_search ps ON ps.product_id = rrf.product_id
ORDER BY rrf_score DESC
LIMIT 20;
```

The data returned is always fresh — the `product_search` stream table is at most 3 seconds stale. There's no second round-trip to get "fresh data" because `product_search` *is* the fresh data.

---

## The Numbers

| Metric | Celery+ES pipeline | pg_trickle+PostgreSQL |
|--------|-------------------|----------------------|
| P50 search latency | 12ms | 8ms |
| P99 search latency | 85ms | 22ms |
| Data freshness (average) | 5s | 1.5s |
| Data freshness (P99) | 180s (during sync) | 3s |
| Deployment components | 6 | 1 (the PostgreSQL extension) |
| Monthly incidents | 1.5 | 0 |
| Engineering time per quarter maintaining the pipeline | ~3 weeks | ~0 |

The P99 improvement from 180s to 3s was the most impactful change for users. The supplier sync no longer caused a visible degradation window.

The search latency improvement was a bonus — the PostgreSQL query planner was better at pruning `product_search` with partial indexes than Elasticsearch was with filter clauses, and the elimination of the network round-trip and deserialization overhead from Elasticsearch helped.

---

## What We Gave Up

Elasticsearch has capabilities that PostgreSQL+pgvector doesn't fully match:

- **Approximate nearest neighbor at very large scale** (hundreds of millions of vectors): HNSW in pgvector is competitive up to ~50M rows with appropriate hardware. Above that, dedicated vector databases have more tuning options.
- **Built-in relevance tuning**: Elasticsearch has a mature BM25 + learning-to-rank stack. We replicated most of what we needed with RRF, but a dedicated ML ranking system is more flexible.
- **Cross-cluster search and federation**: Elasticsearch's distributed search across multiple clusters is mature. PostgreSQL's distribution story requires Citus or similar.
- **Elasticsearch-specific query DSL**: Some power-user queries we'd built on top of Elasticsearch's query language required rewriting in SQL.

For our scale (500k products, 10M queries/month), none of these were blockers. For a much larger deployment, the calculus might be different.

---

## The Maintenance Story

The Celery pipeline had a 6-page oncall runbook. The pg_trickle replacement has:

```sql
-- Check freshness
SELECT name, last_refresh_at, staleness_secs
FROM pgtrickle.stream_table_status()
WHERE name = 'product_search';

-- Check queue depth (how many pending changes)
SELECT source_table, pending_rows
FROM pgtrickle.change_buffer_status();

-- Force a refresh if needed
SELECT pgtrickle.refresh('product_search');
```

That's the runbook. It fits in a Slack message.

The three SQL statements that created this setup were the end of a 3-year journey through increasingly complex async pipeline infrastructure. The right abstraction was in the database the entire time.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
