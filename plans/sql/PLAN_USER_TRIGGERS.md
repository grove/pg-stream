# Plan: Support User-Defined Triggers on Stream Tables

**Status:** Superseded by [PLAN_USER_TRIGGERS_EXPLICIT_DML.md](PLAN_USER_TRIGGERS_EXPLICIT_DML.md)  
**Author:** GitHub Copilot  
**Date:** 2026-02-23

> **⚠️ This plan has been superseded.** The explicit DML approach in
> [PLAN_USER_TRIGGERS_EXPLICIT_DML.md](PLAN_USER_TRIGGERS_EXPLICIT_DML.md)
> replaces the `session_replication_role` suppression with a more capable
> solution that actually fires user triggers with correct semantics.
> Phase 1 (FULL refresh trigger suppression) is implemented using
> `DISABLE TRIGGER USER` instead of `session_replication_role`. The GUC
> `pg_stream.suppress_user_triggers` was replaced by
> `pg_stream.user_triggers` (auto/on/off). This document is retained for
> historical reference.

---

## Problem

User-defined triggers on stream table (ST) storage tables are currently
documented as unsupported (⚠️). The extension does not block `CREATE TRIGGER`
on STs, but any triggers that exist will fire during the refresh engine's
internal `MERGE`, `INSERT`, `TRUNCATE`, and `DELETE` operations, leading to:

1. **Spurious firing** — triggers fire on internal bookkeeping DML, not
   user-initiated changes. A FULL refresh of a 10k-row ST fires AFTER INSERT
   10,000 times.
2. **Partial state visibility** — triggers fire mid-MERGE and see an
   inconsistent snapshot of the ST.
3. **FULL refresh asymmetry** — `TRUNCATE` does not fire row-level DELETE
   triggers, but the subsequent INSERT fires for all rows.
4. **Cascade / oscillation risk** — if a trigger writes back to a source
   table, CDC captures the change and the next refresh sees new deltas,
   creating an unstable feedback loop.
5. **Performance** — per-row trigger overhead on the MERGE critical path
   (~2–15 ms per refresh today).

## Goal

Allow users to safely define triggers on ST storage tables. Triggers must
**not** fire during refresh engine operations. Optionally, provide a mechanism
to notify users after a refresh completes so they can react to changes.

## Approach: `session_replication_role = replica`

When `session_replication_role` is set to `'replica'`, PostgreSQL only fires
triggers marked `ENABLE REPLICA TRIGGER`. All default ("origin") triggers —
including any user-defined triggers — are suppressed.

- `SET LOCAL` is transaction-scoped and auto-resets on commit/rollback, so
  it is crash-safe.
- The background worker already runs with superuser-equivalent privileges.
- CDC triggers live on **source** tables, not on ST storage tables, so they
  are unaffected.

This is the simplest, safest, and most battle-tested mechanism available in
PostgreSQL for suppressing triggers during bulk maintenance operations.

## Implementation Steps

### Phase 1: Suppress triggers during refresh

#### 1.1 Add `SET LOCAL session_replication_role = 'replica'` to refresh paths

In `src/refresh.rs`, inject the SET LOCAL before any DML in each refresh
function:

- `execute_full_refresh()` (line ~503) — before `TRUNCATE` + `INSERT`
- `execute_differential_refresh()` (line ~560) — before `MERGE` or
  `DELETE + INSERT`
- `execute_reinitialize_refresh()` (line ~1142) — delegates to
  `execute_full_refresh()`, so covered automatically

Implementation pattern:

```rust
// At the start of execute_full_refresh / execute_differential_refresh,
// before any DML on the ST storage table:
Spi::run("SET LOCAL session_replication_role = 'replica'")
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

// ... MERGE / TRUNCATE+INSERT / DELETE+INSERT ...

// Restore explicitly (SET LOCAL auto-resets on txn end, but be explicit
// for clarity and to allow post-refresh logic to run with triggers enabled):
Spi::run("SET LOCAL session_replication_role = 'origin'")
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
```

The `SET LOCAL` scope is the current transaction. Since refresh runs inside
`Spi::connect` which is wrapped in a subtransaction, the reset is automatic
even on error. The explicit restore is for clarity and to allow any future
post-refresh logic (Phase 2) to run with triggers enabled.

**Files changed:** `src/refresh.rs`

#### 1.2 Add a GUC to control the behavior

Add `pg_stream.suppress_user_triggers` (default: `true`) in `src/config.rs`
so users who *want* triggers to fire during refresh (at their own risk) can
opt out:

```rust
pub static PGS_SUPPRESS_USER_TRIGGERS: GucSetting<bool> = GucSetting::<bool>::new(true);
```

Register in `register_gucs()`:

```rust
GucRegistry::define_bool_guc(
    c"pg_stream.suppress_user_triggers",
    c"Suppress user-defined triggers on ST storage tables during refresh.",
    c"When true (default), SET LOCAL session_replication_role = 'replica' \
      is used during refresh to prevent user triggers from firing.",
    &PGS_SUPPRESS_USER_TRIGGERS,
    GucContext::Suset,
    GucFlags::default(),
);
```

Then in refresh, guard the SET LOCAL:

```rust
if config::pg_stream_suppress_user_triggers() {
    Spi::run("SET LOCAL session_replication_role = 'replica'")?;
}
```

**Files changed:** `src/config.rs`, `src/refresh.rs`

#### 1.3 Update documentation

