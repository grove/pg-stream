[← Back to Blog Index](README.md)

# Your pgvector Index Is Lying to You

## How pg_trickle + pgvector keep your embeddings honest

---

You built a RAG pipeline. You embedded your documents with OpenAI. You built an HNSW index with pgvector. Your queries are fast. Life is good.

Then three weeks later, a user searches for something that was definitely added last Tuesday. Your system returns a result from 2023.

Welcome to the silent embedding staleness problem — the operational reality that nobody talks about when they're showing you pgvector benchmarks.

This is the story of what actually goes wrong with pgvector in production, why your index degrades over time without telling you, and how combining pgvector with a PostgreSQL-native incremental view engine (pg_trickle) closes every one of those gaps.

---

## The Embedding Pipeline Most People Build

The typical setup looks like this:

1. Documents live in a `documents` table in PostgreSQL.
2. You run a nightly job that finds "documents added or changed since yesterday," sends them to an embedding API, and writes the vectors back to a `document_embeddings` table.
3. That table has an HNSW index (because you read the pgvector README).
4. Your application queries `ORDER BY embedding <=> $1 LIMIT 10`.

It works. It's fast. And it has at least four silent failure modes that will bite you.

---

## Silent Failure Mode 1: Embeddings Go Stale

Between your batch job runs, every document change is invisible to your vector search. If your batch runs nightly, the maximum staleness is 23 hours and 59 minutes. If the batch job fails, it's longer.

You won't know. The search will still return results. They'll just be wrong.

