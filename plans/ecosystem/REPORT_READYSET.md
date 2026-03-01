# pg_trickle vs ReadySet — Comparison & Synergy Report

**Date:** 2026-03-01  
**Author:** Internal research  
**Status:** Reference document

---

## 1. Executive Summary

`pg_trickle` and ReadySet both tackle the problem of serving pre-computed query
results from PostgreSQL, but they occupy fundamentally different positions in the
stack and make opposite architectural trade-offs.

**pg_trickle** is a PostgreSQL extension that runs *inside* the database. It
materializes complex SQL queries into durable PostgreSQL tables and keeps them
fresh via scheduled differential maintenance — no external infrastructure
required.

**ReadySet** (formerly Noria) is a standalone *proxy* that sits *between* the
application and PostgreSQL. It intercepts SELECT queries at the wire-protocol
level, compiles them into an in-memory dataflow graph, and serves cached results
with sub-millisecond latency — transparently, with zero application code changes.

The two projects are **complementary rather than competing**: pg_trickle excels
at complex analytical materialization with broad SQL coverage and durable results;
ReadySet excels at transparent read-scaling of simple OLTP queries with near-zero
latency. A layered deployment — pg_trickle materializes, ReadySet caches — is the
most powerful combination.

---

## 2. Project Overview

| Attribute | pg_trickle | ReadySet |
|---|---|---|
| Repository | [grove/pg-trickle](https://github.com/grove/pg-trickle) | [readysettech/readyset](https://github.com/readysettech/readyset) |
| Heritage | DBSP (Budiu et al., 2023) | Noria (Gjengset et al., OSDI 2018) |
| Language | Rust (pgrx 0.17) | Rust |
| Latest release | 0.1.2 (2026-02-28) | See note in §14 |
| License | Apache 2.0 | Source-available (BSL 1.1 → Apache 2.0 conversion) |
| PG versions | 18 only | 13 – 16 (MySQL also supported) |
| Architecture | PostgreSQL extension (in-process) | Standalone proxy (out-of-process) |
| Deployment unit | `shared_preload_libraries` + `CREATE EXTENSION` | Separate binary (`readyset` server + adapter) |
| `wal_level = logical` | Optional (trigger CDC works without it) | **Required** |
| Replication slot | Optional (WAL mode only) | **Required** |
| Result storage | Durable PostgreSQL tables | In-memory (evictable, lost on restart) |
| Freshness model | Scheduled (seconds → hours, cron) | Continuous (replication-lag, sub-second) |
| Background worker | Yes (1 worker) | N/A (separate process) |
| Connection pooling | Compatible with session-mode pooling | Replaces the pooler (is the pooler) |

---

## 3. Architecture

### pg_trickle — Extension Inside PostgreSQL

```
┌─────────────────────────────────────────────────┐
│                  PostgreSQL 18                   │
│                                                  │
│  ┌────────────┐    ┌────────────────────────┐   │
│  │ Base       │───▶│ pg_trickle extension    │   │
│  │ Tables     │ CDC│                          │   │
│  │            │    │  ┌─────────────────┐    │   │
│  └────────────┘    │  │ DVM Engine      │    │   │
│                    │  │ (delta SQL gen)  │    │   │
│                    │  └────────┬────────┘    │   │
│                    │           │ MERGE       │   │
│                    │  ┌────────▼────────┐    │   │
│  ┌─────────────┐  │  │ Stream Tables   │    │   │
│  │ Application │◀─┼──│ (PG tables)     │    │   │
│  └─────────────┘  │  └─────────────────┘    │   │
│                    └────────────────────────┘   │
└─────────────────────────────────────────────────┘
```

- Everything runs inside a single PostgreSQL process.
- Stream tables are standard PostgreSQL tables — indexable, joinable, backed up.
- No external processes or network hops.

### ReadySet — External Proxy

```
┌────────────┐     ┌──────────────────────┐     ┌──────────────┐
│            │ SQL │                      │ WAL │              │
│ Application├────▶│   ReadySet Proxy     │◀────┤  PostgreSQL  │
│            │◀────┤                      │ SQL │              │
│            │cache│  ┌────────────────┐  │────▶│              │
└────────────┘ hit │  │ In-memory      │  │fall-│              │
                   │  │ Dataflow Graph │  │back │              │
                   │  │ (Noria engine) │  │     │              │
                   │  └────────────────┘  │     │              │
                   └──────────────────────┘     └──────────────┘
```

- ReadySet is a separate process with its own memory space.
- Applications connect to ReadySet instead of PostgreSQL directly.
- Cache hits are served from in-memory dataflow state; cache misses fall
  through to upstream PostgreSQL.
- WAL logical replication feeds changes into the dataflow graph continuously.

### Combined — Layered Architecture

```
                    ┌──────────────────────┐
┌────────────┐     │   ReadySet Proxy     │
│            │ SQL │                      │
│ Application├────▶│  Caches reads from   │
│            │◀────│  stream tables +     │
└────────────┘     │  base tables         │
                   └──────────┬───────────┘
                              │ WAL + SQL
                   ┌──────────▼───────────────────────┐
                   │          PostgreSQL 18            │
                   │                                    │
                   │  ┌──────────┐   ┌──────────────┐ │
                   │  │ Base     │──▶│ pg_trickle   │ │
                   │  │ Tables   │CDC│              │ │
                   │  └──────────┘   │ ┌──────────┐ │ │
                   │                 │ │ Stream   │ │ │
                   │ ReadySet caches │ │ Tables   │ │ │
                   │ these tables ◀──┤ │ (durable)│ │ │
                   │                 │ └──────────┘ │ │
                   │                 └──────────────┘ │
                   └──────────────────────────────────┘
```

- pg_trickle materializes complex queries into simple, flat stream tables.
- ReadySet caches reads against those stream tables (and base tables) for
  sub-millisecond application-tier latency.
- Each system handles what it does best: pg_trickle for complex SQL
  transformation, ReadySet for read-scaling and transparent caching.

---

## 4. Execution Model

This is the most fundamental design difference.

### pg_trickle — Periodic Batch Refresh

pg_trickle has **no persistent dataflow graph**. On each scheduled refresh cycle:

1. The DVM engine generates a delta SQL query (CTE chain) from the operator tree.
2. PostgreSQL's own planner and executor evaluate that query.
3. Results are merged into the stream table via `MERGE`.
4. No operator state persists between refresh cycles — auxiliary state lives in
   the stream table itself (`__pgt_count` columns) and change buffer tables.

```
  Time ─────────────────────────────────────────────▶

  Writes │ │││  │  ││││ │  │ │││  │  ││││ │  │
         ▼ ▼▼▼  ▼  ▼▼▼▼ ▼  ▼ ▼▼▼  ▼  ▼▼▼▼ ▼  ▼
  CDC    ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓
         │         buffer          │         buffer
         ▼                         ▼
  Refresh █████                    █████
         t=0    stale              t=1    stale
```

**Staleness** is bounded by the refresh schedule: `'30s'`, `'5m'`, `'@hourly'`,
or cron expressions. Between refreshes, stream tables serve stale (but
consistent) data.

### ReadySet — Continuous Streaming Dataflow

ReadySet maintains a **persistent in-memory dataflow graph** with long-lived
stateful operators. Changes flow through the graph continuously:

1. WAL logical replication delivers row changes to ReadySet.
2. Changes propagate through the dataflow operators (join, aggregate, project).
3. Materialized results are updated in-place in memory.
4. Application SELECTs hit the materialized cache directly.

```
  Time ─────────────────────────────────────────────▶

  Writes │ │││  │  ││││ │  │ │││  │  ││││ │  │
         ▼ ▼▼▼  ▼  ▼▼▼▼ ▼  ▼ ▼▼▼  ▼  ▼▼▼▼ ▼  ▼
  WAL    ▓ ▓▓▓  ▓  ▓▓▓▓ ▓  ▓ ▓▓▓  ▓  ▓▓▓▓ ▓  ▓
         ▼ ▼▼▼  ▼  ▼▼▼▼ ▼  ▼ ▼▼▼  ▼  ▼▼▼▼ ▼  ▼
  Cache  ████████████████████████████████████████████
         always fresh (within replication lag)
```

**Staleness** equals replication lag — typically sub-second. There is no
scheduled refresh cycle; the cache is always converging toward the current
database state.

### Implications

| Property | pg_trickle | ReadySet |
|---|---|---|
| Latency to see a write in results | Refresh interval (seconds–hours) | Replication lag (ms–seconds) |
| CPU cost model | Proportional to changes × query complexity, batched | Proportional to changes × dataflow depth, streaming |
| Memory model | PostgreSQL shared buffers (disk-backed) | Dedicated process memory (in-memory only) |
| Result after crash/restart | Intact (durable PG tables) | Lost (must re-snapshot from upstream) |
| State between cycles | Only auxiliary columns in PG tables | Full operator state in memory |

---

## 5. SQL Feature Coverage

### Comparison Table

| Feature | pg_trickle | ReadySet |
|---|---|---|
| Simple SELECT / projection | ✅ | ✅ |
| WHERE filters | ✅ | ✅ |
| HAVING | ✅ | ✅ |
| INNER JOIN | ✅ | ✅ |
| LEFT JOIN | ✅ | ✅ |
| RIGHT JOIN | ✅ | ⚠️ Limited |
| FULL OUTER JOIN | ✅ | ❌ |
| NATURAL JOIN | ✅ | ❌ |
| CROSS JOIN | ✅ | ❌ |
| Multi-way JOIN (3+ tables) | ✅ | ✅ |
| GROUP BY + COUNT, SUM, AVG | ✅ | ✅ |
| GROUP BY + MIN, MAX | ✅ | ✅ |
| GROUP BY + STRING_AGG, ARRAY_AGG | ✅ | ❌ |
| GROUP BY + BOOL_AND/OR | ✅ | ❌ |
| GROUP BY + JSON_AGG, JSONB_AGG | ✅ | ❌ |
| GROUP BY + STDDEV, VARIANCE, regression | ✅ | ❌ |
| GROUPING SETS / CUBE / ROLLUP | ✅ | ❌ |
| DISTINCT | ✅ | ✅ |
| DISTINCT ON | ✅ | ❌ |
| UNION ALL | ✅ | ✅ |
| UNION (deduplicated) | ✅ | ❌ |
| INTERSECT / EXCEPT | ✅ | ❌ |
| Subqueries in FROM | ✅ | ⚠️ Limited |
| EXISTS / NOT EXISTS | ✅ | ❌ |
| IN (subquery) | ✅ | ⚠️ Limited (correlated: ❌) |
| Scalar subqueries | ✅ | ❌ |
| Non-recursive CTEs | ✅ | ⚠️ Limited |
| WITH RECURSIVE | ✅ | ❌ |
| WINDOW functions | ✅ | ❌ |
| LATERAL / SRFs | ✅ | ❌ |
| JSON_TABLE (PG 17+) | ✅ | ❌ |
| ORDER BY | ⚠️ Silently ignored | ✅ |
| LIMIT / OFFSET | ❌ (DIFFERENTIAL) | ✅ |
| Parameterized queries | N/A (defines whole query) | ✅ (key lookup pattern) |
| Views as sources | ✅ (auto-inlined) | ✅ (falls through to PG) |
| Partitioned tables | ✅ | ⚠️ Depends on version |
| Volatile functions | ❌ DIFFERENTIAL / ⚠️ FULL | ❌ |

### Key SQL Gaps

**ReadySet cannot handle:**
- Window functions (`ROW_NUMBER`, `RANK`, `LAG`, `LEAD`, etc.)
- Recursive CTEs (`WITH RECURSIVE`)
- LATERAL joins and set-returning functions
- FULL/CROSS/NATURAL joins
- Complex aggregates (STRING_AGG, ARRAY_AGG, JSON_AGG, statistical)
- Set operations beyond UNION ALL (INTERSECT, EXCEPT, UNION with dedup)
- GROUPING SETS / CUBE / ROLLUP
- Correlated subqueries and EXISTS/NOT EXISTS

**pg_trickle cannot handle:**
- LIMIT/OFFSET in DIFFERENTIAL mode
- Parameterized query caching (it materializes entire result sets, not lookup patterns)
- ORDER BY preservation (silently dropped)

This gap profile makes the two systems highly complementary: pg_trickle can
pre-materialize the complex queries that ReadySet cannot cache, producing
simple flat tables that ReadySet *can* cache.

---

## 6. Change Data Capture

| Attribute | pg_trickle | ReadySet |
|---|---|---|
| Primary CDC mechanism | Row-level AFTER triggers | WAL logical replication |
| WAL-based CDC | ✅ Optional (auto-transitions from triggers) | ✅ Required |
| `wal_level = logical` required | No (trigger mode works without it) | **Yes** |
| Replication slot required | No (trigger mode) / Yes (WAL mode) | **Yes** |
| Replication slot type | Logical (WAL mode only) | Logical |
| Output plugin | `pgoutput` | `pgoutput` |
| Write-path overhead (trigger mode) | ~20–55 μs/row (trigger insert to buffer) | N/A |
| Write-path overhead (WAL mode) | ~0 μs (eliminated trigger overhead) | ~0 μs (WAL is written regardless) |
| Hybrid transition | ✅ Trigger → WAL seamless transition | N/A (WAL-only) |
| Fallback on error | ✅ WAL → trigger fallback | ❌ Must fix replication |
| TRUNCATE handling | Buffer cleared; full refresh queued | Table cache invalidated; re-snapshot |
| DDL handling | Event triggers detect ALTER/DROP | Partial (requires cache re-creation) |
| Change buffer tables | `pgtrickle_changes.changes_<oid>` | In-memory operator state |

### Key Difference

pg_trickle's hybrid CDC is a significant operational advantage: it works
out-of-the-box with zero PostgreSQL configuration changes (trigger mode), and
can optionally transition to WAL for lower overhead. ReadySet **requires**
`wal_level = logical` and a replication slot — which means a PostgreSQL restart
on many managed hosting platforms and consumes WAL retention resources.

For environments where `wal_level = logical` cannot be enabled (e.g., some
managed databases, compliance environments), pg_trickle is the only option.

---

## 7. State & Durability

| Attribute | pg_trickle | ReadySet |
|---|---|---|
| Result storage medium | PostgreSQL heap tables (disk-backed) | In-process memory (RAM only) |
| Survives process restart | ✅ Yes | ❌ No (cache lost, must warm) |
| Survives PostgreSQL restart | ✅ Yes | ✅ (ReadySet is separate process) |
| Survives ReadySet restart | N/A | ❌ Cold start, re-snapshot required |
| Backup / pg_dump | ✅ Standard pg_dump includes stream tables | ❌ Not applicable |
| Indexable | ✅ Standard PostgreSQL indexes | ❌ Internal data structures only |
| Joinable with other tables | ✅ Full SQL on stream tables | ❌ Cache results only via proxy |
| Point-in-time recovery | ✅ Via PostgreSQL PITR | ❌ |
| Eviction | None (fully materialized) | ✅ LRU eviction of cold entries |
| Memory footprint | PostgreSQL shared_buffers (shared) | Dedicated per-cache memory |
| Cold start time | None (tables already populated) | Seconds–minutes (re-snapshot) |

### Implications

pg_trickle's durable storage means stream tables are first-class PostgreSQL
citizens: they can be indexed for fast lookups, joined in ad-hoc queries,
included in `pg_dump` backups, and recovered via PITR. They persist across
all types of restarts.

ReadySet's in-memory storage gives it sub-millisecond read latency but at the
cost of durability — a ReadySet restart requires re-snapshotting base tables
and replaying WAL to rebuild the cache. For large datasets, this cold-start
penalty can be significant (minutes).

---

## 8. Scheduling & Freshness

| Attribute | pg_trickle | ReadySet |
|---|---|---|
| Freshness model | Scheduled (poll-based) | Continuous (push-based) |
| Typical staleness | Seconds to hours (configurable) | Milliseconds (replication lag) |
| Duration schedules | ✅ (`'30s'`, `'5m'`, `'1h'`) | N/A |
| Cron schedules | ✅ (5/6-field cron + `@daily` aliases) | N/A |
| Manual refresh | ✅ `refresh_stream_table()` | N/A (always refreshing) |
| Dependency DAG | ✅ Topological ordering across stream tables | ❌ No chained/cascading views |
| CALCULATED schedule propagation | ✅ Consumers drive upstream schedules | ❌ |
| Freshness guarantee on read | Data is from last refresh cycle | Data is from last replicated WAL position |
| Pause/resume | ✅ `pg_trickle.enabled = false` | Stop/start proxy |

### The Freshness Spectrum

```
  ◀── pg_trickle ──────────────────────────────────▶
  hours    minutes    seconds    100ms    10ms    1ms
  ├──────────┼──────────┼──────────┼───────┼──────┤
  cron     duration   aggressive          repl. lag
  schedules  schedules  schedule          ◀─ ReadySet ─▶
```

pg_trickle covers the left side of the spectrum (analytical summaries that can
tolerate seconds-to-hours staleness). ReadySet covers the right side
(application-facing reads that need near-real-time freshness). Together they
span the full range.

---

## 9. Deployment & Operations

| Attribute | pg_trickle | ReadySet |
|---|---|---|
| External process required | No | **Yes** (ReadySet server + adapter) |
| PostgreSQL configuration | `shared_preload_libraries` addition | `wal_level = logical`, replication slot, publication |
| PostgreSQL restart required | Yes (for `shared_preload_libraries`) | Yes (for `wal_level` change, if not already set) |
| Application code changes | None (query stream tables directly) | None (transparent proxy) |
| Connection string change | No | **Yes** (point to ReadySet instead of PG) |
| Kubernetes deployment | CNPG Image Volume (single pod) | Separate Deployment/StatefulSet |
| Docker local dev | Extension in PG container | Separate container + PG container |
| Resource isolation | Shares PostgreSQL resources | Separate memory, CPU, network |
| Scaling model | Vertical (PG instance) | Horizontal (multiple ReadySet instances) |
| Connection pooling | Compatible with session-mode pools (PgBouncer session mode) | **Replaces** the pooler — ReadySet is itself a connection pool |
| High availability | Follows PostgreSQL HA (patroni, CNPG) | Requires separate HA setup |
| Monitoring | Built-in SQL views, NOTIFY | Prometheus metrics endpoint |

### Operational Complexity

pg_trickle is operationally simpler for teams already running PostgreSQL — it is
just an extension with no additional infrastructure to deploy, monitor, or
maintain. The trade-off is that it shares PostgreSQL's resources (CPU, memory,
I/O) during refresh cycles.

