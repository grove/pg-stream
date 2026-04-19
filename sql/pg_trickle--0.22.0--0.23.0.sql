-- pg_trickle 0.22.0 → 0.23.0 upgrade migration
-- ============================================
--
-- v0.23.0 — TPC-H DVM Scaling Performance
--
-- P1-2: log_delta_sql GUC (config-only, no schema change)
-- P5-1: delta_work_mem GUC (config-only, no schema change)
-- P5-2: delta_enable_nestloop GUC (config-only, no schema change)
-- PERF-5: analyze_before_delta GUC (config-only, no schema change)
-- SCAL-2: max_change_buffer_alert_rows GUC (config-only, no schema change)
-- UX-7: diff_output_format GUC (config-only, no schema change)
-- UX-3: explain_diff_sql() SQL function (Rust-defined via pgrx)
-- STAB-4: pgtrickle_refresh_stats() SQL function (Rust-defined via pgrx)
--
-- This migration adds the two new SQL functions introduced in v0.23.0.
-- GUCs are compile-time settings registered in _PG_init() and need no SQL.

-- UX-3: explain_diff_sql() — return the delta SQL for a stream table
CREATE OR REPLACE FUNCTION pgtrickle."explain_diff_sql"(
    "name" TEXT
) RETURNS TEXT
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'explain_diff_sql_wrapper';

-- STAB-4: pgtrickle_refresh_stats() — per-table refresh timing statistics
CREATE OR REPLACE FUNCTION pgtrickle."pgtrickle_refresh_stats"() RETURNS TABLE (
    "stream_table" TEXT,
    "mode" TEXT,
    "avg_ms" double precision,
    "p95_ms" double precision,
    "p99_ms" double precision,
    "refresh_count" bigint,
    "last_refresh_at" timestamp with time zone
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pgtrickle_refresh_stats_wrapper';

-- Record the schema version for the upgrade chain.
INSERT INTO pgtrickle.pgt_schema_version (version, description, installed_at)
VALUES ('0.23.0', 'TPC-H DVM Scaling Performance', now())
ON CONFLICT (version) DO NOTHING;
