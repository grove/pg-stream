# Plan: User-Provided Watermarks for Cross-Source Gating

Date: 2026-03-02
Status: EXPLORATION
Last Updated: 2026-03-02

---

## 1. Problem Statement

### External Data Loading at Different Rates

pg_trickle's existing consistency mechanisms — diamond atomic groups
([PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md))
and cross-source snapshot consistency
([PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md](PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md))
— address problems whose root causes are **internal** to PostgreSQL: WAL LSN
ordering, snapshot boundaries, and structural DAG topology.

Neither mechanism addresses the case where data arrives from **external
systems** at different rates. An external process (ETL pipeline, API poller,
CDC connector) loads data into PostgreSQL tables, but pg_trickle has no way
to know *how far* each external source has been loaded. The WAL LSN tells
pg_trickle that rows were inserted — it does not say whether those rows
represent a complete picture up to some logical point in the external system.

### Motivating Example

An e-commerce platform fetches data from two APIs:

- **Orders API** — polled every 5 minutes, returns orders up to a timestamp.
- **Order Lines API** — polled every 10 minutes, returns line items up to a
  timestamp.

```
Orders API (every 5 min)      Order Lines API (every 10 min)
        │                              │
   INSERT INTO orders ...        INSERT INTO order_lines ...
        │                              │
        ▼                              ▼
    orders (base)               order_lines (base)
        │                              │
        ▼                              ▼
   order_summary (ST)          line_summary (ST)
        │                              │
        └──────────┬───────────────────┘
                   ▼
            order_report (D)
```

At 12:10, the pipeline has:
- Loaded orders up to 12:05.
- Loaded order lines up to 11:55 (the slower API).

Without watermarks, `order_report` may refresh and join `order_summary`
(reflecting orders through 12:05) with `line_summary` (reflecting lines
through 11:55). Orders placed between 11:55 and 12:05 appear in the report
without their corresponding line items — producing incorrect totals, missing
detail rows, or spurious NULL matches.

### Why Existing Mechanisms Don't Help

| Mechanism | Why insufficient |
|-----------|-----------------|
| **Diamond consistency** (atomic groups) | No shared PostgreSQL ancestor — `orders` and `order_lines` are independent base tables loaded by external processes. The DAG cannot structurally detect a diamond. |
| **Cross-source LSN watermark** (tick watermark) | LSN reflects *when* the row was written to PostgreSQL, not what logical time period the external data covers. Both sources may have the same WAL LSN range but represent different temporal windows in the external system. |
| **Cross-source REPEATABLE READ** (shared snapshot) | Ensures both STs see the same PostgreSQL snapshot, but the external temporal skew is already baked into the base table contents. A consistent PG snapshot of inconsistent external data is still inconsistent. |

### Core Insight

Only the external process knows "how far" each source has been loaded. This
information must flow **into** pg_trickle so it can make informed scheduling
decisions. The mechanism for this is a **user-provided watermark**: a
timestamp that the external process advances after each successful load,
signaling "this source's data is complete through time T."

---

## 2. Proposed Mechanism

### 2.1 Watermark Model

Each base table that receives data from an external process may have an
associated **watermark** — a `TIMESTAMPTZ` value representing the external
data-completeness boundary.

A **watermark group** declares that a set of watermarked sources must be
temporally aligned before certain stream tables are allowed to refresh.

```
┌──────────────────────────────────────────────────────────────────────┐
│  External Process                                                    │
│                                                                      │
│  1. Fetch orders from API up to T                                    │
│  2. INSERT INTO orders ...                                           │
│  3. SELECT pgtrickle.advance_watermark('orders', T)                  │
│                                                                      │
│  4. Fetch order_lines from API up to T'                              │
│  5. INSERT INTO order_lines ...                                      │
│  6. SELECT pgtrickle.advance_watermark('order_lines', T')            │
└──────────────────────────────────────────────────────────────────────┘
         │                                      │
         ▼                                      ▼
   watermark(orders) = T              watermark(order_lines) = T'
         │                                      │
         └──────── alignment check ─────────────┘
                          │
            if |T - T'| <= tolerance → allow gated STs to refresh
            else                     → skip gated STs this tick
```

### 2.2 Watermark Value Type

**Open question.** The watermark type determines expressiveness and
comparison semantics.

#### Option A — TIMESTAMPTZ