ReadySet requires a separate deployment but provides resource isolation: heavy
read traffic is served from ReadySet's memory without touching PostgreSQL at all.
This makes it attractive for teams that need to offload reads from a saturated
primary.

---

## 10. Concurrency & Isolation

### pg_trickle

- Refresh operations acquire a per-stream-table **advisory lock** — only one
  refresh per stream table at a time.
- Base table writes are **never blocked** by refresh operations.
- The background worker processes stream tables sequentially (within the same
  DAG layer) or in parallel across independent branches.
- Stream table reads during refresh see the previous (pre-refresh) state —
  standard MVCC isolation.

### ReadySet

- Reads are served from in-memory state — no PostgreSQL lock interaction.
- Writes pass through to upstream PostgreSQL (if ReadySet is in read/write
  proxy mode) or are routed directly.
- Cache consistency is eventual — reads may briefly lag behind the most
  recent write due to WAL propagation delay.
- No PostgreSQL lock contention from ReadySet's cache maintenance.

### Key Difference

Neither system introduces write-path lock contention on base tables. pg_trickle's
advisory locks are only between concurrent refreshes of the same stream table.
ReadySet's reads are entirely lock-free (from PostgreSQL's perspective) because
they're served from external memory.

---

## 11. Observability

| Feature | pg_trickle | ReadySet |
|---|---|---|
| Catalog of managed objects | ✅ `pgtrickle.pgt_stream_tables` | ✅ `SHOW CACHES` / `SHOW PROXIED QUERIES` |
| Per-refresh timing/history | ✅ `pgtrickle.pgt_refresh_history` | ❌ (no refresh concept) |
| Staleness reporting | ✅ `stale` column in monitoring views | N/A (always streaming) |
| Scheduler status | ✅ `pgtrickle.pgt_status()` | N/A |
| NOTIFY-based alerting | ✅ `pgtrickle_refresh` channel | ❌ |
| Prometheus metrics | ⚠️ Planned (v0.4.0) | ✅ Built-in metrics endpoint |
| Grafana dashboard | ⚠️ Planned (v0.4.0) | ✅ Available |
| Error tracking | ✅ Consecutive error counter, last error | ✅ Query-level error reporting |
| Cache hit/miss rates | N/A | ✅ Per-query hit rate tracking |
| Replication lag monitoring | ✅ (WAL mode: LSN frontier) | ✅ WAL position tracking |
| dbt integration | ✅ `dbt-pgtrickle` package | ❌ |

