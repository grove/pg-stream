# Plan: Make Refresh Mode Selection Optional with Sensible Default

**Status:** In Progress  
**Author:** Copilot  
**Date:** 2026-03-04  
**Updated:** 2026-03-06

---

## 1. Motivation

Today, `create_stream_table` exposes `refresh_mode` as a prominent
fourth positional parameter:

```sql
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) FROM orders GROUP BY region',
    '1m',
    'DIFFERENTIAL'   -- do users really need to think about this?
);
```

While the parameter already defaults to `'DIFFERENTIAL'`, its position in
the function signature and its prominence in docs/examples implies the user
**should** make an active choice between FULL, DIFFERENTIAL, and IMMEDIATE.
In practice:

- **DIFFERENTIAL** is the right choice for ~95% of stream tables. It
  applies delta-only updates when change volume is small, and adaptively
  falls back to FULL when the change ratio exceeds `pg_trickle.differential_max_change_ratio`
  (default 30%). It is strictly superior to FULL except in degenerate cases.
- **FULL** is only beneficial when the defining query is cheaper to recompute
  from scratch than to diff (e.g., trivially small source tables, or queries
  the DVM engine cannot differentiate). Even then, DIFFERENTIAL's adaptive
  fallback covers this automatically.
- **IMMEDIATE** is a fundamentally different execution model (synchronous,
  in-transaction) and should only be used when sub-second latency is
  required at the cost of write-path overhead.

Forcing (or implying) that users choose a mode adds cognitive overhead,
increases onboarding friction, and leads to suboptimal choices (users
picking FULL "to be safe" when DIFFERENTIAL would be faster).

---

## 2. Current State

### API signature

```sql
pgtrickle.create_stream_table(
    name            text,
    query           text,
    schedule        text   DEFAULT '1m',
    refresh_mode    text   DEFAULT 'DIFFERENTIAL',  -- already defaults
    initialize      bool   DEFAULT true,
    diamond_consistency   text DEFAULT NULL,
    diamond_schedule_policy text DEFAULT NULL
) → void
```

### How DIFFERENTIAL is adaptive today

The differential refresh path (`execute_differential_refresh` in
`src/refresh.rs`) already implements adaptive fallback:

1. **TRUNCATE detection** — if a source table was truncated, fall back to
   FULL immediately.
2. **Change-ratio threshold** — before running the delta query, count
   changes per source table. If `change_count / table_size` exceeds
   `pg_trickle.differential_max_change_ratio` (GUC, default 0.30), fall
   back to FULL. Per-ST override via `auto_threshold`.
3. **TopK scoped recomputation** — TopK tables always use a MERGE-based
   recompute strategy regardless of mode.

This means DIFFERENTIAL already behaves like "auto" — use delta when
efficient, fall back to full recompute when not.

### Documentation & examples

Most SQL reference examples and tutorials explicitly pass
`'DIFFERENTIAL'` or `'FULL'`, reinforcing the idea that the user must
choose.

---

## 3. Design

### 3.1 Philosophy

**Refresh mode should be an optimization knob, not a required decision.**

Users should be able to write:

```sql
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) FROM orders GROUP BY region'
);
```

…and get the best behavior automatically. Advanced users can override when
they have specific needs.

### 3.2 API changes

No signature change is needed — `refresh_mode` already defaults to
`'DIFFERENTIAL'`. The change is purely **documentation, examples, and
messaging**:

| Area | Change |
|---|---|
| SQL Reference | Move `refresh_mode` out of the "basic usage" examples. Show the 2-argument form as the primary example. |
| Getting Started | Remove `refresh_mode` from the quickstart. Introduce it later in an "Advanced Configuration" section. |
| Tutorials | Use the minimal form in all beginner tutorials. Only mention `refresh_mode` in the performance-tuning tutorial. |
| FAQ | Add "Do I need to choose a refresh mode?" → "No. The default (DIFFERENTIAL) is adaptive and works well for almost all queries." |
| `alter_stream_table` | No change — `refresh_mode` is already optional (NULL = keep current). |
| Error messages | When DVM parsing fails for a query in DIFFERENTIAL mode, downgrade to FULL automatically with an INFO message instead of rejecting the query. |

### 3.3 Auto-downgrade for non-differentiable queries

Today, if the user creates a DIFFERENTIAL stream table with a query that
the DVM engine cannot differentiate (e.g., unsupported constructs), the
CREATE fails with an error. This forces users to understand the DVM's
limitations and explicitly choose FULL.

**Proposed behavior:** When `refresh_mode` is the default (DIFFERENTIAL)
and DVM parsing fails, automatically downgrade to FULL and emit an INFO:

```
INFO: Query uses constructs not supported by differential maintenance;
      using FULL refresh mode. See docs/DVM_OPERATORS.md for supported operators.
```