The standard fix is to run your batch more often — hourly, every 15 minutes. But this hits a wall: running the full embedding pipeline over every changed document is expensive (API calls cost money), slow (you're deserializing thousands of rows), and fragile (you need to track what changed, handle failures, deduplicate, and guarantee no data loss).

Most teams end up with a homebrewed CDC (change data capture) queue, a worker that polls it, retry logic, and a Slack alert for when it falls behind. That's a lot of infrastructure to maintain for something that is, fundamentally, a derived-data freshness problem.

## Silent Failure Mode 2: Your Index Is Wrong

Let me explain something about IVFFlat indexes that doesn't appear in introductory tutorials.

IVFFlat works by clustering your vectors at build time into `lists` groups (usually 100–1000). At query time, it searches only the nearest `probes` clusters (usually 1–10% of the total). This is what makes it fast.

The problem is that the cluster assignments are computed from the data **at build time**. When you add new documents with different topics (a new product category, a recent news event, a new code language), those vectors don't fit neatly into the existing clusters. IVFFlat puts them in the nearest cluster anyway, but the recall degrades — silently.

The pgvector documentation recommends: "Don't build the index until you have enough data. Rebuild on a schedule."

But what schedule? How do you know when to rebuild? How do you automate it without rebuilding unnecessarily? And how do you `REINDEX` a 10-million-row table without taking your search offline?

Most teams either never rebuild (recall degrades slowly over months), rebuild on a fixed weekly schedule (blunt instrument), or get paged when someone notices quality has dropped.

HNSW has a related problem: deletions create tombstones. HNSW never actually removes deleted nodes from the graph — it marks them. After enough deletions, your index is navigating through a graveyard. Build operations slow down. Query recall drops.

## Silent Failure Mode 3: You're Not Searching What You Think You're Searching

Here's something that sounds obvious but causes real pain: the table your HNSW index sits on is not your documents table. It's a denormalization of it.

Your real question to a semantic search system is something like: "Find me the 10 most relevant documents, from active projects, that my user has permission to see, with their associated category names and last-updated timestamps."

That requires joining `documents` → `document_permissions` → `categories` → `projects`. But pgvector indexes a single table. So you either:

**Option A:** Index the raw documents table, then filter post-query. Except filtering *after* the ANN search means you're retrieving `k * overhead` candidates from the index and hoping enough survive the filters. For fine-grained ACL filtering, you might need to retrieve 10× candidates to end up with 10 results.

**Option B:** Maintain a denormalized flat table manually — one row per document with all the fields joined. Except this flat table needs to stay synchronized with every source table that feeds it. Add a category? The flat table is wrong. Update a permission? The flat table is wrong. Your data engineering team writes a bunch of triggers and prays.

**Option C:** Just use Elasticsearch, which at least has an ETL ecosystem. But now you're maintaining two systems with two consistency models.

## Silent Failure Mode 4: Aggregate Vectors Are Always Stale

If you're doing anything more sophisticated than flat document search — collaborative filtering, user preference vectors, cluster centroids for dimensionality reduction, category-representative embeddings — you're computing aggregate vectors.

```sql
-- The user's "taste" based on items they've interacted with
SELECT avg(item_embeddings.embedding)
FROM user_actions
JOIN item_embeddings ON item_embeddings.item_id = user_actions.item_id
WHERE user_actions.user_id = 42;
```

This query is fine for a single user. For a million users, you precompute it and store the results. But "precompute and store" means you need to recompute whenever a user takes a new action. Back to the batch-job problem.

Every time someone likes an item, their taste vector is stale until the next batch. The longer your batch interval, the more personalization debt you accumulate.

---

## What pg_trickle Actually Does

pg_trickle is a PostgreSQL extension that implements **incremental view maintenance** — IVM for short.

The idea is simple in principle: instead of recomputing a derived table from scratch every time, figure out what changed and apply only that change. If a user likes one new item, compute the difference to their taste vector from that one new item, and apply it. Don't scan their entire history.

In practice, this is mathematically hard. Differential dataflow — the theory underlying pg_trickle's engine — was developed at Microsoft Research in the 2010s and is still an active research area. The key insight is that for certain classes of queries (the ones that matter in practice: JOINs, GROUP BYs, aggregates, filters, window functions), you can express the "how does the result change if one input row changes?" question as a formal algebraic operation.

For `SUM(x)`: if you add a row with `x = 5`, the new sum is `old_sum + 5`. You don't need to re-scan.

For `COUNT(*)`: if you delete a row, the new count is `old_count - 1`.

For `AVG(embedding)` over all items a user has liked: if the user likes one new item with embedding `v`, the new average is `(old_sum_vector + v) / (old_count + 1)`. No re-scan, no batch job.

This works because these operations have well-defined *inverses*. The math for vector aggregation is actually straightforward: a vector mean is a sum divided by a count, and sums are algebraically invertible.

What makes pg_trickle different from just writing clever UPDATE queries is that the engine handles this automatically, across arbitrary SQL queries, including multi-table JOINs and nested aggregations.

---

## The Problem That Makes This Hard (And How pg_trickle Solves It)

The hard part of IVM isn't the aggregate math — it's figuring out *what changed*.

When someone inserts a row into `user_actions`, pg_trickle needs to know:
- Which stream tables depend on `user_actions`
- Which groups in the stream table are affected (only this user's taste vector)
- What the correct delta is for each affected group
- How to apply that delta without violating consistency

pg_trickle handles this with a **trigger-based CDC pipeline**. When you create a stream table, pg_trickle attaches `AFTER INSERT/UPDATE/DELETE` triggers to every source table in the query. These triggers write changes to per-source change buffers (in a `pgtrickle_changes` schema) as part of the original transaction.

This means change capture is:
- **Transactional.** If the original write rolls back, the change record is gone too.
- **Low-latency.** The buffer is populated in microseconds.
- **Correct under concurrency.** Because it's within the same transaction, you can't have a change that gets captured but whose write later fails.

The scheduler then picks up these change buffers on a configurable cadence and applies them through the DVM (differential view maintenance) engine, which computes the incremental delta, then applies it to the stream table via a single `MERGE` statement.

The stream table is a normal PostgreSQL table. It has all the usual guarantees: WAL-logged, MVCC-correct, ACID-transactional. You can index it any way you like, including HNSW.

---

## The pgvector + pg_trickle Pattern

Here's where it clicks.

The problems pgvector has in production — embedding staleness, index drift, denormalization complexity, aggregate vector maintenance — are all instances of the same underlying problem: **derived data that needs to stay synchronized with source data**.

That is exactly what pg_trickle was built to solve.

### Always-Fresh Embeddings

```sql
-- You define the relationship once.
-- pg_trickle handles the synchronization forever.
SELECT pgtrickle.create_stream_table(
  name         => 'docs_embedded',
  query        => $$
    SELECT d.id, d.title, d.body,
           pgai.embed('text-embedding-3-small', d.body) AS embedding,
           d.project_id, d.updated_at
    FROM documents d
    WHERE d.status = 'active'
  $$,
  schedule     => '10 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

CREATE INDEX ON docs_embedded USING hnsw (embedding vector_cosine_ops);
```

Now when a document body changes, pg_trickle's CDC trigger captures the change. Within the next refresh cycle (10 seconds by default), only the changed document's row is updated in `docs_embedded`. The HNSW index receives a precise insert+delete pair. No batch job. No queue. No worker. No drift.

The important word here is **DIFFERENTIAL**. The engine doesn't recompute the entire corpus. It processes only the rows that changed since the last cycle. If 1 document changes out of 1 million, it touches 1 row's worth of work.

### Denormalized Corpora as First-Class Citizens

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'search_corpus',
  query        => $$
    SELECT
      d.id,
      d.title,
      d.body,
      d.embedding,
      array_agg(DISTINCT t.name) AS tags,
      p.name                     AS project_name,
      u.email                    AS owner_email,
      acl.read_roles             AS allowed_roles,
      d.updated_at
    FROM documents     d
    JOIN projects      p   ON p.id = d.project_id
    JOIN users         u   ON u.id = d.owner_id
    LEFT JOIN doc_tags dt  ON dt.doc_id = d.id
    LEFT JOIN tags     t   ON t.id = dt.tag_id
    JOIN doc_acl       acl ON acl.doc_id = d.id
    WHERE d.status = 'active'
    GROUP BY d.id, p.name, u.email, acl.read_roles
  $$,
  schedule     => '10 seconds'
);
```

This is not a view. This is a real table, updated incrementally. When a tag is added to a document, only that document's row is updated in `search_corpus`. When a project is renamed, only documents in that project update. When a permission changes, only the affected document's `allowed_roles` changes.

Your vector search now operates on a fully denormalized flat table with correct metadata — without a single ETL pipeline.

### Centroid Maintenance with `vector_avg`

The `vector_avg` algebraic aggregate (arriving in v0.37.0) is one of the most powerful things in this story.

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'user_taste',
  query        => $$
    SELECT
      ua.user_id,
      vector_avg(e.embedding) AS taste_vec,
      COUNT(*) AS interaction_count
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

When user 42 likes a new item, the DVM engine computes:
```
new_taste = (old_sum_vector + new_item.embedding) / (old_count + 1)
```

Only user 42's row changes. The HNSW index on `taste_vec` receives one update. For a system with 10 million users where thousands interact per second, this is the difference between "runs at scale" and "falls over."

This is possible because `avg(vector)` is an *algebraic aggregate* — it has a well-defined incremental update rule. The same mathematics pg_trickle uses to maintain `AVG(price)` or `SUM(revenue)` applies directly to vector means.

### The Reindex Problem, Solved Differently

Here's a more nuanced take on IVFFlat rebuild. The question isn't "when do I run `REINDEX`?" — it's "how do I know when the drift is bad enough to justify the rebuild cost?"

pg_trickle tracks the number of rows changed since the last reindex as a first-class catalog value. You set a drift threshold and a post-refresh action, and the engine handles the rest:

```sql
SELECT pgtrickle.alter_stream_table(
  'docs_embedded',
  post_refresh_action      => 'reindex_if_drift',
  reindex_drift_threshold  => 0.10   -- 10% of rows changed → rebuild
);
```

Internally: after each refresh, the scheduler checks `rows_changed_since_last_reindex / total_rows`. If it exceeds the threshold, it enqueues an async `REINDEX` job (non-blocking, using `REINDEX CONCURRENTLY`) in a lower-priority tier so it never delays your search.

And through the `pgtrickle.vector_status()` monitoring view, you can see the drift percentage, last reindex time, and embedding lag for every vector stream table in your system — as a live, incrementally-maintained view.

---

## What Makes This Genuinely Unique

There are other approaches to this problem. Let's be honest about why they fall short.

**The standard nightly batch job.** Cheap to build, painful to maintain. Staleness measured in hours. Doesn't handle incremental aggregates. Breaks silently when the source schema changes.

**Change-data-capture pipelines (Debezium + Kafka).** Solves the freshness problem, but requires you to run Kafka, manage a Debezium connector, write a consumer that does the embedding logic, handle replication lag, coordinate schema changes across two systems, and ensure exactly-once semantics. It's a significant operational burden, and the embedding logic lives outside your database with no transactional guarantees.

**Read-through caches (ReadySet, Materialize).** These are purpose-built incremental-view systems, but they're separate processes, not PostgreSQL extensions. Your vector search lives in a different place from your transactional data. Schema changes have to be coordinated. You're back to a distributed system.

**pg_ivm.** The closest PostgreSQL-native alternative. But pg_ivm doesn't support complex queries (no aggregates in JOINs, no OUTER JOINs, limited GROUP BY). And it has no scheduler, no CDC pipeline, no vector-aggregate support. It's more of a research prototype than a production system.

**Pinecone, Weaviate, Qdrant.** These are purpose-built vector databases. They handle the indexing and search side well. But they have no SQL engine, no notion of derived data or incremental views, and require a synchronization pipeline from your source database to keep them fresh. You're back to the Debezium/ETL problem.

The unique property of pg_trickle + pgvector is that **everything happens inside PostgreSQL, transactionally, automatically, with no external dependencies.**

The change buffers are written in the same transaction as the source write. The refresh applies a `MERGE` to the stream table, which is WAL-logged. The HNSW index is updated by PostgreSQL's normal index AM callback, not by any special integration. The monitoring view is itself a stream table.

You cannot have a stale HNSW index while the underlying stream table is up-to-date. The index and the data are maintained by the same ACID engine.

---

## A Concrete Example: A Company's Documentation Site

Let's make this concrete. Imagine you're building a developer documentation platform. You have:

- 500,000 documentation pages across 2,000 projects
- Pages belong to projects, have tags, have explicit access permissions
- 50 edits per minute on average
- Search must return results scoped to the user's permitted projects
- You want semantic search ("find me docs about async error handling in Rust") plus keyword search

**Without pg_trickle:**
You maintain a Python worker that polls for changed pages every 5 minutes, batches them, calls the embedding API, writes back vectors, and then... has no way to update the HNSW index incrementally. So you rebuild the index nightly. Your search is on data that's up to 24 hours stale. Your permission filtering happens after ANN retrieval, requiring over-fetching. You have three separate codebases (app, worker, index pipeline) and two failure modes.

**With pg_trickle + pgvector:**

```sql
-- 1. Define the search corpus once
SELECT pgtrickle.create_stream_table(
  name     => 'doc_search_corpus',
  query    => $$
    SELECT
      d.id,
      d.title,
      d.body,
      d.embedding,
      array_agg(DISTINCT t.name)  AS tags,
      d.project_id,
      p.name                      AS project_name,
      dp.allowed_user_ids         AS allowed_users
    FROM   docs d
    JOIN   projects    p   ON p.id  = d.project_id
    JOIN   doc_perms   dp  ON dp.doc_id = d.id
    LEFT JOIN doc_tags dt  ON dt.doc_id = d.id
    LEFT JOIN tags     t   ON t.id  = dt.tag_id
    WHERE  d.published = true
    GROUP BY d.id, p.name, dp.allowed_user_ids
  $$,
  schedule             => '10 seconds',
  refresh_mode         => 'DIFFERENTIAL',
  post_refresh_action  => 'reindex_if_drift',
  reindex_drift_threshold => 0.15
);

-- 2. Create indexes for hybrid search
CREATE INDEX ON doc_search_corpus
  USING hnsw (embedding vector_cosine_ops);

CREATE INDEX ON doc_search_corpus
  USING gin (to_tsvector('english', title || ' ' || body));

CREATE INDEX ON doc_search_corpus (project_id);
CREATE INDEX ON doc_search_corpus USING gin (allowed_users);
```

That's it. Two SQL statements (one stream table, two indexes). From now on:
- Every page edit propagates to the corpus within 10 seconds.
- Every permission change propagates within 10 seconds.
- The HNSW index stays current automatically.
- When 15% of docs have changed, the index is rebuilt concurrently without downtime.
- Your search query is simple:

```sql
SELECT id, title, project_name, ts_rank_cd(to_tsvector('english', body), q) AS text_rank,
       embedding <=> $1 AS vec_dist
FROM   doc_search_corpus,
       to_tsquery('english', $2) q
WHERE  $3 = ANY(allowed_users)          -- ACL filter
  AND  to_tsvector('english', body) @@ q -- keyword filter
ORDER  BY embedding <=> $1              -- vector sort
LIMIT  20;
```

Vector similarity, full-text ranking, ACL filter — on one table, one query, with correct fresh data.

### The Before and After

Let's count what changed.

**Before:**
- Three codebases to deploy and maintain: app, embedding worker, index rebuild job.
- Average search freshness: up to 24 hours stale (nightly REINDEX).
- Permission filtering happens *after* ANN retrieval: for a user with access to 5% of projects, you over-fetch 20× candidates, discard 95% of them, and return 10 results.
- Index quality: unknown until someone notices results feel off.
- On-call surface: three failure modes — worker stopped, index diverged, outbox grew unbounded.

**After:**
- One SQL statement to create the stream table. The rest is PostgreSQL.
- Average search freshness: 10 seconds from edit to searchable.
- Permission filtering is a native indexed array lookup (`$3 = ANY(allowed_users)`) on the flat table — zero over-fetching, no candidate waste.
- Index quality: tracked as `drift_pct` in `pgtrickle.vector_status()`. Auto-rebuilt when it crosses 15%.
- On-call surface: one alert if the refresh lag exceeds your SLA. The system self-heals on restart.

The 50 edits per minute figure is worth unpacking. Over a 24-hour period, that's 72,000 page changes. Under the nightly-batch model, every one of those changes is invisible to search until the next morning's rebuild. Under pg_trickle, each change is propagated within 10 seconds. The HNSW and GIN indexes are updated by PostgreSQL's normal index maintenance — the same mechanism that would update any other btree or gin index when you run an INSERT or UPDATE. There is no special path.

The drift-aware reindex matters here too. At 50 edits per minute, roughly 72,000 out of 500,000 pages change each day — about 14% daily churn. With a 15% threshold, the HNSW index will be rebuilt roughly every day. Since it uses `REINDEX CONCURRENTLY`, the rebuild happens in the background, the old index serves queries throughout, and the swap is atomic. Your on-call engineer never needs to know it happened.

---

## The Architecture in Plain English

Here's how pg_trickle actually connects the pieces:

**Step 1 — CDC capture.** When your app inserts or updates a document, PostgreSQL fires an AFTER trigger (installed by pg_trickle). This trigger writes the changed row's key and diff to a tiny change buffer table in the same transaction. The trigger runs in microseconds. Your application never notices.

**Step 2 — Scheduler wakeup.** pg_trickle's background worker maintains a schedule. For a 10-second interval stream table, it wakes up every 10 seconds. (With IMMEDIATE mode, it can wake up after every single write — sub-millisecond latency.)

**Step 3 — Differential computation.** The scheduler reads all change buffer entries accumulated since the last refresh. It runs them through the DVM engine, which computes the delta: which rows in the stream table need to be inserted, updated, or deleted.

**Step 4 — MERGE.** The engine applies the delta to the stream table via a single `MERGE` statement. For a change to 5 documents out of 500,000, this touches 5 rows. PostgreSQL's normal index AM callbacks fire for each row, updating the HNSW and GIN indexes automatically.

**Step 5 — Post-refresh actions.** If `reindex_if_drift` is enabled and the threshold is crossed, the scheduler enqueues an async reindex job in a lower-priority tier.

**Step 6 — Monitoring.** `pgtrickle.vector_status()` shows lag, drift, index age, and aggregate counts in real time.

Every step is within PostgreSQL. Every step is ACID-safe. No external processes, no message queues, no separate services.

---

## Monitoring Your Embedding Pipeline

The most dangerous failure mode in any embedding pipeline is the one you can't see. The batch job fell behind three days ago. The worker is running but throwing errors it's swallowing. The HNSW recall degraded over six months and nobody noticed.

pg_trickle's v0.38.0 monitoring view — `pgtrickle.vector_status()` — is designed to make the invisible visible.

```sql
SELECT * FROM pgtrickle.vector_status();
```

```
 stream_table      | embedding_col | index_type | total_rows | rows_changed | drift_pct | last_refresh        | refresh_lag_ms | last_reindex        | index_age_hours
-------------------+---------------+------------+------------+--------------+-----------+---------------------+----------------+---------------------+-----------------
 doc_search_corpus | embedding     | hnsw       |    498,231 |       11,443 |      2.30 | 2026-04-27 14:32:01 |          2,847 | 2026-04-27 03:00:00 |           11.53
 user_taste        | taste_vec     | hnsw       |  1,204,891 |       81,020 |      6.73 | 2026-04-27 14:32:05 |          3,102 | 2026-04-26 20:00:00 |           18.53
```

Each row tells you:

- **`total_rows`** — how large the corpus is right now.
- **`rows_changed`** — how many rows have been inserted, updated, or deleted since the last REINDEX. Divide by `total_rows` to get `drift_pct`.
- **`refresh_lag_ms`** — milliseconds since the last successful refresh. For a 10-second schedule, this should stay below ~12,000ms under normal load. If it climbs, your refresh is taking longer than the cycle.
- **`last_reindex`** and **`index_age_hours`** — when the HNSW index was last rebuilt, and how old it is.

For the documentation platform example, `doc_search_corpus` is 2.3% drifted after ~11 hours. At the current edit rate, it will cross the 15% threshold in about two more days. When it does, `REINDEX CONCURRENTLY` runs automatically overnight without waking anyone up.

### Prometheus and Grafana

The same numbers are exported as Prometheus metrics (shipped alongside the monitoring view in v0.38.0):

```
# HELP pgtrickle_vector_drift_ratio Fraction of rows changed since last reindex
# TYPE pgtrickle_vector_drift_ratio gauge
pgtrickle_vector_drift_ratio{stream_table="doc_search_corpus"} 0.023
pgtrickle_vector_drift_ratio{stream_table="user_taste"} 0.067

# HELP pgtrickle_vector_refresh_lag_ms Milliseconds since last successful refresh
# TYPE pgtrickle_vector_refresh_lag_ms gauge
pgtrickle_vector_refresh_lag_ms{stream_table="doc_search_corpus"} 2847
pgtrickle_vector_refresh_lag_ms{stream_table="user_taste"} 3102

# HELP pgtrickle_vector_index_age_seconds Seconds since last REINDEX
# TYPE pgtrickle_vector_index_age_seconds gauge
pgtrickle_vector_index_age_seconds{stream_table="doc_search_corpus"} 41508
pgtrickle_vector_index_age_seconds{stream_table="user_taste"} 66708
```

Two Grafana alerts are enough to cover the production failure surface:

```yaml
- alert: EmbeddingCorpusStale
  expr: pgtrickle_vector_refresh_lag_ms > 30000
  for: 3m
  annotations:
    summary: "{{ $labels.stream_table }} hasn't refreshed in >30s"
    description: "Check background worker health and source-table CDC triggers."

- alert: VectorIndexDriftHigh
  expr: pgtrickle_vector_drift_ratio > 0.20
  for: 10m
  annotations:
    summary: "{{ $labels.stream_table }} index drift >20%"
    description: "Automatic reindex may not be keeping up. Review reindex_drift_threshold."
```

The first alert catches the case where something is broken (worker dead, schema changed, CDC trigger dropped). The second catches the case where your reindex threshold is too conservative for your write rate. Neither requires the on-call engineer to understand IVFFlat centroids. They just need to know "this dashboard is green when the system is working."

### What You Won't Have to Monitor

The things that break in a typical batch-based embedding pipeline and require monitoring:
- Is the embedding worker running?
- Is the outbox queue draining?
- Is the last batch run date recent?
- Is the HNSW index from this year or last year?
- Did the nightly rebuild complete successfully?

With pg_trickle, none of these exist as failure modes. There's no embedding worker (your app writes vectors, pg_trickle maintains the corpus). There's no outbox queue (CDC buffers are written in-transaction and drained automatically). There's no batch run date (refresh is continuous). There's no "when was the index last rebuilt" anxiety (the scheduler handles it, and `vector_status()` confirms it).

The monitoring surface collapses to two metrics. That's the right outcome.

---

## What's Coming

pg_trickle's pgvector integration is being shipped across four releases:

**v0.37.0 (next release):** `vector_avg` and `vector_sum` algebraic aggregates. Centroid maintenance, recommendation taste vectors, and cluster representatives become fully incremental.

**v0.38.0:** Post-refresh action hooks (`reindex_if_drift`), drift tracking, the `pgtrickle.vector_status()` monitoring view, and a one-command Docker image with pg_trickle + pgvector + pgai pre-installed.

**v0.39.0:** Sparse-vector aggregates (`sparsevec_avg`) for SPLADE and learned sparse models. Half-precision aggregates (`halfvec_avg`) for storage-tiered pipelines. Reactive subscriptions over distance predicates — "alert me when a new transaction embedding enters the fraud zone."

**v0.40.0:** A high-level `embedding_stream_table()` API where you describe your corpus in a few parameters and get a fully configured, indexed, monitored stream table back. Research into materialised k-NN graphs for fixed-pivot retrieval. Per-tenant embedding corpora with row-level security.

---

## The Honest Answer About What's Already Working

Some of this is shipping in the next two months. Some is not built yet. Let's be clear:

**Working today (no engine changes needed):**
- Vector columns pass through the CDC pipeline correctly. If your table has a `vector(1536)` column, pg_trickle can maintain stream tables over it.
- FULL mode stream tables work with any pgvector expression, including distance operators.
- Denormalized corpora (the multi-table JOIN pattern) work today.

**Shipping in v0.37.0:**
- `vector_avg` and `vector_sum` aggregates in DIFFERENTIAL mode.
- Distance operators in stream table definitions with a documented, safe FULL-mode fallback.

**Shipping in v0.38.0–v0.40.0:**
- Drift-aware reindexing, monitoring views, ergonomic API.

If you're using pgvector today and you're tired of babysitting embedding pipelines, the denormalized-corpus pattern works right now. The centroid/aggregate pattern lands in v0.37.

---

## A Different Way to Think About It

pgvector answers: "How do I store and search vectors in PostgreSQL?"

pg_trickle answers: "How do I keep any derived data fresh in PostgreSQL?"

Embeddings are derived data. An embedding of a document is derived from the document's text, metadata, and any transformations you apply. A user taste vector is derived from that user's interaction history. A search corpus is derived from your documents, permissions, tags, and projects.

If you believe that derived data should be maintained automatically and transactionally — rather than rebuilt in batches by external workers — then pg_trickle + pgvector is the natural home for your AI stack.

PostgreSQL already handles your source data transactionally. There's no fundamental reason why the derived data that feeds your AI features should be any different.

---

## Getting Started

Both extensions are single `CREATE EXTENSION` statements. If you're on a managed PostgreSQL provider (RDS, Cloud SQL, Neon, Supabase, Crunchy, CNPG), pgvector is already available. pg_trickle is available as a Docker image, PGXN package, and direct install.

```sql
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trickle;
```

From there:

```sql
-- Create your source table with an embedding column
CREATE TABLE documents (
  id        SERIAL PRIMARY KEY,
  body      TEXT NOT NULL,
  embedding vector(1536),  -- pre-computed by your app or pgai
  project_id INT,
  created_at TIMESTAMPTZ DEFAULT now()
);

-- Create a stream table over it
SELECT pgtrickle.create_stream_table(
  name     => 'docs_search',
  query    => $$ SELECT id, body, embedding, project_id FROM documents $$,
  schedule => '5 seconds'
);

-- Create the ANN index
CREATE INDEX ON docs_search USING hnsw (embedding vector_cosine_ops);

-- Query it
SELECT id, body
FROM   docs_search
ORDER  BY embedding <=> $1
LIMIT  10;
```

That's the starting point. No workers, no queues, no ETL. Just two extensions and a SQL statement.

---

## Conclusion

pgvector is an excellent piece of infrastructure. It brings serious vector storage and approximate nearest-neighbour search to PostgreSQL — an environment that already handles transactions, replication, backups, access control, and full-text search. The case for keeping your AI data in PostgreSQL rather than a purpose-built vector database is compelling.

But pgvector has a production gap that the benchmarks don't show: keeping embeddings fresh, indexes current, and derived data synchronized with its sources is genuinely hard. The default answer is "batch jobs, cron schedules, and manual REINDEX." That works until it doesn't.

pg_trickle fills that gap. Not by adding complexity, but by doing what PostgreSQL has always done best — providing a reliable, transactional foundation where data is always correct, always fresh, and always where you expect it.

The combination isn't a workaround. It's what the stack should have looked like all along.

---

*pg_trickle is an open-source PostgreSQL extension. Source code, documentation, and installation instructions are at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle). The pgvector integration roadmap is detailed in the repository's `plans/ecosystem/PLAN_PGVECTOR.md` and roadmap files.*