---

## 12. Known Limitations

### pg_trickle Limitations

- Data is **stale between refresh cycles** — not suitable for sub-second
  freshness requirements.
- `LIMIT` / `OFFSET` not supported in DIFFERENTIAL mode.
- Volatile SQL functions rejected in DIFFERENTIAL mode.
- Materialized views as sources not supported in DIFFERENTIAL mode.
- Extension upgrade migrations not yet implemented (planned for v0.2.0+).
- Targets **PostgreSQL 18 only** — no backport to PG 13–17.
- Early release — not yet production-hardened.
- Refresh cycles consume PostgreSQL CPU/I/O (shared resources).
- Not compatible with transaction-mode PgBouncer.
- No parameterized query caching — materializes entire result sets.

### ReadySet Limitations

- **Requires `wal_level = logical`** — not available on all managed PG platforms.
- Requires a **replication slot** — consumes WAL retention resources.
- SQL coverage significantly narrower than pg_trickle (no window functions,
  recursive CTEs, LATERAL, complex aggregates, set operations).
- **In-memory only** — cache lost on restart, cold-start penalty.
- No query result durability — cannot `pg_dump`, index, or join cached results.
- No multi-layer view dependencies (no DAG / cascading).
- No dbt integration.
- Cannot handle DDL changes gracefully — requires manual cache recreation.
- **Project status uncertain** — see §15.
- MySQL wire-protocol mode may receive more investment than PostgreSQL mode.
- Cache eviction under memory pressure can cause performance cliffs.
- Full outer joins, complex subqueries, window functions not supported.

