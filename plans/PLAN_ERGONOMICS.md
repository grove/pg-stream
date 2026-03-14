# PLAN_ERGONOMICS — Developer & User Ergonomics Improvements

## Status

**All five original tasks implemented** (2026-03-05). Three v0.4.0 ergonomic-hardening
items implemented 2026-03-11. See git history for details.

| Task | Status |
|------|--------|
| Task 1 — `'calculated'` keyword + default change | ✅ Done |
| Task 2 — Lower `min_schedule_seconds` default to 1 | ✅ Done |
| Task 3 — Add `default_schedule_seconds` GUC | ✅ Done |
| Task 4 — Remove diamond GUCs | ✅ Done |
| Task 5 — Add TOC to SQL_REFERENCE.md and CONFIGURATION.md | ✅ Done |
| ERG-B — Warn at `_PG_init` when `cdc_mode='auto'` but `wal_level != 'logical'` | ✅ Done |
| ERG-C — Warn at `create_stream_table` when source has no PRIMARY KEY | ✅ Done (pre-existing) |
| ERG-F — Emit `WARNING` when `alter_stream_table` triggers implicit full refresh | ✅ Done |
| ERG-D — Record manual `refresh_stream_table()` in pgt_refresh_history with `initiated_by='MANUAL'` | ✅ Done |
| ERG-E — `pgtrickle.quick_health` view (single-row status summary) | ✅ Done |

### Remaining follow-up ✅ (completed 2026-03-14)

- ✅ E2E tests for `'calculated'` as schedule input (verify `NULL` input is rejected) — ERG-T1
- ✅ E2E test that `SHOW pg_trickle.diamond_consistency` returns an error — ERG-T2
- ✅ Update CHANGELOG.md with breaking behavioural changes (default schedule, NULL rejection) — ERG-T5
- ✅ E2E test for ERG-B warning (assert WARNING appears in server log when `wal_level != logical`) — ERG-T4
- ✅ E2E test for ERG-F warning (assert WARNING in client output when `alter_stream_table` with `query =>`) — ERG-T3

---

## Overview

Five focused changes to make pg_trickle friendlier for new users while keeping
full flexibility for production use.

**No catalog schema migration required.** `'calculated'` is accepted/displayed
as a parse-time alias for `NULL`; internal storage stays `NULL`.

---

## Decisions Made

| Topic | Decision |
|---|---|
| `'calculated'` storage | Accepted/rejected at parse time; stored as `NULL` — no SQL migration needed |
| `create_stream_table` default | Change from `'1m'` to `'calculated'` (breaking behavioral change; document in CHANGELOG) |
| `default_schedule_seconds` default | `1` (matches new `min_schedule_seconds` default) |
| Diamond GUC fallbacks | Hardcoded to `'atomic'` / `'fastest'` in Rust (EC-13 set `'atomic'` as correct default); per-table params in `create/alter_stream_table` are kept |

---

## Task 1 — Replace `NULL` with `'calculated'` as the schedule keyword

**Files:** [src/api.rs](../src/api.rs), [docs/SQL_REFERENCE.md](../docs/SQL_REFERENCE.md)

### Background

Currently, passing `schedule => NULL` to `create_stream_table` activates
CALCULATED mode (schedule derived from downstream dependents). `NULL` is
unintuitive — `'calculated'` is explicit and self-documenting.

`NULL` is kept in catalog storage (no migration), but is no longer accepted as
SQL input. The alter sentinel (no-change) stays as SQL `NULL` internally.

### Steps

1. **Input pre-processing** — In `create_stream_table` and
   `alter_stream_table`, before calling `parse_schedule` / `validate_schedule`,
   add a check:
   - If `schedule` input is `Some("calculated")` → convert to `None` (CALCULATED
     mode).
   - If `schedule` input is `None` (SQL `NULL`) on a **create** call → return
     error: *"use 'calculated' instead of NULL to set CALCULATED schedule"*.
   - `alter_stream_table`'s own sentinel for "no change" remains `NULL`
     internally and is handled before the pre-processing step.

2. **Change `create_stream_table` SQL default** — Change the `schedule`
   parameter default from `"'1m'"` to `"'calculated'"` (around line 35 of
   [src/api.rs](../src/api.rs)).

