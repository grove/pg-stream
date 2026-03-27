-- pg_trickle 0.10.0 -> 0.11.0 upgrade script
--
-- Changes:
--   G12-ERM-1: Add effective_refresh_mode column to pgt_stream_tables.
--              Populated by the scheduler after each completed refresh to
--              record the mode that was actually used (FULL, DIFFERENTIAL,
--              APPEND_ONLY, TOP_K, NO_DATA).
--   WB-1: changed_cols column in CDC change buffer tables migrated from
--         BIGINT to VARBIT. The migration is performed at runtime by the
--         extension when each change buffer is first accessed after upgrade
--         (via alter_change_buffer_add_columns / rebuild_change_trigger_with_columns).
--         No explicit ALTER TABLE is required here because change buffer
--         tables live in the pgtrickle_changes schema and are managed
--         per-source-table by the Rust code.
--
--   FUSE-1: Add fuse circuit breaker columns to pgt_stream_tables.
--           fuse_mode, fuse_state, fuse_ceiling, fuse_sensitivity,
--           blown_at, blow_reason.
--
--   WAKE-1: CDC trigger functions now include PERFORM pg_notify('pgtrickle_wake', '')
--           to enable event-driven scheduler wake. Existing trigger functions
--           are rebuilt at runtime by the extension (via rebuild_cdc_trigger_function)
--           on the first refresh cycle after upgrade, which uses CREATE OR REPLACE.
--           The scheduler issues LISTEN pgtrickle_wake at startup when
--           pg_trickle.event_driven_wake = true (the default).
--
--   A1-1: Add st_partition_key column to pgt_stream_tables.
--         Stores the user-declared partition key column name for partitioned
--         stream tables. NULL = not partitioned. Used by the refresh path to
--         inject a partition-key range predicate into the MERGE ON clause for
--         partition pruning (A1-3). All existing stream tables keep NULL.

-- ── Schema Changes ─────────────────────────────────────────────────────────

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS effective_refresh_mode TEXT;

-- FUSE-1: Fuse circuit breaker columns
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS fuse_mode TEXT NOT NULL DEFAULT 'off';
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS fuse_state TEXT NOT NULL DEFAULT 'armed';
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS fuse_ceiling BIGINT;
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS fuse_sensitivity INT;
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS blown_at TIMESTAMPTZ;
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS blow_reason TEXT;

-- Add CHECK constraints for fuse columns (safe for existing data since defaults satisfy them)
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'pgt_stream_tables_fuse_mode_check'
          AND conrelid = 'pgtrickle.pgt_stream_tables'::regclass
    ) THEN
        ALTER TABLE pgtrickle.pgt_stream_tables
            ADD CONSTRAINT pgt_stream_tables_fuse_mode_check
            CHECK (fuse_mode IN ('off', 'on', 'auto'));
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'pgt_stream_tables_fuse_state_check'
          AND conrelid = 'pgtrickle.pgt_stream_tables'::regclass
    ) THEN
        ALTER TABLE pgtrickle.pgt_stream_tables
            ADD CONSTRAINT pgt_stream_tables_fuse_state_check
            CHECK (fuse_state IN ('armed', 'blown', 'disabled'));
    END IF;
END
$$;

-- A1-1: Partition key column for partitioned stream tables.
--       NULL = not partitioned (all existing tables keep NULL).
--       When set, the stream table storage was created as PARTITION BY RANGE
--       on this column, and the refresh path will inject a partition-key
--       range predicate to enable partition pruning during MERGE.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS st_partition_key TEXT;

-- DAG-4: Widen pgt_scheduler_jobs.unit_kind CHECK to include new kinds.
-- The table may be absent on fresh installs (created by _PG_init) but
-- exists on upgrades.  Safe to run unconditionally.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'pgtrickle' AND c.relname = 'pgt_scheduler_jobs'
    ) THEN
        -- Drop the old CHECK and add a wider one.
        ALTER TABLE pgtrickle.pgt_scheduler_jobs
            DROP CONSTRAINT IF EXISTS pgt_scheduler_jobs_unit_kind_check;
        ALTER TABLE pgtrickle.pgt_scheduler_jobs
            ADD CONSTRAINT pgt_scheduler_jobs_unit_kind_check
            CHECK (unit_kind IN ('singleton', 'atomic_group', 'immediate_closure',
                                 'cyclic_scc', 'repeatable_read_group', 'fused_chain'));
    END IF;
END
$$;

-- ── Updated Function Signatures ───────────────────────────────────────────
--
--   A1-1: create_stream_table / create_stream_table_if_not_exists /
--         create_or_replace_stream_table — added "partition_by" parameter.
--   FUSE-1: alter_stream_table — added "fuse", "fuse_ceiling",
--           "fuse_sensitivity" parameters.
--
--   Each function must be dropped-and-recreated so the new C wrapper
--   receives the correct argument count at runtime (no PostgreSQL mechanism
--   allows in-place extension of a function's argument list).

-- A1-1: create_stream_table
DROP FUNCTION IF EXISTS pgtrickle."create_stream_table"(TEXT, TEXT, TEXT, TEXT, bool, TEXT, TEXT, TEXT, bool, bool);
CREATE FUNCTION pgtrickle."create_stream_table"(
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
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'create_stream_table_wrapper';

-- A1-1: create_stream_table_if_not_exists
DROP FUNCTION IF EXISTS pgtrickle."create_stream_table_if_not_exists"(TEXT, TEXT, TEXT, TEXT, bool, TEXT, TEXT, TEXT, bool, bool);
CREATE FUNCTION pgtrickle."create_stream_table_if_not_exists"(
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
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'create_stream_table_if_not_exists_wrapper';

-- A1-1: create_or_replace_stream_table
DROP FUNCTION IF EXISTS pgtrickle."create_or_replace_stream_table"(TEXT, TEXT, TEXT, TEXT, bool, TEXT, TEXT, TEXT, bool, bool);
CREATE FUNCTION pgtrickle."create_or_replace_stream_table"(
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
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'create_or_replace_stream_table_wrapper';

-- FUSE-1: alter_stream_table
DROP FUNCTION IF EXISTS pgtrickle."alter_stream_table"(TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, bool, bool, TEXT);
CREATE FUNCTION pgtrickle."alter_stream_table"(
        "name" TEXT,
        "query" TEXT DEFAULT NULL,
        "schedule" TEXT DEFAULT NULL,
        "refresh_mode" TEXT DEFAULT NULL,
        "status" TEXT DEFAULT NULL,
        "diamond_consistency" TEXT DEFAULT NULL,
        "diamond_schedule_policy" TEXT DEFAULT NULL,
        "cdc_mode" TEXT DEFAULT NULL,
        "append_only" bool DEFAULT NULL,
        "pooler_compatibility_mode" bool DEFAULT NULL,
        "tier" TEXT DEFAULT NULL,
        "fuse" TEXT DEFAULT NULL,
        "fuse_ceiling" bigint DEFAULT NULL,
        "fuse_sensitivity" INT DEFAULT NULL
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'alter_stream_table_wrapper';

-- ── New Functions ──────────────────────────────────────────────────────────

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
