# PLAN_ERGONOMICS — Developer & User Ergonomics Improvements

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
| Diamond GUC fallbacks | Hardcoded to `'none'` / `'fastest'` in Rust; per-table params in `create/alter_stream_table` are kept |

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