3. **Update docs** — In [docs/SQL_REFERENCE.md](../docs/SQL_REFERENCE.md),
   replace all `NULL` schedule examples with `'calculated'` and add a note
   that `NULL` is no longer valid as schedule input.

---

## Task 2 — Lower `pg_trickle.min_schedule_seconds` default to 1

**Files:** [src/config.rs](../src/config.rs),
[docs/CONFIGURATION.md](../docs/CONFIGURATION.md)

### Background

The current default of `60` is appropriate for production but makes local
development and testing slow and awkward. New users hitting "minimum 60 seconds"
immediately is a bad first experience. Production operators know what they are
doing and can raise this explicitly.

### Steps

4. In [src/config.rs](../src/config.rs), change the default value of
   `PGS_MIN_SCHEDULE_SECONDS` from `60` to `1`.

5. Update [docs/CONFIGURATION.md](../docs/CONFIGURATION.md): change the
   documented default from `60` to `1`.

---

## Task 3 — Introduce `pg_trickle.default_schedule_seconds` GUC

**Files:** [src/config.rs](../src/config.rs), [src/api.rs](../src/api.rs),
[src/scheduler.rs](../src/scheduler.rs),
[docs/CONFIGURATION.md](../docs/CONFIGURATION.md)

### Background

`min_schedule_seconds` currently plays two distinct roles:

1. **Floor** (`validate_schedule`): rejects schedules shorter than this value.
2. **Default** (`resolve_calculated_schedule`): isolated CALCULATED stream
   tables (no downstream dependents) fall back to this as their effective
   refresh interval.

These should be separate GUCs so each can be tuned independently.

### Current caller map

| Location | Current role |
|---|---|
| `src/api.rs` ~L1786 `validate_schedule()` | Floor — keep using `min_schedule_seconds` |
| `src/api.rs` ~L1321 `build_from_catalog()` | Default — switch to `default_schedule_seconds` |
| `src/api.rs` ~L2247 `build_from_catalog()` | Default — switch to `default_schedule_seconds` |
| `src/scheduler.rs` ~L395 `build_from_catalog()` | Default — switch to `default_schedule_seconds` |

### Steps

6. In [src/config.rs](../src/config.rs), add a new GUC static:
   ```rust
   static PGS_DEFAULT_SCHEDULE_SECONDS: GucSetting<i32> = GucSetting::<i32>::new(1);
   ```
   - Default: `1`
   - Range: `1`–`86400`
   - Context: `SUSET`
   - Description: *"Default effective schedule (in seconds) for isolated
     CALCULATED stream tables that have no downstream dependents."*
   Register it alongside the existing GUCs.

7. Add accessor:
   ```rust
   pub fn pg_trickle_default_schedule_seconds() -> i32 {
       PGS_DEFAULT_SCHEDULE_SECONDS.get()
   }
   ```

8. In [src/api.rs](../src/api.rs) at ~L1321 and ~L2247, replace:
   ```rust
   config::pg_trickle_min_schedule_seconds()
   ```
   with:
   ```rust
   config::pg_trickle_default_schedule_seconds()
   ```
   when passing `fallback_schedule_secs` to `StDag::build_from_catalog()`.

9. In [src/scheduler.rs](../src/scheduler.rs) at ~L395, same replacement.

10. The `validate_schedule()` call at ~L1786 continues to use
    `pg_trickle_min_schedule_seconds()` as the **floor** — no change there.

11. Update [docs/CONFIGURATION.md](../docs/CONFIGURATION.md): add a new GUC
    section for `pg_trickle.default_schedule_seconds`.

---

## Task 4 — Remove `pg_trickle.diamond_consistency` and `pg_trickle.diamond_schedule_policy` GUCs

**Files:** [src/config.rs](../src/config.rs), [src/api.rs](../src/api.rs),
[src/scheduler.rs](../src/scheduler.rs),
[tests/e2e_diamond_tests.rs](../tests/e2e_diamond_tests.rs),
[docs/CONFIGURATION.md](../docs/CONFIGURATION.md)

### Background