Timestamps are natural for API-sourced data. Most external systems expose
data with a temporal dimension ("orders created before T"). Comparison and
ordering are native PostgreSQL operations.

| Pros | Cons |
|------|------|
| Natural for API/event data with server-side timestamps | Assumes external data has a meaningful timestamp dimension |
| PostgreSQL-native comparison and arithmetic | Clock skew across external sources must be managed by the user |
| Tolerance expressed as `INTERVAL` — intuitive | Not meaningful for non-temporal batch numbering ("batch 47") |
| Composable with `data_timestamp` on STs | |

#### Option B — Monotonic BIGINT (batch/epoch number)

A simple counter incremented by the external process after each load cycle.
Both sources use the same counter namespace.

| Pros | Cons |
|------|------|
| Simple, universal — works for any load pattern | No inherent temporal meaning — "batch 47" says nothing about time |
| No clock skew concerns | Tolerance must be expressed in batch counts, which is harder to reason about |
| Unambiguous ordering | External process must coordinate a shared counter across sources |

#### Option C — Support both (polymorphic)

Store the watermark as a tagged union or two optional columns. The
comparison function dispatches based on type.

| Pros | Cons |
|------|------|
| Maximum flexibility | More complex catalog schema and comparison logic |
| Users choose what fits their pipeline | Mixing types within a group must be prohibited |
| | Additional validation and error surface |

#### Option D — Opaque string with user-supplied ordering

The watermark is a `TEXT` value. The user registers an ordering function.

| Pros | Cons |
|------|------|
| Fully extensible | pg_trickle cannot compute tolerance or lag without the custom function |
| | Fragile — user-supplied SQL functions in the scheduler hot path |
| | Over-engineered for the common case |

**No decision made.** Leaning toward Option A (TIMESTAMPTZ) for the common
case, with Option B as a future extension if demand materializes.

---

## 3. Injection API

### 3.1 Explicit SQL Function Call

The external process calls a SQL function after each load batch:

```sql
SELECT pgtrickle.advance_watermark('orders', '2026-03-01T12:05:00Z'::timestamptz);
```

**Semantics:**

- **Monotonic:** Rejects watermarks that go backward (`new < current`),
  raises an error. Re-advancing to the same value is a no-op.
- **Transactional:** The advancement is part of the caller's transaction. If
  the external process wraps the data load and watermark advancement in a
  single `BEGIN ... COMMIT`, the watermark only becomes visible when the data
  does — preventing the scheduler from seeing a watermark that refers to
  data not yet committed.
- **Signal:** Notifies the scheduler that watermark state has changed (via a
  lightweight shared-memory signal or `pg_notify`), so gating decisions are
  re-evaluated on the next tick.

### 3.2 Alternative: Magic Column on Source Table

A reserved column (e.g. `__pgt_watermark`) on the source table. The CDC
trigger reads the maximum value per batch and uses it as the implicit
watermark.

| Pros | Cons |
|------|------|
| Automatic — no extra function call needed | Invasive — requires column on user tables |
| Row-level granularity | CDC trigger overhead increases (per-row max tracking) |
| | Not all rows carry a meaningful watermark (e.g. dimension tables) |
| | Harder to implement atomically (partial batches) |

### 3.3 Alternative: Separate Watermark Table

The external process writes directly to a pg_trickle-managed table:

```sql
INSERT INTO pgtrickle.pgt_watermarks (source_relid, watermark)
VALUES ('orders'::regclass, '2026-03-01T12:05:00Z')
ON CONFLICT (source_relid) DO UPDATE SET watermark = EXCLUDED.watermark;
```

| Pros | Cons |
|------|------|
| No function call overhead | Bypasses monotonicity enforcement |
| Simple for users familiar with DML | No transactional signal to the scheduler |
| | Direct catalog writes are fragile and not API-stable |

**The explicit function call (§3.1) is the primary candidate.** It provides
clean semantics (monotonicity, transactional visibility, signaling) without
schema changes to user tables.

---

## 4. Watermark Groups

### 4.1 Purpose

A watermark group declares that a set of watermarked sources are
**semantically coupled** — their data should only be consumed together when
their watermarks are sufficiently aligned.

```sql
SELECT pgtrickle.create_watermark_group(
    'order_pipeline',
    sources   => ARRAY['orders', 'order_lines'],
    tolerance => '0 seconds'::interval
);
```

