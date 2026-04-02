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
--
-- Phase 4 — Refresh Mode Diagnostics:
--   DIAG-1a-b: Pure signal scoring + SPI data gathering (Rust-side).
--   DIAG-1c: pgtrickle.recommend_refresh_mode() function (Rust #[pg_extern]).
--   DIAG-1d: pgtrickle.refresh_efficiency() function (Rust #[pg_extern]).
--            No catalog DDL required — functions are automatically available
--            after extension update via ALTER EXTENSION pg_trickle UPDATE.
--
-- Phase 5 — Export API:
--   G15-EX: pgtrickle.export_definition() function (Rust #[pg_extern]).
--           No catalog DDL required.

-- ── ERR-1a: Error state columns (idempotent) ─────────────────────────────

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS last_error_message TEXT;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS last_error_at TIMESTAMPTZ;

-- ── ERR-1d: Recreate st_refresh_stats() with new columns ────────────────
-- The return type changed (added consecutive_errors, schedule, refresh_tier,
-- last_error_message), so we must drop the old signature and let pgrx
-- re-create it with the new one.

DROP FUNCTION IF EXISTS pgtrickle."st_refresh_stats"();

CREATE FUNCTION pgtrickle."st_refresh_stats"() RETURNS TABLE (
        "pgt_name" TEXT,
        "pgt_schema" TEXT,
        "status" TEXT,
        "refresh_mode" TEXT,
        "is_populated" bool,
        "total_refreshes" bigint,
        "successful_refreshes" bigint,
        "failed_refreshes" bigint,
        "total_rows_inserted" bigint,
        "total_rows_deleted" bigint,
        "avg_duration_ms" double precision,
        "last_refresh_action" TEXT,
        "last_refresh_status" TEXT,
        "last_refresh_at" timestamp with time zone,
        "staleness_secs" double precision,
        "stale" bool,
        "consecutive_errors" INT,
        "schedule" TEXT,
        "refresh_tier" TEXT,
        "last_error_message" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'st_refresh_stats_wrapper';

-- ── New functions (Phases 3–5): explicit registration required ──────────────
-- Although these functions are implemented as #[pg_extern] in Rust, PostgreSQL
-- does NOT automatically register new shared-library functions during
-- ALTER EXTENSION UPDATE. They must be explicitly CREATE'd here so they are
-- visible after `ALTER EXTENSION pg_trickle UPDATE`.

-- D-1c: convert_buffers_to_unlogged()
CREATE  FUNCTION pgtrickle."convert_buffers_to_unlogged"() RETURNS bigint
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'convert_buffers_to_unlogged_wrapper';

-- G15-EX: export_definition()
CREATE  FUNCTION pgtrickle."export_definition"(
        "st_name" TEXT
) RETURNS TEXT
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'export_definition_wrapper';

-- DIAG-1c: recommend_refresh_mode()
CREATE  FUNCTION pgtrickle."recommend_refresh_mode"(
        "st_name" TEXT DEFAULT NULL
) RETURNS TABLE (
        "pgt_schema" TEXT,
        "pgt_name" TEXT,
        "current_mode" TEXT,
        "effective_mode" TEXT,
        "recommended_mode" TEXT,
        "confidence" TEXT,
        "reason" TEXT,
        "signals" jsonb
)
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'recommend_refresh_mode_wrapper';

-- DIAG-1d: refresh_efficiency()
CREATE  FUNCTION pgtrickle."refresh_efficiency"() RETURNS TABLE (
        "pgt_schema" TEXT,
        "pgt_name" TEXT,
        "refresh_mode" TEXT,
        "total_refreshes" bigint,
        "diff_count" bigint,
        "full_count" bigint,
        "avg_diff_ms" double precision,
        "avg_full_ms" double precision,
        "avg_change_ratio" double precision,
        "diff_speedup" TEXT,
        "last_refresh_at" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'refresh_efficiency_wrapper';
