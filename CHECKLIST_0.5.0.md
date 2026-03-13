# v0.5.0 Implementation Checklist

> Recommended implementation order for v0.5.0 — Row-Level Security & Operational Controls.
> See [ROADMAP.md](ROADMAP.md#v050--row-level-security--operational-controls) for full item descriptions.

---

## Phase 1 — Security context hardening

No schema changes. These are correctness fixes that must land before any other
new code touches the refresh path — they prevent silent data truncation when RLS
is in play.

- [x] **R1** — Document RLS semantics in `SQL_REFERENCE.md` and `FAQ.md`
  *(read this first; establishes the mental model for all subsequent RLS work)*
- [x] **R2** — Disable RLS on all change buffer tables
  (`ALTER TABLE … DISABLE ROW LEVEL SECURITY` in `cdc.rs` during buffer creation)
- [x] **R3** — Force superuser context for `refresh_stream_table()`
  (prevent caller's RLS policies from filtering the refresh query)
- [x] **R4** — Force `SECURITY DEFINER` on IVM trigger functions
  (IMMEDIATE mode delta queries must see all rows regardless of caller role)
- [x] **R5** — E2E test: RLS on source table does **not** affect stream table content
- [x] **R7** — E2E test: RLS on stream table filters reads per role
- [x] **R8** — E2E test: IMMEDIATE mode + RLS on stream table

---

## Phase 2 — RLS DDL tracking

Extends the DDL event-trigger hook to react when an operator enables or disables
RLS on a source table.

- [x] **R9** — Track `ENABLE`/`DISABLE ROW LEVEL SECURITY` DDL on source tables in
  `hooks.rs` (`AT_EnableRowSecurity` et al. → set `needs_reinit`)
- [x] **R10** — E2E test: `ENABLE RLS` on source table triggers stream table reinit
- [x] **R6** — Tutorial: per-tenant RLS policies on stream tables
  *(write after all code is working and tested)*

---

## Phase 3 — Bootstrap source gating

Self-contained new feature; no dependency on the RLS phases. Can be started in
parallel with Phase 1 if work is shared across contributors.

- [ ] **BOOT-1** — `pgtrickle.pgt_source_gates` catalog table
  (`source_relid`, `gated`, `gated_at`, `gated_by`)
- [ ] **BOOT-2** — `gate_source(source TEXT)` SQL function + `pg_notify` scheduler signal
- [ ] **BOOT-3** — `ungate_source(source TEXT)` + `source_gates()` introspection view
- [ ] **BOOT-4** — Scheduler integration: load gated-source set per tick; skip dependent
  STs and log `SKIP` in `pgt_refresh_history`
- [ ] **BOOT-5** — E2E tests: single-source gate, coordinated multi-source, partial DAG,
  bootstrap with `initialize => false`

---

## Phase 4 — Ergonomics & API polish

All items are ≤ 2 h each and independent — fill gaps between the larger phases.

- [ ] **ERG-D** — Record manual `refresh_stream_table()` calls in `pgt_refresh_history`
  with `initiated_by='MANUAL'`
- [ ] **ERG-E** — `pgtrickle.quick_health` view
  (`total_stream_tables`, `error_tables`, `stale_tables`, `scheduler_running`, `status`)
- [ ] **COR-2** — `create_stream_table_if_not_exists()` convenience wrapper
- [ ] **NAT-CALL** — `CREATE PROCEDURE` wrappers enabling `CALL pgtrickle.create_stream_table(…)` syntax

---

## Phase 5 — Performance Wave 1

Scoped to the append-only fast path only. A-4, B-2, and C-4 are deferred to
v0.6.0 where they fit better alongside the Wave 2 optimizations.

- [ ] **A-3a** — Append-Only INSERT path (MERGE bypass)
  - `APPEND ONLY` declaration on `CREATE STREAM TABLE`
  - CDC heuristic fallback: use fast-path until first DELETE/UPDATE seen, then revert to MERGE
  - Catalog: `is_append_only BOOLEAN` column + upgrade SQL
  - Benchmark: INSERT vs MERGE throughput on event-sourced workload

---

## Phase 6 — Upgrade path & release

- [ ] Write `sql/pg_trickle--0.4.0--0.5.0.sql` incrementally as each phase lands:
  - New catalog tables (`pgt_source_gates`, `is_append_only` column, etc.)
  - Updated function signatures
- [ ] Run `just check-upgrade` — completeness check must pass
- [ ] E2E upgrade test: install at 0.4.0 → `ALTER EXTENSION pg_trickle UPDATE` →
  verify all stream tables, gates, and RLS configuration survive intact
- [ ] Update `CHANGELOG.md` and bump version in `Cargo.toml` / `pg_trickle.control`
- [ ] Tag `v0.5.0`