### 4.2 Tolerance

The `tolerance` parameter defines the maximum allowed temporal skew between
the most-advanced and least-advanced watermark in the group:

$$\max(W_i) - \min(W_i) \leq \tau$$

- `'0 seconds'` (default): strict alignment — all sources must report the
  exact same watermark value.
- `'30 seconds'`: allows up to 30 seconds of skew. Useful when APIs have
  known propagation delays or slightly different clock bases.

### 4.3 Catalog

**Open question:** Exact catalog design depends on the watermark value type
decision (§2.2). Sketch for TIMESTAMPTZ:

```sql
-- Per-source watermark state
CREATE TABLE pgtrickle.pgt_watermarks (
    source_relid   OID PRIMARY KEY,
    watermark      TIMESTAMPTZ NOT NULL,
    advanced_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    advanced_by    TEXT        -- session_user / application_name
);

-- Watermark groups
CREATE TABLE pgtrickle.pgt_watermark_groups (
    group_id       SERIAL PRIMARY KEY,
    group_name     TEXT UNIQUE NOT NULL,
    source_relids  OID[] NOT NULL,
    tolerance      INTERVAL NOT NULL DEFAULT '0 seconds',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

### 4.4 Introspection

```sql
-- Current watermark state per source
SELECT * FROM pgtrickle.watermarks();

-- Group definitions
SELECT * FROM pgtrickle.watermark_groups();

-- Live alignment status: per-group lag, gating state, effective watermark
SELECT * FROM pgtrickle.watermark_status();
```

`watermark_status()` would return something like:

| group_name | min_watermark | max_watermark | lag | aligned | effective_watermark |
|------------|---------------|---------------|-----|---------|---------------------|
| order_pipeline | 2026-03-01 11:55 | 2026-03-01 12:05 | 00:10:00 | false | 2026-03-01 11:50 |

where `effective_watermark` is the last watermark value at which the group
was aligned and downstream STs were allowed to refresh.

---

## 5. Gating Strategy

This is the most consequential design decision in the plan. The question:
**where in the DAG should watermark gating be enforced, and what does "gated"
mean for a given ST?**

### 5.1 Option 1 — Gate Only at Convergence Points

Only STs that transitively depend on **multiple** sources from the same
watermark group are gated. Intermediate STs that depend on a single
watermarked source refresh freely.

```
orders ──→ order_summary ──┐
                           ├──→ order_report (GATED)
order_lines ──→ line_summary ──┘
```

`order_summary` and `line_summary` refresh on every tick. `order_report` is
skipped when watermarks are misaligned.

| Pros | Cons |
|------|------|
| Minimal impact — only convergence STs are affected | Intermediate STs may contain data beyond the aligned watermark |
| Intermediates stay as fresh as possible | When `order_report` refreshes, `order_summary` may have rows from 12:05 while the aligned watermark is 11:55 — the join includes "extra" rows from one side |
| Simple to implement — gating check only at multi-source fan-in | For row-level consistency (e.g. only show orders where lines exist), this is insufficient |
| Backward compatible — no change to single-source STs | |

### 5.2 Option 2 — Gate at Intermediate STs (Hold-Back)

Any ST whose transitive source set includes a watermarked source can be
configured to **hold back** — only consuming changes up to the group's
effective watermark, even if more data is available.

```
orders ──→ order_summary (HOLD-BACK to effective_watermark) ──┐
                                                              ├──→ order_report
