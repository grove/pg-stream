-- pg_trickle 0.5.0 -> 0.6.0 upgrade script
--
-- v0.6.0 adds:
--   Idempotent DDL: create_or_replace_stream_table() — declarative
--   stream table deployment that creates, no-ops, or alters as needed.
--   CYC-3: Circular dependency foundation catalog columns.
--     - scc_id on pgt_stream_tables (SCC group identifier)
--     - fixpoint_iteration on pgt_refresh_history
--   BOOT-F3: bootstrap_gate_status() introspection function.

-- Idempotent DDL: create_or_replace_stream_table()
CREATE FUNCTION pgtrickle."create_or_replace_stream_table"(
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
AS 'MODULE_PATHNAME', 'create_or_replace_stream_table_wrapper';

-- CYC-3: SCC identifier for circular dependency tracking.
-- NULL means the stream table is not part of a cyclic SCC.
-- All members of the same cycle share the same scc_id value.
ALTER TABLE pgtrickle.pgt_stream_tables
  ADD COLUMN IF NOT EXISTS scc_id INT;

-- CYC-3: Fixpoint iteration counter for refresh history.
-- Records which iteration of the fixed-point loop produced this refresh.
-- NULL for non-cyclic refreshes.
ALTER TABLE pgtrickle.pgt_refresh_history
  ADD COLUMN IF NOT EXISTS fixpoint_iteration INT;

-- BOOT-F3: bootstrap_gate_status() introspection function.
-- Rich view of gate lifecycle including duration and affected stream tables.
CREATE FUNCTION pgtrickle."bootstrap_gate_status"()
RETURNS TABLE (
    "source_table"            TEXT,
    "schema_name"             TEXT,
    "gated"                   BOOLEAN,
    "gated_at"                TIMESTAMPTZ,
    "ungated_at"              TIMESTAMPTZ,
    "gated_by"                TEXT,
    "gate_duration"           INTERVAL,
    "affected_stream_tables"  TEXT
)
LANGUAGE c
AS 'MODULE_PATHNAME', 'bootstrap_gate_status_fn_wrapper';
