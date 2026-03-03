# PLAN: Row-Level Security (RLS) Support in pg_trickle

**Status:** Plan
**Priority:** Low (P4) — niche feature, but important for pg_ivm parity
**Effort:** ~12–20 hours total across 4 phases
**Last updated:** 2026-03-03

---

## 1. Problem Statement

Row-Level Security (RLS) is a PostgreSQL feature that restricts which rows are
visible to a given role. When RLS is enabled on a base table, queries executed
by non-superuser roles return only the rows permitted by the active policies.

pg_ivm respects RLS on base tables during IMMV maintenance — its trigger
functions run with the IMMV owner's permissions, and RLS policies on base
tables are enforced. pg_trickle has **partial** RLS support but has not
documented, tested, or hardened it systematically.

### Current state

| Area | Status | Details |
|------|--------|---------|
| **RLS policy DDL tracking** | ✅ Implemented | `hooks.rs` detects CREATE/ALTER/DROP POLICY on source tables and marks affected stream tables for reinit (G3.3). This is *better* than pg_ivm, which requires manual refresh after policy changes. |
| **Change buffer table RLS** | ⚠️ Gap (G6.4) | Change buffer tables in `pgtrickle_changes` don't explicitly disable RLS. If a DBA enables RLS on that schema, CDC trigger inserts could fail. |
| **Scheduled refresh security context** | ⚠️ Undocumented | The background worker runs as the database superuser (via `BackgroundWorker::connect_worker_to_spi`). Superusers bypass RLS by default. Stream tables therefore always contain the **full unfiltered result set**, regardless of RLS on base tables. |
| **Manual refresh security context** | ⚠️ Undocumented | `pgtrickle.refresh_stream_table()` runs via SPI in the calling user's session. If the calling user is subject to RLS, the refresh query may produce a filtered result. This creates a **correctness hazard**: the stream table contents depend on *who* calls refresh. |
| **IMMEDIATE mode context** | ⚠️ Undocumented | IVM triggers fire in the context of the DML-issuing user. If that user is subject to RLS on source tables referenced in the defining query, the delta computation may produce incorrect results (partial visibility). |
| **RLS on stream tables** | ✅ Works naturally | Stream tables are regular PostgreSQL tables. Users can `ALTER TABLE ... ENABLE ROW LEVEL SECURITY` and `CREATE POLICY` on them. Reads are filtered at query time. No pg_trickle changes needed. |

### Comparison with pg_ivm

