# Datomic Gap Analysis — Ideas for pg_trickle

> **Created:** 2026-04-18
> **Status:** Research / brainstorming
> **Scope:** Identify Datomic architectural ideas that could benefit pg_trickle

---

## 1  Executive Summary

Datomic is an immutable, append-only database built on a model of **datoms**
(Entity, Attribute, Value, Transaction). Its architecture makes strong
guarantees about temporal consistency, auditability, and read scalability that
emerge naturally from immutability. This document maps Datomic's distinctive
features against pg_trickle's current architecture (v0.21.0), identifies gaps,
and evaluates whether each gap represents a real opportunity or a conceptual
mismatch.

### Scoring

Each gap is rated on three axes:

| Axis | Scale |
|------|-------|
| **Relevance** | How directly applicable to pg_trickle's IVM mission |
| **Effort** | Rough implementation cost (S / M / L / XL) |
| **Impact** | Potential benefit to correctness, performance, or usability |

---

## 2  Datomic Architectural Principles

| Principle | Description |
|-----------|-------------|
| **Datoms** | All data is immutable 4-tuples `(E, A, V, Tx)`. No in-place UPDATE. |
| **Append-only log** | Indelible transaction log is a first-class queryable index. |
| **Total Tx ordering** | Monotonically increasing transaction IDs serialize all writes. |
| **Temporal queries** | `as-of(t)`, `since(t)`, `history(db)` — zero-cost time travel. |
| **Transaction-as-entity** | Every Tx is an entity with user-attachable metadata. |
| **Multi-index** | Four sorted views (EAVT, AEVT, AVET, VAET) of the same data. |
| **Immutable segments** | Index segments stored in tiers; no coordination for reads. |
| **Reactive tx-report-queue** | Clients can subscribe to a stream of committed transactions. |
| **Datalog queries** | Logic-programming query language with rules and recursion. |
| **Speculative transactions** | `with(db, tx-data)` returns a hypothetical DB value without committing. |

---

## 3  Feature-by-Feature Gap Matrix

### 3.1  Change Tracking & Append-Only Log

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| All changes are append-only datoms | Change buffers (`changes_<oid>`) are **append-only per refresh cycle** but **truncated** after consumption | Partial |
| Transaction log is a queryable index | `pgt_refresh_history` logs refresh events; raw change buffers are ephemeral | Yes |
| Explicit retractions (never physical delete) | Trigger CDC captures `'D'` action rows, but they are discarded after refresh | Yes |
| `__pgt_lsn` + `__pgt_xid` provide ordering | LSN ordering exists but is not exposed as a queryable timeline | Partial |

**Gap analysis:**

pg_trickle's change buffers are architecturally similar to Datomic's transaction
log — both capture assertions and retractions. The critical difference is
**retention**: pg_trickle truncates change buffers after refresh, losing the
historical delta stream. Datomic retains everything.

**Potential idea — Retained Change Log:**

Keep a configurable window of consumed deltas (e.g. last N refreshes or last T
hours) in an append-only audit table per source. This enables:

- **Post-hoc debugging:** "What changes triggered the bad refresh at 14:02?"
- **Replay:** Re-derive a stream table from a prior point without full refresh.
- **Downstream consumers:** External systems (Kafka, CDC sinks) can poll the
  retained log instead of tapping triggers directly.

| Relevance | Effort | Impact |
|-----------|--------|--------|
| High | M | High — debugging, replay, downstream integration |

**Trade-off:** Storage cost. Mitigated by TTL-based pruning, UNLOGGED tables
for non-durable use cases, and optional per-ST opt-in.

---

### 3.2  Temporal Queries (as-of / since / history)

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| `as-of(t)` — snapshot at any historical point | Stream tables are current-snapshot only | Yes |
| `since(t)` — only data newer than t | Frontier-based LSN range filtering (internal only) | Partial |
| `history(db)` — full assertion/retraction timeline | No equivalent | Yes |

**Gap analysis:**

pg_trickle already has the **machinery** for temporal semantics: the frontier
tracks per-source LSN ranges, and the scheduler maintains `data_timestamp`
(canonical periods). But these are internal bookkeeping, not user-facing query
capabilities.

**Potential idea — Temporal Stream Table Queries:**

```sql
-- Hypothetical syntax
SELECT * FROM my_stream_table AS OF data_timestamp '2026-04-17 12:00:00';
SELECT * FROM my_stream_table SINCE data_timestamp '2026-04-17 11:00:00';
```

Implementation approach:
1. **Snapshot retention:** Periodically snapshot stream table state (or store
   deltas with sequence numbers) alongside the current materialized data.
2. **Point-in-time reconstruction:** Replay deltas backward/forward from
   nearest snapshot to target timestamp.
