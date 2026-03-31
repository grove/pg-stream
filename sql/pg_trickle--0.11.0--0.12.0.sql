-- pg_trickle 0.11.0 -> 0.12.0 upgrade script
--
-- Phase 1 (quick wins):
--   PERF-3: tiered_scheduling GUC default changed to true. No DDL required.
--
-- Phase 5 (scalability foundations):
--   A-2: Columnar change tracking -- per-column bitmask superset support.
--        Changes may require ALTER TABLE on existing change buffer tables;
--        handled at runtime by alter_change_buffer_add_columns().
--   D-4: Shared change buffers -- new pgt_shared_change_buffers catalog
--        table; multi-frontier cleanup coordination.
--
-- Note: Developer tooling functions (DT-1 through DT-4) and the updated
-- alter_stream_table signature were released in 0.13.0, not 0.12.0.
-- They are handled by the 0.12.0→0.13.0 upgrade script.
--
-- ── Phase 0: Ensure 0.11.0 Functions Are Present ─────────────────────────────
-- (These functions should have been in v0.11.0 and need to be carried forward)

-- FUSE-1: reset_fuse — resets a blown fuse on a stream table.
CREATE OR REPLACE FUNCTION pgtrickle."reset_fuse"(
        "name" TEXT,
        "action" TEXT DEFAULT 'apply'
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'reset_fuse_wrapper';

-- FUSE-1: fuse_status — returns fuse circuit breaker status for all stream tables.
CREATE OR REPLACE FUNCTION pgtrickle."fuse_status"() RETURNS TABLE (
        "stream_table" TEXT,
        "fuse_mode" TEXT,
        "fuse_state" TEXT,
        "fuse_ceiling" bigint,
        "effective_ceiling" bigint,
        "fuse_sensitivity" INT,
        "blown_at" timestamp with time zone,
        "blow_reason" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'fuse_status_wrapper';

-- G12-ERM-1: explain_refresh_mode — explains the effective refresh mode for a stream table.
CREATE OR REPLACE FUNCTION pgtrickle."explain_refresh_mode"(
        "name" TEXT
) RETURNS TABLE (
        "configured_mode" TEXT,
        "effective_mode" TEXT,
        "downgrade_reason" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'explain_refresh_mode_wrapper';

-- ── Phase 1.5: Update Stream Table Creation Functions ───────────────────────────
-- (Ensure partition_by parameter is present on stream table creation functions)
-- Note: the 0.10.0→0.11.0 upgrade already added partition_by, but a fresh
-- 0.11.0 install from the archived SQL did not include it. We use
-- CREATE OR REPLACE to handle both cases safely.

CREATE OR REPLACE FUNCTION pgtrickle."create_stream_table"(
        "name" TEXT,
        "query" TEXT,
        "schedule" TEXT DEFAULT 'calculated',
        "refresh_mode" TEXT DEFAULT 'AUTO',
        "initialize" bool DEFAULT true,
        "diamond_consistency" TEXT DEFAULT NULL,
        "diamond_schedule_policy" TEXT DEFAULT NULL,
        "cdc_mode" TEXT DEFAULT NULL,
        "append_only" bool DEFAULT false,
        "pooler_compatibility_mode" bool DEFAULT false,
        "partition_by" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."create_stream_table_if_not_exists"(
        "name" TEXT,
        "query" TEXT,
        "schedule" TEXT DEFAULT 'calculated',
        "refresh_mode" TEXT DEFAULT 'AUTO',
        "initialize" bool DEFAULT true,
        "diamond_consistency" TEXT DEFAULT NULL,
        "diamond_schedule_policy" TEXT DEFAULT NULL,
        "cdc_mode" TEXT DEFAULT NULL,
        "append_only" bool DEFAULT false,
        "pooler_compatibility_mode" bool DEFAULT false,
        "partition_by" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_if_not_exists_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."create_or_replace_stream_table"(
        "name" TEXT,
        "query" TEXT,
        "schedule" TEXT DEFAULT 'calculated',
        "refresh_mode" TEXT DEFAULT 'AUTO',
        "initialize" bool DEFAULT true,
        "diamond_consistency" TEXT DEFAULT NULL,
        "diamond_schedule_policy" TEXT DEFAULT NULL,
        "cdc_mode" TEXT DEFAULT NULL,
        "append_only" bool DEFAULT false,
        "pooler_compatibility_mode" bool DEFAULT false,
        "partition_by" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_or_replace_stream_table_wrapper';