---

## 13. Performance Characteristics

### Write Path

| Metric | pg_trickle (trigger mode) | pg_trickle (WAL mode) | ReadySet |
|---|---|---|---|
| Per-row overhead | ~20–55 μs (trigger insert) | ~0 μs (WAL is already written) | ~0 μs (WAL is already written) |
| Locking impact on writes | None (trigger is async-safe) | None | None |
| WAL volume impact | Change buffer inserts generate WAL | Reads existing WAL | Reads existing WAL |

### Read Path

| Metric | pg_trickle | ReadySet |
|---|---|---|
| Read latency | Standard PostgreSQL table read (disk/buffer) | Sub-millisecond (in-memory) |
| Index support | Full PostgreSQL index support | Internal lookup structures |
| Concurrent readers | PostgreSQL MVCC (unlimited) | Lock-free in-memory reads |
| Cold cache | No cold cache (durable tables) | Full table scan from PG to warm |

### Refresh / Maintenance Cost

| Metric | pg_trickle (DIFFERENTIAL) | pg_trickle (FULL) | ReadySet |
|---|---|---|---|
| Cost model | Proportional to Δ(changes) × query complexity | Full recomputation | Proportional to Δ(changes) × dataflow depth |
| Single-row change on 1M table | Touches ~1 row's computation | Recomputes all 1M rows | Updates affected cache entries |
| CPU location | PostgreSQL backend (shared) | PostgreSQL backend | ReadySet process (isolated) |
| Blocking during maintenance | No (MVCC) | No (MVCC) | No (in-memory) |