| Aspect | pg_ivm | pg_trickle |
|--------|--------|-----------|
| RLS on base tables during maintenance | Respected (IMMV owner's role) | Bypassed (superuser bgworker) or inconsistent (manual refresh) |
| RLS policy change detection | ❌ Manual refresh required | ✅ Auto-reinit via event trigger |
| RLS on the materialized view itself | Possible (it's a regular table) | Possible (it's a regular table) |
| TRUNCATE + RLS | Respected | Not explicitly tested |
| Multi-tenant / per-role views | One IMMV per role (manual) | Not supported |

---

## 2. Design Decisions

### D1: Stream tables materialize the full result set (like MATERIALIZED VIEW)

PostgreSQL's built-in `MATERIALIZED VIEW` does not respect RLS — `REFRESH
MATERIALIZED VIEW` always returns all rows regardless of the caller's role.
pg_trickle should follow this **well-understood semantic**: the stream table
is a **system-level cache** that contains all rows. Access control is applied
at read time via RLS policies on the stream table itself.

**Rationale:**
- Matches PostgreSQL's own materialized view behavior.
- Avoids the "who refreshed it?" correctness hazard.
- One stream table serves all roles (no per-role duplication).
- The scheduled background worker already runs as superuser.

### D2: Optional `security_invoker` for per-role materialization (future)

For multi-tenant SaaS scenarios where data-at-rest must be role-filtered, an
optional `security_invoker => 'tenant_role'` parameter could be added to
`create_stream_table`. This would `SET ROLE tenant_role` before executing the
defining query during refresh, so RLS policies are enforced.

**Deferred to post-1.0** because:
- Requires one stream table per tenant role (significant storage overhead).
- Adds complexity to the refresh pipeline (role switching, permission checks).
- The mainstream pattern is RLS on the stream table itself (D1).

### D3: Change buffer tables must be RLS-immune

CDC trigger functions must always be able to INSERT into change buffer tables,
regardless of the caller's role or any global RLS settings. Buffer tables
should explicitly disable RLS.

---

## 3. Implementation Phases

### Phase 1 — Documentation + Hardening (P2, ~4–6h)

Minimal-effort work that documents current behavior, fixes the change buffer
gap, and ensures the scheduled refresh context is consistent.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R1 | **Document RLS semantics** in SQL_REFERENCE.md and FAQ.md: stream tables materialize full result set (like MATERIALIZED VIEW); apply RLS on the stream table for read-side filtering; RLS policy changes on source tables trigger auto-reinit. | 1h | §2 D1 |
| R2 | **Disable RLS on change buffer tables**: add `ALTER TABLE {schema}.changes_{oid} DISABLE ROW LEVEL SECURITY` after `CREATE TABLE` in `cdc.rs::create_change_buffer_table()`. | 30min | G6.4 |
| R3 | **Force superuser context for manual refresh**: in `api.rs::refresh_stream_table()`, verify that the defining query is executed with RLS bypassed (either check `current_setting('is_superuser')` or add `SET LOCAL row_security = off` within the SPI transaction). This prevents the "who refreshed it?" hazard. | 2h | §1 manual refresh |
| R4 | **Force superuser context for IMMEDIATE mode delta queries**: in `ivm.rs`, ensure the delta SQL runs with `SET LOCAL row_security = off` so that partial visibility doesn't corrupt the stream table. The DML itself is still filtered by the user's RLS policies (correct), but the stream table update must see all rows to compute the correct delta. | 2h | §1 IMMEDIATE mode |
| R5 | **E2E test: RLS on source table does not affect stream table content** — create a table with RLS, insert as role A, verify stream table contains all rows regardless. | 1h | — |

**Exit criteria:**
- [x] Documented in SQL_REFERENCE.md and FAQ.md
- [x] Change buffer tables have RLS disabled
- [x] Manual refresh always produces full result set
- [x] IMMEDIATE mode delta queries bypass RLS
- [x] At least 1 E2E test covering RLS + stream table

### Phase 2 — RLS on Stream Tables Guide (P3, ~2–3h)

Document and test the recommended pattern: RLS on the stream table itself.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R6 | **Tutorial**: add a "Row-Level Security" section to `docs/tutorials/` showing: (a) create stream table, (b) enable RLS on it, (c) create per-tenant policies, (d) verify row filtering at read time. | 1.5h | — |
| R7 | **E2E test: RLS on stream table filters reads per role** — enable RLS on a stream table, create two roles with different policies, verify each sees only their permitted rows. | 1h | — |
| R8 | **E2E test: IMMEDIATE mode + RLS on stream table** — same scenario but with IMMEDIATE refresh mode. | 30min | — |

**Exit criteria:**
- [x] Tutorial published
- [x] RLS-on-stream-table tested in both DIFFERENTIAL and IMMEDIATE modes

### Phase 3 — ENABLE/DISABLE RLS DDL Tracking (P3, ~2–3h)

The existing hooks track CREATE/ALTER/DROP POLICY but not `ALTER TABLE ...
ENABLE ROW LEVEL SECURITY` or `ALTER TABLE ... DISABLE ROW LEVEL SECURITY`.
These DDL commands change whether policies are enforced at all.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R9 | **Detect ENABLE/DISABLE RLS on source tables**: in `hooks.rs`, handle `ALTER TABLE` subcommands `AT_EnableRowSecurity`, `AT_DisableRowSecurity`, `AT_ForceRowSecurity`, `AT_NoForceRowSecurity`. Mark affected STs for reinit. | 2h | G3.3 |
| R10 | **E2E test: ENABLE RLS triggers reinit** — create ST on a table, enable RLS + add a restrictive policy, observe reinit, verify stream table is still fully populated (superuser context). | 1h | — |

**Exit criteria:**
- [x] All 4 RLS-related ALTER TABLE subcommands tracked
- [x] Reinit fires on ENABLE/DISABLE RLS

### Phase 4 — Security Invoker Mode (P4, ~4–8h, post-1.0)

Optional per-role materialization for multi-tenant scenarios.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R11 | **Add `security_invoker` parameter** to `create_stream_table()` and catalog column `pgt_stream_tables.security_invoker TEXT NULL`. | 1h | §2 D2 |
| R12 | **SET ROLE before refresh**: in `refresh.rs`, if `security_invoker` is set, execute `SET LOCAL ROLE <role>` before running the defining query and `RESET ROLE` after. For IMMEDIATE mode, do the same in `ivm.rs` around the delta query. | 2–3h | D2 |
| R13 | **Validation**: check that the specified role exists and has `SELECT` on all source tables at stream table creation time. | 1h | — |
| R14 | **E2E tests**: (a) security_invoker with RLS filters stream table content, (b) role without SELECT on source table → clear error, (c) role removal → reinit on next refresh. | 2h | — |
| R15 | **Documentation**: update SQL_REFERENCE and tutorials for security_invoker pattern. Include caveats (per-role duplication, storage overhead). | 1h | — |

**Exit criteria:**
- [x] `security_invoker` parameter works for FULL, DIFFERENTIAL, and IMMEDIATE modes
- [x] Stream table content is role-filtered when security_invoker is set
- [x] Clear error messages for permission issues

---

## 4. Technical Details

### 4.1 `SET LOCAL row_security = off`

PostgreSQL's `row_security` GUC controls whether RLS policies are enforced for
non-superuser roles. Setting it to `off` causes an error if RLS would filter
rows (unless the user is superuser). However, superusers bypass RLS regardless.

For the background worker (already superuser), this is a no-op but serves as
defense-in-depth. For manual `refresh_stream_table()` calls by non-superuser
roles, we need to either:

1. **Require superuser or `pg_trickle_admin` role** for refresh — then RLS is
   naturally bypassed. This is the simplest option.
2. **Execute the defining query as the extension owner** via `SECURITY DEFINER`
   — the trigger functions and SPI calls run as the function creator.
3. **Use `SET LOCAL row_security = off`** — requires the caller to have
   the `BYPASSRLS` attribute, which is almost as privileged as superuser.

**Recommendation:** Option 1. The `refresh_stream_table()` function is already
`SECURITY DEFINER` (it modifies catalog tables in `pgtrickle` schema). The SPI
query for the defining query inherits this context. We should verify this is
the case and add a test.

### 4.2 IMMEDIATE Mode Delta Query Context

In IMMEDIATE mode, the IVM trigger function runs in the DML-issuing user's
context. The delta SQL reads from transition tables and the stream table. The
transition tables contain exactly the rows affected by the DML (already
filtered by the user's RLS), but the delta query also reads from the base
tables to compute joins and aggregates. If the user has restricted visibility,
the delta may be computed against a partial view of the base tables.

**Solution:** The IVM trigger function should be `SECURITY DEFINER` (owned by
the extension installer / superuser), ensuring the delta query always sees all
rows. The DML itself is still filtered by the caller's RLS policies — only the
**stream table maintenance** runs with elevated privileges.

This is exactly how pg_ivm handles it — the trigger functions are owned by the
IMMV creator (typically superuser), and the maintenance runs in that context.

### 4.3 Change Buffer RLS Hardening

Add to `cdc.rs::create_change_buffer_table()`:

```rust
// G6.4: Explicitly disable RLS on change buffer tables so CDC trigger
// inserts always succeed, regardless of any schema-level RLS settings.
let disable_rls_sql = format!(
    "ALTER TABLE {schema}.changes_{oid} DISABLE ROW LEVEL SECURITY",
    schema = change_schema,
    oid = source_oid.to_u32(),
);
Spi::run(&disable_rls_sql).map_err(|e| {
    PgTrickleError::SpiError(format!("Failed to disable RLS on change buffer: {}", e))
})?;
```

### 4.4 Trigger Function Security Context

Both the CDC trigger functions (`pgt_cdc_*`) and IVM trigger functions
(`pgt_ivm_*`) are created via `CREATE FUNCTION ... LANGUAGE plpgsql`. They do
not currently specify `SECURITY DEFINER` or `SECURITY INVOKER`. By default,
PL/pgSQL functions are `SECURITY INVOKER`, meaning they run as the calling
user.

For **CDC triggers**, this is fine — the trigger inserts into change buffer
tables, which are owned by the extension installer. As long as RLS is disabled
on buffer tables (R2), the insert succeeds.

For **IVM triggers**, we need `SECURITY DEFINER` to ensure the delta query
always sees all rows. The trigger function should be created with:

```sql
CREATE OR REPLACE FUNCTION pgt_ivm_after_ins_<oid>()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgtrickle, pgtrickle_changes
AS $$ ... $$;
```

The `SET search_path` is a security best practice for `SECURITY DEFINER`
functions to prevent search_path hijacking.

---

## 5. Risk Analysis

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| SECURITY DEFINER escalation allows unprivileged users to bypass RLS via crafted DML | Low | High | The trigger function only executes the pre-defined delta SQL — it does not accept user input. The defining query is validated at creation time. |
| Performance regression from `SET LOCAL` calls | Very Low | None | `SET LOCAL` is a no-op for superusers. For non-superusers, the overhead is negligible (~1 μs). |
| `security_invoker` parameter creates per-role storage explosion | Medium | Medium | Document clearly. Make the parameter optional and warn about storage implications. Consider a companion `pgtrickle.drop_all_security_invoker_tables(role)` cleanup function. |
| Existing stream tables created before RLS hardening have SECURITY INVOKER trigger functions | Medium | Low | Migration script (extension upgrade path) should `ALTER FUNCTION` existing IVM triggers to SECURITY DEFINER. |

---

## 6. Prioritized Recommendations

### Implementation Order

| Priority | Phase | Items | Effort | Value |
|----------|-------|-------|--------|-------|
| **P1** | Phase 1 | R1–R5 (document + harden) | 4–6h | Closes the pg_ivm comparison gap cleanly. Prevents the "who refreshed it?" correctness hazard. |
| **P2** | Phase 2 | R6–R8 (tutorial + tests) | 2–3h | Establishes the recommended pattern (RLS on stream table). |
| **P2** | Phase 3 | R9–R10 (ENABLE/DISABLE tracking) | 2–3h | Completes the DDL tracking story for RLS. |
| **P4** | Phase 4 | R11–R15 (security_invoker) | 4–8h | Post-1.0. Niche use-case for multi-tenant SaaS. |

### Specific recommendations

1. **Do Phase 1 immediately** (pre-v0.2.0 release). The documentation and
   hardening work is low-effort and eliminates a correctness hazard in manual
   refresh and IMMEDIATE mode.

2. **Do Phases 2–3 for v0.3.0** (production readiness milestone). The tutorial
   and ENABLE/DISABLE tracking round out the RLS story.

3. **Defer Phase 4 to post-1.0**. Per-role materialization is a niche feature
   that adds significant complexity. The mainstream pattern (RLS on the stream
   table) handles 90%+ of use cases.

4. **Update GAP_PG_IVM_COMPARISON.md** after Phase 1 to change the RLS row
   from "pg_ivm" to "Tie" or "**pg_trickle**" (since auto-reinit on policy
   change is unique to pg_trickle).

---

## 7. References

| Document | Relevance |
|----------|-----------|
| [GAP_PG_IVM_COMPARISON.md](../ecosystem/GAP_PG_IVM_COMPARISON.md) §4.1.3 | RLS comparison with pg_ivm |
| [GAP_SQL_PHASE_6.md](GAP_SQL_PHASE_6.md) G6.4 | Change buffer RLS gap |
| [GAP_SQL_PHASE_7.md](GAP_SQL_PHASE_7.md) G3.3 | RLS policy tracking gap (now implemented) |
| [hooks.rs](../../src/hooks.rs) L549–L615 | Current RLS policy change handling |
| [cdc.rs](../../src/cdc.rs) L281–L345 | Change buffer table creation |
| [ivm.rs](../../src/ivm.rs) L153–L400 | IVM trigger creation |
| [PostgreSQL RLS docs](https://www.postgresql.org/docs/current/ddl-rowsecurity.html) | Official RLS reference |
| [PostgreSQL row_security GUC](https://www.postgresql.org/docs/current/runtime-config-client.html#GUC-ROW-SECURITY) | GUC for controlling RLS enforcement |