3. **`since` filter:** Expose the frontier's LSN-range filtering as a SQL
   predicate that returns only rows affected since a given data_timestamp.

| Relevance | Effort | Impact |
|-----------|--------|--------|
| Medium | XL | Medium — compelling for audit/compliance use cases but orthogonal to IVM performance |

**Trade-off:** Significant storage and complexity. Consider a lightweight
version first: expose `data_timestamp` as a queryable column on
`pgt_refresh_history`, and allow `pgt_diagnose()` to show "state as of refresh
N" without full temporal query support.

---

### 3.3  Transaction-as-Entity (Rich Metadata)

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| Tx is a first-class entity with arbitrary attributes | `pgt_refresh_history` has fixed columns | Partial |
| User can attach `{:source "import-job-42"}` to a transaction | No equivalent for source DML transactions | Yes |
| Queries can filter by Tx attributes | No Tx-attribute-based filtering in delta SQL | Yes |

**Gap analysis:**

pg_trickle's `pgt_refresh_history` logs operational metadata (duration, rows
affected, mode, status), but the *source* transactions that fed the refresh are
anonymous. There is no way to ask "which import batch caused this stream table
to change?"

**Potential idea — Change Buffer Metadata Columns:**

Add optional user-defined metadata columns to change buffers:

```sql
-- Hypothetical: user sets session variable before DML
SET pg_trickle.tx_metadata = '{"batch_id": "import-42", "source": "etl"}';
INSERT INTO orders ...;
-- Trigger captures tx_metadata into __pgt_tx_meta JSONB column
```

Differential refresh could then:
- **Filter deltas** by metadata (only refresh from trusted sources).
- **Propagate metadata** downstream (ST-to-ST change buffers carry lineage).
- **Expose in diagnostics** (`pgt_diagnose()` shows which batches are pending).

| Relevance | Effort | Impact |
|-----------|--------|--------|
| Medium | M | Medium — valuable for ETL pipelines, audit, multi-tenant filtering |

**Trade-off:** Per-row JSONB column adds ~40 bytes overhead. Make it opt-in
per stream table (`WITH (track_metadata = true)`).

---

### 3.4  Immutable Index Segments & Multi-Tier Caching

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| Four immutable indexes (EAVT, AEVT, AVET, VAET) | Standard PostgreSQL B-tree/GIN indexes on stream tables | No |
| Index segments never mutated; cached across readers | PostgreSQL shared buffers + OS page cache | No |
| "Live index" for recent changes merged at query time | Change buffers are separate tables; merged at refresh time | Partial |

**Gap analysis:**

PostgreSQL's MVCC and shared buffer cache already provide many of Datomic's
caching benefits. The meaningful gap is Datomic's **live index** concept: recent
changes are queryable *immediately* without waiting for a merge step.

**Potential idea — Queryable Pending Deltas:**

Expose a view that UNIONs the current stream table with pending (unconsumed)
change buffer rows, giving users an **approximate real-time view** without
waiting for the next refresh cycle:

```sql
-- Hypothetical
SELECT * FROM pgtrickle.pending_view('my_stream_table');
-- Returns: current ST rows ∪ pending inserts − pending deletes
```

This is conceptually similar to Datomic's live index — recent facts are visible
immediately, with eventual merge into the persistent index.

| Relevance | Effort | Impact |
|-----------|--------|--------|
| Medium | L | Medium — useful for latency-sensitive dashboards |

**Trade-off:** Correctness is tricky — pending deltas may include changes that
will be compacted or rolled back. Must be clearly labeled as "approximate."

---

### 3.5  Reactive Subscriptions (tx-report-queue)

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| Clients subscribe to committed Tx stream | No push-based notification | Yes |
| tx-report-queue delivers `{:tx-data [...datoms...]}` | LISTEN/NOTIFY used internally for scheduler wake | Partial |

**Gap analysis:**

pg_trickle's scheduler processes changes on a polling interval
(`scheduler_interval_ms`). There is no way for an external client to subscribe
to "stream table X was refreshed" or "these rows changed."

**Potential idea — NOTIFY-Based Refresh Events:**

After each successful refresh, emit a `pg_notify` on a per-ST channel:

```sql
-- Automatic after refresh
NOTIFY pgtrickle_refreshed_42, '{"pgt_id":42,"mode":"DIFFERENTIAL","rows":150,"ts":"..."}';
```

Clients (application servers, CDC connectors, websocket bridges) can
`LISTEN pgtrickle_refreshed_42` for push-based invalidation.

A more advanced version could include the **delta summary** (row IDs of changed
rows) in a separate queryable table, enabling fine-grained subscriptions.

