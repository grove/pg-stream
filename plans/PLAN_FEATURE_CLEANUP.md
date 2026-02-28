# PLAN: Feature Cleanup — Remove Low-Value Surface Before Public Release

**Status:** In Progress
**Target milestone:** v0.2.0
**Last updated:** 2026-02-27

---

## Motivation

Before the first public release accepts external users, every GUC, SQL
function, and status value becomes a compatibility commitment. This plan
identifies surface to remove or consolidate now — while there are zero
external users — to keep the API simple, honest, and maintainable.

Items are sourced from direct code audit of `config.rs`, `api.rs`,
`monitor.rs`, `refresh.rs`, and `scheduler.rs`. This plan does **not**
include features that were speculated about but verified not to exist
(ON COMMIT REFRESH, named schedules, per-table delta_threshold,
stream_table_definition, force_full_refresh — none of these were built).

---

## Items

---

### C1 — Remove `pg_stream.merge_strategy` GUC entirely

**Priority:** P0 — remove before public release
**Effort:** 1–2 hours
**Location:** `config.rs`, `refresh.rs`

#### Background

`merge_strategy` accepts three values: `auto`, `merge`, `delete_insert`.

- `delete_insert` is being removed (F1 — unsafe, superseded by `auto`).
- After removing `delete_insert`, the only remaining values are `auto` and
  `merge`.
- `auto` uses MERGE for all deltas except when the estimated delta exceeds
  ~25% of the stream table row count, at which point it switches to
  DELETE+INSERT. Since DELETE+INSERT is being removed, `auto` will
  **always use MERGE** — making it identical to `merge`.
- There is no remaining scenario where a user would get a different
  behaviour from `merge` vs `auto`.

#### Options

**A. Keep `merge_strategy` but reduce to a single `auto` value**
- ✅ Technically harmless; GUC still validates the value
- ❌ A GUC that accepts one value is confusing and adds documentation debt
- ❌ The name "merge_strategy" implies there are strategies to choose from

**B. Remove `merge_strategy` GUC entirely**
- ✅ Eliminates a GUC that provides zero choice after `delete_insert` removal
- ✅ Simplifies `refresh.rs`: removes the strategy-selection branch
- ✅ Simplifies `config.rs`: removes one GUC registration
- ✅ Users who set `pg_stream.merge_strategy = 'merge'` or `'auto'` get an
  unknown-GUC warning (harmless) rather than silent no-op behaviour
- ❌ Breaking change for any existing configuration — acceptable at pre-1.0

**Decision:** Option B — remove `merge_strategy` GUC entirely.

**Status: DONE ✅**

#### Implementation

1. ~~Remove `PGS_MERGE_STRATEGY` from `config.rs`~~ ✅
2. ~~Remove `pg_stream_merge_strategy()` accessor~~ ✅
3. ~~Remove `use_delete_insert` branch in `refresh.rs`~~ ✅
4. ~~Remove `delete_insert_template` and `delete_insert_sql` fields from the
   refresh cache entry struct~~ ✅
5. Update docs and GUC documentation table ✅

---

### C2 — Fix `SUSPENDED` status: implement missing `resume_stream_table()`

**Priority:** P0 — referenced in error message but function does not exist
**Effort:** 1–2 hours
**Location:** `api.rs` line 500, `scheduler.rs`

#### Background

When a stream table accumulates `pg_stream.max_consecutive_errors`
consecutive refresh failures, the scheduler sets its status to `SUSPENDED`
and fires an `auto_suspended` NOTIFY alert. The error message in
`refresh_stream_table()` tells the user:

```
"stream table {}.{} is suspended; use pgstream.resume_stream_table() first"
```

**`pgstream.resume_stream_table()` does not exist as a `pg_extern`
function.** The user is told to call a function that doesn't exist.

This is a bug, not a feature to remove. It reveals that the SUSPENDED
lifecycle is half-implemented: the scheduler can set the state, but there
is no user-facing way to clear it and re-enable refreshes other than
directly mutating the catalog table.

#### Options

**A. Remove SUSPENDED status and auto-suspension logic**
- ✅ Eliminates the inconsistency
- ✅ Simplifies the scheduler — no error-count tracking or state transitions
- ❌ Removes a safety mechanism: without auto-suspension, a table with a
  permanent error (e.g., dropped source column) will retry indefinitely,
  consuming background worker cycles and filling the log
- ❌ Users lose NOTIFY alerting for persistent failures

