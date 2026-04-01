-- pg_trickle 0.13.0 -> 0.14.0 upgrade script
--
-- v0.14.0: Tiered Scheduling, UNLOGGED Buffers & Diagnostics
--
-- Phase 1 — Quick Polish:
--   C4:     planner_aggressive GUC (replaces merge_planner_hints + merge_work_mem_mb).
--   DIAG-2: Aggregate cardinality warning at create_stream_table time.
--           agg_diff_cardinality_threshold GUC. No catalog DDL required.
--   DOC-OPM: Operator matrix summary in SQL_REFERENCE.md. No catalog DDL.
--
-- Phase 1b — Error State Circuit Breaker:
--   ERR-1a: last_error_message TEXT and last_error_at TIMESTAMPTZ columns.
--   ERR-1b: Permanent failure immediately sets ERROR status (Rust-side).
--   ERR-1c: alter/create_or_replace/refresh clear error state (Rust-side).
--   ERR-1d: Columns visible in stream_tables_info view via st.*.
--
-- Phase 2 — Manual Tiered Scheduling:
--   C-1a/b: refresh_tier column + ALTER ... SET (tier=...) already exist (v0.11).
--   C-1c:   Scheduler tier multipliers + frozen skip (already in Rust since v0.12).
--   C-1b:   Added NOTICE on tier demotion from hot to cold/frozen (Rust-side).
--           No catalog DDL required.
--
-- Phase 3 — UNLOGGED Change Buffers:
--   D-1a:   pg_trickle.unlogged_buffers GUC (Rust-side, no catalog DDL).
--   D-1b:   Crash recovery detection for UNLOGGED buffers (Rust-side).
--   D-1c:   pgtrickle.convert_buffers_to_unlogged() function (Rust #[pg_extern]).
--           No SQL DDL required — function is automatically available after
--           extension update via ALTER EXTENSION pg_trickle UPDATE.

-- ── ERR-1a: Error state columns (idempotent) ─────────────────────────────

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS last_error_message TEXT;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS last_error_at TIMESTAMPTZ;