The GUC defaults (`'none'` and `'fastest'`) are sensible for all practical
use cases, already match the SQL column `DEFAULT` values in [src/lib.rs](../src/lib.rs),
and are already the values asserted by the diamond E2E tests. Exposing them as
settable GUCs adds API surface without meaningful benefit — users who need
non-default values can specify them per stream table via `create_stream_table`
/ `alter_stream_table` parameters, which are **not** removed.

### Current GUC usage

| Location | GUC | Replace with |
|---|---|---|
| `src/api.rs` ~L80 | `pg_trickle_diamond_consistency()` | hardcoded `"none"` |
| `src/api.rs` ~L94 | `pg_trickle_diamond_schedule_policy()` | `DiamondSchedulePolicy::Fastest` |
| `src/scheduler.rs` ~L679 | `pg_trickle_diamond_schedule_policy()` | `DiamondSchedulePolicy::Fastest` |

### Steps

12. In [src/config.rs](../src/config.rs), remove:
    - `PGS_DIAMOND_CONSISTENCY` and `PGS_DIAMOND_SCHEDULE_POLICY` statics
    - Their GUC registrations
    - Accessor functions `pg_trickle_diamond_consistency()` and
      `pg_trickle_diamond_schedule_policy()`

13. In [src/api.rs](../src/api.rs) at ~L80 and ~L94, replace the GUC accessor
    calls with hardcoded defaults:
    - `"none"` for `diamond_consistency`
    - `DiamondSchedulePolicy::Fastest` (or equivalent string `"fastest"`) for
      `diamond_schedule_policy`

14. In [src/scheduler.rs](../src/scheduler.rs) at ~L679, replace the GUC
    accessor call with the hardcoded `DiamondSchedulePolicy::Fastest` value.

15. The SQL column `DEFAULT 'none'` and `DEFAULT 'fastest'` in
    [src/lib.rs](../src/lib.rs) (~L119–122) already match — no change needed.

16. In [tests/e2e_diamond_tests.rs](../tests/e2e_diamond_tests.rs), scan for
    any tests that explicitly **set** these GUCs via
    `SET pg_trickle.diamond_consistency` or reference the GUC names, and
    update those to use per-table params instead. Tests that only check
    per-table default values (e.g. `test_diamond_consistency_default`,
    `test_diamond_schedule_policy_default`) should continue to pass unchanged.

17. Update [docs/CONFIGURATION.md](../docs/CONFIGURATION.md):
    - Remove the two GUC sections.
    - Update the "fifteen configuration variables" count in the Overview.

---

## Task 5 — Add table of contents to SQL_REFERENCE.md and CONFIGURATION.md

**Files:** [docs/SQL_REFERENCE.md](../docs/SQL_REFERENCE.md),
[docs/CONFIGURATION.md](../docs/CONFIGURATION.md)

### Steps

18. In [docs/SQL_REFERENCE.md](../docs/SQL_REFERENCE.md): add a markdown TOC
    after the `# SQL Reference` heading, linking to all `##` and `###`
    headings.

19. In [docs/CONFIGURATION.md](../docs/CONFIGURATION.md): add a markdown TOC
    after the `# Configuration` heading, linking to all `##` and `###`
    headings. The TOC should reflect the post-Task-4 state: diamond GUC
    sections removed, `default_schedule_seconds` section added.

---

## Verification

```bash
just fmt && just lint                # zero warnings required
just test-unit                       # pure Rust tests
just test-integration                # Testcontainers tests
just test-e2e                        # E2E including diamond tests
```

### Manual smoke checks

```sql
-- 'calculated' should be the default; no schedule argument needed
SELECT pgtrickle.create_stream_table('t', 'SELECT 1 AS x', 'src', 'public');

-- Passing NULL should return an error
SELECT pgtrickle.create_stream_table('t2', 'SELECT 1 AS x', 'src', 'public',
    schedule => NULL);
-- Expected: ERROR: use 'calculated' instead of NULL to set CALCULATED schedule

-- New GUC defaults
SHOW pg_trickle.min_schedule_seconds;       -- 1
SHOW pg_trickle.default_schedule_seconds;   -- 1

-- Removed GUCs should error
SHOW pg_trickle.diamond_consistency;        -- ERROR: unrecognized configuration parameter
SHOW pg_trickle.diamond_schedule_policy;    -- ERROR: unrecognized configuration parameter
```

---

---

## Proposals (not yet decided)