**B. Implement `pgstream.resume_stream_table(name text)` function**
- ✅ Makes the documented lifecycle complete and consistent
- ✅ Fixes the dangling error message
- ✅ Auto-suspension remains as a safety valve for persistent errors
- ✅ Small, clean implementation: reset `status = 'ACTIVE'`,
  `consecutive_errors = 0` in catalog, emit a NOTIFY
- ❌ Adds one more SQL function to the public API (minor)

**Decision:** Option B — implement `resume_stream_table()`. The function is
already promised by the error message; this just fulfils that promise.

**Status: DONE ✅**

#### Implementation

```rust
#[pg_extern(schema = "pgstream")]
fn resume_stream_table(name: &str) {
    // Parse schema.name, look up in catalog, assert status == SUSPENDED,
    // reset status = ACTIVE + consecutive_errors = 0, emit NOTIFY.
}
```

---

### C3 — Remove `pg_stream.max_concurrent_refreshes` GUC (or mark as no-op)

**Priority:** P1 — documents a capability that does not exist
**Effort:** 30 min (document) or 2 hours (remove)
**Location:** `config.rs`, `scheduler.rs`

#### Background

`pg_stream.max_concurrent_refreshes` (default: 4, max: 32) implies the
scheduler runs multiple refreshes in parallel. It does not. The scheduler
processes stream tables sequentially in topological order within a single
background worker (G8.5 in SQL_GAPS_7.md). The GUC is read but has no
effect on behaviour.

#### Options

**A. Document as "reserved for future use" in GUC description**
- ✅ Zero code change
- ❌ A GUC that does nothing is misleading; users will set it expecting
  an effect and see none
- ❌ Will need to remain backward-compatible once documented

**B. Remove the GUC entirely until parallel refresh is implemented (v0.3.0)**
- ✅ Eliminates false advertising
- ✅ Pre-1.0: no compatibility cost
- ✅ Re-add when the feature is real (P2/P3 in v0.3.0)
- ❌ Minor churn: add back in v0.3.0

**C. Hard-code to 1 and remove the GUC**
- Same as B, just more explicit about the current behaviour

**Decision:** Option A — document `max_concurrent_refreshes` as "reserved for
future use" in the GUC description. No code change; update the description
string in `config.rs` to make it clear the value is not yet honoured.

**Status: DONE ✅**

---

### C4 — Consolidate `pg_stream.merge_work_mem_mb` and `pg_stream.merge_planner_hints`

**Priority:** P2 — low user value, high documentation burden
**Effort:** 1–2 hours
**Location:** `config.rs`, `refresh.rs`

#### Background

Two GUCs control planner hint injection before MERGE execution:

- `pg_stream.merge_planner_hints` (bool, default: true) — enables injection
  of `SET LOCAL enable_nestloop = off` and `SET LOCAL work_mem = '<N>MB'`
- `pg_stream.merge_work_mem_mb` (int, default: 64) — sets the `work_mem`
  injected when delta ≥ 10,000 rows

These exist because PostgreSQL's planner sometimes chooses nested-loop
plans for medium/large delta MERGEs, causing P95 latency spikes. The
solution is correct but exposes two internal tuning knobs.

#### Options

**A. Keep both GUCs as-is**
- ✅ Maximum operator control
- ❌ Two GUCs that 99% of users will never adjust
- ❌ `merge_work_mem_mb` is only meaningful when `merge_planner_hints = true`
  — the interdependency is confusing

**B. Remove `merge_work_mem_mb`, hard-code a sensible value**
- ✅ Reduces to one GUC (`merge_planner_hints` on/off)
- ✅ The 64 MB default is well-chosen; there is no evidence any user needs
  to tune this independently
- ❌ Removes one escape hatch for edge cases with unusually large deltas
- ✅ Users who genuinely need a different `work_mem` can set it at the
  session/role level

**C. Remove both GUCs, always inject hints**
- ✅ Simplest — the hints are always beneficial; no reason to disable
- ❌ Removes the ability to disable hints if they cause regressions on
  specific PostgreSQL minor versions or hardware configurations
- ❌ More risk than benefit

**Decision:** Option A — keep both `pg_stream.merge_work_mem_mb` and
`pg_stream.merge_planner_hints` as-is. Maximum operator control is preserved
for users who need to tune MERGE performance on large deltas.

**Status: No action required ✅**

---

### C5 — Simplify `pg_stream.user_triggers` GUC values

**Priority:** P3 — minor API surface simplification
**Effort:** 1 hour
**Location:** `config.rs`, `refresh.rs`

#### Background

`pg_stream.user_triggers` accepts: `auto` (default), `on`, `off`.