- Update `README.md` line 207: change `⚠️ Unsupported` to `✅ Yes` with a
  note: "User triggers are suppressed during refresh by default
  (`pg_stream.suppress_user_triggers = true`). Set to `false` to allow
  triggers to fire during refresh (not recommended)."
- Update `docs/SQL_REFERENCE.md` restrictions section.
- Update `docs/CONFIGURATION.md` with the new GUC.
- Update `AGENTS.md` code review checklist if needed.

**Files changed:** `README.md`, `docs/SQL_REFERENCE.md`, `docs/CONFIGURATION.md`

### Phase 2: Post-refresh notification (optional, future)

After the MERGE/INSERT completes and `session_replication_role` is restored to
`'origin'`, emit a `NOTIFY` with the ST name and refresh metadata so
downstream listeners can react:

```rust
// After restore:
Spi::run(&format!(
    "NOTIFY pg_stream_refresh, '{{\
       \"stream_table\": \"{name}\", \
       \"rows_inserted\": {ins}, \
       \"rows_deleted\": {del}, \
       \"refresh_mode\": \"{mode}\"\
     }}'",
))?;
```

This lets users set up `LISTEN pg_stream_refresh` and react to changes
without needing triggers on the ST itself. This is a low-cost addition that
provides a clean alternative to user triggers for most use cases.

**Files changed:** `src/refresh.rs`

### Phase 3: DDL event hook warning (optional, defensive)

Add `CREATE TRIGGER` detection to the DDL event trigger in `src/hooks.rs`.
When a user creates a trigger on a ST storage table, emit a `WARNING`:

```
WARNING: pg_stream: trigger "my_trigger" on stream table "regional_totals"
will not fire during refresh (suppressed by pg_stream.suppress_user_triggers).
Use LISTEN pg_stream_refresh for post-refresh notifications.
```

This requires extending `handle_ddl_command()` to match `command_tag =
"CREATE TRIGGER"` and checking whether the target table OID is a ST storage
table via the catalog.

**Files changed:** `src/hooks.rs`

## Testing

### Unit tests

- Verify that the GUC default is `true`.
- Verify GUC accessor function works.

### E2E tests (new file: `tests/e2e_user_trigger_tests.rs`)

| Test | Description |
|------|-------------|
| `test_user_trigger_suppressed_during_differential` | Create ST, add AFTER INSERT trigger that inserts into an audit table. Refresh differentially. Assert audit table is empty. |
| `test_user_trigger_suppressed_during_full` | Same, but with FULL refresh. |
| `test_user_trigger_fires_when_guc_disabled` | Set `pg_stream.suppress_user_triggers = false`. Refresh. Assert audit table has rows. |
| `test_user_trigger_after_manual_insert` | Verify that if user somehow inserts directly (bypassing the read-only guard), the trigger fires normally outside of refresh context. |
| `test_notify_after_refresh` | (Phase 2) `LISTEN pg_stream_refresh`, trigger a refresh, verify notification payload. |
| `test_ddl_warning_on_create_trigger` | (Phase 3) Create trigger on ST, verify WARNING is emitted. |

### Regression

- Run full E2E suite to confirm no regressions from `SET LOCAL session_replication_role`.
- Confirm CDC triggers on source tables still fire (they should — they're on different tables).

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| `session_replication_role` requires superuser | Low | Low | Background worker already has superuser privileges. Manual `refresh_stream_table()` runs as extension owner (superuser). |
| Suppresses ALL user triggers, not selectively | Medium | Low | GUC opt-out available. Phase 3 warning informs users. |
| Future PostgreSQL changes to `session_replication_role` semantics | Very low | Medium | Well-established, stable PostgreSQL feature since 8.3. |
| CDC triggers accidentally suppressed | Very low | High | CDC triggers are on source tables, not STs. Verified by architecture. Add regression test. |

## Alternatives Considered

| Alternative | Pros | Cons | Decision |
|---|---|---|---|
| `ALTER TABLE ... DISABLE TRIGGER USER` | Targeted, no superuser needed | Takes `ACCESS EXCLUSIVE` lock (blocks readers). Unsafe if refresh crashes between DISABLE and ENABLE. | Rejected |
| Synthetic trigger firing post-MERGE | Users see consistent state | Very complex. Needs to synthesize `NEW`/`OLD` records. Non-standard semantics. | Rejected |
| Block `CREATE TRIGGER` entirely | Simple enforcement | Doesn't solve the problem, just prevents it. Users may have legitimate use cases. | Rejected (but add warning in Phase 3) |
| `pg_trigger` visibility flag | Per-trigger control | Requires pg catalog hacking. Not portable. | Rejected |

## Effort Estimate

| Phase | Effort | Priority |
|-------|--------|----------|
| Phase 1 (suppress + GUC + docs) | ~2–3 hours | High |
| Phase 2 (NOTIFY) | ~1 hour | Medium |
| Phase 3 (DDL warning) | ~1–2 hours | Low |
| E2E tests | ~2–3 hours | High |

## Commit Plan

1. `feat: suppress user triggers during refresh via session_replication_role`
2. `feat: add pg_stream.suppress_user_triggers GUC`
3. `test: add E2E tests for user trigger suppression`
4. `docs: update restrictions — user triggers now supported`
5. `feat: emit NOTIFY pg_stream_refresh after each refresh` (Phase 2)
6. `feat: warn on CREATE TRIGGER targeting a stream table` (Phase 3)