This keeps the zero-config promise: *any valid SELECT works without
choosing a mode.* Users who explicitly pass `'DIFFERENTIAL'` still get
the error (they asked for it specifically and should know why it fails).

Implementation detail — distinguish "user explicitly passed DIFFERENTIAL"
from "used the default" by checking whether the parameter was provided.
Since pgrx `default!()` doesn't expose this, we have two options:

**Option A: Sentinel value.** Change the default to `'AUTO'` and treat it as
DIFFERENTIAL-with-fallback:

```sql
refresh_mode text DEFAULT 'AUTO'
```

```rust
match mode_str.to_uppercase().as_str() {
    "AUTO" => {
        // Try DIFFERENTIAL; fall back to FULL if DVM rejects query
    }
    "DIFFERENTIAL" => { /* strict: error on DVM failure */ }
    "FULL" => { /* always full */ }
    "IMMEDIATE" => { /* IVM triggers */ }
}
```

**Option B: Nullable mode.** Change to `Option<&str>` with default NULL
meaning "auto":

```sql
refresh_mode text DEFAULT NULL   -- NULL = auto (DIFFERENTIAL with FULL fallback)
```

**Recommendation: Option A** (`'AUTO'`). It is self-documenting, backward
compatible (existing `'DIFFERENTIAL'` calls retain strict behavior), and
requires no schema migration for the catalog column.

### 3.4 Catalog representation

The `pgt_stream_tables.refresh_mode` column stores the **resolved** mode:

| User specifies | DVM parse succeeds | Stored mode |
|---|---|---|
| `'AUTO'` (default) | Yes | `DIFFERENTIAL` |
| `'AUTO'` (default) | No | `FULL` |
| `'DIFFERENTIAL'` | Yes | `DIFFERENTIAL` |
| `'DIFFERENTIAL'` | No | Error (rejected) |
| `'FULL'` | n/a | `FULL` |
| `'IMMEDIATE'` | Yes | `IMMEDIATE` |
| `'IMMEDIATE'` | No | Error (rejected) |

The catalog never stores `'AUTO'` — it's resolved at creation time. This
means `alter_stream_table` and the scheduler don't need to know about AUTO;
they see only FULL / DIFFERENTIAL / IMMEDIATE.

### 3.5 `alter_stream_table` with query change

When ALTER changes the query (per PLAN_ALTER_QUERY.md), the same auto-
downgrade logic applies: if the user doesn't specify a new `refresh_mode`,
the current mode is **re-evaluated** against the new query. If the ST was
DIFFERENTIAL but the new query isn't differentiable, it downgrades to FULL
with an INFO message.

If the user explicitly passes `refresh_mode => 'DIFFERENTIAL'` alongside
the query change, the strict behavior applies (error on DVM failure).

---

## 4. Documentation Rewrite

### 4.1 Primary example (SQL Reference)

**Before:**
```sql
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    '2m',
    'DIFFERENTIAL'
);
```

**After:**
```sql
-- Minimal: just name and query. Refreshes every minute using adaptive
-- differential maintenance.
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region'
);

-- With custom schedule:
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    '2m'
);
```

### 4.2 New FAQ entry

> **Q: Do I need to choose a refresh mode?**
>
> No. The default mode is adaptive: it uses differential (delta-only)
> maintenance when efficient, and automatically falls back to full
> recomputation when the change volume is high or the query cannot be
> differentiated. This works well for the vast majority of queries.
>
> You only need to specify a mode explicitly when:
> - You want **FULL** mode to force recomputation every time (rare).
> - You want **IMMEDIATE** mode for sub-second, in-transaction updates
>   (adds overhead to every write on source tables).
> - You want strict **DIFFERENTIAL** mode and prefer an error over silent
>   fallback when the query isn't differentiable.

### 4.3 Getting Started simplification

The quickstart guide should use the 2-argument form throughout, deferring
refresh mode to an "Advanced: Refresh Modes" section.

---

## 5. Implementation Steps

### Step 1: Add AUTO mode parsing ✅
**File:** `src/dag.rs`  
**Status:** Complete (2026-03-06)

Added `"AUTO"` branch in `RefreshMode::from_str()` that resolves to
`RefreshMode::Differential`. Added `RefreshMode::is_auto_str()` helper
to detect when the user passed AUTO vs an explicit mode. Unit tests added
for both.

### Step 2: Auto-downgrade in create_stream_table_impl ✅
**File:** `src/api.rs`  
**Status:** Complete (2026-03-06)

Three auto-downgrade points in `create_stream_table_impl`:
1. `reject_unsupported_constructs()` failure → downgrade to FULL with INFO
2. `reject_materialized_views()` failure → downgrade to FULL with INFO
3. `parse_defining_query_full()` failure → downgrade to FULL with INFO

