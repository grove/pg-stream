# The pgvector Tooling Landscape in 2026

## What each tool actually does — and where pg_trickle fits in

---

If you've been building RAG applications on PostgreSQL, you've probably noticed the ecosystem has grown considerably in the last two years. There's pgai, pg_vectorize, Debezium with pgvector support, and a handful of managed-platform approaches. They all promise to "keep your embeddings in sync" or "automate your vector pipeline."

The problem is that these tools operate at different layers, solve different problems, and make different tradeoffs. Comparing them directly is like comparing a bread knife to a bread maker. They're related, but they're not doing the same job.

This is an honest look at what each tool actually does, where each one falls short, and where pg_trickle sits relative to all of them. No marketing. Just the actual mechanics.

---

## First, a note about pgai

Let's start with the elephant in the room.

**pgai was archived by Timescale on February 26, 2026.**

If you've read any article about keeping embeddings fresh in PostgreSQL from the last year, pgai was probably the top recommendation. Timescale built it, marketed it heavily, and it accumulated 5,800 GitHub stars. The approach was elegant: declare a `create_vectorizer()` configuration, run a stateless Python worker process, and the system would keep your embeddings synchronized as data changes.

The archive doesn't mean pgai was bad — it means Timescale made a strategic pivot. The repository is now read-only. The Python library still exists and works. But the project is no longer actively developed as an open-source extension.

Why is this interesting? Because pgai's architecture revealed something about the fundamental problem with embedding pipelines. The vectorizer worker was *always* an external process. It used a queue inside PostgreSQL (a `ai.work_queue` table) to track what needed re-embedding. The worker polled that queue, called the external embedding API, and wrote the results back.

This architecture is correct for the problem it solves — handling unreliable external API calls in the background, with retries, rate limiting, and error isolation. But it's fundamentally a distributed system. You have two processes that need to stay in sync: the database and the worker. Schema changes, table drops, API failures, and worker restarts all require coordination.

The archive suggests this coordination cost was higher than expected, or that Timescale found a simpler architecture achieved the same goal with less operational surface area. Either way, it's a useful data point for anyone designing embedding infrastructure today.

---

## pg_vectorize: What it does well

pg_vectorize (now maintained independently by Chuck Henderson, formerly of Tembo) is the most active open-source tool for incremental embedding updates in PostgreSQL. It's written in Rust, supports both a PostgreSQL extension mode and a standalone HTTP server mode, and uses pgmq (a message queue built on PostgreSQL) for asynchronous processing.

The basic flow looks like this:

```bash
# Start the embedding service alongside PostgreSQL
docker compose up -d

# Register a vectorization job
curl -X POST http://localhost:8080/api/v1/table \
  -d '{
    "job_name": "my_products",
    "src_table": "products",
    "src_columns": ["product_name", "description"],
    "primary_key": "product_id",
    "update_time_col": "updated_at",
    "model": "sentence-transformers/all-MiniLM-L6-v2"
  }'

# Search
curl "http://localhost:8080/api/v1/search?job_name=my_products&query=camping+gear&limit=5"
```

In extension mode, you use SQL functions (`vectorize.table()`, `vectorize.search()`) for the same operations. The extension relies on pgmq under the hood — source-table changes enqueue messages, the worker dequeues them, calls the embedding service, and writes back.

**What pg_vectorize does well:**
- Keeps a single table's embeddings synchronized with its source text column.
- Works with managed PostgreSQL (RDS, Cloud SQL) where you can't install arbitrary extensions — just run the HTTP server separately.
- Handles failures gracefully via the pgmq retry mechanism.
- Local model support (via `vector-serve`, a bundled embedding server) — no dependency on external API if you run your own models.
- Actively maintained: the v0.26.x line was released this week.

