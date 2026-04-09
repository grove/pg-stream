# pg_trickle on PGlite — Feasibility Report

Date: 2026-04-09
Status: RESEARCH

---

## 1. Executive Summary

**Can pg_trickle run on PGlite?** Not in its current form. The extension's
architecture fundamentally depends on PostgreSQL features that are unavailable in
PGlite's single-process WASM environment: background workers, shared memory, and
WAL-based logical replication. However, a meaningful **subset** — specifically the
IMMEDIATE (transactional IVM) mode — is architecturally compatible and could be
adapted for PGlite with significant but bounded engineering effort.

This document analyses the compatibility constraints, identifies what can and
cannot work, and proposes a phased plan to bring incremental view maintenance to
PGlite users.

---

## 2. What is PGlite?

[PGlite](https://pglite.dev/) is a WASM build of PostgreSQL packaged as a
TypeScript/JavaScript client library by ElectricSQL. It enables running a full
Postgres instance in the browser, Node.js, Bun, or Deno — under 3 MB gzipped.

### Key Characteristics

| Property | Value |
|----------|-------|
| **Architecture** | Postgres compiled to WASM via Emscripten |
| **Process model** | Single-process, single-connection ("single user mode") |
| **Forking** | Not supported (Emscripten limitation) |
| **Background workers** | Not supported |
| **Shared memory** | Not supported (no multi-process) |
| **WAL / Logical replication** | Not available |
| **Storage** | In-memory, filesystem (Node), or IndexedDB (browser) |
| **Extensions** | Supported via dynamic WASM loading (40+ available) |
| **PostgreSQL version** | 17.x (as of 2026) |
| **Live query support** | Built-in `live` extension with reactive queries |
| **Existing IVM** | `pg_ivm` available as bundled extension |

### PGlite Extension Ecosystem

PGlite supports loading Postgres extensions compiled to WASM. Extensions are
distributed as `.tar.gz` bundles containing the WASM code and any data files.
The extension development process requires:

1. **Backend:** Add the extension source to `postgres-pglite/pglite/other_extensions/`,
   build via the WASM toolchain.
2. **Frontend:** Create a TypeScript wrapper that points to the `.tar.gz` bundle.

Notable existing extensions: pgvector, PostGIS, pg_ivm, Apache AGE, pg_trgm.

### PGlite's Live Query Extension

PGlite already has a built-in `live` extension providing reactive queries:

- **`live.query()`** — Re-runs the full query when dependent tables change.
- **`live.incrementalQuery()`** — Materialises results and diffs against previous
  state; only transfers changed rows to JS.
- **`live.changes()`** — Emits INSERT/UPDATE/DELETE change events keyed by a
  primary key column.

This is conceptually similar to pg_trickle's FULL refresh mode but operates at
the JavaScript layer rather than inside the database engine. It does not perform
differential view maintenance — every change triggers a full re-evaluation of the
query.

---

## 3. pg_trickle Architecture vs PGlite Constraints

### 3.1 Incompatibility Matrix

| pg_trickle Feature | PGlite Support | Severity | Notes |
|---|---|---|---|
| **Background workers** (launcher, per-DB scheduler, dynamic refresh workers) | ❌ Not available | 🔴 CRITICAL | PGlite runs in single-process mode; cannot fork or spawn workers |
| **Shared memory** (PgLwLock, PgAtomic, pg_shmem_init!) | ❌ Not available | 🔴 CRITICAL | No multi-process coordination needed, but init code will fail |
| **WAL-based CDC** (logical replication slots, pg_logical_slot_get_changes) | ❌ Not available | 🔴 CRITICAL | No WAL infrastructure in single-user mode |
| **WAL LSN watermarking** (pg_current_wal_lsn() for frontier tracking) | ❌ Not available | 🔴 CRITICAL | Core to differential refresh sequencing |
| **Row-level CDC triggers** (change buffer tables) | ✅ Available | 🟢 OK | Triggers work; PL/pgSQL functions are supported |
| **Statement-level triggers with transition tables** | ✅ Available | 🟢 OK | REFERENCING NEW/OLD TABLE AS works in single-user mode |
| **IMMEDIATE mode** (transactional IVM via statement triggers) | ✅ Available | 🟢 OK | No background workers, no WAL, no shared memory needed |
| **DVM engine** (operator tree, delta SQL generation) | ✅ Available | 🟢 OK | Pure SQL generation from parse tree; no OS dependencies |
| **Query parser** (pgrx raw_parser FFI) | ⚠️ Needs WASM build | 🟡 MEDIUM | Requires pgrx → Emscripten WASM compilation |
| **DAG / dependency graph** | ✅ Available | 🟢 OK | Pure Rust computation; no OS dependencies |
| **Catalog tables** (pgtrickle schema) | ✅ Available | 🟢 OK | Standard CREATE TABLE / SPI |
| **GUC variables** | ⚠️ Partially relevant | 🟡 MEDIUM | Many GUCs control disabled features; subset still useful |
| **Advisory locks** (IVM concurrency control) | ⚠️ Single-connection | 🟡 MEDIUM | Only one connection, so locks are trivially held |
| **LISTEN/NOTIFY** | ✅ PGlite supports it | 🟢 OK | Used for scheduler wake; irrelevant without scheduler |
| **pgrx framework** | ❌ No WASM target | 🔴 CRITICAL | pgrx issue #2159 closed as "Not planned" |

### 3.2 The pgrx WASM Problem

pg_trickle is built on **pgrx 0.17.x**, which generates C-FFI bindings to
PostgreSQL internals (`pg_sys::*`). pgrx does not support compiling to WASM —
the upstream project explicitly closed a WASM support request
([pgcentralfoundation/pgrx#2159](https://github.com/pgcentralfoundation/pgrx/issues/2159))
as "Not planned."

This is the **single largest blocker**. Even features that are conceptually
compatible (like the DVM engine and query parser) cannot be loaded into PGlite
without solving the compilation problem.

**Possible approaches:**
1. **Cross-compile via Emscripten** — PGlite's extension build system compiles C
   extensions using Emscripten. Rust can target `wasm32-unknown-emscripten`, but
   pgrx's build toolchain (`cargo-pgrx`) assumes native compilation with
   `pg_config`. A custom build harness would be needed that bypasses
   `cargo-pgrx` and links directly against PGlite's WASM PostgreSQL headers.
2. **Extract pure-Rust components** — Factor the DVM engine, parser types, and
   DAG logic into a standalone Rust crate with no pgrx dependency. This crate
   compiles to WASM natively. The SPI/FFI layer is reimplemented as a thin
   PGlite-specific wrapper.
3. **Rewrite as C extension** — Port the IMMEDIATE mode subset to a C extension
   that PGlite's existing build system can handle directly. High effort, loses
   Rust safety guarantees.
4. **Pure SQL/PL/pgSQL reimplementation** — Implement the delta SQL generation
   logic in PL/pgSQL. No compilation needed; works on any PostgreSQL including
   PGlite. Very limited expressiveness and performance.

---

## 4. What Could Work: IMMEDIATE Mode Subset

pg_trickle's **IMMEDIATE** refresh mode is the most PGlite-compatible feature.
It uses statement-level AFTER triggers with transition tables to maintain stream
tables synchronously within the same transaction as base table DML. It requires:

- ✅ Statement-level triggers with `REFERENCING NEW TABLE / OLD TABLE`
- ✅ Temp tables for delta staging
- ✅ Standard DML (INSERT, DELETE, UPDATE) for delta application
- ✅ Advisory locks (trivial in single-connection)
- ❌ ~~Background workers~~ (not needed)
- ❌ ~~Shared memory~~ (not needed)
- ❌ ~~WAL / logical replication~~ (not needed)
- ❌ ~~Change buffer tables~~ (not needed — uses transition tables directly)
- ❌ ~~Scheduler~~ (not needed — triggers fire synchronously)

### 4.1 Feature Scope for PGlite

| Feature | IMMEDIATE on PGlite | Notes |
|---------|---------------------|-------|
| `create_stream_table(refresh_mode => 'IMMEDIATE')` | ✅ | Core API |
| `drop_stream_table()` | ✅ | Cleanup triggers + storage |
| `alter_stream_table()` | ⚠️ Partial | Query/status changes; schedule irrelevant |
| Inner joins (INNER, CROSS) | ✅ | DVM operator |
| Outer joins (LEFT, RIGHT, FULL) | ✅ | DVM operator |
| Aggregates (GROUP BY, COUNT, SUM, AVG, etc.) | ✅ | DVM operator |
| DISTINCT | ✅ | Count-based tracking |
| UNION ALL / INTERSECT / EXCEPT | ✅ | Set operators |
| Window functions | ✅ | Partition-based recomputation |
| Subqueries (EXISTS, NOT EXISTS, IN, scalar) | ✅ | Semi/anti/scalar operators |
| CTEs (non-recursive) | ✅ | Inline expansion or shared delta |
| CTEs (recursive, WITH RECURSIVE) | ✅ | Semi-naive / DRed |
| LATERAL subqueries | ✅ | Row-scoped recomputation |
| View inlining | ✅ | Auto-rewrite pass |
| DISTINCT ON, GROUPING SETS | ✅ | Auto-rewrite passes |
| Cascading (ST → ST) | ⚠️ Limited | Single-connection; triggers cascade naturally |
| Scheduled refresh | ❌ | No background workers |
| DIFFERENTIAL mode | ❌ | Requires CDC + WAL LSN tracking |
| FULL mode | ⚠️ Trivial | Just `REFRESH MATERIALIZED VIEW`-equivalent |
| Parallel refresh | ❌ | Single-process |
| CDC health monitoring | ❌ | No CDC |
| WAL transition | ❌ | No WAL |

### 4.2 Value Proposition for PGlite Users

PGlite's existing `live.incrementalQuery()` re-evaluates the **full query** on
every change and diffs at the JavaScript layer. pg_trickle's IMMEDIATE mode
would provide **true incremental view maintenance** inside the database engine:

| Aspect | PGlite `live.incrementalQuery` | pg_trickle IMMEDIATE |
|--------|-------------------------------|---------------------|
| Delta computation | Full query re-evaluation + JS diff | SQL-level delta from transition tables |
| Work on small change | O(full result set) | O(changed rows × join fan-out) |
| Complex queries | Any SQL | Any supported DVM operator |
| Aggregates | Full recomputation | Group-key-scoped recomputation |
| Joins | Full recomputation | Delta-join (change × other side's current state) |
| Data stays in DB | No (copied to JS for diff) | Yes (delta applied in-engine) |
| Latency | Higher for large results | Lower (proportional to change size) |

For interactive applications with large base tables and small incremental
changes (e.g., collaborative editors, dashboards, local-first apps), the
performance improvement could be **orders of magnitude**.

---

## 5. Implementation Strategies

### Strategy A: Factor Out Pure-Rust DVM Crate (Recommended)

**Effort: High (3–6 months) · Risk: Medium · Reward: High**

Extract the DVM engine, parser, and IMMEDIATE mode logic into a standalone Rust
crate (`pg_trickle_core`) that compiles to both native (for the full extension)
and `wasm32-unknown-emscripten` (for PGlite).

```
pg_trickle_core/         (new crate, no pgrx dependency)
├── dvm/
│   ├── parser/          (OpTree, Expr, Column types)
│   ├── operators/       (delta SQL generation per operator)
│   ├── diff.rs          (final diff SQL assembly)
│   └── validation.rs    (volatility checks, IVM support)
├── dag.rs               (dependency graph, topo sort)
├── rewrites.rs          (view inlining, DISTINCT ON, etc.)
└── types.rs             (shared type definitions)

pg_trickle/              (existing extension)
├── src/lib.rs           (depends on pg_trickle_core + pgrx)
├── src/api.rs           (SQL API, delegates to core)
└── ...

pg_trickle_pglite/       (new PGlite extension wrapper)
├── src/lib.c            (thin C shim calling core via FFI)
├── src/ivm.sql          (trigger setup, IMMEDIATE mode glue)
└── package.json         (PGlite extension definition)
```

**Key challenges:**
- The parser currently uses `pgrx::raw_parser()` (calls `pg_sys::raw_parser`).
  For PGlite, this can call PostgreSQL's built-in parser directly since the WASM
  build includes the full parser. Only the FFI bridge differs.
- SPI calls must be abstracted behind a trait so the core crate doesn't depend on
  pgrx's SPI implementation.
- Unsafe pg_sys node traversal must be wrapped in an abstraction layer.

**Benefits:**
- Core logic tested once, deployed twice.
- The native pg_trickle extension benefits from cleaner separation.
- PGlite gets the full DVM operator vocabulary.

### Strategy B: Pure SQL/PL/pgSQL Implementation

**Effort: Medium (2–3 months) · Risk: Low · Reward: Medium**

Implement a simplified IVM system entirely in PL/pgSQL, inspired by pg_trickle's
delta SQL patterns but without the Rust DVM engine.

```sql
-- Example: Create an IVM-maintained table
SELECT pgtrickle_lite.create_ivm_table(
  'order_totals',
  $$SELECT customer_id, SUM(amount) as total
    FROM orders GROUP BY customer_id$$
);

-- Triggers auto-created on `orders` table
-- Delta SQL auto-generated for aggregate recomputation
```

**Delta SQL generation** would be done at `CREATE` time by a PL/pgSQL function
that analyses the query (using `pg_parse_query()` or regexp-based heuristics)
and emits the trigger function body.

**Limitations:**
- Restricted operator support (joins, basic aggregates, filters).
- No auto-rewrite passes (no view inlining, no DISTINCT ON rewrite).
- No recursive CTEs, window functions, or LATERAL support.
- Harder to maintain parity with the full extension.

**Benefits:**
- Zero compilation; works on any PGlite version immediately.
- Could ship as a quick proof-of-concept.
- `pg_ivm` already exists on PGlite via this approach.

### Strategy C: PGlite Plugin (JavaScript Layer)

**Effort: Low (1–2 months) · Risk: Low · Reward: Low**

Instead of running inside PostgreSQL, implement delta computation in the PGlite
plugin layer (TypeScript). This extends the existing `live` extension:

```typescript
import { trickle } from '@pgtrickle/pglite';

const pg = await PGlite.create({
  extensions: { trickle },
});

// Creates a maintained view with IVM
await pg.trickle.createStreamTable(
  'order_totals',
  'SELECT customer_id, SUM(amount) FROM orders GROUP BY customer_id'
);

// Automatically maintained on every DML
await pg.query('INSERT INTO orders VALUES (1, 100)');
const results = await pg.query('SELECT * FROM order_totals');
```

Internally, the plugin would:
1. Parse the defining query to identify source tables and operator types.
2. Register AFTER triggers on source tables.
3. On trigger fire, compute deltas using SQL and apply them.

**Limitations:**
- Logic split between JS and SQL; harder to optimise.
- Cannot leverage PostgreSQL's internal parser as easily.
- Performance ceiling lower than in-engine approach.

**Benefits:**
- No WASM compilation needed for the extension itself.
- Leverages PGlite's existing plugin API directly.
- Fastest path to a working prototype.

---

## 6. Comparison with pg_ivm on PGlite

PGlite already ships `pg_ivm` (24.3 KB), a C extension providing incremental
view maintenance. Understanding its limitations clarifies pg_trickle's value add:

| Feature | pg_ivm | pg_trickle (IMMEDIATE subset) |
|---------|--------|-------------------------------|
| Refresh mode | Synchronous (IMMEDIATE only) | Synchronous (IMMEDIATE) |
| JOIN support | INNER only | INNER, LEFT, RIGHT, FULL, CROSS, SEMI, ANTI |
| Aggregates | COUNT, SUM, AVG, MIN, MAX | All standard + custom |
| DISTINCT | ❌ | ✅ (count-based tracking) |
| Window functions | ❌ | ✅ (partition recomputation) |
| Subqueries | ❌ | ✅ (EXISTS, NOT EXISTS, IN, scalar, LATERAL) |
| CTEs | ❌ | ✅ (non-recursive + recursive) |
| UNION/INTERSECT/EXCEPT | ❌ | ✅ |
| DISTINCT ON | ❌ | ✅ (auto-rewrite to window) |
| GROUPING SETS | ❌ | ✅ (auto-rewrite to UNION ALL) |
| View inlining | ❌ | ✅ (fixpoint expansion) |
| Self-joins | ❌ | ✅ |
| Outer joins | ❌ | ✅ |
| TABLESAMPLE | ❌ | ❌ |
| Row identity | ctid-based | Hash-based (__pgt_row_id) |

pg_trickle's DVM engine supports a **significantly wider** SQL surface area than
pg_ivm, making it valuable even as an IMMEDIATE-only PGlite extension.

---

## 7. Proposed Phased Plan

### Phase 0: Proof of Concept (Strategy C — JS Plugin)

**Goal:** Validate demand and basic approach.
**Scope:** PGlite plugin that intercepts DML via triggers and applies pre-computed
delta SQL for simple cases (single-table aggregates, two-table joins).
**Deliverable:** npm package `@pgtrickle/pglite-lite` with 3–5 supported patterns.
**Does NOT require:** WASM compilation, pgrx changes, or core refactoring.

### Phase 1: Core Extraction (Strategy A — Preparation)

**Goal:** Create `pg_trickle_core` crate with no pgrx dependency.
**Scope:**
- Extract `OpTree`, `Expr`, `Column`, `AggExpr` types into core crate.
- Extract operator delta SQL generation (all operators).
- Extract auto-rewrite passes.
- Extract DAG computation.
- Define `trait DatabaseBackend` for SPI/parser abstraction.
- All existing tests continue to pass via the pgrx backend implementation.

**Deliverable:** `pg_trickle_core` crate that compiles to `wasm32-unknown-emscripten`.

### Phase 2: PGlite Extension Build

**Goal:** Compile and load pg_trickle IMMEDIATE mode into PGlite.
**Scope:**
- Create C shim that bridges PGlite's Emscripten environment to `pg_trickle_core`.
- Implement `DatabaseBackend` for PGlite (direct SPI, built-in parser).
- Build WASM bundle via PGlite's extension toolchain.
- Create TypeScript wrapper with PGlite plugin API.
- IMMEDIATE mode: `create_stream_table`, `drop_stream_table`, `alter_stream_table`.

**Deliverable:** PGlite extension package `@pgtrickle/pglite`.

### Phase 3: Reactive Integration

**Goal:** Integrate with PGlite's live query ecosystem.
**Scope:**
- Bridge stream table changes to PGlite's `live.changes()` API.
- React/Vue hooks for stream table subscriptions.
- Documentation and examples for local-first app patterns.

**Deliverable:** Framework-integrated reactive stream tables.

---

## 8. Technical Risks & Open Questions

### Risks

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| **pgrx → WASM compilation fails** | Blocks Strategy A Phase 2 | Medium | Phase 1 extracts core without pgrx; C shim avoids pgrx entirely |
| **PGlite's Postgres version mismatch** | Parse tree incompatibility | Medium | PGlite tracks Postgres 17; pg_trickle targets 18. Node struct layouts may differ. |
| **Single-connection advisory lock semantics** | Subtle IVM bugs | Low | Locks are no-ops in single-connection; remove lock acquisition |
| **Statement-level trigger limitations in WASM** | Feature gap | Low | Triggers are core Postgres; should work unchanged |
| **Bundle size too large** | User adoption friction | Medium | pg_trickle_core compiled to WASM may be 1–5 MB; monitor and strip |
| **Performance regression in WASM** | Slower than native | High | WASM is ~1.5–3× slower than native; IVM overhead must stay below full-reeval threshold |

### Open Questions

1. **Does PGlite's single-user mode support `REFERENCING NEW TABLE AS ... OLD TABLE AS ...` in statement-level triggers?**
   This is the foundation of IMMEDIATE mode. Needs empirical testing.

2. **Can Rust `wasm32-unknown-emscripten` target link against PGlite's PostgreSQL WASM headers?**
   PGlite uses a custom PostgreSQL fork (`postgres-pglite`). The FFI boundary
   must match.

3. **What is the maximum reasonable WASM bundle size for a PGlite extension?**
   PostGIS is 8.2 MB; pgcrypto is 1.1 MB. pg_trickle should aim for < 2 MB.

4. **Is there demand for IVM in PGlite beyond what pg_ivm already provides?**
   pg_ivm is limited to INNER joins and basic aggregates. If PGlite users need
   outer joins, window functions, or subqueries, pg_trickle fills a real gap.

5. **Would ElectricSQL be interested in collaborating?**
   PGlite is developed by ElectricSQL. Their `live` extension already
   does naive reactivity; true IVM could be a significant upgrade to their
   offering.

---

## 9. Recommendations

1. **Do not attempt a full port.** The scheduler, WAL CDC, shared memory, and
   parallel refresh features are fundamentally incompatible with PGlite. Trying
   to stub them out would result in an unmaintainable fork.

2. **Start with Phase 0 (JS plugin proof-of-concept).** This validates demand
   with minimal investment and no core changes. If PGlite users adopt it,
   proceed to the full extraction.

3. **Invest in Phase 1 (core extraction) regardless.** Separating
   `pg_trickle_core` from pgrx improves the main extension's testability and
   modularity. It's worthwhile even if PGlite support is never shipped.

4. **Engage with ElectricSQL early.** Their cooperation on the WASM build
   toolchain and extension API stability is essential for Phase 2.

5. **Test PGlite's trigger infrastructure empirically.** Before committing to
   Phase 1, verify that statement-level triggers with transition tables work
   correctly in PGlite's single-user mode.

---

## 10. References

- [PGlite documentation](https://pglite.dev/docs/)
- [PGlite extension development guide](https://pglite.dev/extensions/development)
- [PGlite GitHub repository](https://github.com/electric-sql/pglite)
- [PGlite live queries](https://pglite.dev/docs/live-queries)
- [pg_ivm on PGlite](https://pglite.dev/extensions/#pg_ivm)
- [pgrx WASM issue #2159](https://github.com/pgcentralfoundation/pgrx/issues/2159) (closed, not planned)
- [postgres-pglite fork](https://github.com/electric-sql/postgres-pglite)
- [pg_trickle ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
- [pg_trickle IMMEDIATE mode plan](../sql/PLAN_TRANSACTIONAL_IVM.md)