The following improvements were identified during an ergonomics review. Each is
described with enough context to make a decision. None are committed to
implementation yet.

---

### Proposal A — Return a record from `create_stream_table` and `refresh_stream_table`

**Priority:** High  
**Complexity:** Low  
**Type:** API addition (non-breaking if done as overload; breaking if current void signature changes)

Currently both functions return `VOID`. To check the outcome, callers must
issue a follow-up `SELECT` on the catalog. This pattern is repeated everywhere
in the test suite and will be repeated in every operator script.

Proposed return shapes:

```sql
-- create_stream_table
(pgt_id bigint, status text, created_at timestamptz)

-- refresh_stream_table
(status text, rows_inserted bigint, rows_deleted bigint, duration_ms float8)
```

**Decision factors:**
- Does changing the return type break existing callers? Yes, if they use
  `SELECT pgtrickle.create_stream_table(...)` in a script that ignores the
  result — PostgreSQL will still succeed, but any code using `PERFORM` would
  need to be unchanged.
- Should these be new overloaded functions or replace the existing ones?
- Should `status` use a fixed enum (`'created'`, `'already_exists'`,
  `'skipped'`, `'error'`) or a free-form string?

---

### Proposal B — Warn at init if `cdc_mode = 'auto'` but `wal_level != 'logical'`

**Priority:** High  
**Complexity:** Low  
**Type:** Safety / UX

`pg_trickle.cdc_mode = 'auto'` is designed to automatically upgrade from TRIGGER
to WAL-based CDC once PostgreSQL's `wal_level` is set to `logical`. On a
standard PostgreSQL install, `wal_level` defaults to `replica`. In this
state, `auto` silently stays in TRIGGER mode indefinitely — producing no
error, no warning, and no visible status change.

FAQ documentation acknowledges this but it is easy to miss.

**Proposal:** At extension load (`_PG_init`), check:
```rust
if cdc_mode == "auto" && wal_level != "logical" {
    pgrx::warning!("pg_trickle: cdc_mode='auto' but wal_level is '{}', not 'logical'. \
        WAL CDC will not activate until wal_level is changed and PostgreSQL restarted.", wal_level);
}
```

**Decision factors:**
- Is it acceptable to emit a WARNING at every backend startup if this condition
  is true? Could be noisy in shared environments.
- Alternative: emit the warning only via a dedicated health check function, not
  at init time.

---

### Proposal C — Warn on `create_stream_table` if source table has no PRIMARY KEY

**Priority:** High  
**Complexity:** Low  
**Type:** Proactive safety check

Stream tables over keyless sources silently mishandle duplicate rows: identical
rows share the same internal `__pgt_row_id` and a duplicate INSERT may be
treated as a no-op. This is documented as a known limitation under
"Keyless Table Duplicate Row Limitation (G7.1)" in the FAQ, but users hit it
without warning.

**Proposal:** During `create_stream_table`, query `pg_constraint` for the source
table(s). If none of the sources referenced in the query have a PRIMARY KEY,
emit:
```
WARNING: source table "foo" has no PRIMARY KEY — duplicate rows will not be
tracked correctly. Add a primary key or use a unique column as a row identity.
```

**Decision factors:**
- Should this be a WARNING (non-fatal) or an ERROR (require explicit opt-in
  with `allow_keyless => true` parameter)?
- The check only applies to the directly named source tables, not to derived
  sources (e.g., subqueries, CTEs). That limitation should be noted.
- Cost: requires a catalog lookup per source table at creation time (cheap).

---

### Proposal D — Record manual `refresh_stream_table` calls in `pgt_refresh_history`

**Priority:** Medium  
**Complexity:** Medium  
**Type:** Observability / consistency

`pgt_refresh_history` only contains rows inserted by the background scheduler
worker (`initiated_by = 'SCHEDULER'`). Manual calls to
`pgtrickle.refresh_stream_table()` are not recorded. This means:

- Operators auditing history miss manual refreshes entirely.
- Graphs of refresh frequency are inaccurate.
- Test assertions that check "were there N refreshes?" reach around the history
  table and read `data_timestamp` instead.

**Proposal:** At the end of a successful `refresh_stream_table` call, insert a
history row with `initiated_by = 'MANUAL'` and the same `duration_ms`,
`rows_affected`, and `status` fields the scheduler already writes.