**What pg_vectorize doesn't do:**
- Multi-table denormalization. It syncs `source_column → embedding`. It doesn't maintain a denormalized join of `documents + tags + permissions + metadata` as a searchable flat table.
- Aggregate vectors. There's no `vector_avg` concept — no way to maintain per-user or per-cluster centroids incrementally.
- ANN index management. It embeds rows; it does not know about or manage IVFFlat drift or HNSW tombstone accumulation.
- SQL expressiveness. The vectorization pipeline is: one table, one text column, one embedding model. The richer "define any query, maintain the result" pattern is out of scope.

pg_vectorize solves a narrow but real problem cleanly. If your use case is exactly "text column changed → re-embed → update vector column," it's a solid choice.

---

## The DIY approach: Why most teams still roll their own

Despite both pgai and pg_vectorize existing, most production RAG systems use a homebrew pipeline. The pattern:

1. A database trigger or application-level change tracking (e.g., an `updated_at` timestamp, a `needs_reembedding` boolean flag).
2. A background job (Celery, Sidekiq, AWS Lambda on SQS, a simple `while True:` loop) that polls for rows needing re-embedding.
3. A call to the embedding API.
4. A write-back to the database.

This pattern persists because it's flexible. You control the embedding logic. You can batch efficiently. You can handle weird edge cases (chunks, metadata injection, different models for different content types) without fighting an abstraction layer.

The cost is operational: you own the retry logic, the failure alerting, the backlog monitoring, the queue depth, and the correctness guarantees. When the worker falls behind or crashes silently, someone gets paged.

**The fundamental limitation** of all these approaches — DIY, pgai, and pg_vectorize alike — is that they answer a single question: "When the source text changes, what's the new embedding?"

They don't answer: "When any input to my search corpus changes — text, metadata, permissions, tags, related records — how do I propagate that change to the thing users are actually searching over?"

---

## Debezium + pgvector: The enterprise approach

Debezium is a CDC (change data capture) tool from Red Hat that reads PostgreSQL's write-ahead log, converts row-level changes into structured events, and streams them to Kafka. Since 2024, Debezium has supported `vector` column types — changes to pgvector columns are serialized correctly and can flow through the Kafka ecosystem.

The typical architecture:

```
PostgreSQL (WAL) → Debezium → Kafka → Consumer (Python/Java) → Embedding API → Write back to PostgreSQL
```

Or for a search-specific variant:

```
PostgreSQL (WAL) → Debezium → Kafka → Consumer → Compute denormalized record → Write to search PG instance
```

**What Debezium brings:**
- Rock-solid, battle-tested CDC for PostgreSQL.
- Works at scale — it's what you use when you're running a multi-hundred-GB database with heavy write load.
- Flexible: the Kafka consumer can do anything — embedding, enrichment, routing to multiple sinks, exactly-once delivery.
- Supports `vector` type natively now (useful for migrating vector data between systems).

**What Debezium doesn't bring:**
- Anything in-database. Debezium requires Kafka, a Kafka Connect deployment, and at minimum one consumer service. You're building and maintaining a distributed system.
- SQL-level reasoning. The consumer sees row-level deltas, not query-level semantics. If your denormalized search document depends on five tables, the consumer must join and reconcile those changes itself — which is basically writing a CDC-aware ETL pipeline from scratch.
- Incremental aggregation. Debezium streams changes. It doesn't compute vector means, maintain group aggregates, or understand what a "change to one source row" means for a derived result.
- Anything approaching "keep my ANN index fresh" — you'd need to build that on top.

Debezium is infrastructure, not application logic. It moves data reliably. It doesn't maintain derived state.

---

## The Dedicated Vector Databases: Not What You Think

Pinecone, Weaviate, Qdrant, Milvus — these are purpose-built systems that offer excellent approximate nearest-neighbor search with rich filtering. They're also frequently compared to pgvector.

Worth saying directly: **these aren't really pgvector tooling**. They're alternative systems, not enhancements. If you're on pgvector, you've already decided to stay in PostgreSQL. The dedicated vector databases solve a different problem for a different choice.

