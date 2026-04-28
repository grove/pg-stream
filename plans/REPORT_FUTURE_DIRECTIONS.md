# pg_trickle — Future Directions Report

**Status:** Strategic exploration / discussion document
**Created:** 2026-04-28
**Audience:** Maintainers, contributors, prospective users, and anyone
trying to picture where this project could go after the v1.0 horizon.

> This report is a *broad picture*, not a roadmap. Its purpose is to map
> the directional space pg_trickle could grow into — including
> directions the official roadmap has not committed to. Items here range
> from "obvious next step" to "ten-year stretch idea". Each direction
> is examined for its motivation, technical shape, prerequisites,
> risks, and the strategic logic for or against pursuing it.
>
> For the committed plan, see [ROADMAP.md](../ROADMAP.md). For the
> design specification of the existing engine, see
> [docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md). For the project's
> founding intent, see [ESSENCE.md](../ESSENCE.md).

---

## 0. TL;DR

pg_trickle today is a single-node, in-process **incremental view
maintenance (IVM) engine for PostgreSQL** built on the DBSP
differential-dataflow calculus, written in Rust against pgrx 0.18, and
hardened across 38 minor releases (v0.1 → v0.38). The current
trajectory points at v1.0 in the near term, followed by a PostgreSQL 17
back-port and a PGlite/WASM expansion arc.

Beyond that explicit plan, pg_trickle has *latent directions* that are
visible in the design but not yet committed:

1. **Become the default IVM substrate of PostgreSQL** — displace the
   "cron + REFRESH MATERIALIZED VIEW" pattern industry-wide.
2. **Extend to the browser and the edge** via PGlite/WASM and
   client-side reactive bindings.
3. **Generalise as a cross-database appliance** — an Epsio-style
   "stand-alone IVM Postgres" that pulls CDC from any source.
4. **Become the real-time application substrate** — outbox, inbox,
   reactive subscriptions, CQRS, and event sourcing as first-class
   primitives.
5. **Specialise as the incremental AI/RAG infrastructure layer** for
   embeddings, vector aggregates, and drift-aware reindex.
6. **Distributed IVM** across Citus, Neon, multi-database, and
   eventually multi-region deployments.
7. **Self-tuning and cost-aware** — the system observes its own
   workload, picks refresh modes, schedules, and parallelism
   automatically.
8. **Open the engine** — extract `pg_trickle_core` and license the
   IVM substrate to other databases (DuckDB? SQLite? OrioleDB? Even
   non-Postgres engines).
9. **Standardise** — push for IVM primitives in the SQL standard or
   in the PostgreSQL core itself.
10. **Build the surrounding economy** — managed cloud, certifications,
    training, support contracts, partner ecosystem.
11. **Bridge PostgreSQL to lakehouse/open-table formats** — Iceberg,
    Delta Lake, Hudi, and Paimon sinks fed by stream-table deltas.
12. **Make data quality, governance, and compliance live** — continuously
    maintained invariants, reconciliations, controls, and audit trails.
13. **Ship vertical solution kits** — prebuilt stream-table packages for
    fraud, SaaS metrics, commerce, IoT, ledgers, and gaming.
14. **Move into geospatial, graph, and scientific IVM** — PostGIS,
    pgRouting, graph algorithms, simulations, and temporal spatial data.
15. **Own operational time-series rollups** — observability, SLOs,
    downsampling, retention, and incident analytics inside PostgreSQL.
16. **Power reverse ETL and data-product delivery** — push maintained
    facts into CRMs, search indexes, caches, SaaS APIs, and warehouses.
17. **Build a first-class developer experience** — visual DAGs, query
    advisors, migration tools, IDE extensions, and cost simulators.
18. **Lead the IVM benchmark and research ecosystem** — reproducible
    benchmark corpora, papers, public leaderboards, and correctness suites.
19. **Become a security and privacy-preserving computation primitive** —
    RLS-aware stream tables, tenant isolation, masked aggregates, and
    compliance-grade lineage.
20. **Co-design with PostgreSQL planner and storage evolution** — parallel
    delta execution, planner hooks, adaptive indexes, columnar paths, and
    future executor improvements.

Sections 1–20 below walk through each of those in depth.

---

## 1. Where pg_trickle is today

Establishing the baseline matters because the directional logic
depends on what is already true.

### 1.1. The product, in one paragraph

pg_trickle adds **stream tables** to PostgreSQL: tables defined by a
SQL query that stay continuously up to date as their inputs change,
without external infrastructure. Changes are captured by either
row-level triggers or logical-replication WAL decoding (hybrid CDC).
A scheduler refreshes each stream table at a declared cadence — or
inside the same transaction (`IMMEDIATE` mode), or whenever the user
asks. The refresh planner derives a *delta query* from the operator
tree of the defining SQL using DBSP-style differentiation rules, so
the work done per refresh is proportional to the change size, not to
the source table size.

### 1.2. The technical surface that already exists

| Capability | Status | Where |
|---|---|---|
| DBSP-style operator differentiation | Mature, 22/22 TPC-H | [`src/dvm/operators/`](../src/dvm/) |
| Hybrid CDC (triggers ↔ WAL) | Mature, with safe fallback | [`src/cdc.rs`](../src/cdc.rs), [`src/wal_decoder.rs`](../src/wal_decoder.rs) |
| DAG scheduler with calculated cadence | Mature | [`src/scheduler.rs`](../src/scheduler.rs), [`src/dag.rs`](../src/dag.rs) |
| Refresh modes (FULL, DIFFERENTIAL, IMMEDIATE, AUTO) | Mature | [`src/refresh.rs`](../src/refresh.rs) |
| Self-monitoring (pg_trickle observes itself) | v0.20.0 | [`src/api/self_monitoring.rs`](../src/api/) |
| Outbox / inbox primitives | v0.28.0 | [`src/api/outbox.rs`](../src/api/), [`src/api/inbox.rs`](../src/api/) |
| Citus distributed sources | v0.32–v0.34 | [`docs/CITUS.md`](../docs/CITUS.md) |
| Snapshots & PITR | v0.27.0 | [`src/api/snapshot.rs`](../src/api/), [`docs/SNAPSHOTS.md`](../docs/SNAPSHOTS.md) |
| pgVectorMV (vector aggregates) | v0.37.0 | embedding pipeline arc |
| Temporal IVM | v0.36.0 | [`docs/PATTERNS.md`](../docs/PATTERNS.md) |
| CloudNativePG integration | Done | [`cnpg/`](../cnpg/) |
| OpenTelemetry trace propagation | v0.37.0 | tracing infra |
| Property-tested join correctness (EC-01 closeout) | v0.38.0 | hard release gate |

### 1.3. The committed forward plan (compressed)

- **v0.39 → v0.40**: operational truthfulness, drain mode proof,
  Citus chaos rig, generated docs, alerting.
- **v0.41 → v0.43**: embedding pipeline, hybrid search, sparse and
  half-precision vector aggregates, ergonomic
  `embedding_stream_table()` API.
- **v1.0**: stable API, signed artifacts, SBOMs, package registries,
  CNPG-first cloud story, PG 19 forward-compat audit.
- **v1.1**: PostgreSQL 17 support (prerequisite for PGlite).
- **v1.2 → v1.5**: PGlite proof of concept → core extraction
  (`pg_trickle_core`) → WASM build → reactive UI bindings (React,
  Vue).

Everything past v1.5 is open territory. The rest of this report is
about that territory.

---

## 2. Direction 1 — Become PostgreSQL's default IVM

### 2.1. The opportunity

Materialized views were added to PostgreSQL in 9.3 (2013) and have
not fundamentally evolved since. `REFRESH MATERIALIZED VIEW`, even in
its `CONCURRENTLY` form, recomputes from scratch. Tens of thousands of
production deployments build the same workaround:

> A `pg_cron` job that calls `REFRESH MATERIALIZED VIEW` periodically,
> with manual coordination between dependent views, occasional
> refresh storms, and silent staleness when something fails.

pg_trickle replaces that pattern entirely. The directional question is
*how aggressively* it should pursue the role of "the default way you
keep a PostgreSQL view fresh."