order_lines ──→ line_summary (HOLD-BACK to effective_watermark) ──┘
```

Both `order_summary` and `line_summary` cap their refresh to the aligned
watermark. When `order_report` refreshes, its inputs are guaranteed to
reflect the same temporal window.

| Pros | Cons |
|------|------|
| Strongest guarantee — every ST in the pipeline reflects the same external temporal window | Intermediates are artificially stale — data is in the base table but not yet in the ST |
| Row-level consistency — no "extra" rows in any ST | More complex implementation — requires capping the change window during refresh |
| Convergence point sees perfectly aligned inputs | Requires a mechanism to filter changes by external watermark (not just WAL LSN) |
| | If only one leg of the pipeline queries the intermediate ST directly, staleness is unwanted |

**Implementation challenge:** The current CDC pipeline captures changes by
WAL LSN range. Hold-back requires filtering by the *external* watermark,
which means the external timestamp must be either:
- (a) Stored per-row in the change buffer (e.g. an additional column on
  `pgtrickle_changes.changes_<oid>`), or
- (b) Correlated with the WAL LSN at ingestion time (the external process
  loads data in watermark-ordered batches, so all rows for watermark T have
  LSN ≤ some threshold — and that threshold is recorded alongside the
  watermark).

Option (b) is simpler: when `advance_watermark('orders', T)` is called, the
function also records `pg_current_wal_insert_lsn()` alongside `T`. This
creates a mapping `T → max_lsn`. Hold-back then caps the CDC consumption to
`max_lsn` for that source, which the existing frontier machinery already
supports.

### 5.3 Option 3 — Per-ST Configurable Gating Mode

Rather than a global strategy, each ST declares its own gating behavior.
This provides maximum flexibility: some STs in the pipeline hold back, while
others refresh freely.

```sql
-- This intermediate ST holds back to the effective watermark
SELECT pgtrickle.alter_stream_table('order_summary',
    watermark_gating => 'hold_back'
);

-- This intermediate ST refreshes freely (default)
SELECT pgtrickle.alter_stream_table('line_summary',
    watermark_gating => 'none'
);

-- The convergence ST is gated (skips when misaligned)
SELECT pgtrickle.alter_stream_table('order_report',
    watermark_gating => 'gate'
);
```

**Gating modes per ST:**

| Mode | Behavior |
|------|----------|
| `'none'` | Default. ST refreshes normally, ignoring watermarks. |
| `'gate'` | ST is skipped when any overlapping watermark group is misaligned. No change to what data it reads — it simply doesn't refresh. |
| `'hold_back'` | ST refreshes, but caps its change window to the group's effective watermark (via the LSN mapping described in §5.2). |

This is the most expressive option. A user who only cares about the final
report can use `'gate'` on the leaf. A user who needs row-level consistency
throughout the pipeline can apply `'hold_back'` at every level.

| Pros | Cons |
|------|------|
| Maximum flexibility — users place gating exactly where needed | More configuration surface — users must understand three modes |
| Composable — different STs in the same pipeline can have different policies | Per-ST setting must be validated against the DAG (e.g. `'hold_back'` without a watermark group is meaningless) |
| Subsumes Options 1 and 2 as special cases | Interactions between modes across a DAG may be hard to reason about |
| Can evolve incrementally — ship `'gate'` first, add `'hold_back'` later | |

### 5.4 Discussion

Option 3 (per-ST configurable) is the most general but also the most complex.
A pragmatic path may be to implement `'gate'` mode first (equivalent to
Option 1), then add `'hold_back'` as a follow-up when the LSN-mapping
infrastructure is in place.

The key open questions:

1. **Is gate-only-at-convergence sufficient for the orders/order_lines use
   case?** If users only query `order_report` (the leaf), then yes — they
   never see misaligned data. But if they also query `order_summary`
   directly and expect it to stay in sync with `line_summary`, they need
   hold-back or an additional gate.

2. **Should the default gating mode be inferred from DAG position?** For
   example, convergence STs could default to `'gate'` when they belong to a
   watermark group, while intermediates default to `'none'`. This reduces
   configuration burden while still allowing overrides.

3. **What happens to a `'hold_back'` ST when one source in its watermark
   group has *never* had a watermark advanced?** The effective watermark is
   undefined. Proposed behavior: treat as `effective_watermark = NULL`,
   which means the ST cannot refresh via differential and falls back to
   current behavior (no cap). The ST is ungated until all sources in
   the group have reported at least one watermark.

---

## 6. Effective Watermark Tracking

When a gated ST successfully refreshes, we record the watermark value that
was used for the gating decision:

$$W_{\text{eff}}(D) = \min_{s \in \text{group}} W_s$$

This `effective_watermark` value:
- Advances monotonically.
- Is stored alongside the existing `frontier` / `data_timestamp` in the
  catalog (either as a new column on `pgt_stream_tables` or as a field in
  the `frontier` JSONB).
- Provides observability: "this ST's data represents the external world as
  of $W_{\text{eff}}$."
- For `'hold_back'` STs, `effective_watermark` constrains the change window
  ceiling.

### Relationship to Existing data_timestamp

`data_timestamp` records when the ST's contents are logically consistent
from PostgreSQL's perspective (the DVS guarantee). `effective_watermark`
adds an **external** temporal dimension: the ST's contents are consistent
with the external world up to this point.

A stream table could have:
- `data_timestamp = 2026-03-01 12:10:00` (refreshed at 12:10 PG time)
- `effective_watermark = 2026-03-01 11:55:00` (external data complete
  through 11:55)

Both are useful for different audiences: `data_timestamp` for operational
monitoring, `effective_watermark` for business-logic correctness.

---

## 7. Composability with Existing Plans

The three consistency mechanisms form composable layers:

| Layer | Problem | Mechanism | Detectable? |
|-------|---------|-----------|-------------|
| Diamond consistency | Same-source split at fan-in | SAVEPOINT atomic groups | Auto (DAG structure) |
| Cross-source snapshot | Independent PG sources, different snapshots | REPEATABLE READ / LSN watermark | Partially (user-declared groups) |
| **User watermarks** | **External sources loaded at different rates** | **User-injected watermark + gating** | **No — requires user declaration** |

All three can apply simultaneously to the same ST:

```sql
-- 1. Watermark group: external data completeness
SELECT pgtrickle.create_watermark_group('order_pipeline',
    sources   => ARRAY['orders', 'order_lines'],
    tolerance => '0 seconds'
);