But they're relevant here because they're the most common alternative when pgvector users hit production scale problems. The pattern is: start with pgvector, hit a friction point (staleness, index maintenance, multi-table search complexity), and investigate moving to a dedicated vector database.

**What dedicated vector databases do well:**
- Very fast ANN search with rich metadata filtering, optimized end-to-end.
- Managed services with automatic index maintenance.
- High-dimensional vectors (many support >10,000 dimensions natively).
- Real-time ingestion at high write volume.

**What they don't do:**
- Transactional consistency with your PostgreSQL source data. You have a synchronization problem between your source database and the vector store.
- SQL joins, GROUP BY, window functions, CTEs over your vector corpus — you're limited to metadata filters.
- Any notion of incremental view maintenance. You push rows in; they're indexed. There's no concept of "this row's value is derived from these five other tables."
- Free. The managed options are expensive at scale.

The migration from pgvector to Pinecone/Weaviate/etc. typically doesn't simplify the embedding pipeline — it just moves the synchronization problem from "database ↔ embedding column" to "database ↔ external service." You still need to know when things changed and push them over.

---

## What All These Tools Have in Common

Step back from the specifics and a pattern emerges.

Every tool in this list — pgai (archived), pg_vectorize, DIY batch jobs, Debezium pipelines, dedicated vector databases — is solving the same narrow problem:

**When source text or data changes, how do I generate or re-deliver the embedding to where it needs to be?**

That's **Layer 1**: embedding generation and delivery.

What none of them solve is **Layer 2**: maintaining the derived structures that live downstream of those embeddings, and keeping them consistent with both their source embeddings *and* the rest of your data.

---

## Where pg_trickle Fits

pg_trickle operates entirely at Layer 2 — and occasionally Layer 1 overlap, but in a fundamentally different way.

The core of pg_trickle is incremental view maintenance (IVM). You define a SQL query, and pg_trickle maintains the result of that query as a live table, updating it incrementally as inputs change. The query can be anything: joins, aggregates, window functions, CTEs, subqueries.

For embeddings specifically, this matters in three ways that none of the Layer 1 tools address.

### The denormalization problem

Your embedding column lives in `documents.embedding`. Your users search for documents. But what they're actually searching over is:

> "A document, from an active project, with its category name, its author's display name, and its ACL groups — searchable by the text of the document and the vector of that text."

That's four tables. The embedding is one attribute of one of them. What users search over is a denormalized flat record composed of all four.

pg_trickle maintains that flat record automatically:

```sql
SELECT pgtrickle.create_stream_table(
  name => 'doc_search_corpus',
  query => $$
    SELECT
      d.id,
      d.body,
      d.embedding,              -- your embedding, wherever it lives
      p.name AS project_name,
      u.display_name AS author,
      acl.allowed_groups,
      array_agg(t.name) AS tags
    FROM documents d
    JOIN projects p ON p.id = d.project_id
    JOIN users u ON u.id = d.author_id
    JOIN doc_acl acl ON acl.doc_id = d.id
    LEFT JOIN doc_tags t ON t.doc_id = d.id
    WHERE d.published = true
    GROUP BY d.id, p.name, u.display_name, acl.allowed_groups
  $$,
  schedule => '10 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

When someone updates a tag, renames a project, changes a permission, or edits the document text — only the affected rows in `doc_search_corpus` are updated. The rest is untouched. The HNSW index on `doc_search_corpus.embedding` receives targeted insert/delete pairs, not a full rebuild.

pg_vectorize cannot do this. It doesn't have a join concept. pgai couldn't do this either. This is a fundamentally different operation: SQL-level derivation, not embedding-API orchestration.

### The aggregate vector problem

Recommendation systems, clustering pipelines, and personalization engines all maintain aggregate vectors — centroid-style representations computed over groups of individual embeddings.

```sql
-- The "taste" of user 42, averaged over every item they've liked
SELECT vector_avg(item.embedding)
FROM user_likes ul
JOIN items i ON i.id = ul.item_id
WHERE ul.user_id = 42;
```

If you precompute this per user and store it (the only production-viable approach), it goes stale the instant someone likes a new item. The classic solutions are "recompute nightly" or "trigger a background job on each like."

pg_trickle maintains this incrementally:

```sql
SELECT pgtrickle.create_stream_table(
  name => 'user_taste',
  query => $$
    SELECT ul.user_id,
           vector_avg(i.embedding) AS taste_vec,
           COUNT(*) AS like_count
    FROM user_likes ul
    JOIN items i ON i.id = ul.item_id
    GROUP BY ul.user_id
  $$,
  refresh_mode => 'DIFFERENTIAL'
);
```

When user 42 likes item 1701, the engine computes:
```
new_taste = (old_sum_vector + item_1701.embedding) / (old_count + 1)
```

Only user 42's row changes. The HNSW index on `taste_vec` receives one update. One million users, thousands of likes per second — each cycle touches only the affected rows.

The algebraic trick here (`vector_avg` as a running `sum / count`) is the same mathematics pg_trickle uses for `AVG(price)` or `SUM(revenue)`. Extending it to vectors requires a rule-set addition, not an engine change. That's shipping in v0.37.

### The index maintenance problem

IVFFlat recall drops as the distribution of your embeddings shifts. HNSW accumulates tombstones as you delete old documents. Neither pgvector nor any of the embedding tools handle this automatically.

pg_trickle tracks `rows_changed_since_last_reindex` as a first-class catalog metric. You set a policy:

```sql
SELECT pgtrickle.alter_stream_table(
  'doc_search_corpus',
  post_refresh_action     => 'reindex_if_drift',
  reindex_drift_threshold => 0.10
);
```

After each refresh, the scheduler checks whether 10% of rows have changed since the last `REINDEX`. If so, it queues a concurrent `REINDEX` in a lower-priority tier that runs without blocking your queries. This ships in v0.38.

No other tool in this space does this. It's an operational gap that has existed since pgvector launched, and the standard answer is still "rebuild on a schedule."

---

## The Natural Architecture

The important thing to understand is that pg_trickle and the Layer 1 embedding tools are **not competitors**. They're designed for adjacent problems.

The natural stack looks like this:

```
Source Tables (documents, users, products, ...)
    │
    ▼  [Layer 1: pgai (archived) / pg_vectorize / your app code]
    │  "text changed → call embedding API → write vector column"
    │
Documents now have an embedding column
    │
    ▼  [Layer 2: pg_trickle]
    │  "embedding (and metadata) changed → maintain derived structures"
    │
Denormalized corpora (search_corpus, user_taste, product_clusters, ...)
    │
    ▼  [pgvector: HNSW / IVFFlat index, ANN queries]
    │