| Relevance | Effort | Impact |
|-----------|--------|--------|
| High | S | High — low-cost, high-value for real-time applications |

**Note:** This aligns with the v0.28.0 roadmap item (PGlite Reactive
Integration). Implementing a basic NOTIFY mechanism earlier would provide
immediate value and serve as a foundation for the reactive layer.

---

### 3.6  Total Transaction Ordering & Deterministic Replay

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| Monotonic Tx IDs serialize all writes | LSN ordering per source, `data_timestamp` per refresh cycle | Partial |
| Replay from log reproduces exact DB state | Refresh is not replayable — change buffers are truncated | Yes |
| CAS (compare-and-swap) for optimistic concurrency | Advisory locks for compaction; no CAS on stream table rows | Partial |

**Gap analysis:**

pg_trickle's frontier (per-source LSN) provides **partial ordering** — changes
within a source are ordered, but cross-source ordering depends on the
scheduler's DAG traversal. Datomic's total ordering is stronger: every datom
across all entities has a single global position.

**Potential idea — Global Refresh Sequence Number:**

Introduce a monotonically increasing `refresh_seq` (backed by a PostgreSQL
SEQUENCE) that increments on every refresh across all stream tables. This
provides:

- **Total ordering** of all refresh events (not just per-ST).
- **Deterministic replay** — given the same change buffers and the same
  `refresh_seq` range, produce identical results.
- **Cross-ST consistency queries** — "show me all STs at `refresh_seq ≤ 500`."

| Relevance | Effort | Impact |
|-----------|--------|--------|
| Medium | S | Medium — debugging, deterministic testing, cross-ST consistency |

**Current partial equivalent:** `pgt_refresh_history.id` is a SERIAL but is
per-ST, not global. A global sequence is trivial to add.

---

### 3.7  Speculative Transactions (what-if analysis)

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| `with(db, tx-data)` returns hypothetical DB | No equivalent | Yes |

**Gap analysis:**

Datomic's `with` function applies a transaction to a database value *in memory*
without committing, returning the resulting database for query. This enables
"what-if" analysis: "If I insert these 1000 orders, what would the dashboard
show?"

**Potential idea — Speculative Refresh:**

```sql
-- Hypothetical
SELECT * FROM pgtrickle.speculative_refresh(
  'sales_summary',
  $$INSERT INTO orders VALUES (1, 'widget', 100)$$
);
-- Returns: what sales_summary would look like after this INSERT + refresh
```

Implementation: Run in a transaction with SAVEPOINT, apply the DML, execute
differential refresh, return results, then ROLLBACK TO SAVEPOINT.

| Relevance | Effort | Impact |
|-----------|--------|--------|
| Low | L | Low — niche use case; interesting for BI/planning tools |

**Trade-off:** Locks and resource usage during speculative refresh could
interfere with real workloads. Better suited as a development/testing tool than
a production feature.

---

### 3.8  Datalog / Rule-Based Queries

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| Datalog with rules, negation, aggregation | SQL (comprehensive operator coverage) | Different |
| Rules compose naturally for recursive queries | Recursive CTEs with semi-naive evaluation | Partial |

**Gap analysis:**

This is a fundamental language difference, not a gap. pg_trickle's SQL coverage
(21 operators, recursive CTEs, window functions, lateral subqueries) exceeds
Datomic's query capabilities in many areas. Datomic's Datalog is more natural
for graph traversal and rule composition, but pg_trickle's target audience uses
SQL.

**No action recommended.** pg_trickle's SQL-first approach is correct for its
market. If graph query support becomes relevant, PostgreSQL's `RECURSIVE` CTEs
and Apache AGE extension cover most use cases.

| Relevance | Effort | Impact |
|-----------|--------|--------|
| N/A | — | — |

---

### 3.9  Entity-Attribute-Value (EAV) Model

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| Schema-flexible EAV (any entity, any attribute) | Relational (fixed schema per table) | Different |
| VAET index enables reverse-reference traversal | DAG tracks forward dependencies; no reverse-ref index | Partial |

**Gap analysis:**

EAV is Datomic's core data model; pg_trickle operates on relational tables.
These are fundamentally different paradigms. However, one specific EAV insight
is relevant:

**Potential idea — Reverse Dependency Index:**

Datomic's VAET index answers "what references entity X?" efficiently. pg_trickle's
DAG tracks "ST X depends on source Y" (forward), but the reverse query — "which
STs are affected by a change to table Y?" — requires scanning the full DAG.

A **reverse dependency index** (source_oid → [pgt_id, ...]) would enable:
- **O(1) impact analysis** when a source table changes.
- **Targeted NOTIFY** — only wake STs affected by the changed source.
- **Faster cascade detection** on DDL events.