### 2.2. What "default" would actually require

1. **Distribution everywhere.** PGDG `apt`/`rpm`, PGXN, Homebrew,
   AWS RDS extension allow-list, Google Cloud SQL allow-list, Azure
   Database for PostgreSQL allow-list, Supabase, Neon, Aiven,
   Crunchy Bridge, Tembo, Nile, Xata. Each of these is a relationship
   and a packaging story.
2. **Operational invisibility.** `CREATE EXTENSION pg_trickle;` and
   nothing else. Sensible defaults for shared memory, GUCs, worker
   counts, schedules, refresh modes. Self-tuning so the user does not
   need to learn the term "frontier" or "calculated cadence" to be
   successful.
3. **Compatibility in both directions.** Existing materialized views
   should be migratable in one statement. pg_ivm migration should be
   a single `pgtrickle.import_pg_ivm_view()` call (the migration
   guide already exists; the tool does not).
4. **Trust artefacts.** Signed builds, SBOMs, CVE response process,
   reproducible builds, audit by an independent reviewer, an SLA
   for security disclosures.
5. **A clear answer to "why not pg_ivm?"** Today: broader SQL,
   scheduler, DAG, hybrid CDC, distributed support. The
   [pg_ivm comparison](../docs/research/PG_IVM_COMPARISON.md) is
   honest about both projects' strengths.

### 2.3. The asymptote: pushing IVM into core PostgreSQL

The maximalist version of this direction is contributing IVM to
PostgreSQL itself — either as a contrib module like `pg_trgm` or as
new SQL syntax (`CREATE INCREMENTAL MATERIALIZED VIEW`). This has
precedent: pg_stat_statements started as an extension and is now
practically core. The community would scrutinise the operator-coverage
matrix, the WAL decoder integration, and the catalog footprint very
carefully, but DBSP-derived IVM is a compelling enough idea that the
conversation is at least worth having.

The realistic intermediate step is pursuing inclusion in
`postgresql-contrib`, where the bar is lower than core but the
distribution becomes universal.

### 2.4. Risks

- **Maintenance burden** scales with deployment surface. Every
  managed-PG vendor that adopts pg_trickle becomes a stakeholder.
- **Compatibility lock-in.** Once people deploy at scale, every
  catalog change becomes a multi-year migration. v1.0's API freeze
  is the line in the sand.
- **Competing extensions.** pg_ivm is mature in its niche; future
  PostgreSQL versions may add IVM features that overlap.

### 2.5. Strategic verdict

This direction is the *centre of gravity* of every other direction in
this report. Most other directions either depend on this one
succeeding (PGlite, AI/RAG infra, self-tuning) or are alternatives
that hedge against it not succeeding (the appliance model, opening
the engine to other databases). The default-IVM direction should be
the implicit success criterion for v1.0–v2.0.

---

## 3. Direction 2 — Browser and edge via PGlite/WASM

### 3.1. What is already planned

The v1.2 → v1.5 arc commits to a PGlite story:

- **v1.2** — JS-only proof of concept (`@pgtrickle/pglite-lite`)
  for 3–5 simple SQL patterns. Validates demand without core changes.
- **v1.3** — Extract `pg_trickle_core` as a pgrx-free Rust crate
  that compiles to WASM. ~51K lines of code, ~500 unsafe blocks
  abstracted behind a `DatabaseBackend` trait. The single most
  technically demanding refactoring in the project's history.
- **v1.4** — Wrap `pg_trickle_core` in a C shim and ship the
  full PGlite WASM extension (`@pgtrickle/pglite`) — outer joins,
  window functions, recursive CTEs, all in IMMEDIATE mode.
- **v1.5** — Reactive UI bindings: `useStreamTable()` hooks for
  React and Vue, bridged through PGlite's `live.changes()` API.

### 3.2. The strategic shape this could take

Reactive bindings are the *visible* part. The deeper opportunity is
the **local-first computing stack**:

```
┌─────────────────────────────┐
│  Browser app (React/Vue/…)  │
│  useStreamTable()           │
├─────────────────────────────┤
│  PGlite (WASM PostgreSQL)   │
│  + @pgtrickle/pglite        │  ← incremental SQL in the browser
├─────────────────────────────┤
│  CRDT / sync layer (e.g.    │
│  Electric SQL, Y.js, …)     │
└─────────────────────────────┘
                ↕
┌─────────────────────────────┐
│  Server PostgreSQL          │
│  + pg_trickle (native)      │  ← same IVM substrate, server-side
└─────────────────────────────┘
```

If the same query definition runs on both sides, you get
*end-to-end incremental computation*: a write on the server propagates
through server-side stream tables, syncs to the client (via Electric
SQL or any logical-replication-aware sync), and is incorporated by
the client-side stream table — which then re-renders only the
affected DOM. No diffing, no polling, no full query re-execution at
any layer.

This would be genuinely novel. As of writing, no production
differential-dataflow engine ships in the browser.

### 3.3. Hard problems

- **Parser version skew** — PGlite tracks PG 17, the native
  extension targets PG 18. Parse tree node structures differ. The
  v1.4 plan addresses this with a parse-tree compatibility audit
  but it is recurring work.
- **WASM bundle size** — target < 2 MB. PostGIS-WASM is 8 MB,
  pgcrypto-WASM is 1.1 MB. The DVM operator surface is large; some
  operators (recursive CTE, window functions) may need to be
  feature-gated for size-conscious bundles.
- **WASM heap discipline** — browsers cap heaps at ~256 MB.
  Stream tables on large data sets need either bounded buffers or
  spill-to-OPFS strategies.
- **Reactive correctness** — React 18 concurrent mode and React 19
  re-render semantics interact with batched delta application in
  subtle ways. The v1.5 correctness item set (CORR-1 through CORR-4)
  is non-trivial.

### 3.4. Beyond browsers

The same WASM artifact runs on:

- **Edge runtimes** — Cloudflare Workers, Vercel Edge, Deno Deploy,
  Bun. Stream tables in a worker-local PGlite, fed by a sync layer
  from a central database, with sub-millisecond local reads.
- **Mobile via React Native + op-sqlite or PGlite-native** — though
  this requires a non-WASM build target for iOS/Android.
- **Embedded devices** — anywhere a small PostgreSQL fits, IVM
  follows.

Each of these is a real, currently underserved use case.

### 3.5. Verdict

PGlite is the most differentiated long-horizon direction. It is also
the highest-effort. The committed plan stages risk well: v1.2 is a
~3-week experiment, v1.3 is a ~3-month refactor, v1.4 is a
~6-week shim, v1.5 is a ~3-week binding layer. A failure at any stage
can stop without sunk-cost spirals.

---

## 4. Direction 3 — The cross-database "IVM appliance"

### 4.1. The model

The appliance model (documented in
[`plans/ecosystem/PLAN_APPLIANCE.md`](ecosystem/PLAN_APPLIANCE.md))
turns pg_trickle into a stand-alone Postgres-shaped IVM box that
sits next to your *primary* database — which can be MySQL, SQL
Server, MariaDB, Snowflake, BigQuery, or another PostgreSQL — and
keeps maintained results fresh by:

1. Polling the primary's native CDC stream (logical replication,
   binlog, CHANGES TVF, etc.) via a background worker.
2. Writing change events into local
   `pgtrickle_changes.changes_<oid>` buffer tables.
3. Running the unchanged differential refresh pipeline.
4. Optionally writing results back to the primary via FDW.

### 4.2. Why this matters

