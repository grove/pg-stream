# pg_trickle × ParadeDB (pg_search) — Synergy Report

**Date:** 2026-04-16  
**Author:** Internal research  
**Status:** PROPOSED

---

## 1. Executive Summary

**pg_trickle** incrementally maintains materialized views inside PostgreSQL.
**ParadeDB** (the `pg_search` extension) brings Elastic-quality full-text search,
BM25 scoring, columnar analytics, and faceted aggregates to PostgreSQL via a
custom BM25 index type backed by Tantivy and Apache DataFusion.

The two extensions are **strongly complementary**: pg_trickle pre-computes
denormalized or aggregated datasets with low-latency differential refresh;
ParadeDB makes those datasets instantly searchable with BM25 ranking, facets,
and sub-millisecond Top K queries — all without leaving PostgreSQL.

The combination eliminates the Elasticsearch sidecar and the ETL pipeline that
feeds it, replacing them with a single PostgreSQL instance (or replica) running
both extensions.

---

## 2. ParadeDB Overview

| Attribute | Details |
|---|---|
| **Repository** | [paradedb/paradedb](https://github.com/paradedb/paradedb) |
| **Language** | Rust (pgrx) — 86.5% Rust, 12.9% PLpgSQL |
| **Core dependencies** | pgrx, Tantivy (full-text), Apache DataFusion (OLAP) |
| **License** | AGPL-3.0 (Community); commercial (Enterprise) |
| **Stars / Contributors** | ~8.7k ★ / 113 contributors |
| **Latest release** | v0.22.6 (April 2026) |
| **PG versions** | 15–18 (logical replica mode supports PG 17+) |
| **Deployment models** | In-process extension, Docker image, logical replica of primary PG |

### 2.1 Key Capabilities

| Capability | Description |
|---|---|
| **BM25 Index** | Custom index access method (`USING bm25`) backed by Tantivy. Covers all indexed columns in a single covering index per table. |
| **Full-Text Search** | BM25 scoring, match/phrase/term/fuzzy/regex queries, highlighting, custom tokenizers & token filters (ICU, edge n-gram, stemming, etc.). |
| **Columnar Storage** | Non-text fields (and optionally text) stored column-oriented inside the BM25 index for fast filtering, sorting, and aggregates. |
| **Filter Pushdown** | `WHERE` predicates on indexed columns pushed into the Tantivy scan — avoids heap access. |
| **Top K** | Optimized `ORDER BY … LIMIT` execution inside the index (TopKScanExecState). |
| **Aggregates / Facets** | `pdb.agg()` function executes Elasticsearch-compatible aggregate JSON against the columnar index. Faceted queries return results + aggregate in one pass. |
| **Join Pushdown (beta)** | `INNER`, `SEMI`, `ANTI` joins between BM25-indexed tables executed inside DataFusion. `LEFT`, `RIGHT`, `FULL OUTER`, `LATERAL` coming soon. |
| **Custom Operators** | `|||` (match disjunction), `@@@` (match), `===` (exact term), `###` (phrase), etc. |
| **Real-Time Writes** | LSM-tree architecture: every INSERT/UPDATE/COPY creates a new segment; background merging compacts segments. |
| **Logical Replication** | Can subscribe to a primary PG via logical replication, maintaining BM25 indexes locally. |

### 2.2 Architecture Highlights

```
┌─────────────────────────────────────────────────┐
│             PostgreSQL + pg_search              │
│                                                  │
│  ┌────────────┐    ┌──────────────────────────┐ │
│  │  Heap      │    │  BM25 Index (Tantivy)    │ │
│  │  Table     │◄──▶│  ┌──────────────────┐    │ │
│  │            │    │  │ Inverted Index    │    │ │
│  └────────────┘    │  │ (token → postings)│    │ │
│                    │  ├──────────────────┤    │ │
│                    │  │ Columnar Index   │    │ │
│                    │  │ (fast fields)    │    │ │
│                    │  ├──────────────────┤    │ │
│                    │  │ LSM Tree         │    │ │
│                    │  │ (segments + merge)│    │ │
│                    │  └──────────────────┘    │ │
│                    └──────────────────────────┘ │
│                                                  │
│  Custom Scan → DataFusion query engine           │
│  (filter pushdown, Top K, aggregates, joins)     │
└─────────────────────────────────────────────────┘
```

- The BM25 index is updated synchronously within the transaction (ACID with the
  caveat that Community edition doesn't WAL-log the index — Enterprise does).
- Custom scan nodes intercept queries containing ParadeDB operators and execute
  them via DataFusion, pushing filters, sorts, and aggregates into the index.
- Parallel workers are used for large scans and aggregates.

### 2.3 Guarantees & Limitations

| Guarantee | Community | Enterprise |
|---|---|---|
| ACID transactions | ACI (no crash durability for index) | Full ACID (WAL-logged) |
| Logical replication | Subscriber only | Subscriber + HA |
| Physical replication | Not supported | Supported |
| Max tested DB size | ~10 TB | Same |
| Concurrent R/W | Full MVCC | Full MVCC |

**Key limitations:**
- One BM25 index per table (covering index model).
- Adding/removing indexed columns requires `REINDEX`.
- Join pushdown requires `LIMIT` and is restricted to INNER/SEMI/ANTI today.
- Aggregate pushdown via `pdb.agg()` uses Elasticsearch JSON DSL, not plain SQL
  (DataFusion-powered SQL aggregates are on the roadmap).
- DDL is not replicated via logical replication.

---

## 3. Synergy Analysis

### 3.1 The Core Value Proposition

**Without pg_trickle:** Users who need to search across JOINed or aggregated
data must either (a) denormalize manually into a flat table and index it with
ParadeDB, or (b) rely on ParadeDB's nascent join pushdown, which has significant
restrictions (beta, LIMIT required, no aggregates, only 3 join types).

**With pg_trickle:** A stream table can define an arbitrarily complex query
(multi-way JOINs, GROUP BY, window functions, CTEs, subqueries, UNION, EXCEPT,
etc.) and pg_trickle keeps the result incrementally maintained. ParadeDB then
indexes that single flat stream table with a BM25 index. The combination gives
users:

1. **Full SQL expressiveness** for the denormalization (pg_trickle).
2. **Elastic-quality search** over the result (ParadeDB).
3. **Automatic freshness** — no ETL pipeline to babysit.
4. **Single PostgreSQL process** — no Elasticsearch, no Kafka, no sidecar.

### 3.2 Synergy Matrix

| Synergy | pg_trickle Provides | ParadeDB Provides | Combined Value |
|---|---|---|---|
| **S-1: Searchable Materialization** | Incrementally maintained flat table from complex joins/aggregates | BM25 full-text search, scoring, highlighting | Elastic-quality search over live, incrementally refreshed data |
| **S-2: Faceted Dashboard Acceleration** | Pre-aggregated fact tables (GROUP BY dimensions) refreshed on schedule | Faceted aggregates via `pdb.agg()` with columnar pushdown | Sub-second faceted dashboards without Elasticsearch or OLAP engine |
| **S-3: Real-Time Search Replica** | Stream tables fed by trigger-based CDC (sub-second capture) | BM25 index updated in real-time via LSM tree | Near-real-time full-text search over live OLTP data |
| **S-4: Denormalized Search Index** | Automatic denormalization of star/snowflake schemas | Single BM25 covering index on the flat output | Eliminates manual ETL for search index maintenance |
| **S-5: Hybrid Analytical Search** | Window functions, statistical aggregates (CORR, REGR_*) in stream tables | Columnar scans + filter pushdown over numeric fields | Analytical queries with full-text predicates, no data warehouse needed |
| **S-6: Multi-Level Pipelines** | Stream tables can depend on other stream tables (DAG) | BM25 index on any level of the pipeline | Search at any materialization tier (raw → enriched → aggregated) |
| **S-7: Vector Search Prep** | pg_trickle can materialize embedding-generation queries (when using pgml or similar) | ParadeDB roadmap includes native vector search; today works with pgvector | Pre-computed embeddings + hybrid search in one database |

### 3.3 Deployment Architectures

#### Architecture A: Co-Located (Single Instance)

```
┌──────────────────────────────────────────────────────────┐
│                  PostgreSQL 18                             │
│                                                           │
│  ┌──────────┐  CDC  ┌───────────────┐                    │
│  │ Source    │──────▶│  pg_trickle   │                    │
│  │ Tables    │       │  Stream Table │                    │
│  └──────────┘       └───────┬───────┘                    │
│                             │ (flat PG table)            │
│                     ┌───────▼───────┐                    │
│                     │  BM25 Index   │ ← pg_search        │
│                     │  (Tantivy)    │                    │
│                     └───────────────┘                    │
│                                                           │
│  Application ──SQL──▶ SELECT * FROM stream_table          │
│                       WHERE description ||| 'query'       │
│                       ORDER BY pdb.score(id) DESC         │
│                       LIMIT 10;                           │
└──────────────────────────────────────────────────────────┘
```

**Pros:** Simplest deployment; zero network hops; single backup unit.  
**Cons:** Search index writes compete with OLTP workload on same instance.  
**Best for:** Small-to-medium workloads (< 1 TB), dev/staging environments.

#### Architecture B: Search Replica (Logical Replication)

```
┌─────────────────────┐     logical     ┌─────────────────────────┐
│  Primary PostgreSQL  │   replication   │  Search Replica (PG 18) │
│                      │───────────────▶│                          │
│  ┌──────────────┐   │                │  ┌───────────────┐       │
│  │ Source Tables │   │                │  │ pg_trickle    │       │
│  │ (writes)     │   │                │  │ Stream Tables │       │
│  └──────────────┘   │                │  └───────┬───────┘       │
│                      │                │          │               │
│  Application writes  │                │  ┌───────▼───────┐       │
│  go here             │                │  │ BM25 Index    │       │
└─────────────────────┘                │  │ (pg_search)   │       │
                                       │  └───────────────┘       │
                                       │                          │
                                       │  Application reads /     │
                                       │  search queries go here  │
                                       └─────────────────────────┘
```

**Pros:** Search workload fully isolated from OLTP; scales reads independently.  
**Cons:** Replication lag; more operational complexity; requires `wal_level=logical`.  
**Best for:** Production workloads where search load is heavy or latency-sensitive.

#### Architecture C: ParadeDB as Logical Subscriber + pg_trickle on Primary

```
┌───────────────────────────┐   logical   ┌──────────────────────┐
│  Primary PG + pg_trickle  │  replication│  ParadeDB Subscriber │
│                           │────────────▶│                       │
│  Source Tables             │  (stream    │  BM25 indexes on     │
│        ↓ CDC              │   table     │  replicated stream   │
│  Stream Tables (flat)     │   data)     │  tables               │
│                           │             │                       │
└───────────────────────────┘             └──────────────────────┘
```

**Pros:** pg_trickle does the heavy delta computation on the primary; ParadeDB
subscriber receives pre-denormalized rows and only needs to maintain BM25
indexes. The subscriber can be ParadeDB's optimized Docker image.  
**Cons:** Slightly more lag; DDL must be coordinated.  
**Best for:** Managed PG primaries (RDS, Cloud SQL) where installing pgrx
extensions is hard but a self-managed search replica is acceptable.

---

## 4. Concrete Use Cases

### 4.1 E-Commerce Product Search

**Problem:** Products span multiple tables (products, categories, brands,
reviews, inventory). Searching across them requires either application-side
denormalization or Elasticsearch with ETL.

**Solution:**
```sql
-- pg_trickle: incrementally maintained denormalized product view
SELECT pgtrickle.create_stream_table(
  'product_search',
  $$
    SELECT p.id, p.name, p.description, c.name AS category,
           b.name AS brand, AVG(r.rating) AS avg_rating,
           COUNT(r.id) AS review_count, i.quantity AS stock
    FROM products p
    JOIN categories c ON p.category_id = c.id
    JOIN brands b ON p.brand_id = b.id
    LEFT JOIN reviews r ON r.product_id = p.id
    LEFT JOIN inventory i ON i.product_id = p.id
    GROUP BY p.id, p.name, p.description, c.name, b.name, i.quantity
  $$,
  schedule := '5 seconds'
);

-- pg_search: BM25 index for full-text search + faceted filtering
CREATE INDEX product_bm25 ON product_search
USING bm25 (id, name, description, (category::pdb.literal),
            (brand::pdb.literal), avg_rating, review_count, stock)
WITH (key_field = 'id');

-- Application query: full-text + filter + sort + facets
SELECT id, name, description, category, avg_rating,
       pdb.score(id) AS relevance,
       pdb.agg('{"terms": {"field": "category"}}') OVER ()
FROM product_search
WHERE description ||| 'wireless headphones'
  AND avg_rating >= 4.0
  AND stock > 0
ORDER BY pdb.score(id) DESC
LIMIT 20;
```

### 4.2 Log / Event Analytics with Search

**Problem:** Event logs need both full-text search (error messages, stack traces)
and time-series aggregation (counts by hour, error rate).

**Solution:**
```sql
-- pg_trickle: hourly error summary, incrementally maintained
SELECT pgtrickle.create_stream_table(
  'error_summary',
  $$
    SELECT date_trunc('hour', created_at) AS hour,
           service_name, error_type, severity,
           string_agg(DISTINCT message, ' | ') AS messages,
           COUNT(*) AS error_count,
           COUNT(DISTINCT user_id) AS affected_users
    FROM events
    WHERE severity IN ('ERROR', 'CRITICAL')
    GROUP BY 1, 2, 3, 4
  $$,
  schedule := '30 seconds'
);

-- pg_search: search error messages + aggregate by service
CREATE INDEX error_bm25 ON error_summary
USING bm25 (hour, (service_name::pdb.literal), (error_type::pdb.literal),
            (severity::pdb.literal), messages, error_count, affected_users)
WITH (key_field = 'hour');  -- or synthetic PK

SELECT service_name, hour, error_count, messages
FROM error_summary
WHERE messages ||| 'timeout connection refused'
  AND severity === 'CRITICAL'
ORDER BY error_count DESC
LIMIT 10;
```

### 4.3 Content Management / Knowledge Base

**Problem:** Articles span authors, tags, and sections. Users need relevance-
ranked search with category facets and freshness sorting.

**Solution:** pg_trickle denormalizes articles + tags + authors into a flat
stream table; ParadeDB provides BM25-scored search with faceted tag counts.

### 4.4 Financial Transaction Monitoring

**Problem:** Compliance teams search transactions by description, counterparty,
and amount range. Data comes from multiple normalized tables (accounts,
transactions, counterparties, risk_scores).

**Solution:** pg_trickle joins and enriches in real-time; ParadeDB enables
instant full-text + range filtering with BM25 scoring.

---

## 5. Technical Integration Considerations

### 5.1 Index Maintenance on Stream Tables

When pg_trickle refreshes a stream table, it applies deltas via `MERGE`
(differential mode) or `TRUNCATE` + re-populate (full mode). Both paths are
standard DML that the BM25 index hooks into via PostgreSQL's index AM callbacks.

| Refresh Mode | BM25 Index Impact | Notes |
|---|---|---|
| **DIFFERENTIAL** | Small INSERTs/DELETEs → new LSM segments | Ideal: minimal index churn, fast segment merges |
| **FULL** | TRUNCATE + bulk INSERT → full segment rebuild | Higher cost but still automatic; background merge amortizes it |
| **IMMEDIATE** | Per-statement trigger → per-statement index update | Highest freshness but most index write pressure |

**Key insight:** Differential refresh produces small, targeted deltas — exactly
what the LSM-tree architecture is optimized for. The combination should perform
better than a full-refresh + reindex pattern.

### 5.2 `REINDEX` Coordination

ParadeDB requires `REINDEX` when index column definitions change. If
`alter_stream_table` changes the defining query (adding/removing columns), the
BM25 index must be rebuilt. Options:

1. **Documentation guidance:** Advise users to `REINDEX` after `ALTER`.
2. **Future hook:** pg_trickle could emit a `NOTIFY` event on schema change;
   a helper function or dbt macro could automate `REINDEX`.

### 5.3 Shared `pgrx` Runtime

Both extensions are built with pgrx. When loaded in the same PostgreSQL instance:

- They share the same `shared_preload_libraries` mechanism.
- Each has its own GUC namespace (`pg_trickle.*` vs `paradedb.*`).
- pgrx version compatibility should be verified (pg_trickle uses pgrx 0.17.x;
  ParadeDB currently also uses pgrx but may differ in minor version).
- Potential for shared-memory or worker contention — both register background
  workers. Ensure `max_worker_processes` is sized for both.

### 5.4 Memory & Resource Planning

| Resource | pg_trickle Impact | ParadeDB Impact | Recommendation |
|---|---|---|---|
| `shared_buffers` | Delta computation reads | Index segment cache | Increase by ~25% when running both |
| `work_mem` | MERGE operations | Aggregate computation | 64–256 MB per session |
| `max_worker_processes` | 1 scheduler worker | Parallel scan workers | ≥ 16 for both |
| `maintenance_work_mem` | Full refresh | `CREATE INDEX` / `REINDEX` | ≥ 1 GB |
| Disk I/O | Change buffer tables | LSM segment writes/merges | NVMe recommended |

### 5.5 Durability Considerations

pg_trickle stores all data in standard PostgreSQL tables (fully WAL-logged).
ParadeDB Community does **not** WAL-log the BM25 index — it can be rebuilt from
the heap after a crash. ParadeDB Enterprise adds full WAL integration.

For production deployments:
- Stream table data is always durable (pg_trickle guarantee).
- BM25 index is reconstructable from the stream table after crash (Community) or
  durable (Enterprise).
- This is strictly better than Elasticsearch, where both data and index can be
  lost.

---

## 6. Competitive Landscape

### 6.1 What This Replaces

| Traditional Stack | pg_trickle + ParadeDB |
|---|---|
| PostgreSQL → Debezium → Kafka → Elasticsearch | PostgreSQL + pg_trickle + pg_search |
| PostgreSQL → custom ETL → Elasticsearch | PostgreSQL + pg_trickle + pg_search |
| PostgreSQL → materialized views + cron → tsvector/GIN | PostgreSQL + pg_trickle + pg_search |
| PostgreSQL → ReadySet (cache) + Elasticsearch (search) | PostgreSQL + pg_trickle + pg_search |

### 6.2 Comparison with Alternatives

| Approach | Freshness | Search Quality | SQL Coverage | Ops Complexity |
|---|---|---|---|---|
| **pg_trickle + ParadeDB** | Seconds (differential) | BM25, facets, highlighting | Full SQL (JOINs, aggs, windows, CTEs) | Low (1 PG instance) |
| Debezium → Elasticsearch | Seconds (CDC) | BM25, facets, highlighting | Limited (denormalize in connector) | High (Kafka + ES cluster) |
| Manual matview + tsvector | Minutes (cron) | Basic (no BM25, no facets) | Full SQL | Low but fragile |
| pg_ivm + tsvector | Immediate | Basic | Limited SQL | Low |
| ReadySet + Elasticsearch | Sub-second + seconds | BM25, facets, highlighting | Limited (ReadySet SQL subset) | High (proxy + ES) |

---

## 7. Implementation Roadmap

### Phase 1 — Documentation & Compatibility Testing (Low effort)

| Item | Description | Effort |
|---|---|---|
| **D-1** | Compatibility test: install both extensions in PG 18, verify coexistence | 4h |
| **D-2** | Cookbook / tutorial: "Searchable Stream Tables with pg_trickle + ParadeDB" | 8h |
| **D-3** | Docker Compose example with both extensions pre-loaded | 4h |
| **D-4** | Document resource planning (shared_buffers, max_worker_processes) | 2h |

### Phase 2 — Docker Image & Playground (Medium effort)

| Item | Description | Effort |
|---|---|---|
| **I-1** | `pg_trickle + pg_search` combined Docker image (FROM paradedb/paradedb) | 8h |
| **I-2** | Playground extension: add ParadeDB search example to `playground/` | 4h |
| **I-3** | dbt macro: `post_hook` for auto-creating BM25 index on stream tables | 8h |
| **I-4** | CI smoke test: create ST → create BM25 index → search → verify results | 8h |

### Phase 3 — Deep Integration (Higher effort, future)

| Item | Description | Effort |
|---|---|---|
| **F-1** | `NOTIFY` event on stream table schema change → trigger `REINDEX` advisory | 4h |
| **F-2** | `pgtrickle.search_status()` view: show stream tables with BM25 indexes, index freshness, segment count | 8h |
| **F-3** | Benchmark suite: measure delta refresh impact on BM25 index throughput | 16h |
| **F-4** | Explore: pg_trickle IMMEDIATE mode + ParadeDB for sub-second searchable writes | 8h |
| **F-5** | Explore: ParadeDB as source for pg_trickle (search results as materialized input) | 8h |

### Phase 4 — Ecosystem (Long-term)

| Item | Description | Effort |
|---|---|---|
| **E-1** | Joint blog post / case study with ParadeDB team | — |
| **E-2** | CNPG Helm chart with both extensions | 8h |
| **E-3** | Grafana dashboard panels for combined monitoring (refresh latency + search QPS) | 8h |
| **E-4** | Vector search integration when ParadeDB ships native vector support | TBD |

---

## 8. Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| **pgrx version mismatch** | Medium | Compilation failure when both loaded | Pin compatible pgrx versions; test in CI |
| **Background worker contention** | Low | Reduced refresh or search throughput | Size `max_worker_processes` appropriately |
| **AGPL license conflict** | Medium | AGPL is copyleft; may affect distribution | pg_search is loaded separately; Apache 2.0 pg_trickle code is not derived from AGPL code. Clarify with legal for bundled images |
| **ParadeDB API instability** | Medium | Breaking SQL syntax changes (happened at v0.20.0) | Pin ParadeDB version in Docker images; integration tests catch regressions |
| **BM25 index rebuild cost** | Low | `REINDEX` after schema change can be slow for large tables | Differential refresh minimizes schema changes; document `REINDEX CONCURRENTLY` |
| **Community durability gap** | Low | BM25 index lost on crash | Stream table data is safe; `REINDEX` rebuilds. Enterprise removes this risk |

---

## 9. Open Questions

1. **ParadeDB interest in co-marketing?** A joint blog post or case study would
   benefit both projects. Worth reaching out to their team.
2. **pgrx ABI compatibility:** Do both extensions need to be compiled against
   the exact same pgrx version, or is ABI compatibility maintained across minor
   versions? Needs testing.
3. **`TRUNCATE` handling:** When pg_trickle does a full refresh with TRUNCATE,
   does ParadeDB's index AM properly handle the truncation callback? Needs
   verification.
4. **Enterprise vs Community for production:** Should we recommend Community for
   dev/staging and Enterprise for production in our docs, or stay neutral?
5. **Citus compatibility:** Both pg_trickle and ParadeDB claim Citus
   compatibility. Has the three-way combination been tested?

---

## 10. Conclusion

pg_trickle and ParadeDB/pg_search are architecturally complementary at every
level:

- **Data model:** pg_trickle produces flat, incrementally refreshed tables —
  exactly what ParadeDB's covering BM25 index is designed to index.
- **Write path:** Differential refresh produces small deltas — ideal for
  Tantivy's LSM-tree segment model.
- **Query path:** ParadeDB's custom scan pushes filters, sorts, and aggregates
  into the index — the stream table never needs sequential scans for search.
- **Deployment:** Both are in-process PostgreSQL extensions with no external
  dependencies, sharing the "zero-infrastructure" philosophy.

The combination replaces a PostgreSQL + Kafka + Elasticsearch stack with a single
PostgreSQL instance running two extensions. Phase 1 (compatibility testing +
documentation) is low-risk and high-value; it should be prioritized as soon as
pg_trickle reaches v1.0 stability.