**Decision factors:**
- History table is currently append-only and retention is controlled by
  `pg_trickle.history_retention_days`. Manual records would share that policy
  and clean up automatically — no special handling needed.
- Should error outcomes (failed refreshes) also be recorded? Currently even
  scheduler failures are recorded; consistency suggests yes.
- Volume concern: a hot path calling `refresh_stream_table` in a loop would
  create many history rows. Acceptable for now given the append-only design.

---

### Proposal E — `pgtrickle.quick_health` convenience view

**Priority:** Medium  
**Complexity:** Low  
**Type:** Monitoring / new SQL object

The extension exposes 15+ monitoring functions with overlapping scopes. New
operators have no obvious starting point for "is everything OK right now?".
The closest existing function is `pgtrickle.health_check()`, but it returns
multiple rows with varying structure and requires understanding which columns
to focus on.

**Proposed view:**

```sql
CREATE VIEW pgtrickle.quick_health AS
SELECT
    (SELECT COUNT(*) FROM pgtrickle.pgt_stream_tables) AS total_stream_tables,
    (SELECT COUNT(*) FROM pgtrickle.pgt_stream_tables WHERE consecutive_errors > 0) AS error_tables,
    (SELECT COUNT(*) FROM pgtrickle.pgt_stream_tables WHERE data_timestamp < now() - make_interval(secs => schedule_secs * 3)) AS stale_tables,
    pg_trickle_scheduler_running() AS scheduler_running,
    -- ... cdc health summary ...
    CASE WHEN error_tables = 0 AND scheduler_running THEN 'OK' ELSE 'DEGRADED' END AS status;
```

One row, one glance. If `status = 'OK'`, stop reading.

**Decision factors:**
- Should this be a VIEW (no parameters, always computes) or a function
  `quick_health()` returning a record?
- A VIEW requires thinking about column stability across versions — harder to
  evolve. A function is easier to change.
- Should it be in schema `pgtrickle` (user-facing) or `pgtrickle_internal`?
- The "staleness" heuristic (3× schedule interval) needs a defined policy.

---

### Proposal F — Emit WARNING when `alter_stream_table` triggers a full refresh

**Priority:** Medium  
**Complexity:** Low  
**Type:** Transparency / silent expensive operation

When `alter_stream_table` changes the `refresh_mode`, definition query, or
certain parameters that invalidate the current materialization, it
automatically executes a full refresh (equivalent to truncating and repopulating
the stream table). This can be expensive on large tables and currently only
emits a `pgrx::info!()` log, which is suppressed at most log levels and never
surfaced to the calling client.

**Proposal:** Upgrade the message to `pgrx::warning!()` so it appears at the
client's session regardless of server `log_min_messages`:
```
WARNING: pg_trickle: stream table "foo" refresh mode changed from INCREMENTAL to FULL;
a full refresh was applied (N rows). This may take time on large tables.
```

Optionally also send a `NOTIFY pgtrickle_events, '{"event":"full_refresh","table":"foo"}'`
for monitoring automation.

**Decision factors:**
- Is WARNING the right severity, or should it be NOTICE? NOTICE is always shown
  to the client; WARNING implies something requires attention.
- Should a full NOTIFY channel be introduced (could be useful for Proposal E
  as well), or is the log message sufficient?
- Are there cases where the full refresh is expected and the warning would be
  spurious (e.g., during initial setup scripts)?

---

### Proposal G — "Which monitoring function?" decision tree in docs

**Priority:** Medium  
**Complexity:** Very Low  
**Type:** Documentation

With 15+ monitoring functions in `pgtrickle`, new operators don't know which
to call. Common questions and their current answers:

| Question | Function needed |
|---|---|
| Is my stream table up to date? | `pgtrickle.get_staleness('name')` |
| What failed and why? | `pgtrickle.get_refresh_history('name')` |
| Is CDC capturing changes? | `pgtrickle.check_cdc_health()` |
| Are triggers installed correctly? | `pgtrickle.trigger_inventory()` |
| Is the scheduler running? | `pgtrickle.pg_stat_stream_tables` |
| Overall system health? | `pgtrickle.health_check()` |

