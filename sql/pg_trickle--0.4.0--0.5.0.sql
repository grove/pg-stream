-- pg_trickle 0.4.0 -> 0.5.0 upgrade script
--
-- v0.5.0 adds:
--   Phase 1: Row-Level Security (RLS) passthrough for stream tables.
--   Phase 2: RLS-aware refresh executor.
--   Phase 3 (Bootstrap Source Gating): gate_source() / ungate_source() /
--            source_gates() + scheduler skip logic.
--   Phase 5: Append-only INSERT fast path (MERGE bypass).
--
-- New catalog table: pgtrickle.pgt_source_gates
-- Tracks which source tables are currently "gated" (bootstrapping in progress).
-- When a source is gated the scheduler skips all stream tables that depend on
-- it, logging SKIP+SKIPPED in pgt_refresh_history, until ungate_source() is
-- called.

-- Bootstrap source gates (Phase 3)
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_source_gates (
    source_relid    OID PRIMARY KEY,
    gated           BOOLEAN NOT NULL DEFAULT true,
    gated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    ungated_at      TIMESTAMPTZ,
    gated_by        TEXT
);

-- Phase 5: Append-only INSERT fast path
ALTER TABLE pgtrickle.pgt_stream_tables
  ADD COLUMN IF NOT EXISTS is_append_only BOOLEAN NOT NULL DEFAULT FALSE;

-- Phase 5: Re-create create_stream_table with new append_only parameter
DROP FUNCTION IF EXISTS pgtrickle."create_stream_table"(text, text, text, text, bool, text, text, text);

CREATE FUNCTION pgtrickle."create_stream_table"(
    "name"                    TEXT,
    "query"                   TEXT,
    "schedule"                TEXT    DEFAULT 'calculated',
    "refresh_mode"            TEXT    DEFAULT 'AUTO',
    "initialize"              bool    DEFAULT true,
    "diamond_consistency"     TEXT    DEFAULT NULL,
    "diamond_schedule_policy" TEXT    DEFAULT NULL,
    "cdc_mode"                TEXT    DEFAULT NULL,
    "append_only"             bool    DEFAULT false
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_wrapper';

-- Phase 5: Re-create alter_stream_table with new append_only parameter
DROP FUNCTION IF EXISTS pgtrickle."alter_stream_table"(text, text, text, text, text, text, text, text);

CREATE FUNCTION pgtrickle."alter_stream_table"(
    "name"                    TEXT,
    "query"                   TEXT    DEFAULT NULL,
    "schedule"                TEXT    DEFAULT NULL,
    "refresh_mode"            TEXT    DEFAULT NULL,
    "status"                  TEXT    DEFAULT NULL,
    "diamond_consistency"     TEXT    DEFAULT NULL,
    "diamond_schedule_policy" TEXT    DEFAULT NULL,
    "cdc_mode"                TEXT    DEFAULT NULL,
    "append_only"             bool    DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'alter_stream_table_wrapper';

-- Phase 3: gate_source() — mark a source as gated, notify scheduler
CREATE OR REPLACE FUNCTION pgtrickle."gate_source"(
    "source" TEXT
) RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'gate_source_wrapper';

-- Phase 3: ungate_source() — clear the gate, notify scheduler
CREATE OR REPLACE FUNCTION pgtrickle."ungate_source"(
    "source" TEXT
) RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'ungate_source_wrapper';

-- Phase 3: source_gates() — introspection table function
CREATE OR REPLACE FUNCTION pgtrickle."source_gates"() RETURNS TABLE (
    "source_table" TEXT,
    "schema_name"  TEXT,
    "gated"        bool,
    "gated_at"     timestamp with time zone,
    "ungated_at"   timestamp with time zone,
    "gated_by"     TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'source_gates_fn_wrapper';

-- Phase 4: create_stream_table_if_not_exists() — idempotent creation wrapper
CREATE OR REPLACE FUNCTION pgtrickle."create_stream_table_if_not_exists"(
    "name"                    TEXT,
    "query"                   TEXT,
    "schedule"                TEXT    DEFAULT 'calculated',
    "refresh_mode"            TEXT    DEFAULT 'AUTO',
    "initialize"              bool    DEFAULT true,
    "diamond_consistency"     TEXT    DEFAULT NULL,
    "diamond_schedule_policy" TEXT    DEFAULT NULL,
    "cdc_mode"                TEXT    DEFAULT NULL,
    "append_only"             bool    DEFAULT false
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_if_not_exists_wrapper';

-- Phase 4 (ERG-E): quick_health view — single-row health summary
CREATE OR REPLACE VIEW pgtrickle.quick_health AS
SELECT
    (SELECT count(*) FROM pgtrickle.pgt_stream_tables)::bigint
        AS total_stream_tables,
    (SELECT count(*) FROM pgtrickle.pgt_stream_tables
     WHERE status = 'ERROR' OR consecutive_errors > 0)::bigint
        AS error_tables,
    (SELECT count(*) FROM pgtrickle.pgt_stream_tables
     WHERE schedule IS NOT NULL
       AND schedule !~ '[\s@]'
       AND data_timestamp IS NOT NULL
       AND EXTRACT(EPOCH FROM (now() - data_timestamp)) >
           pgtrickle.parse_duration_seconds(schedule))::bigint
        AS stale_tables,
    (SELECT count(*) > 0 FROM pg_stat_activity
     WHERE backend_type = 'pg_trickle scheduler')
        AS scheduler_running,
    CASE
        WHEN (SELECT count(*) FROM pgtrickle.pgt_stream_tables) = 0 THEN 'EMPTY'
        WHEN (SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE status = 'SUSPENDED') > 0 THEN 'CRITICAL'
        WHEN (SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE status = 'ERROR' OR consecutive_errors > 0) > 0 THEN 'WARNING'
        WHEN (SELECT count(*) FROM pgtrickle.pgt_stream_tables
              WHERE schedule IS NOT NULL
                AND schedule !~ '[\s@]'
                AND data_timestamp IS NOT NULL
                AND EXTRACT(EPOCH FROM (now() - data_timestamp)) >
                    pgtrickle.parse_duration_seconds(schedule)) > 0 THEN 'WARNING'
        ELSE 'OK'
    END AS status;