-- 2. Refresh group: PG snapshot coherence + atomicity
SELECT pgtrickle.create_refresh_group('order_views',
    members   => ARRAY['order_summary', 'line_summary', 'order_report'],
    isolation => 'repeatable_read'
);

-- 3. Diamond consistency (auto-detected if applicable, e.g. if both
--    STs share a common PG-internal source in addition to the external
--    pipeline)
SET pg_trickle.diamond_consistency = 'atomic';
```

Result for `order_report`:
1. **External completeness** — gated until orders and order_lines watermarks
   align.
2. **PG snapshot coherence** — all three STs see the same PostgreSQL
   snapshot.
3. **Atomicity** — if any member fails, all roll back.

### Interaction Matrix

| Scenario | Diamond | Cross-Source | Watermark | Behavior |
|----------|---------|-------------|-----------|----------|
| Single PG source, diamond DAG | Active | N/A | N/A | Atomic group refresh |
| Multiple PG sources, no external | N/A | Active | N/A | Shared snapshot or LSN watermark |
| External sources, no PG diamond | N/A | N/A | Active | Gating on external watermarks |
| External + PG diamond | Active | Optional | Active | All three layers compose |
| External sources + PG snapshot | N/A | Active | Active | Watermark gating + REPEATABLE READ |

---

## 8. Extended Use Cases

### A. Nightly Batch ETL with Daily Watermarks

```sql
-- End of nightly load for 2026-03-01
BEGIN;
  COPY orders FROM '/data/orders_20260301.csv';
  SELECT pgtrickle.advance_watermark('orders', '2026-03-01');
COMMIT;

BEGIN;
  COPY order_lines FROM '/data/lines_20260301.csv';
  SELECT pgtrickle.advance_watermark('order_lines', '2026-03-01');
COMMIT;

-- order_report refreshes on the next tick after both watermarks reach 2026-03-01
```

### B. Streaming Micro-Batches with Tolerance

```sql
SELECT pgtrickle.create_watermark_group(
    'realtime_pipeline',
    sources   => ARRAY['trades', 'quotes'],
    tolerance => '5 seconds'   -- quotes API has ~3s propagation lag
);

-- External process advances watermarks every few seconds
-- trades may be at 12:00:05, quotes at 12:00:02 — within tolerance, D refreshes
```

### C. Multiple Independent Pipelines

```sql
SELECT pgtrickle.create_watermark_group('order_pipeline',
    sources => ARRAY['orders', 'order_lines']);
SELECT pgtrickle.create_watermark_group('inventory_pipeline',
    sources => ARRAY['stock_levels', 'warehouses']);

-- A ST spanning both groups must satisfy both alignment predicates.
-- Each group is evaluated independently.
```

### D. Mixed Internal + External Sources

A stream table joins an API-loaded table with a PG-native table:

```sql
-- External API data
SELECT pgtrickle.advance_watermark('fx_rates', T);