When `is_auto` is false (user explicitly passed DIFFERENTIAL), errors
propagate as before.

### Step 3: Change default parameter value ✅
**File:** `src/api.rs`  
**Status:** Complete (2026-03-06)

Changed `default!(&str, "'DIFFERENTIAL'")` to `default!(&str, "'AUTO'")`.

### Step 4: Update SQL upgrade script
**File:** `sql/pg_trickle--0.2.1--0.3.0.sql` (or current version)  
**Status:** Deferred — no catalog migration needed since AUTO is never
persisted. The new default only affects the pgrx-generated function
signature, which is recreated on `CREATE EXTENSION` / `ALTER EXTENSION
UPDATE`.

### Step 5: Update documentation ✅
**Files:** `docs/SQL_REFERENCE.md`, `docs/GETTING_STARTED.md`, `docs/FAQ.md`  
**Status:** Complete (2026-03-06)

- SQL_REFERENCE.md: updated signature default and parameter table.
- GETTING_STARTED.md: removed explicit `refresh_mode =>` from examples,
  updated refresh modes table to include AUTO.
- FAQ.md: added "Do I need to choose a refresh mode?" entry.

### Step 6: Update dbt materialization ✅
**File:** `dbt-pgtrickle/macros/materializations/stream_table.sql`  
**Status:** Complete (2026-03-06)

Changed default from `'DIFFERENTIAL'` to `'AUTO'`.

### Step 7: E2E Tests
**Files:** `tests/e2e_*_tests.rs`  
**Status:** Not started

| Test | Scenario | Priority |
|---|---|---|
| `test_create_auto_mode_differentiable` | AUTO + differentiable query → stored as DIFFERENTIAL | P1 |
| `test_create_auto_mode_not_differentiable` | AUTO + non-differentiable query → stored as FULL, INFO emitted | P1 |
| `test_create_explicit_differential_not_differentiable` | Explicit DIFFERENTIAL + non-differentiable → error | P1 |
| `test_create_no_mode_specified` | Omit refresh_mode entirely → defaults to AUTO behavior | P2 |
| `test_alter_query_auto_downgrade` | ALTER changes query to non-differentiable → downgrade to FULL | P2 |
| `test_backward_compat_differential` | Explicit `'DIFFERENTIAL'` still works identically | P2 |
| `test_backward_compat_full` | Explicit `'FULL'` still works identically | P2 |

---

## 6. Remaining Work (Prioritized)

| Priority | Task | Effort |
|---|---|---|
| P1 | E2E tests for AUTO mode (Step 7 — P1 tests) | ~1h |
| P2 | E2E tests for backward compat (Step 7 — P2 tests) | ~1h |
| P3 | Tutorial docs update (`docs/tutorials/*.md`) | ~30min |
| P3 | SQL Reference examples — reduce `refresh_mode =>` repetition | ~30min |

---

## 7. Backward Compatibility

| Scenario | Impact |
|---|---|
| Existing `create_stream_table(..., 'DIFFERENTIAL')` calls | No change — explicit DIFFERENTIAL retains strict behavior |
| Existing `create_stream_table(..., 'FULL')` calls | No change |
| Existing `create_stream_table(..., 'IMMEDIATE')` calls | No change |
| New calls omitting `refresh_mode` | Was DIFFERENTIAL (strict), now AUTO (with fallback). Strictly more permissive — queries that previously failed now succeed with FULL mode |
| Catalog data | No migration needed — AUTO is never stored |
| `pg_trickle.differential_max_change_ratio` GUC | Unchanged — still governs adaptive fallback at runtime |

---

## 7. Alternatives Considered

### A. Keep DIFFERENTIAL as default, no AUTO

**Pros:** Simpler. No new mode name.  
**Cons:** Users still hit errors when their query isn't differentiable.
They must understand DVM limitations to pick FULL. The "zero-config"
promise is broken.

### B. Silently downgrade DIFFERENTIAL too (not just AUTO)

**Pros:** Even simpler — no distinction between explicit and default.  
**Cons:** Violates principle of least surprise. If a user explicitly
requests DIFFERENTIAL, they expect differential behavior and should be
told when it's not possible.

### C. Remove FULL mode entirely

**Pros:** Simplest API.  
**Cons:** Some users legitimately want to force full recompute (e.g.,
for debugging, for queries where the delta query is pathologically
slow). FULL remains useful as an escape hatch.

---

## 9. Milestones

| Milestone | Steps | Status |
|---|---|---|
| M1: Core implementation | Steps 1–3 | ✅ Complete |
| M2: Documentation | Steps 5–6 | ✅ Complete |
| M3: E2E tests | Step 7 | Not started |