| Relevance | Effort | Impact |
|-----------|--------|--------|
| High | S | Medium — already partially implemented via `pgt_dependencies`; formalizing the reverse index is cheap |

**Note:** The DAG module likely already builds this internally for topological
sort. Exposing it as a queryable structure (or caching in shmem) is the gap.

---

### 3.10  Adaptive Background Indexing

| Datomic Feature | pg_trickle Status | Gap? |
|-----------------|-------------------|------|
| Background merges index trees without blocking reads | Refresh blocks concurrent reads during MERGE | Partial |
| Sublinear indexing cost relative to DB size | MERGE cost is proportional to delta + target size | Partial |
| Writes never blocked by indexing | Trigger CDC adds per-row overhead (~2–15 μs) | Yes |

**Gap analysis:**

Datomic's indexing is non-blocking because index segments are immutable —
merging creates new segments without modifying existing ones. PostgreSQL's MVCC
provides some of this benefit (readers don't block writers), but pg_trickle's
MERGE step holds locks on the stream table.

**Potential idea — Double-Buffer Refresh:**

Maintain two physical tables per stream table: `_active` and `_staging`. Refresh
writes to `_staging`, then atomically swap via `ALTER TABLE ... RENAME` (or a
view redirect). Readers always see `_active` without lock contention.

| Relevance | Effort | Impact |
|-----------|--------|--------|
| Medium | L | High for read-heavy workloads — eliminates refresh-time read blocking |

**Trade-off:** Doubles storage per stream table. Only valuable for STs with
high read concurrency during refresh windows. Could be opt-in
(`WITH (double_buffer = true)`).

**Alternative:** For differential refresh, the MERGE already minimizes lock
duration (only rows being updated are locked). The double-buffer pattern is
more relevant for FULL refresh mode.

---

## 4  Summary Matrix

| # | Idea | Relevance | Effort | Impact | Priority |
|---|------|-----------|--------|--------|----------|
| 3.1 | Retained change log | High | M | High | **P1** |
| 3.5 | NOTIFY-based refresh events | High | S | High | **P1** |
| 3.9 | Reverse dependency index | High | S | Medium | **P1** |
| 3.6 | Global refresh sequence number | Medium | S | Medium | **P2** |
| 3.3 | Change buffer metadata columns | Medium | M | Medium | **P2** |
| 3.4 | Queryable pending deltas | Medium | L | Medium | **P3** |
| 3.10 | Double-buffer refresh | Medium | L | High | **P3** |
| 3.2 | Temporal stream table queries | Medium | XL | Medium | **P4** |
| 3.7 | Speculative refresh | Low | L | Low | **P5** |
| 3.8 | Datalog queries | N/A | — | — | — |

---

## 5  Recommended Next Steps

### Quick wins (P1 — could ship in v0.22.0 or v0.23.0)

1. **NOTIFY on refresh completion** — 20–30 lines of Rust in `refresh.rs`.
   Emit `pg_notify("pgtrickle_refreshed", json)` after each successful refresh.
   Enables real-time downstream consumers with zero infrastructure.

2. **Reverse dependency index** — Expose the reverse mapping from
   `pgt_dependencies` as a helper function or cached shmem structure. May
   already exist internally; formalize and document.

3. **Global refresh sequence** — Add a `refresh_seq BIGSERIAL` to
   `pgt_refresh_history` (or a standalone sequence) to provide total ordering
   across all stream tables.

### Medium-term (P2 — v0.23.0–v0.25.0)

4. **Retained change log** — Add optional TTL-based retention of consumed
   change buffer rows. Design the retention policy (row count, time window,
   per-ST opt-in) and storage format.

5. **Change buffer metadata** — Add `__pgt_tx_meta JSONB` column to change
   buffers, populated from `pg_trickle.tx_metadata` session variable.

### Long-term exploration (P3–P5)

6. **Queryable pending deltas** and **temporal queries** — Evaluate demand
   from real-world use cases before investing in implementation.

7. **Double-buffer refresh** — Profile lock contention in production workloads
   to determine whether this optimization is justified.

---

## 6  Key Insight

The most transferable idea from Datomic is not any single feature but the
**mindset shift**: treating the change stream as a durable, queryable,
first-class data structure rather than an ephemeral implementation detail.

pg_trickle already captures rich change data (typed columns, LSN ordering,
action codes, PK hashes, changed-column bitmasks). The gap is that this data
is discarded after each refresh cycle. Retaining it — even briefly — unlocks
debugging, replay, downstream integration, and temporal semantics with modest
storage overhead.

The second key insight is **reactivity**: Datomic's tx-report-queue turns a
database into an event source. PostgreSQL's LISTEN/NOTIFY provides the
mechanism; pg_trickle just needs to emit events at the right moments.