- `auto`: detect user-defined row-level triggers on the stream table and
  use explicit DML (DELETE + UPDATE + INSERT) so they fire correctly
- `on`: always use explicit DML, even if no user triggers exist
- `off`: always use MERGE; user triggers will NOT fire correctly

The `on` value is only useful if trigger detection is unreliable. If
detection is reliable (which it is — pg_stream checks `pg_trigger`
directly), `on` is redundant with `auto`.

The `off` value explicitly disables trigger compatibility, which means
user triggers silently fire with wrong `TG_OP` / `OLD` / `NEW` — a
footgun.

#### Options

**A. Keep all three values**
- ✅ Maximum flexibility
- ❌ `on` is redundant with `auto`; `off` is a footgun

**B. Remove `on` (redundant with `auto`) and rename `off` to
`pg_stream.user_triggers = false` (boolean GUC)**
- ✅ Cleaner: a boolean "respect user triggers yes/no" is clearer than
  three string values
- ✅ Auto-detection is reliable enough that `on` adds no value
- ❌ Minor API change

**C. Remove `off` entirely, keep `auto` and `on` as `true`/`false`**
- ✅ Eliminates the footgun (`off` silently breaks user triggers)
- ❌ Removes an escape hatch for the rare case where explicit DML has
  unacceptable overhead and the user knowingly has no triggers

**Decision:** Defer to v0.3.0. The value difference between `on` and `auto`
is small. Document the footgun risk of `off` in the GUC description. Not
worth the churn before v0.2.0.

---

### C6 — Hide WAL-mode GUCs until WAL CDC is production-ready

**Priority:** P3 — reduces false impression of production readiness
**Effort:** 30 min (rename/comment)
**Location:** `config.rs`

#### Background

`pg_stream.wal_transition_timeout` is visible to users, implying that WAL
CDC is a normal operational mode to configure. It is not — WAL CDC has
three P1 correctness issues (F2, F3, F4 in SQL_GAPS_7.md) and is not
production-ready until v0.3.0.

#### Options

**A. Prefix WAL-mode GUCs with `_experimental_` or `_unsafe_`**
- ✅ Signals that these are not stable
- ❌ Ugly; PostgreSQL GUC convention doesn't use such prefixes

**B. Keep GUCs but add prominent warnings in documentation and GUC
description strings**
- ✅ No API change
- ✅ GUC descriptions are shown by `SHOW pgstream.wal_transition_timeout`
- ✅ The existing GUC description can say explicitly: "WAL CDC is not
  production-ready in v0.2.0; see docs."

**C. Remove or hide WAL-mode GUCs until v0.3.0**
- ✅ Cleanest signal of "not ready"
- ❌ `pg_stream.cdc_mode = 'auto'` (the default) will attempt WAL
  transition after a successful refresh — the GUC is needed to control
  the timeout even in pre-production

**Decision:** Option B — update GUC description strings to explicitly state
WAL CDC is pre-production in v0.2.0. No code change.

**Status: DONE ✅**

---

## Summary

| Item | Action | Priority | Effort | Status |
|------|--------|----------|--------|--------|
| C1 | Remove `merge_strategy` GUC (entire) | P0 | 1–2h | ✅ Done |
| C2 | Implement missing `resume_stream_table()` | P0 | 1–2h | ✅ Done |
| C3 | Document `max_concurrent_refreshes` as reserved | P1 | 30 min | ✅ Done |
| C4 | Keep both merge GUCs as-is | P2 | — | ✅ No-op |
| C5 | Simplify `user_triggers` GUC | P3 | Defer to v0.3.0 | ⏳ Deferred |
| C6 | Document WAL GUCs as pre-production | P3 | 30 min | ✅ Done |

**Total for v0.2.0 (C1–C3 + C6):** completed.

---

## What Does Not Exist (Speculative List Corrections)

The following items were considered but verified to not exist in the
codebase and therefore require no removal:

| Feature | Status |
|---------|--------|
| `ON COMMIT REFRESH` clause | Never built |
| `pgstream.force_full_refresh()` separate function | Never built — full refresh is `refresh_stream_table(name, force := true)` |
| Per-table `delta_threshold` override | Never built |
| `pgstream.stream_table_definition()` DDL reconstruction | Never built |
| Named/reusable refresh schedules | Never built |
| Separate `pgstream.pause_stream_table()` function | Never built — suspension is automatic only |
| Per-operator timing breakdowns | Never built — `explain_st()` exposes the operator tree but not timings |