### Resource Isolation

pg_trickle's refresh cycles compete with application queries for PostgreSQL's
CPU and I/O budget. ReadySet's maintenance runs in a separate process with its
own resource allocation. For write-heavy workloads where PostgreSQL CPU is the
bottleneck, ReadySet's resource isolation is advantageous.

---

## 14. Use-Case Fit

| Scenario | Recommended |
|---|---|
| Complex analytical materialization (multi-join, aggregates, CTEs, windows) | **pg_trickle** |
| Transparent read-scaling of simple OLTP queries | **ReadySet** |
| Zero additional infrastructure | **pg_trickle** |
| Sub-second freshness for simple queries | **ReadySet** |
| Multi-layer view pipelines with dependency ordering | **pg_trickle** |
| Results must survive restarts / be backed up | **pg_trickle** |
| Offload read traffic from saturated primary | **ReadySet** |
| Parameterized key-value lookup caching | **ReadySet** |
| dbt transformation pipelines | **pg_trickle** |
| `wal_level = logical` not available | **pg_trickle** |
| Kubernetes / CNPG deployment with minimal pods | **pg_trickle** |
| Horizontal read scaling across multiple cache nodes | **ReadySet** |
| Complex SQL + low-latency reads | **Both** (see §16) |
| PostgreSQL 13–17 support required | **ReadySet** |
| PostgreSQL 18 | **pg_trickle** (or both) |
| Long-term project stability requirement | **pg_trickle** (see §15) |
| Application cannot change connection string | **pg_trickle** |