**Proposal:** Add a "Monitoring Quick Reference" section near the top of
[docs/SQL_REFERENCE.md](../docs/SQL_REFERENCE.md) (or as a dedicated
`docs/MONITORING.md`) mapping common operational questions to the right
function, with a one-line example for each.

**Decision factors:**
- Where should it live: a new `MONITORING.md`, a section in `SQL_REFERENCE.md`,
  or a separate `GETTING_STARTED` sub-section?
- Should it be a prose decision tree, a table, or a flowchart (Mermaid)?

---

### Proposal H — "Special Cases & Known Limitations" section in SQL_REFERENCE.md

**Priority:** Medium  
**Complexity:** Very Low  
**Type:** Documentation

Several important behavioral edge cases are currently scattered through
different documents:

| Case | Currently documented in |
|---|---|
| TopK (ORDER BY + LIMIT rewrite) | FAQ + buried note in SQL_REFERENCE |
| Keyless table duplicate rows | FAQ "Known Delta Computation Limitations" |
| SELECT \* schema drift | GETTING_STARTED + FAQ |
| JOIN key-change + delete edge case | Footnote deep in SQL_REFERENCE |
| Direct DML blocked on stream tables | On-demand error only |

Users who hit one of these problems must search across multiple documents. A
dedicated section consolidating all special cases and limitations would reduce
support burden.

**Decision factors:**
- Should this be a new top-level `## Special Cases` section within
  `SQL_REFERENCE.md`, or a standalone `docs/EDGE_CASES.md`?
- Should existing references in FAQ/GETTING_STARTED be replaced or kept
  with cross-links?

---

### Proposal I — Improve cycle-detection error messages

**Priority:** Low  
**Complexity:** Low  
**Type:** Error quality

The current cycle detection error shows the cycle path but doesn't guide the
user on how to resolve it:
```
ERROR: dependency cycle detected: foo → bar → foo
```

Users unfamiliar with DAG concepts won't know what to do. A better message:
```
ERROR: dependency cycle detected: foo → bar → foo
HINT: Stream tables cannot form circular dependencies. To fix this, ensure
each stream table only reads from base tables or stream tables that do not
(directly or transitively) read from it.
```

**Decision factors:**
- Should the HINT be added inline in the error string, or via a separate
  `pgrx::hint!()` call (which PostgreSQL displays as a "HINT:" line)?

---

### Proposal J — Emit NOTICE when a query is auto-rewritten

**Priority:** Low  
**Complexity:** Medium  
**Type:** Transparency

pg_trickle rewrites user queries in up to five passes (view inlining, CTE
flattening, alias stripping, etc.) before storing them in the catalog. Users
who query `pgt_stream_tables.definition` after creating a stream table see a
different query from the one they wrote, with no explanation.

**Proposal:** After the rewrite passes, if the stored definition differs from
the input, emit a NOTICE:
```
NOTICE: query was rewritten for incremental maintenance.
Original:  SELECT v.x FROM my_view v WHERE v.y > 0
Stored:    SELECT base.x FROM base_table base WHERE base.y > 0
```

**Decision factors:**
- How verbose should the notice be? Full before/after, or just "query was
  simplified"?
- Rewriting is always intentional; would this notice confuse users into
  thinking something is wrong?
- The rewrite is currently undone if refresh fails — the NOTICE should only
  fire after the definition is successfully committed.

---

## CHANGELOG entries (to add under unreleased)

- **Breaking**: `create_stream_table` now defaults `schedule` to `'calculated'`
  instead of `'1m'`. Stream tables without an explicit schedule are now
  CALCULATED by default.
- **Breaking**: Passing `schedule => NULL` to `create_stream_table` is now an
  error. Use `schedule => 'calculated'` instead.
- **Breaking**: GUCs `pg_trickle.diamond_consistency` and
  `pg_trickle.diamond_schedule_policy` have been removed. Use per-table
  parameters in `create_stream_table` / `alter_stream_table` instead.
- **Changed**: `pg_trickle.min_schedule_seconds` default lowered from `60` to
  `1` for better out-of-the-box developer experience.
- **New**: `pg_trickle.default_schedule_seconds` GUC (default `1`) controls
  the effective refresh interval for isolated CALCULATED stream tables.