-- PG-native orders table (no watermark needed — CDC handles it)
-- Stream table joins both:
SELECT pgtrickle.create_stream_table('order_totals_usd',
    'SELECT o.id, o.amount * fx.rate AS amount_usd
     FROM orders o JOIN fx_rates fx ON o.currency = fx.currency');
```

The watermark group only includes `fx_rates`. `orders` is tracked normally
via CDC. The gating ensures `fx_rates` has been loaded to a known point
before `order_totals_usd` refreshes — but does not gate on `orders` (which
is always as fresh as the last CDC tick).

---

## 9. Open Questions

1. **Watermark value type:** TIMESTAMPTZ vs BIGINT vs polymorphic. See §2.2.
   Timestamp is the natural fit for the motivating use case but may not
   generalize. Should we support both with a tagged union, or pick one and
   extend later?

2. **Gating strategy:** Per-ST configurable (§5.3) vs convergence-only
   (§5.1). Can we ship `'gate'` mode first and add `'hold_back'` later
   without breaking the API? What is the default for a ST that belongs to
   a watermark group?

3. **Hold-back implementation:** If hold-back is supported, should the LSN
   mapping (`watermark T → max WAL LSN`) be stored in the `pgt_watermarks`
   table, or in a separate mapping table that records each advancement's
   LSN? The latter supports range queries ("what LSN corresponds to
   watermark T?") but adds storage.

4. **Watermark group membership validation:** Should a source be allowed in
   multiple watermark groups? Use case: `orders` participates in both an
   `order_pipeline` group and an `audit_pipeline` group. Seems valid, but
   complicates the "which gating applies to ST X?" resolution.

5. **Scheduler signal weight:** Should `advance_watermark()` trigger a
   full `DAG_REBUILD_SIGNAL`, or a lighter-weight signal that only
   re-evaluates gating predicates without rebuilding the DAG? The DAG
   structure doesn't change when a watermark advances — only the
   gating arithmetic changes.

6. **Interaction with manual refresh:** If a user calls
   `pgtrickle.refresh_stream_table('order_report')`, should watermark
   gating be enforced? Arguments for: consistency guarantee should not be
   bypassable. Arguments against: manual refresh implies "I know what I'm
   doing."

7. **Stale watermark alerting:** If a watermark hasn't advanced in N
   minutes (configurable), should pg_trickle emit a warning? This helps
   detect broken ETL pipelines.

8. **Watermark group and tolerance changes:** If a user changes the
   tolerance on a live group from `'0 seconds'` to `'30 seconds'`, should
   previously-gated STs immediately become eligible for refresh on the next
   tick? Probably yes, but worth documenting.

9. **Bootstrap:** When `advance_watermark()` is called for a source that
   already has a populated stream table (i.e. the ST was created and
   initially populated before watermarks were introduced), what is the
   effective watermark for historical data? Proposed: `NULL` (unset) until
   the first explicit advancement — meaning gating does not apply until all
   sources in a group have reported at least one watermark.

10. **Per-ST vs per-group tolerance:** The current design puts tolerance on
    the watermark group. Should individual STs be able to override the
    group's tolerance (e.g. a less-critical dashboard ST allows more skew
    than a financial reporting ST)?

---

## 10. Sketch Implementation Steps

> These steps are preliminary and subject to change based on the decisions
> above. They assume TIMESTAMPTZ watermarks, explicit function call
> injection, and per-ST configurable gating.

### Step 1 — Catalog: `pgt_watermarks` + `pgt_watermark_groups`

Add tables (§4.3), `Watermark` and `WatermarkGroup` structs, and CRUD
helpers in `catalog.rs`.

### Step 2 — `advance_watermark()` SQL function

`#[pg_extern(schema = "pgtrickle")]` in `api.rs`. Monotonicity check,
store `pg_current_wal_insert_lsn()` alongside the watermark (for future
hold-back support), transactional semantics, lightweight signal to
scheduler.

### Step 3 — `create_watermark_group()` / `drop_watermark_group()`

Validation (sources exist, are in `pgt_dependencies` for at least one ST),
catalog insert, DAG signal.

### Step 4 — Gating pre-check in scheduler

For each ST in topological order, before refresh:
1. Compute transitive source set.
2. Find overlapping watermark groups.
3. Check alignment predicate.
4. If gated, skip with logged reason.

Respects per-ST `watermark_gating` mode (`'none'`, `'gate'`, `'hold_back'`).

### Step 5 — Effective watermark tracking

Store `effective_watermark` in catalog on successful gated refresh.

### Step 6 — Introspection functions

`watermarks()`, `watermark_groups()`, `watermark_status()` in `api.rs`.

### Step 7 — (Future) Hold-back mode

Cap change window using the LSN mapping from Step 2. Requires extending
the frontier machinery in `refresh.rs` to accept a ceiling LSN from the
watermark system rather than always using `pg_current_wal_insert_lsn()`.

### Step 8 — Tests

| Test | Type | What it proves |
|------|------|----------------|
| `test_advance_watermark_monotonic` | Unit | Backward watermark rejected |
| `test_advance_watermark_idempotent` | Unit | Same value is no-op |
| `test_watermark_group_alignment_check` | Unit | Alignment predicate with tolerance |
| `test_watermark_gating_blocks_downstream` | E2E | Gated ST skips on misalignment |
| `test_watermark_gating_allows_after_alignment` | E2E | Gated ST refreshes after alignment |
| `test_watermark_tolerance` | E2E | ST refreshes within tolerance, blocks beyond |
| `test_watermark_transactional` | E2E | Watermark not visible until caller commits |
| `test_watermark_intermediate_refreshes_freely` | E2E | Non-gated intermediates are unaffected |
| `test_watermark_multiple_groups` | E2E | ST spanning two groups satisfies both |
| `test_watermark_hold_back` | E2E | (Future) Intermediate caps change window |

### Step 9 — Documentation

- `docs/SQL_REFERENCE.md`: `advance_watermark()`, `create_watermark_group()`,
  `watermark_gating` parameter.
- `docs/CONFIGURATION.md`: any GUCs if added.
- `docs/tutorials/`: "Loading External Data with Watermarks" tutorial.
- `CHANGELOG.md`.

---

## 11. Prior Art

1. **Apache Flink Watermarks** — Flink's event-time processing uses
   watermarks to track progress through an event stream. A watermark
   $W(t)$ asserts that no events with timestamp $\leq t$ will arrive.
   Downstream operators (windows, joins) hold state until the watermark
   advances. Direct inspiration for this plan's gating model.

2. **Kafka Streams Timestamp Extraction** — Kafka Streams assigns
   timestamps to records via `TimestampExtractor`. Stream-time advances
   only when timestamps advance. Late records are handled by configurable
   grace periods — analogous to our tolerance parameter.

3. **Google Dataflow / Apache Beam** — Beam's watermark model distinguishes
   *input watermark* (how far the source has progressed) from *output
   watermark* (how far the operator has emitted). The hold-back concept in
   §5.2 corresponds to an operator that advances its output watermark only
   as fast as the minimum input watermark across all sources.

4. **Materialize Source Timestamps** — Materialize assigns timestamps to
   records at ingestion based on their source. A `SINCE` frontier tracks
   the minimum timestamp that can still be queried. Multi-source queries
   wait until all sources have advanced past a timestamp before producing
   output at that time — functionally equivalent to our watermark gating.

5. **dbt Freshness Tests** — dbt's `source freshness` checks assert that
   a source table has been loaded within an expected window. This is
   observability-only (it doesn't gate downstream model execution). Our
   watermark mechanism goes further by using the freshness signal to
   actually control refresh scheduling.

---

## 12. Relationship to Other Plans

| Plan | Relationship |
|------|--------------|
| [PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md) | Orthogonal. Diamonds are structural (shared PG ancestor). Watermarks are semantic (shared external temporal domain). Both can apply to the same ST. |
| [PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md](PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md) | Complementary. Cross-source snapshots ensure PG-level consistency. Watermarks ensure external-level consistency. The LSN watermark (Approach C) can be composed with user watermark gating. |
| [PLAN_HYBRID_CDC.md](PLAN_HYBRID_CDC.md) | The WAL-based CDC mode's LSN tracking is compatible with watermark gating. The `advance_watermark()` function already records `pg_current_wal_insert_lsn()`, which works regardless of whether the source uses trigger or WAL CDC. |
| [PLAN_CIRCULAR_REFERENCES.md](PLAN_CIRCULAR_REFERENCES.md) | A circular dependency involving a watermark-gated ST would need careful consideration — the watermark gating decision could interfere with the fixed-point iteration. Likely requires treating the SCC as a single gating unit. |