This is the [Epsio](https://www.epsio.io/) shape. It is also where
[Materialize](https://materialize.com/) and
[RisingWave](https://risingwave.com/) live. The strategic claim: you
get those products' value without leaving the PostgreSQL operational
model. Backups, monitoring, RBAC, replication, HA — all handled by
the surrounding Postgres ecosystem.

For teams whose primary database is MySQL or MSSQL, this is the
*only* way pg_trickle can serve them today. Adding a sidecar Postgres
is much cheaper than adopting a streaming database.

### 4.3. What it would require

- **Source matrix**: postgres_fdw + WAL polling (already done),
  mysql_fdw + binlog polling (new), tds_fdw + MSSQL CDC tables (new),
  snowflake_fdw + STREAMS (new), multicorn-based bigquery_fdw +
  CHANGES TVF (new).
- **Bidirectional FDW writes** — pushing maintained results back
  into the primary as MERGE or UPSERT, idempotently.
- **Operator pushdown** — many DVM operators could push parts of
  their delta SQL down into the FDW, avoiding round-trips.
- **A packaged distribution** — an OCI image and a Helm chart that
  ship as "the pg_trickle appliance".

### 4.4. Risks

- **The appliance becomes its own product** with its own roadmap,
  competing for engineering attention.
- **CDC for non-Postgres sources is hard** — binlog parsers,
  Snowflake quota limits, BigQuery CHANGES TVF cost.
- **Writing back to the primary** raises consistency questions
  pg_trickle has never had to answer (the source-of-truth is now
  external).

### 4.5. Verdict

This is a hedge against the "default IVM in PostgreSQL" direction
not winning fast enough. It expands the addressable market by an
order of magnitude (any database, not just Postgres). It is also the
most plausible commercial lever — a managed appliance is something a
business can sell.

---

## 5. Direction 4 — The real-time application substrate

### 5.1. What already exists

v0.28 shipped transactional outbox/inbox primitives. v0.29 added a
relay CLI. v0.35 introduced reactive subscriptions. The pieces of an
event-driven application platform are present in pg_trickle today.

### 5.2. Where this could go

The directional question is whether pg_trickle should be the
*default* event-bus + read-model layer for PostgreSQL applications,
in the same way that Kafka + Debezium + a streaming engine became the
default in the 2015–2020 era.

**The pitch**:

```
┌──────────────┐  one transaction  ┌─────────────────────────┐
│ Application  │ ────────────────▶ │ Postgres + pg_trickle   │
│ INSERT INTO  │                   │  • outbox event written  │
│ orders ...   │                   │  • read model updated    │
└──────────────┘                   │  • subscriber notified   │
                                   │  • analytics aggregate   │
                                   │     incremented          │
                                   └─────────────────────────┘
                                                ↓
                                   ┌─────────────────────────┐
                                   │ Relay → Kafka / NATS /  │
                                   │ SQS / webhook / SSE     │
                                   └─────────────────────────┘
```

All of that, transactionally consistent, in a single Postgres. No
Kafka cluster, no Connect workers, no Flink job, no separate read
database, no eventual-consistency window.

### 5.3. The features it would need

- **First-class consumer groups** with at-least-once and
  exactly-once-with-keyed-dedup semantics (most exists; needs to be
  the obvious story).
- **Schema evolution for outbox events** — Avro / JSON Schema /
  Protobuf integration. A schema registry equivalent built on
  pg_trickle catalogs.
- **WebSocket / SSE / GraphQL Subscription gateways** that subscribe
  to stream tables and push changes to clients. Either as part of
  the relay, or as separate processes.
- **Saga / state-machine primitives** built on inboxes plus stream
  tables — the "I want a workflow engine in my database" pattern
  that Temporal solves outside the database.
- **CQRS as a first-class API** — the current "use IMMEDIATE mode for
  the read model" pattern is correct but unsung. A
  `pgtrickle.create_read_model()` ergonomic wrapper would make this
  the default.

### 5.4. Risks

- **Scope creep** — every feature here is the surface area of a
  separate product. pg_trickle becoming a "real-time application
  framework" risks losing the "incremental views" focus that makes
  it understandable.
- **Operational expectations** — application substrates need 99.99%
  SLAs. Today pg_trickle is a v0.x extension; it has the safety
  story but not yet the deployment-at-scale evidence.
- **Existing competitors** — Kafka + Debezium has years of
  ecosystem inertia. Outracing them on adoption requires the
  zero-infrastructure pitch to land hard.

### 5.5. Verdict

This is the shortest path from "useful tool" to "category-defining
product". It rests on the v0.28 foundation already shipped. The
risk is scope. The opportunity is large enough to justify the risk
*if* the project keeps the surface area narrow and the abstractions
sharp.

---

## 6. Direction 5 — Incremental AI / RAG infrastructure

### 6.1. The roadmap signal

v0.41 → v0.43 commits to:

- Post-refresh hooks for embedding pipelines.
- Drift-based reindex (HNSW/IVFFlat staleness detection).
- Vector aggregates: `vector_avg`, `vector_sum`, sparse
  (`sparsevec_*`), half-precision (`halfvec_*`).
- Reactive distance subscriptions.
- `embedding_stream_table()` ergonomic API.
- Per-tenant ANN patterns.
- Outbox-emitted embedding events.
- Hybrid-search benchmarks.

### 6.2. The deeper opportunity

Today, keeping a vector index fresh for a RAG system is a custom
pipeline:

1. Detect changed source rows (manually, or with Debezium).
2. Re-compute embeddings (call OpenAI / a local model / a self-hosted
   embedding service).
3. Update pgvector / Qdrant / Pinecone.
4. Maybe rebuild HNSW if drift is too high.

pg_trickle could turn that pipeline into a *single SQL declaration*:

```sql
SELECT pgtrickle.embedding_stream_table(
    name        => 'doc_embeddings',
    source      => 'documents',
    text_column => 'body',
    model       => 'text-embedding-3-small',
    index_kind  => 'hnsw',
    drift_alert => 0.05  -- reindex when recall drops 5%
);
```

Behind the scenes: capture changes, batch them into post-refresh
hooks, call the embedding model, maintain the vector index, monitor
recall via held-out probes, trigger reindex when drift exceeds the
threshold. All incremental, all in Postgres.

### 6.3. Adjacencies

- **pgai** integration — Timescale's
  [pgai](https://github.com/timescale/pgai) handles model invocation
  from inside PostgreSQL. pg_trickle can drive it on every delta.
- **pgvector + pgvectorscale** — vector storage and ANN indexing.
  pg_trickle maintains the embeddings; pgvector serves them.
- **Hybrid search** — combine BM25 (via
  [`pg_search`](https://github.com/paradedb/paradedb)) with vector
  similarity, both kept fresh by pg_trickle.
- **Re-ranking pipelines** — stream tables that maintain
  candidate-set scores using cross-encoder models.
- **Agent memory** — incrementally maintained vector indices as
  long-term memory for LLM agents.

### 6.4. Why this is strategically important

The AI infrastructure stack is being built right now. Whoever owns
"incrementally maintained embeddings inside PostgreSQL" owns a
defensible niche. pg_trickle has the math (DBSP), the engine, and
the integration surface. The window is open but not infinite.

### 6.5. Risks

- **Model API churn** — embedding APIs change shape (OpenAI's
  `text-embedding-3-*` API is the third generation in two years).
  Coupling tightly to any vendor is dangerous.
- **Cost** — embedding APIs charge per token. Misconfigured stream
  tables could re-embed billions of rows. Must default to safe.
- **Recall measurement** — drift-aware reindex requires ground-truth
  evaluation sets the user has to provide.

### 6.6. Verdict

Pursue. The investment is concentrated in the v0.41–v0.43 arc and the
directional payoff is large. Make sure embedding integrations are
*pluggable* (abstraction over the model call) so vendor churn does
not break the engine.

---

## 7. Direction 6 — Distributed and multi-region IVM

### 7.1. Horizontal scale today

pg_trickle already runs on Citus distributed sources (v0.32–v0.34).
The next horizon is more diverse topologies:

- **Neon** (compute/storage separation) — covered in
  [`PLAN_NEON.md`](ecosystem/PLAN_NEON.md). Stateless compute means
  workers must rebuild state from catalog on every wake. Branching
  forks stream tables in a consistent state — interesting use case
  for "preview environments".
- **AlloyDB / Aurora** — managed-PG variants with their own
  storage layers. Plug-in CDC backends would be needed.
- **OrioleDB** — a new MVCC + columnar storage engine. pg_trickle's
  storage tables could live in Oriole for compression + cache
  benefits, while CDC and DVM stay heap-aware.
- **Multi-region active-active** (BDR / pgEdge / Patroni clusters)
  — stream tables that converge across regions, with conflict-free
  delta application.

### 7.2. Multi-database

v0.27 introduced cluster observability. The plausible next step is
**cross-database stream tables**: a stream table in database B that
maintains a query over foreign tables in database A. The relay layer
already provides the wire format. The remaining work is making
foreign source CDC a first-class citizen.

### 7.3. The asymptote: distributed differential dataflow

Real DBSP supports **partitioned dataflow** with shuffle operators.
pg_trickle today executes differential operators on a single
PostgreSQL backend. The big-O / scalability ceiling is whatever a
single backend can do. A distributed-differential variant would
push delta computation across worker nodes (Citus shards, Neon
read replicas, sibling Postgres instances), shuffle on join keys,
and merge results. This is genuinely hard but it is the path to
"web-scale" stream tables.

### 7.4. Risks

- **Operational complexity** scales superlinearly with topology
  count. Each backend type is a CDC backend, a backup story, a
  failure mode.
- **Consistency surface** — distributed IVM intersects with the
  consistency model of the underlying topology. Active-active
  introduces conflict-resolution choices that have no neutral
  defaults.

### 7.5. Verdict

Stay focused on Citus and CNPG/Kubernetes for now. Multi-database is
an obvious next step. Multi-region is a v2.0+ horizon and likely
requires partner engagement (with pgEdge, BDR vendors, etc.) rather
than going alone.

---

## 8. Direction 7 — Self-tuning, cost-aware, autonomous

### 8.1. The trajectory

The roadmap already shows an arc toward autonomy:

- v0.17 — cost-based refresh strategy
- v0.20 — self-monitoring (pg_trickle observes itself)
- v0.22 — SLA-tier auto-assignment
- v0.25 — predictive cost model
- v0.27 — schedule recommendations
- v0.31 — smarter scheduling

The next step is **closing the loop**: not just *recommending*
schedules and refresh modes, but *applying* them automatically when
the cost-model and self-monitoring data agree.

### 8.2. What full autonomy would look like

```
┌──────────────────────────────────────────────────────────┐
│                  Autonomous Mode                         │
│                                                          │
│  Self-monitoring observes: refresh latency, change-rate, │
│  CPU usage, memory pressure, downstream read SLA.        │
│                                                          │
│  Cost model predicts: full vs differential vs immediate  │
│  cost for each ST. Cron-like schedule vs event-driven.   │
│                                                          │
│  Decision engine: applies refresh-mode and schedule      │
│  changes automatically. Logs every decision for          │
│  auditability. Reverts on regression.                    │
└──────────────────────────────────────────────────────────┘
```

Operators set high-level goals ("stream table X must be ≤ 5s stale,
under 1 vCPU of refresh budget"); the engine figures out everything
else.

### 8.3. Risks

- **Surprise** — autonomy that changes behaviour without consent
  violates the "no surprises" principle (v0.10 work). Must be
  opt-in, must log every decision, must be reversible.
- **Trust** — auto-mode regressions create blast radii larger than
  the original problem. The shadow-canary infrastructure (v0.21)
  is the mitigation but it needs to be the default for tuning
  changes.

### 8.4. Verdict

This is mostly the natural extension of work already in the roadmap.
The novelty is making it *the default* rather than an option. That
is a v1.x or v2.x discussion, not pre-v1.0.

---

## 9. Direction 8 — Open the engine

### 9.1. The shape

`pg_trickle_core` is being extracted in v1.3 as a side-effect of the
PGlite work. The crate is a pure-Rust DBSP-style IVM kernel with no
PostgreSQL dependency, sitting behind a `DatabaseBackend` trait.

The directional question is: should that crate become a **public,
multi-host substrate** that other databases adopt?

### 9.2. Plausible host backends beyond PostgreSQL/PGlite

- **DuckDB** — embeds easily, has a well-defined extension API,
  serves the analytical-Postgres-alternative niche. An IVM layer
  for DuckDB would be genuinely novel — DuckDB's materialized views
  are full-recompute today.
- **OrioleDB** — same parser as Postgres, different storage. A
  trivial backend.
- **SQLite** — the smallest substrate. IVM in SQLite would be a
  visible, evangelism-friendly proof point.
- **CockroachDB / YugabyteDB** — Postgres-wire-compatible but not
  Postgres-extension-compatible. The core could plug in as a
  separate process.
- **MotherDuck / Tinybird / etc.** — managed-DuckDB platforms that
  could integrate pg_trickle_core natively.

### 9.3. The license question

pg_trickle is Apache 2.0. The core extraction inherits that.
Open-engine-multi-host implies someone else benefits from the work.
That is fine for an open-source project. It becomes a tension if
there is also a commercial appliance or managed cloud (Direction 4
or Direction 10).

The standard pattern is *Apache for the core, BSL or commercial for
the appliance*. That should be a deliberate decision before any
multi-host adoption is courted.

### 9.4. Verdict

Optional, not strategic. Pursue *if* a partner approaches with a
concrete adoption plan. Do not invest engineering ahead of demand.

---

## 10. Direction 9 — Standardisation

### 10.1. The asks

- An **IVM extension to SQL** (an `INCREMENTAL` keyword on
  `MATERIALIZED VIEW`, or a `CREATE STREAM TABLE` clause) is a
  decadelong conversation. pg_trickle has the lived experience to
  contribute meaningful proposals.
- A **delta-feed standard** for downstream consumers — today
  Debezium-format JSON is the de facto winner. pg_trickle's
  publication output already speaks logical replication; it could
  also speak Iceberg's V3 row-level deletes, Delta Lake's CDF, and
  Apache Paimon's CDC log to bridge into the lakehouse world.
- **Differential dataflow** as a topic of academic and industrial
  research. pg_trickle's TPC-H 22/22 is a meaningful benchmark
  result; presenting it at VLDB / SIGMOD would attract collaborators.

### 10.2. Verdict

Low cost, high reputational return. Worth doing in parallel with
v1.0 stabilisation. Pick one venue (PGCon, PGConf.dev, VLDB
Demonstrations) and submit a paper.

---

## 11. Direction 10 — The surrounding economy

### 11.1. The non-engineering directions

A successful open-source database extension has more than code:

- **Managed cloud** — the appliance from Direction 3, run as a
  service. The Materialize / RisingWave business model.
- **Support contracts** — paid 24/7 incident response. Crunchy Data,
  EDB, Percona model.
- **Certifications** — "pg_trickle Certified" engineers, similar
  to Snowflake's Snowpro program.
- **Training** — first-party workshops, university partnerships,
  free online courses.
- **A foundation** — at the right scale, donating governance to a
  neutral foundation (CNCF, ASF, Linux Foundation) accelerates
  enterprise adoption.
- **Books, courses, conferences** — the long-tail evangelism that
  turns a tool into a category.

### 11.2. Verdict

Out of scope for the engineering roadmap, but the engineering
roadmap should *enable* these (signed artifacts, SBOMs, an LTS
branch policy, a documented support contract surface) rather than
foreclose them.

---

## 12. Direction 11 — Lakehouse and open-table-format bridge

### 12.1. Why this matters

PostgreSQL owns operational truth for many teams, while the modern
analytics estate increasingly lives in object-storage-backed table
formats: Apache Iceberg, Delta Lake, Apache Hudi, and Apache Paimon.
The common path from Postgres to those systems is still a CDC pipeline:
Debezium → Kafka → sink connector → object storage → compaction job →
warehouse metadata. That path works, but it is a lot of machinery for
"make these derived facts available in the lakehouse."

pg_trickle already computes **net deltas** for derived tables. If those
deltas could be written directly as open-table-format changes, pg_trickle
would become a Postgres-native bridge between operational data and
lakehouse tables.

### 12.2. The product shape

```
┌──────────────────────────┐
│ PostgreSQL + pg_trickle  │
│ source tables            │
│ stream tables            │
│ delta frontier           │
└─────────────┬────────────┘
              │ append/delete/equality-delete files
              ▼
┌──────────────────────────┐
│ Iceberg / Delta / Hudi   │
│ object storage table     │
│ catalog metadata         │
└─────────────┬────────────┘
              │
              ▼
┌──────────────────────────┐
│ Trino / Spark / DuckDB   │
│ Snowflake external table │
│ Databricks / Athena      │
└──────────────────────────┘
```

A future API could look like:

```sql
SELECT pgtrickle.create_lakehouse_sink(
    stream_table => 'gold_customer_ltv',
    format       => 'iceberg',
    catalog      => 'glue',
    location     => 's3://analytics/gold/customer_ltv',
    mode         => 'delta'
);
```

The stream table remains the source of truth inside PostgreSQL; the
lakehouse sink is a maintained projection into open storage.

### 12.3. Technical building blocks

- **Delta-to-file writer.** Convert refresh deltas into Parquet row
  groups plus table-format metadata commits. This likely belongs in a
  relay-side component rather than inside the PostgreSQL backend.
- **Exactly-once frontier commits.** Tie pg_trickle's refresh frontier
  to the open-table commit ID. If the process crashes after writing data
  files but before publishing metadata, replay must either reuse or clean
  up orphan files.
- **Equality-delete support.** Iceberg and Delta have different delete
  semantics. Stream-table deltas naturally produce inserts and deletes;
  mapping those cleanly is central.
- **Compaction policy.** Small deltas generate small files. A
  pg_trickle-aware compactor can merge files when frontiers advance far
  enough to make compaction safe.
- **Schema evolution.** `ALTER STREAM TABLE` must map to Iceberg / Delta
  schema evolution without losing historical compatibility.

### 12.4. Why pg_trickle is well-positioned

The key advantage is that pg_trickle emits **derived deltas**, not just
source-table CDC. Debezium can tell a lakehouse that one order row
changed; pg_trickle can tell it that `customer_ltv`, `regional_revenue`,
and `open_support_risk` changed by exactly these rows. That difference
collapses an entire transformation layer.

### 12.5. Risks

- **Object-store semantics are awkward.** S3 and GCS do not behave like a
  database. Commit protocols, retries, and cleanup need careful design.
- **This competes with mature pipelines.** Airbyte, Debezium, Fivetran,
  Spark, and Flink already occupy the lakehouse-ingest story.
- **Format churn.** Iceberg, Delta, Hudi, and Paimon evolve quickly.
  Supporting all of them first-class could become a project by itself.

### 12.6. Verdict

Worth treating as a relay-side research track after v1.0. The first
useful deliverable would be narrow: Iceberg only, append/equality-delete
only, S3/MinIO only, one metadata catalog, with crash-recovery proof.
Do not put object-store clients inside the extension backend.

---

## 13. Direction 12 — Live data quality, governance, and compliance

### 13.1. The opportunity

Most data-quality systems are batch systems. They run dbt tests,
Great Expectations suites, reconciliation jobs, or warehouse queries
after the fact. They answer "did yesterday's data look wrong?" pg_trickle
could answer a different question: **is the system violating an invariant
right now?**

Stream tables are a natural representation for live controls:

- unmatched payments
- negative inventory
- orphaned foreign-key-like relationships across services
- missing audit events
- suspicious privilege grants
- rows that violate a business rule but are legal at the schema layer
- stale SLA windows

The output table is not a report. It is the current set of violations.
If it is empty, the invariant holds.

### 13.2. Compliance patterns

**SOX-style reconciliation.** Maintain a stream table of ledger entries
whose debits and credits do not balance, grouped by accounting period,
subsidiary, and currency. The moment the stream table is non-empty, the
control fails.

**PCI / PII controls.** Maintain live views of tables and columns that
contain sensitive fields, joined to access logs and role grants. Alert
when a role with broad access appears outside approved groups.

**GDPR / data retention.** Maintain stream tables of rows past retention
deadline or user-erasure requests not yet fulfilled. This converts a
legal obligation into a continuously maintained operational queue.

**Healthcare audit trails.** Maintain live "access without encounter"
or "chart viewed without assignment" stream tables. pg_trickle does not
need to know healthcare law; it just maintains the control query.

### 13.3. Product shape

A future `pgtrickle_controls` package could ship:

- a `create_control()` wrapper around stream tables
- severity, owner, escalation, and evidence metadata
- `pgt_control_status` views
- OpenTelemetry / Prometheus metrics for control failures
- automatic outbox events when violations appear or disappear
- signed audit snapshots for point-in-time evidence

Example:

```sql
SELECT pgtrickle.create_control(
    name     => 'unbalanced_journal_entries',
    severity => 'critical',
    owner    => 'finance-platform',
    query    => $$
      SELECT journal_id, SUM(debit) AS debits, SUM(credit) AS credits
      FROM accounting_entries
      GROUP BY journal_id
      HAVING SUM(debit) <> SUM(credit)
    $$,
    schedule => '1s'
);
```

### 13.4. Why this is distinct from observability

Observability tells operators what the system is doing. Controls tell
the organization whether the system is allowed to be doing it. The data
model is similar, but the audience and failure semantics are different.
A control failure may require evidence retention, sign-off, and audit
history. That plays directly into pg_trickle's existing refresh history,
snapshot, and outbox surfaces.

### 13.5. Risks

- **False confidence.** A stream table only proves the SQL query it
  encodes. Bad controls create dangerous assurance. Documentation must be
  blunt about this.
- **Legal sensitivity.** Avoid marketing claims like "HIPAA compliant" or
  "SOX compliant" unless backed by actual certification.
- **Audit immutability.** PostgreSQL tables are mutable; signed snapshots
  and append-only evidence logs would be needed for serious compliance use.

### 13.6. Verdict

High-value, low-core-change direction. Most of the work is packaging,
templates, docs, dashboards, and evidence workflows. This could become a
separate ecosystem project after v1.0 without disturbing the core engine.

---

## 14. Direction 13 — Vertical solution kits

### 14.1. Why kits matter

The core abstraction — "incrementally maintained SQL" — is powerful but
abstract. Many users adopt infrastructure only when they see their own
problem named. Vertical kits translate pg_trickle from a database feature
into a ready-to-run solution.

The project already has blog posts covering fraud, CQRS, event sourcing,
funnels, medallion architecture, time-series downsampling, vector search,
and more. A natural next step is turning those into **installable pattern
packs**.

### 14.2. Candidate kits

| Kit | Stream tables | Add-ons |
|---|---|---|
| Fraud / risk | velocity rules, suspicious accounts, chargeback exposure | Grafana dashboard, alert outbox |
| SaaS metrics | MRR, churn, cohort retention, seat expansion | dbt macros, Metabase dashboards |
| Ecommerce | inventory risk, cart funnel, customer lifetime value | search-index outbox, revenue alerts |
| Fintech ledger | unbalanced journals, exposure, settlement aging | signed snapshots, controls pack |
| IoT / telemetry | rolling aggregates, device health, anomaly queues | retention policies, downsampling |
| Gaming | leaderboards, matchmaking segments, economy balances | TopK patterns, reactive subscriptions |
| Support / CRM | account risk, SLA breaches, escalation queues | reverse ETL to Salesforce/HubSpot |
| Marketplace | seller quality, fraud risk, settlement summaries | tenant-scoped dashboards |

### 14.3. What a kit contains

- SQL migrations to create canonical stream tables.
- Seed data and load generators for demos.
- Grafana / Metabase / Superset dashboards.
- Alert rules and outbox event schemas.
- dbt macros or model templates.
- Performance expectations and scaling notes.
- A "how to adapt this to your schema" guide.

### 14.4. Why this helps adoption

Infrastructure projects often fail because the first successful use case
requires too much imagination. Kits reduce that distance. They also create
benchmark fixtures, docs, and demos that exercise real combinations of
operators more effectively than synthetic tests.

### 14.5. Risks

- **Maintenance sprawl.** Each kit becomes a mini-product.
- **Domain claims.** Industry-specific kits can imply domain expertise the
  project may not have.
- **Schema mismatch.** Real user schemas vary wildly; kits must be
  examples, not rigid products.

### 14.6. Verdict

Pursue as separate repositories with loose coupling to the core. The best
first kit is probably **SaaS metrics** or **fraud/risk**: both are easy to
demo, operator-rich, and visibly improved by low-latency refresh.

---

## 15. Direction 14 — Geospatial, graph, and scientific IVM

### 15.1. Geospatial

PostGIS users often maintain derived spatial tables:

- assets inside geofences
- parcels intersecting planning zones
- drive-time catchments
- nearest facilities
- fleet positions by region
- heatmap tiles

These are expensive to recompute and often change incrementally. pg_trickle
could maintain geospatial stream tables if the DVM engine treats PostGIS
functions carefully: deterministic functions are safe, volatile functions
are not, and expensive spatial predicates need index-aware delta plans.

The interesting path is not "reimplement PostGIS". It is:

1. Track which spatial columns changed.
2. Generate delta SQL that preserves `&&`, `ST_Intersects`, `ST_DWithin`,
   and GiST/SP-GiST index usage.
3. Maintain small derived tables or tiles used by maps.

### 15.2. Graph analytics

Recursive CTE support already gives pg_trickle a foothold in graph-shaped
queries. Future graph directions include:

- incremental transitive closure for small/medium graphs
- dependency-impact analysis
- authorization reachability
- incremental PageRank-style scores
- community detection approximations
- fraud rings / connected-component detection

The hard line: exact incremental graph algorithms can explode in state.
The project should be honest about where stream-table SQL is suitable and
where a specialized graph engine remains better.

### 15.3. Scientific and simulation workloads

Some scientific workloads are naturally incremental:

- streaming sensor calibration
- rolling aggregates over lab instruments
- online experiment metrics
- approximate statistics over changing cohorts
- incremental feature engineering for ML

These are less likely to be the first commercial wedge, but they are
excellent for research collaboration. They also exercise aggregates,
windows, and temporal semantics in ways business dashboards do not.

### 15.4. Risks

- **Function volatility.** PostGIS and scientific extensions expose many
  functions; only deterministic subsets are safe for IVM.
- **Index plan sensitivity.** A delta query that misses a GiST index can be
  slower than full refresh.
- **State growth.** Graph algorithms may require auxiliary state that does
  not fit the current stream-table storage model.

### 15.5. Verdict

Worth a research branch and a few high-quality examples, especially
PostGIS geofencing and tile maintenance. Do not promise arbitrary graph
analytics. Promise specific, measured patterns.

---

## 16. Direction 15 — Operational time-series and observability rollups

### 16.1. The use case

Observability systems continuously compute rollups:

- requests per service per minute
- p95 latency by endpoint
- error-budget burn rate
- active incidents by team
- log-derived counters
- high-cardinality label summaries

Many teams already store operational events in PostgreSQL because it is
near the application, transactional, and easy to query. pg_trickle can turn
those event tables into live rollups without adding Prometheus recording
rules, Kafka Streams, or a warehouse job.

### 16.2. Relation to TimescaleDB

TimescaleDB owns a strong time-series niche: hypertables, compression,
retention, continuous aggregates. pg_trickle should not try to become
TimescaleDB. The more interesting story is **composition**:

- Timescale hypertables as source tables.
- pg_trickle stream tables as operational projections across time-series
  and relational dimensions.
- Continuous aggregates for pure time buckets; pg_trickle for richer joins,
  DAGs, controls, and downstream eventing.

### 16.3. Product features

- `create_rollup_stream_table()` wrappers for common bucketed metrics.
- Native patterns for late-arriving events and watermark hold-back.
- Retention-aware stream tables that know when source partitions drop.
- Error-budget templates (SLO burn rate, alert fatigue tracking).
- Incident timelines built from stream-table snapshots.

### 16.4. Hard problems

- **Late data.** Time-series systems often receive out-of-order events.
  pg_trickle already has watermarks, but SLO/observability users need
  clear late-data semantics.
- **High-cardinality labels.** Group-by explosion is a real cost hazard.
  The cost model should warn when label sets will produce millions of
  groups.
- **Retention.** Dropping old partitions must create correct negative
  deltas or be treated as a controlled truncation event.

### 16.5. Verdict

Strong direction, especially as a companion to Timescale rather than a
competitor. The first milestone should be docs and examples, then a small
rollup helper API if patterns repeat.

---

## 17. Direction 16 — Reverse ETL and data-product delivery

### 17.1. The idea

Reverse ETL takes computed facts from a warehouse and pushes them into
operational systems: Salesforce, HubSpot, Zendesk, Stripe, Elasticsearch,
Redis, customer-facing APIs. pg_trickle can make those facts fresh inside
PostgreSQL; the relay/outbox surface can deliver them outward.

This direction treats stream tables as **data products**:

- `customer_health_score`
- `account_expansion_candidate`
- `support_escalation_queue`
- `search_document_projection`
- `fraud_review_case`
- `eligible_coupon_offer`

Each table is maintained incrementally, versioned, documented, and emitted
to consumers.

### 17.2. Product shape

The natural extension is a sink framework:

```sql
SELECT pgtrickle.create_sink(
    stream_table => 'customer_health_score',
    sink_type    => 'salesforce',
    key_column   => 'account_id',
    mode         => 'upsert',
    schedule     => '10s'
);
```

Under the hood this should probably live in the relay process rather than
the backend. PostgreSQL emits durable outbox events; connectors deliver
them to SaaS APIs with retries, rate-limit handling, and dead-letter queues.

### 17.3. Why pg_trickle is different from warehouse reverse ETL

Warehouse reverse ETL is usually stale by design: data lands in the
warehouse, transformations run, sync jobs push results out. pg_trickle
can collapse the latency from hours/minutes to seconds, and for IMMEDIATE
mode read models to the same transaction. That matters for fraud, support,
authorization, and pricing.

### 17.4. Risks

- **Connector sprawl.** SaaS APIs are endless. The project should avoid
  owning dozens of brittle connectors inside the main repo.
- **Rate limits and idempotency.** Operational sinks are messy. Every sink
  needs replay-safe semantics.
- **Data contracts.** Consumers need stable schemas. This reinforces the
  need for stream-table schema versioning.

### 17.5. Verdict

Build the generic sink contract and maybe one reference connector
(Elasticsearch/OpenSearch is a good first candidate because search-index
freshness is a common pain). Let the community build SaaS connectors.

---

## 18. Direction 17 — First-class developer experience and visual tooling

### 18.1. The need

The engine is sophisticated. Users should not need to understand every
operator rule to use it confidently. A high-quality developer experience
could make pg_trickle feel like a natural PostgreSQL capability rather than
an advanced extension.

### 18.2. Tooling ideas

- **Visual DAG workbench.** Render stream-table dependencies, schedules,
  frontiers, refresh latency, and error states. Drill into each edge to see
  source tables and change rates.
- **Query support advisor.** Paste a SQL query and get: supported/not
  supported, expected refresh mode, rewritten SQL, operator tree, index
  recommendations, and likely hot spots.
- **Migration advisor.** Scan existing materialized views and recommend
  `create_stream_table()` calls, primary keys, schedules, and refresh modes.
- **Cost simulator.** Feed sample change rates and table sizes; estimate
  differential vs full refresh cost.
- **IDE extension.** VS Code / JetBrains integration for stream-table SQL:
  syntax snippets, diagnostics, explain links, and quick-fix suggestions.
- **Playground generator.** Create a docker-compose demo from a user's
  schema, with load generator and dashboards.
- **Failure explainer.** Convert internal error states into "what happened,
  what data is stale, what command repairs it" guidance.

### 18.3. The CLI

A `pgtrickle` CLI could wrap the SQL API without becoming a second API:

```bash
pgtrickle explain --query query.sql
pgtrickle migrate-materialized-views --schema public --dry-run
pgtrickle doctor --database postgres://...
pgtrickle dag --format svg
pgtrickle bench --stream-table revenue_by_region
```

The CLI's job is discovery, diagnostics, and automation. The source of
truth remains SQL.

### 18.4. Why this matters strategically

Developer tools are adoption multipliers. If the first experience is
"paste query, see green support matrix, click create," the project feels
safe. If the first experience is "read DVM operator docs for an hour," it
remains niche.

### 18.5. Risks

- **Tooling can outrun engine truth.** The advisor must use the same parser
  and validation paths as the extension, or it will lie.
- **Maintenance.** IDE extensions and GUIs require a different maintenance
  cadence than core Rust.

### 18.6. Verdict

High leverage. The best sequence is CLI first, then web workbench, then IDE
integrations. Keep all tooling thin over the SQL API and catalog views.

---

## 19. Direction 18 — Benchmark and research leadership

### 19.1. Why this matters

IVM projects are hard to compare. Benchmarks often measure different things:
source CDC overhead, end-to-end freshness, query latency, memory, write
amplification, recovery time, operator coverage, correctness under churn. A
project that publishes serious, reproducible benchmarks becomes the reference
point for the category.

pg_trickle already has meaningful assets:

- TPC-H 22/22 in DIFFERENTIAL, IMMEDIATE, and FULL modes.
- Nexmark work.
- SQLancer fuzzing.
- Criterion regression gates.
- Light and full E2E tiers.
- Citus chaos testing planned.

### 19.2. The benchmark suite to build

**IVMBench.** A public benchmark suite specifically for incremental view
maintenance, with:

- TPC-H at multiple scale factors and change rates.
- Nexmark streaming scenarios.
- Synthetic DAG shapes (chain, diamond, fan-out, cycle where allowed).
- Operator-specific cases (outer joins, subqueries, windows, TopK,
  recursive CTEs).
- Write-side overhead tests for trigger and WAL CDC.
- Recovery tests (crash mid-refresh, restart, replay).
- Distributed variants for Citus.
- Correctness oracle: DIFF vs FULL equivalence.

### 19.3. Public leaderboard

The suite could compare:

- pg_trickle
- pg_ivm
- Materialize
- RisingWave
- Flink SQL
- ksqlDB
- Snowflake Dynamic Tables
- DuckDB full refresh
- vanilla PostgreSQL materialized views

This must be done carefully and fairly. The point is not marketing
cherry-picks; it is making the category measurable.

### 19.4. Research agenda

- Mechanized proofs of selected DVM rewrite rules.
- Formal model of frontier advancement and crash recovery.
- Benchmarks of MERGE as bottleneck and proposed PostgreSQL executor
  improvements.
- Distributed differential refresh over sharded PostgreSQL.
- Cost-model learning from production telemetry.

### 19.5. Verdict

Very high reputational value. This is also a way to attract academic
collaborators and contributors who care about correctness. It should be
started before v2.0, even if the first version is small.

---

## 20. Direction 19 — Security and privacy-preserving IVM

### 20.1. The premise

Stream tables can accidentally amplify sensitive data. A base table with
RLS may feed a derived aggregate; a tenant-scoped view may be joined to a
global dimension; an embedding table may leak semantic information even
when raw text is hidden. As pg_trickle moves into production, security is
not a side concern. It becomes part of the core value proposition.

### 20.2. Future capabilities

- **RLS-preserving stream tables.** Stronger guarantees that policies on
  sources propagate to derived tables, or explicit warnings when they do
  not.
- **Tenant-isolation analyzer.** A linter that proves every stream table
  in a multi-tenant deployment carries `tenant_id` through joins and
  aggregates correctly.
- **Masked / redacted aggregates.** Helpers for maintaining aggregated
  facts while redacting small groups or sensitive dimensions.
- **Differential privacy wrappers.** Optional noise injection for certain
  aggregate stream tables, with privacy-budget tracking.
- **Lineage-aware access review.** Catalog views that show which stream
  tables derive from sensitive columns.
- **Secret hygiene for connectors.** If sinks and sources proliferate,
  credentials need rotation, KMS integration, and least-privilege docs.

### 20.3. Why this is more than compliance

Security analysis is also a selling point. Many teams avoid materialized
views because they do not know whether derived tables preserve the same
access boundaries as the source. If pg_trickle can make those boundaries
visible and enforceable, it reduces a real adoption blocker.

### 20.4. Risks

- **False proofs.** A tenant-isolation analyzer that misses an edge case is
  worse than no analyzer. It must be conservative.
- **Differential privacy complexity.** DP is easy to market badly and hard
  to implement rigorously. Treat it as research until proven.
- **RLS semantics.** PostgreSQL RLS is evaluated at query time; stream
  tables materialize data. Preserving semantics may require per-tenant
  storage or security-barrier views.

### 20.5. Verdict

Pursue the conservative pieces first: lineage, analyzers, warnings,
tenant-id propagation tests, and documentation. Treat DP and advanced
privacy as research.

---

## 21. Direction 20 — PostgreSQL planner and storage co-design

### 21.1. The thesis

pg_trickle's bottlenecks are increasingly PostgreSQL bottlenecks:
`MERGE`, executor overhead, index maintenance, heap layout, planner choices,
parallel query, memory contexts. The long-term performance path is not to
become a separate database. It is to co-design with PostgreSQL's planner and
storage evolution.

### 21.2. Performance frontiers

- **Parallel delta execution.** Push more delta SQL through PostgreSQL's
  parallel query machinery, and make refresh workers coordinate without
  fighting the global worker pool.
- **Planner hooks for delta queries.** Teach PostgreSQL or a pg_trickle
  hook that change-buffer CTEs are small and should drive join order.
- **Adaptive indexes.** Recommend and optionally create indexes on source,
  storage, and change-buffer tables based on observed refresh plans.
- **MERGE optimization.** Work with PostgreSQL features like `MERGE ...
  RETURNING OLD.*, NEW.*` to reduce round-trips and displaced-row scans.
- **Columnar paths.** Use columnar storage extensions or future core
  features for wide analytical stream tables without building a custom
  storage engine.
- **JIT and vectorized execution.** Identify when delta queries benefit
  from JIT and when compilation overhead dominates.
- **L0 cache and spill-to-disk.** Continue the v0.36 work on hot-path
  caching and bounded memory under bursty change rates.

### 21.3. A realistic collaboration path

The project can produce small, upstreamable PostgreSQL improvements:

- better MERGE instrumentation
- planner estimates for transition/change tables
- skip-scan-friendly index recommendations
- replication-slot health visibility
- extension-friendly background worker APIs
- `PG_MODULE_MAGIC_EXT` adoption and extension introspection

These are not glamorous, but they compound.

### 21.4. What not to do

Do not build a private storage engine inside pg_trickle. Do not fork the
PostgreSQL executor. Do not own a custom SQL dialect. The winning move is
to stay close enough to PostgreSQL that the whole ecosystem improves.

### 21.5. Verdict

This is the quiet, high-leverage path. It will not produce flashy product
announcements, but it protects pg_trickle's core advantage: native
PostgreSQL integration with world-class incremental performance.

---

## 22. Cross-cutting concerns

Several concerns cut across all directions and deserve their own
treatment.

### 22.1. Correctness as a moat

The v0.38 EC-01 sprint set a precedent: **correctness is a release
gate**, not an aspirational property. As pg_trickle's deployment
surface expands, the cost of every silent-correctness bug expands
with it. The directional implication: invest in correctness
infrastructure ahead of every other expansion.

Tools to consider:
- **Property-based testing** at SQL level (already done for joins;
  expand to all operators).
- **Random query generation + DIFF-vs-FULL equivalence** as a
  permanent CI workload.
- **Formal verification** of operator delta SQL using TLA+, Coq,
  or Lean. The DBSP paper is amenable to mechanization.
- **Fuzzing** at the parser, planner, and merge layers. SQLancer
  is a start; differential fuzzing against pg_ivm and Materialize
  would add cross-engine confidence.

### 22.2. The v1.0 API freeze is an irrevocable commitment

Every direction in this report depends on v1.0 being shippable and
its API being credible for years. The freeze should include:

- Catalog schema (with a migration story for any future change).
- SQL function signatures.
- GUC names and meanings.
- Refresh-mode semantics.
- Failure-mode messaging (SQLSTATE codes are part of the API).

The rate of change between v0.x releases has been enormous; v1.x
will be slower, and that is a feature.

### 22.3. Documentation as code

The current roadmap has an unusually disciplined documentation
practice: every release ships full plans, blog posts, and
plain-language companions. Maintaining that discipline as the
project grows is more valuable than most features. *Generated
documentation* (from catalog introspection) should become the
default, not the exception, by v1.0.

### 22.4. Community governance

Today the project has a small, focused maintainer set. As adoption
grows, community contributors and a documented governance model
become necessary. The PostgreSQL community's "core team" model is
a reasonable template.

### 22.5. Funding

Every direction in this report has a realistic engineering cost in
person-years. A volunteer-only project can do many things; a
funded project can do more, faster. The funding directions are
roughly:

- **Sponsorship** (GitHub Sponsors, OpenCollective, NLnet grants,
  Sovereign Tech Fund) — neutral, slow, low-strings.
- **Commercial product** (managed cloud, support, appliance) —
  sustainable but introduces the open-core tension.
- **Acquisition** by an incumbent (Crunchy, EDB, Supabase, Neon,
  Timescale, Microsoft) — accelerates adoption, costs
  independence.
- **Foundation grant** (PostgreSQL Foundation, CNCF, OSS funds) —
  reputational boost, modest dollars.

Each option has strong implications for which directions in this
report become viable.

---

## 23. A speculative ten-year picture

Combining the strongest threads:

- **By 2027** (v1.0 era): pg_trickle is the obvious answer for any
  team using `REFRESH MATERIALIZED VIEW`. It ships in PGDG, Docker
  Hub, and the major managed-PG vendor allow-lists. PGlite proof
  of concept proves browser viability. The embedding-pipeline arc
  is shipped; pg_trickle is the default RAG-freshness tool for
  Postgres-based stacks.
- **By 2028** (early v1.x): first vertical kits exist for SaaS metrics,
  fraud/risk, and time-series rollups. The CLI and visual DAG workbench
  make stream-table diagnosis approachable. The first lakehouse sink
  research prototype writes Iceberg-compatible deltas from a stream table.
- **By 2029** (v1.5 era): Reactive UI bindings (React, Vue,
  potentially SolidJS and Svelte) are mature. Local-first apps use
  the same query definition on server and client. The cross-database
  appliance is a separately-marketed product (commercial or
  community). pg_trickle has presented at VLDB and published the first
  IVMBench leaderboard.
- **By 2030** (v2.0 planning): privacy and governance tooling is strong
  enough that regulated teams use stream tables for live controls, not
  only analytics. Reverse-ETL sinks push maintained facts into search,
  support, and CRM systems. Planner/storage co-design work has produced
  at least one upstream PostgreSQL improvement motivated by pg_trickle.
- **By 2031** (v2.x era): A managed-cloud offering exists.
  Distributed differential dataflow across Citus shards is shipping.
  IVM appears in PostgreSQL core as a consequence (or in spite)
  of pg_trickle's existence. Standardisation work begins in the
  SQL committee. pg_trickle has 100k+ production deployments.
- **By 2034** (v3.x era): pg_trickle (or its successor) is so
  embedded in the PostgreSQL operational expectation that "manual
  refresh of a materialized view" is treated the way "manual
  vacuum" is treated today — something you can do, but never
  should.

This is one trajectory. There are others. The point is that the
*technical foundation already exists* for every step. The remaining
work is execution, distribution, trust, and time.

---

## 24. Anti-directions (things to *not* do)

A list of plausible directions that, on examination, look like
strategic mistakes:

- **A separate streaming engine** (Materialize / Flink shape).
  pg_trickle's defining advantage is "lives inside Postgres". A
  standalone engine throws away that advantage and competes with
  better-funded incumbents.
- **A commercial fork** with closed features. Every comparable
  PostgreSQL-extension project that has tried this has fragmented
  its community. Apache 2.0 + a separate commercial appliance is
  cleaner.
- **GPU acceleration** of the DVM operators. Interesting CS, but
  the bottleneck is `MERGE` and PostgreSQL's executor, not the
  DVM. Wrong layer.
- **Custom storage engine.** Tempting (the storage tables could be
  columnar, log-structured, or compressed). But it forks pg_trickle
  away from being a Postgres extension and into being a database.
  OrioleDB-as-host is the better answer.
- **A non-SQL surface** (a Python or DataFrame API for stream
  tables). pg_trickle's leverage is that SQL is already the API.
  Polars / pandas / DuckDB integration *consuming* stream tables
  is great. Hiding SQL behind a DataFrame layer dilutes the
  identity.
- **Locking into a specific cloud.** Every cloud-specific feature
  (S3 storage tables, AWS-only CDC, Azure-only KMS) reduces the
  surface where pg_trickle can run. Stay portable.
- **Owning every connector.** Reverse ETL, lakehouse sinks, SaaS APIs,
  and CDC sources are infinite surfaces. pg_trickle should own contracts,
  reference implementations, and correctness semantics — not dozens of
  vendor-specific connectors in the core repo.
- **Overpromising compliance.** Live controls and audit trails are
  valuable, but compliance claims require legal, procedural, and
  organizational evidence. The project can provide primitives and
  evidence logs; it should not claim regulatory certification casually.
- **Replacing proven adjacent tools for their own sake.** TimescaleDB,
  PostGIS, pgvector, Debezium, dbt, and lakehouse engines are not enemies.
  The strongest pg_trickle direction is often composition, not replacement.

---

## 25. Open questions

This report does not — and cannot — answer the following. They are
left for the maintainer team and the community to resolve over the
v1.0 → v2.0 horizon.

1. Is the long-term funding model sponsorship, commercial,
   foundation, or a hybrid?
2. Is the ten-year ambition to be a *PostgreSQL extension forever*,
   or to grow into a *substrate that hosts other databases*?
3. How aggressive should the push for SQL-standard IVM be?
4. What is the policy on accepting code from large vendors who
   want to upstream their own backends (Snowflake, Aurora,
   Databricks)?
5. Is there an LTS release model, and if so, what is the support
   window?
6. What is the criterion for promoting an experimental backend
   (e.g., `mysql_fdw` source) from research to first-class?
7. At what point — if ever — does pg_trickle stop being one
   maintainer's project and become a community-governed one?
8. Is the PGlite story a strategic priority or an opportunistic
   bet that should be deprioritised if the core direction needs
   the engineering bandwidth?
9. What is the relationship to pg_ivm long-term? Friendly
   coexistence, eventual merger, friendly competition?
10. How does pg_trickle want to be remembered if it is *successful*
    — as a product, as a primitive, as an idea?
11. Which integrations belong inside the extension, which belong in the
    relay, and which belong in separate ecosystem repositories?
12. Should lakehouse/open-table-format sinks be first-party, partner-led,
    or intentionally left to external connectors?
13. What is the minimum evidence required before marketing a vertical kit
    as production-ready rather than demonstrational?
14. Should privacy-preserving aggregates become a first-party feature, or
    remain a research topic until formal guarantees exist?
15. What telemetry can be collected from real deployments without
    violating user privacy, and can that telemetry improve the cost model?
16. Should `pg_trickle_core` promise a stable Rust API after extraction,
    or remain an internal crate until the PGlite work matures?
17. How much UI surface should the project own directly — CLI only,
    web workbench, IDE extension, hosted control plane?
18. What is the first external benchmark where pg_trickle should be
    compared publicly against Materialize, RisingWave, pg_ivm, and Flink?
19. Is reverse ETL strategically central, or just a useful relay-side
    example of downstream stream-table consumption?
20. Which PostgreSQL upstream changes would most improve pg_trickle, and
    who should champion those patches?

---

## 26. Closing

pg_trickle has built, in 38 minor releases, the technical
foundation of an idea that PostgreSQL has been missing since 2013:
**materialized views that maintain themselves correctly,
incrementally, and without external infrastructure**.

The engine works. The math is sound. The test surface is broad.
TPC-H 22/22 is green in DIFFERENTIAL mode. Operationally the
extension behaves like a well-mannered PostgreSQL citizen.

The directional space is now wide open. The decisions of the next
two years — about API stability, about which adjacent ecosystems
to court, about whether to optimise for adoption or
differentiation, about funding, about governance — will determine
which of the futures sketched in this document actually arrives.

The single most important constraint is correctness. The single
most important opportunity is distribution. Everything else is a
choice between good options.

---

*End of report.*
