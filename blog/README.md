# pg_trickle Blog

> **Note:** This blog directory is an experiment. All posts were generated with
> AI assistance (GitHub Copilot / Claude) as a way to explore how well
> LLM-generated technical writing holds up for a niche systems engineering
> topic. The technical content has been reviewed for accuracy, but treat the
> posts as drafts — not as officially reviewed documentation.

---

## Posts

### Core Concepts

| Post | Summary |
|------|---------|
| [Why Your Materialized Views Are Always Stale](stale-materialized-views.md) | Explains why `REFRESH MATERIALIZED VIEW` fails at scale — locking, cost, and the full-scan ceiling — and how switching to a stream table with `DIFFERENTIAL` mode fixes staleness in 5 lines of SQL. |
| [Differential Dataflow for the Rest of Us](differential-dataflow-explained.md) | A plain-language walkthrough of the mathematics behind incremental view maintenance: delta rules for filters, joins, aggregates, the MERGE application step, and why some aggregates (MEDIAN, RANK) can't be made incremental. |
| [Incremental Aggregates in PostgreSQL: No ETL Required](incremental-aggregates-no-etl.md) | How `SUM`, `COUNT`, `AVG`, and (in v0.37) `vector_avg` are maintained as running algebraic state rather than full scans. Covers multi-table aggregates, conditional aggregates, and the non-differentiable cases. |

### Operational Deep Dives

| Post | Summary |
|------|---------|
| [The Hidden Cost of Trigger-Based Denormalization](trigger-denormalization-cost.md) | Four failure modes of hand-rolled trigger sync — blind UPDATE divergence, statement vs. row trigger semantics, invisible deletes, and multi-row races — and how declarative IVM avoids all of them. |
| [How We Replaced a Celery Pipeline with 3 SQL Statements](replaced-celery-with-sql.md) | A before/after case study of a Celery + Elasticsearch product search pipeline across three generations of growing complexity, and the pg_trickle stream table that replaced it. Includes benchmark numbers. |
| [Stop Rebuilding Your Search Index at 3am](stop-rebuilding-search-index.md) | How pg_trickle's scheduler, SLA tiers (`critical` / `standard` / `background`), backpressure, and parallel workers let you tune refresh behaviour per workload — and why the 3am maintenance window disappears with continuous incremental refresh. |

### pgvector Integration

| Post | Summary |
|------|---------|
| [Your pgvector Index Is Lying to You](incremental-pgvector.md) | Four silent failure modes of unmanaged pgvector deployments: stale embedding corpora, drifting aggregates, IVFFlat recall loss, and over-fetching. How pg_trickle's differential IVM and drift-aware reindexing closes each gap. |
| [HNSW Recall Is a Lie: Distribution Drift Explained](hnsw-recall-distribution-drift.md) | Deep dive on IVFFlat centroid staleness and HNSW tombstone accumulation — how to measure drift, what the right threshold is, and how `post_refresh_action => 'reindex_if_drift'` (v0.38) automates the fix. |
| [The pgvector Tooling Landscape in 2026](pgvector-tooling-landscape.md) | Honest comparison of pg_trickle against pgai (archived Feb 2026), pg_vectorize, DIY batch pipelines, and Debezium. Introduces the two-layer model: Layer 1 = embedding generation, Layer 2 = derived-state maintenance. |

### Advanced Patterns

| Post | Summary |
|------|---------|
| [Reactive Alerts Without Polling](reactive-alerts-without-polling.md) | How pg_trickle's reactive subscriptions (v0.39) replace polling loops: SLA breach detection, inventory alerts, fraud velocity checks, and vector distance subscriptions. Covers `OLD.*`/`NEW.*` transition semantics and PostgreSQL `LISTEN`. |
| [Multi-Tenant Vector Search with Row-Level Security](multi-tenant-vector-search-rls.md) | Zero cross-tenant data leakage using RLS policies on stream tables, tiered tenancy (large / medium / small tenant strategies), per-tenant partial HNSW indexes, and drift-aware reindexing per partition. |
| [The Outbox Pattern, Turbocharged](outbox-pattern-turbocharged.md) | Using stream tables as transactionally consistent event sources for the outbox pattern — derived aggregate events, fat payloads, transition-based routing, and why stream tables naturally debounce high-frequency changes into fewer events. |

### Benchmarks & Infrastructure

| Post | Summary |
|------|---------|
| [TPC-H at 1GB in 40ms](tpch-benchmarking-ivm.md) | Reproducible benchmark of differential vs. full refresh across five TPC-H queries (Q1, Q3, Q5, Q6, Q12). Results: 13–22× faster per refresh cycle, with differential lag under 2.5 seconds vs. 186 seconds at 5,000 rows/second sustained write load. |
| [pg_trickle on CloudNativePG](pg-trickle-cloudnativepg-kubernetes.md) | Production Kubernetes deployment using the CloudNativePG operator: Dockerfile, Cluster manifest, GUC configuration, HA failover behaviour, Prometheus metrics ConfigMap, alerting rules, upgrade procedure, and sizing guidance. |

---

## Contributing

These posts are deliberately rough-edged — they're drafts exploring how the extension works, not polished marketing copy. If you spot a technical inaccuracy, open an issue or PR. If you want to write a post, open a discussion first to avoid duplication.