Application queries
```

Layer 1 solves: "How do I re-embed a row when its text changes?"
Layer 2 solves: "How do I maintain everything downstream of that embedding?"

They're different questions with different answers.

If you're using pg_vectorize to keep `documents.embedding` fresh, pg_trickle then builds on top of that. When pg_vectorize writes a new embedding back to `documents.embedding`, pg_trickle's CDC trigger captures that change and propagates it to every stream table that depends on `documents`. The denormalized search corpus updates automatically. User taste vectors that depend on item embeddings update automatically. The index maintenance policy fires automatically.

The key principle: **embeddings are derived data, just like any other derived data.** The fact that computing them requires an external API call is a Layer 1 concern. Everything that happens after those embeddings exist — how they combine with other data, how they aggregate, how they're indexed, how freshness is monitored and enforced — is a Layer 2 concern, and that's where pg_trickle operates.

---

## The Honest Comparison Matrix

Here's where each tool actually sits:

| Capability | DIY batch | pg_vectorize | pgai (archived) | Debezium | pg_trickle |
|---|---|---|---|---|---|
| Generate embeddings via API | Hand-rolled | ✅ | ✅ | ❌ | ❌ (not its job) |
| Async retry & rate-limit handling | Hand-rolled | ✅ | ✅ | ❌ | ❌ |
| Single-table embedding sync | ✅ (manual) | ✅ | ✅ | ✅ | ✅ (passthrough) |
| Multi-table denormalized corpus | Hand-rolled | ❌ | ❌ | Partial (consumer code) | ✅ (native) |
| Incremental `vector_avg` aggregates | ❌ | ❌ | ❌ | ❌ | ✅ (v0.37) |
| ANN index drift detection | ❌ | ❌ | ❌ | ❌ | ✅ (v0.38) |
| Full SQL expressiveness | ❌ | ❌ | ❌ | ❌ | ✅ |
| In-database, no external processes | ❌ | Partial | ❌ | ❌ | ✅ |
| ACID-correct derivation | ❌ | Partial | Partial | ❌ | ✅ |
| Reactive alerts on distance predicates | ❌ | ❌ | ❌ | ❌ | ✅ (v0.39) |
| Actively maintained | ✅ | ✅ | ❌ (archived) | ✅ | ✅ |

---

## The Case For Simplifying Your Stack

The common outcome of adding Layer 1 tools, then adding Layer 2 orchestration, then adding Debezium for auditing, is a system with five moving parts that each require monitoring, versioning, and operational expertise.

The case for pg_trickle is not that it eliminates all infrastructure — you still need something to generate embeddings (an app, pgai, pg_vectorize, or pgml). The case is that it eliminates the *ad hoc* infrastructure: the hand-rolled denormalization sync, the custom batch job for centroid recomputation, the manual reindex schedule, the DIY staleness monitoring.

Those pieces live outside the database and outside the transaction boundary, which means they're brittle. They can fail silently, fall behind, or produce state inconsistent with the rest of your data. Pulling them into the database — where changes are captured transactionally, derivations are computed algebraically, and monitoring is a live view — makes them observable and correct by construction.

One embedding architecture that's held up well in production RAG systems:

1. **Application writes** `chunk_text` to `documents`. If you're generating embeddings synchronously (fast models, low latency), write `embedding` in the same transaction. If you're using an async embedding service (slow models, high cost), use pg_vectorize or a lightweight background job to write the embedding asynchronously within a few seconds.

2. **pg_trickle** maintains the search corpus, user taste vectors, product clusters, and any other derived structure as stream tables. These update automatically — within 5–10 seconds of the embedding arriving.

3. **pgvector** provides the HNSW or IVFFlat indexes on the stream tables. pg_trickle's drift-aware reindex policy keeps them healthy.

4. **Application queries** run against flat, fresh, indexed stream tables with clean metadata. No post-fetch filtering, no over-fetching.

The embedding generation is handled. The derived-state problem is handled. The index maintenance is handled. Everything is visible in one monitoring view.

---

## What to Take Away

The pgvector tooling ecosystem in 2026 is in an interesting moment. pgai's archive is a signal that embedding pipelines as out-of-database processes carry real operational cost. pg_vectorize is the most pragmatic open-source answer to the embedding generation problem. Debezium is the enterprise answer to streaming data at scale. And pg_trickle is the answer to the problem that none of them address: derived state maintenance downstream of embeddings.

If you're at the stage of "I need to keep one embedding column in sync with one text column," pg_vectorize is the right tool. Start there.

If you're at the stage of "my search corpus is stale, my user taste vectors are rebuilt nightly, my IVFFlat index is drifting, and my denormalized search document is hand-maintained with triggers," that's the Layer 2 problem. That's pg_trickle's domain.

The two aren't alternatives. They're layers. And understanding which layer your problem lives in is the most important step in choosing the right tool.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance.
Source, documentation, and the pgvector integration roadmap are at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
