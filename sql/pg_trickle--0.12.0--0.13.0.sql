-- pg_trickle 0.12.0 -> 0.13.0 upgrade script
--
-- v0.13.0: Scalability Foundations, Partitioning Enhancements,
--          MERGE Profiling & Multi-Tenant Scheduling
--
-- Phase 3 (MERGE profiling):
--   PROF-DLT: pgtrickle.explain_delta(name, format) SQL function.
--   G14-MDED: pgtrickle.dedup_stats() SQL function.
--
-- Phase 5 (partitioning enhancements):
--   A1-1c: partition_by parameter on alter_stream_table.
--   Catalog columns: effective_refresh_mode, fuse_*, st_partition_key.
--
-- Phase 6 (columnar change tracking / A-2):
--   No catalog DDL changes. Columnar change tracking, key column
--   classification, __pgt_key_changed annotation, P5 value-only
--   fast path, and MERGE D-side filtering are all implemented in
--   Rust (delta SQL generation and MERGE template building).
--
-- Phase 9 (multi-tenant scheduler):
--   C-3: per_database_worker_quota GUC. No DDL required.
--
-- Developer tooling (DT-1 through DT-4):
--   explain_query_rewrite, diagnose_errors, list_auxiliary_columns,
--   validate_query, explain_refresh_mode.
--
-- Fuse support:
--   reset_fuse, fuse_status. Catalog columns for fuse state tracking.

-- ── Catalog column additions (idempotent) ─────────────────────────────────
-- These columns may already exist if the user upgraded from 0.11.0 → 0.12.0
-- via the 0.11.0→0.12.0 upgrade path (which added them). For fresh 0.12.0
-- installs from the archive, they need to be added here.

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS effective_refresh_mode TEXT;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS fuse_mode TEXT NOT NULL DEFAULT 'off';

-- Cannot use ADD COLUMN IF NOT EXISTS with CHECK inline in all PG versions,
-- so add constraint separately.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'pgtrickle'
          AND table_name = 'pgt_stream_tables'
          AND column_name = 'fuse_mode'
    ) THEN
        -- Column was just added above; constraint will be added below.
        NULL;
    END IF;
    -- Add CHECK constraint if it doesn't exist yet.
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'pgt_stream_tables_fuse_mode_check'
          AND conrelid = 'pgtrickle.pgt_stream_tables'::regclass
    ) THEN
        ALTER TABLE pgtrickle.pgt_stream_tables
            ADD CONSTRAINT pgt_stream_tables_fuse_mode_check
            CHECK (fuse_mode IN ('off', 'on', 'auto'));
    END IF;
END $$;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS fuse_state TEXT NOT NULL DEFAULT 'armed';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'pgt_stream_tables_fuse_state_check'
          AND conrelid = 'pgtrickle.pgt_stream_tables'::regclass
    ) THEN
        ALTER TABLE pgtrickle.pgt_stream_tables
            ADD CONSTRAINT pgt_stream_tables_fuse_state_check
            CHECK (fuse_state IN ('armed', 'blown', 'disabled'));
    END IF;
END $$;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS fuse_ceiling BIGINT;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS fuse_sensitivity INT;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS blown_at TIMESTAMPTZ;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS blow_reason TEXT;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS st_partition_key TEXT;

-- DI-7: Strategy selector columns for automatic FULL fallback
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS max_differential_joins INT;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS max_delta_fraction DOUBLE PRECISION;

-- ── Developer diagnostic functions (DT-1 through DT-4) ───────────────────

-- DT-1: explain_query_rewrite
CREATE OR REPLACE FUNCTION pgtrickle."explain_query_rewrite"(
        "query" TEXT
) RETURNS TABLE (
        "pass_name" TEXT,
        "changed" bool,
        "sql_after" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'explain_query_rewrite_wrapper';

-- DT-2: diagnose_errors
CREATE OR REPLACE FUNCTION pgtrickle."diagnose_errors"(
        "name" TEXT
) RETURNS TABLE (
        "event_time" timestamp with time zone,
        "error_type" TEXT,
        "error_message" TEXT,
        "remediation" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'diagnose_errors_wrapper';

-- DT-3: list_auxiliary_columns
CREATE OR REPLACE FUNCTION pgtrickle."list_auxiliary_columns"(
        "name" TEXT
) RETURNS TABLE (
        "column_name" TEXT,
        "data_type" TEXT,
        "purpose" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'list_auxiliary_columns_wrapper';

-- DT-4: validate_query
CREATE OR REPLACE FUNCTION pgtrickle."validate_query"(
        "query" TEXT
) RETURNS TABLE (
        "check_name" TEXT,
        "result" TEXT,
        "severity" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'validate_query_wrapper';

-- ── Explain refresh mode ──────────────────────────────────────────────────

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

-- ── Fuse support functions ────────────────────────────────────────────────

CREATE OR REPLACE FUNCTION pgtrickle."reset_fuse"(
        "name" TEXT,
        "action" TEXT DEFAULT 'apply'
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'reset_fuse_wrapper';

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

-- ── Phase 3: MERGE Profiling Functions ────────────────────────────────────

-- PROF-DLT: explain_delta — capture EXPLAIN output for auto-generated delta SQL.
CREATE OR REPLACE FUNCTION pgtrickle."explain_delta"(
        "name" TEXT,
        "format" TEXT DEFAULT 'text'
) RETURNS SETOF TEXT
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'explain_delta_text_wrapper';

-- G14-MDED: dedup_stats — deduplication frequency monitoring.
CREATE OR REPLACE FUNCTION pgtrickle."dedup_stats"() RETURNS TABLE (
        "total_diff_refreshes" bigint,
        "dedup_needed" bigint,
        "dedup_ratio_pct" double precision
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'dedup_stats_fn_wrapper';

-- ── A1-1c: ALTER partition_by support ─────────────────────────────────────
-- Drop all historical overloads and recreate with the full 0.13.0 signature.
-- The 0.12.0 archive has an 11-param version; older upgrade paths may have
-- left a 14-param or 15-param overload. All are replaced by the 17-param
-- signature that includes max_differential_joins and max_delta_fraction.
DROP FUNCTION IF EXISTS pgtrickle."alter_stream_table"(TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, bool, bool, TEXT);
DROP FUNCTION IF EXISTS pgtrickle."alter_stream_table"(TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, bool, bool, TEXT, TEXT, bigint, INT);
DROP FUNCTION IF EXISTS pgtrickle."alter_stream_table"(TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, bool, bool, TEXT, TEXT, bigint, INT, TEXT);
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
        "fuse_sensitivity" INT DEFAULT NULL,
        "partition_by" TEXT DEFAULT NULL,
        "max_differential_joins" INT DEFAULT NULL,
        "max_delta_fraction" double precision DEFAULT NULL
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'alter_stream_table_wrapper';

-- ── D-4: Shared change buffer observability ───────────────────────────────
CREATE FUNCTION pgtrickle."shared_buffer_stats"() RETURNS TABLE (
        "source_oid" bigint,
        "source_table" text,
        "consumer_count" integer,
        "consumers" text,
        "columns_tracked" integer,
        "safe_frontier_lsn" text,
        "buffer_rows" bigint,
        "is_partitioned" boolean
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'shared_buffer_stats_fn_wrapper';