---

## 15. ReadySet Project Status

ReadySet Inc. launched a managed cloud product (ReadySet Cloud) but **shut it
down**. The company's trajectory has been turbulent — layoffs, strategic pivots,
and periods of reduced open-source activity. As of early 2026:

- The open-source repository remains available but commit frequency has declined.
- No new major releases in the PostgreSQL adapter for several months.
- The MySQL adapter appears to receive more attention than PostgreSQL.
- Community engagement (issues, PRs, Discord) has slowed.
- The BSL 1.1 license (converting to Apache 2.0 after 4 years) may limit
  commercial use before conversion.

**Risk assessment for integration planning:**

| Risk | Impact | Mitigation |
|---|---|---|
| ReadySet project becomes unmaintained | High — no bug fixes, PG version support | pg_trickle stands alone; ReadySet is additive |
| License terms change | Medium — BSL 1.1 restricts some commercial use | Use pg_trickle as primary; ReadySet as optional |
| PostgreSQL adapter lags behind MySQL | Medium — PG-specific bugs may persist | Test thoroughly before production deployment |
| API/wire-protocol breaking changes | Low — wire protocol is stable | Pin ReadySet version in deployments |

This uncertainty is the primary reason pg_trickle should be treated as the
foundational layer, with ReadySet as an **optional, additive** optimization —
not a dependency.

---

## 16. Synergies & Integration Opportunities

### Opportunity 1: Layered Architecture — Materialize + Cache

The highest-value synergy is using pg_trickle for complex SQL transformation
and ReadySet for read-scaling:

```
  Complex query ──▶ pg_trickle ──▶ Stream Table ──▶ ReadySet ──▶ App
  (window funcs,     (scheduled      (durable PG     (sub-ms      (zero
   recursive CTEs,    DIFFERENTIAL    table, flat     in-memory    code
   37 aggregates)     refresh)        and simple)     cache)       change)
```

**Example workflow:**

```sql
-- 1. pg_trickle materializes a complex analytical query
SELECT pgtrickle.create_stream_table(
    'customer_lifetime_value',
    $$
    WITH monthly AS (
        SELECT customer_id,
               date_trunc('month', order_date) AS month,
               SUM(amount) AS monthly_total,
               COUNT(*) AS order_count
        FROM orders
        GROUP BY customer_id, date_trunc('month', order_date)
    )
    SELECT customer_id,
           SUM(monthly_total) AS lifetime_value,
           AVG(monthly_total) AS avg_monthly,
           COUNT(DISTINCT month) AS active_months,
           MAX(month) AS last_active_month,
           STDDEV(monthly_total) AS spend_volatility
    FROM monthly
    GROUP BY customer_id
    $$,
    '5m',
    'DIFFERENTIAL'
);

-- 2. ReadySet caches the simple lookup against the stream table
--    (application connects through ReadySet proxy)
--    This query is trivially cacheable by ReadySet:
SELECT * FROM customer_lifetime_value WHERE customer_id = $1;
```

pg_trickle handles the complex SQL that ReadySet cannot (CTE, STDDEV,
COUNT(DISTINCT), multi-level aggregation). ReadySet handles the parameterized
key-value lookup pattern that pg_trickle doesn't optimize for (it materializes
the whole table, but ReadySet caches the specific lookup).

### Opportunity 2: SQL Coverage Gap-Filling

ReadySet's SQL parser rejects queries it cannot incrementally maintain. For
these queries, pg_trickle can pre-materialize the result:

| Unsupported by ReadySet | pg_trickle Materializes | ReadySet Caches |
|---|---|---|
| Window functions (`ROW_NUMBER() OVER(...)`) | Stream table with pre-computed rank | Simple `SELECT WHERE rank <= N` |
| `WITH RECURSIVE` (graph traversal) | Stream table with flattened paths | Key lookup on flattened result |
| `INTERSECT` / `EXCEPT` | Stream table with set operation result | Full scan or filtered read |
| `FULL OUTER JOIN` | Stream table with full join result | Key lookup against result |
| GROUPING SETS / CUBE | Stream table with rolled-up aggregates | Parameterized group filter |
| Complex aggregates (STDDEV, ARRAY_AGG) | Pre-aggregated stream table | Simple reads |

### Opportunity 3: Mixed Freshness Tiers

Different data consumers have different freshness requirements. Deploy both
systems to serve the full spectrum:

| Data | Freshness Need | System | Refresh |
|---|---|---|---|
| User profile lookups | Near-real-time | ReadySet (direct cache) | Continuous WAL |
| Order totals by region | Minutes | pg_trickle | `'2m'` schedule |
| Daily revenue dashboard | Hourly | pg_trickle | `'@hourly'` cron |
| Product recommendations | Seconds | ReadySet (from pg_trickle ST) | Continuous WAL + 30s pg_trickle |
| Inventory counts | Sub-second | ReadySet (direct cache) | Continuous WAL |

### Opportunity 4: CDC Knowledge Sharing

Both projects implement PostgreSQL WAL consumption. Areas of shared learning:

- **Replication slot lifecycle**: Creation, monitoring, cleanup, and handling
  of slot invalidation due to WAL retention limits.
- **Failover behavior**: How logical replication slots behave during primary
  failover (Patroni, CNPG) — both projects must handle slot migration or
  re-creation.
- **WAL retention pressure**: Replication slots prevent WAL cleanup. Monitoring
  and alerting for growing `pg_wal` sizes is critical for both.
- **`pgoutput` plugin quirks**: Schema change handling, TOAST column behavior,
  and replica identity settings affect both systems.

### Opportunity 5: dbt Ecosystem Bridge

pg_trickle has `dbt-pgtrickle` for managing stream tables in dbt projects.
A hypothetical extension could orchestrate both systems:

```yaml
# dbt model: models/customer_clv.sql
# pg_trickle materializes the transformation
{{ config(
    materialized='stream_table',
    schedule='5m',
    refresh_mode='DIFFERENTIAL'
) }}
SELECT customer_id, SUM(amount) AS lifetime_value
FROM {{ ref('orders') }}
GROUP BY customer_id

# A dbt post-hook or meta tag could emit ReadySet cache hints:
# {{ config(meta={'readyset_cache': true}) }}
```

This is speculative but illustrates how dbt could orchestrate both the
materialization layer (pg_trickle) and the caching layer (ReadySet) from a
single project definition.

### Opportunity 6: Partial Materialization Inspiration

ReadySet (via Noria) pioneered **partially-stateful dataflow** — only
materializing the working set and evicting cold entries. pg_trickle currently
always fully materializes stream tables.

For very large stream tables (millions of rows) where only a small fraction is
actively queried, a future pg_trickle feature could borrow this concept:

- **Partial refresh**: Only maintain incremental state for rows matching a
  predicate (e.g., `WHERE order_date > now() - interval '90 days'`).
- **Tiered storage**: Keep hot rows in the stream table, archive cold rows to
  a partitioned history table, refresh only the hot partition differentially.

This would not replace ReadySet's real-time eviction but could reduce
pg_trickle's storage and refresh cost for large, time-partitioned datasets.

---

## 17. Coexistence

pg_trickle and ReadySet can coexist in the same deployment without conflicts:

| Aspect | Interaction |
|---|---|
| PostgreSQL schemas | No overlap (`pgtrickle` / `pgtrickle_changes` vs. none) |
| Replication slots | Each uses its own slot (if pg_trickle is in WAL mode) |
| Triggers | pg_trickle's CDC triggers are invisible to ReadySet |
| Connection routing | App → ReadySet → PostgreSQL; pg_trickle runs inside PG |
| WAL consumption | Both read the same WAL; no interference |
| Monitoring | Separate systems (SQL views vs. Prometheus endpoint) |
| Resource contention | ReadySet offloads reads; pg_trickle refresh may compete with PG writes |

### Deployment Topology

```
┌─────────────────────────────────────────────────────────────┐
│  Kubernetes Cluster                                          │
│                                                              │
│  ┌──────────────────┐    ┌────────────────────────────────┐ │
│  │  App Pods         │    │  ReadySet Deployment           │ │
│  │  (connect to RS)  │───▶│  - readyset-server             │ │
│  └──────────────────┘    │  - readyset-adapter             │ │
│                          └──────────────┬─────────────────┘ │
│                                         │ WAL + SQL          │
│                          ┌──────────────▼─────────────────┐ │
│                          │  CNPG Cluster                   │ │
│                          │  PostgreSQL 18                   │ │
│                          │  + pg_trickle extension          │ │
│                          │  ┌──────────────────────────┐   │ │
│                          │  │ Stream Tables (durable)   │   │ │
│                          │  │ Base Tables               │   │ │
│                          │  └──────────────────────────┘   │ │
│                          └─────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

Monitoring integration:
- pg_trickle: query `pgtrickle.pgt_status()` views via SQL (or future
  Prometheus exporter, planned for v0.4.0).
- ReadySet: scrape the built-in `/metrics` endpoint.
- Both metrics can feed into the same Grafana instance for unified visibility.

---

## 18. Summary Table

| Dimension | pg_trickle | ReadySet |
|---|---|---|
| **Architecture** | PostgreSQL extension (in-process) | Standalone proxy (out-of-process) |
| **Deployment** | `CREATE EXTENSION` — zero extra infra | Separate server + adapter binary |
| **Theoretical basis** | DBSP (Budiu 2023) | Noria (Gjengset 2018) |
| **Execution model** | Periodic batch (SQL CTE execution) | Continuous streaming dataflow |
| **Result storage** | Durable PostgreSQL tables | In-memory (evictable) |
| **Freshness** | Scheduled (seconds–hours) | Continuous (replication-lag) |
| **SQL coverage** | Broad (21 operators, 37+ aggregates) | Narrow (simple joins + aggregates) |
| **Window functions** | ✅ | ❌ |
| **Recursive CTEs** | ✅ | ❌ |
| **LATERAL / SRFs** | ✅ | ❌ |
| **Set operations** | ✅ UNION/INTERSECT/EXCEPT | ⚠️ UNION ALL only |
| **Parameterized queries** | ❌ (full materialization) | ✅ (key lookup caching) |
| **Multi-layer DAG** | ✅ | ❌ |
| **CDC mechanism** | Hybrid trigger → WAL | WAL-only |
| **`wal_level = logical` required** | No (optional) | **Yes** |
| **Write-path overhead** | ~20–55 μs (trigger) / ~0 (WAL) | ~0 (WAL) |
| **Read latency** | PostgreSQL table scan | Sub-millisecond (memory) |
| **Crash durability** | ✅ Tables survive all restarts | ❌ Cache lost on restart |
| **Connection pooling** | Session-mode compatible | Replaces the pooler |
| **dbt integration** | ✅ `dbt-pgtrickle` | ❌ |
| **Kubernetes (CNPG)** | ✅ Image Volume | Separate deployment |
| **Monitoring** | SQL views + NOTIFY | Prometheus endpoint |
| **PG version support** | 18 only | 13–16 |
| **License** | Apache 2.0 | BSL 1.1 (→ Apache 2.0) |
| **Project status** | Active, early-stage | Uncertain (see §15) |
| **Best for** | Complex analytical materialization | Simple OLTP read-scaling |
| **Together** | Materializes → flat table | Caches → sub-ms reads |

---

## 19. Recommendations

1. **Treat pg_trickle as the foundational layer.** It requires no external
   infrastructure and produces durable, queryable results. ReadySet is additive.

2. **Evaluate ReadySet for read-scaling only if** the deployment already has
   `wal_level = logical` and the team is comfortable operating a separate proxy.

3. **The layered pattern is the highest-value synergy:** use pg_trickle for
   complex SQL materialization, ReadySet for caching simple lookups against
   those materialized tables.

4. **Monitor ReadySet's project health** before committing to operational
   dependency. Given the uncertain project trajectory (§15), treat ReadySet as
   optional and replaceable (e.g., with application-level caching or Redis if
   ReadySet becomes unavailable).

5. **No implementation work needed in pg_trickle** to enable ReadySet
   integration — stream tables are standard PostgreSQL tables. ReadySet can
   cache them immediately. The synergy is architectural, not code-level.

6. **Consider a tutorial/guide** (separate document) showing the layered
   deployment pattern if user demand warrants it.

---

## References

- ReadySet repository: https://github.com/readysettech/readyset
- ReadySet documentation: https://docs.readyset.io/
- Noria paper: Gjengset, J., Schwarzkopf, M., Behrens, J., Araújo, L.T.,
  Lam, E., Kohler, E., Kaashoek, M.F., & Morris, R. (2018). "Noria:
  Dynamic, Partially-Stateful Data-Flow for High-Performance Web Applications."
  *Proceedings of OSDI 2018*, 213–231.
- DBSP paper: Budiu, M., Ryzhyk, L., McSherry, F., & Tannen, V. (2023).
  "DBSP: Automatic Incremental View Maintenance for Rich Query Languages."
  *PVLDB*, 16(7), 1601–1614. https://arxiv.org/abs/2203.16684
- pg_trickle architecture: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
- pg_trickle DVM operators: [../../docs/DVM_OPERATORS.md](../../docs/DVM_OPERATORS.md)
- pg_trickle ESSENCE: [../../ESSENCE.md](../../ESSENCE.md)
- pg_trickle vs pg_ivm comparison: [REPORT_PG_IVM_COMPARISON.md](REPORT_PG_IVM_COMPARISON.md)
