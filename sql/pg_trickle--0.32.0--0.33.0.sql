-- pg_trickle 0.32.0 → 0.33.0 upgrade migration
-- ============================================
--
-- v0.33.0 — CITUS-7: output_distribution_column parameter
--
-- Changes in this version:
--   - CITUS-7: Add `output_distribution_column TEXT DEFAULT NULL` parameter to
--              pgtrickle.create_stream_table(), create_stream_table_if_not_exists(),
--              and create_or_replace_stream_table().  When the parameter is non-NULL
--              and Citus is loaded, the output storage table is converted to a
--              Citus distributed table on that column immediately after creation.
--              Pass 's' to co-locate stream table output with pg_ripple VP shards.
--
-- Migration is safe to run on a live system.  The function replacements below
-- add a trailing DEFAULT NULL parameter to each function, so existing call
-- sites continue to work unchanged.

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 1: Replace create_stream_table with new signature
-- ─────────────────────────────────────────────────────────────────────────

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
        "partition_by" TEXT DEFAULT NULL,
        "max_differential_joins" INT DEFAULT NULL,
        "max_delta_fraction" double precision DEFAULT NULL,
        "output_distribution_column" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_wrapper';

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 2: Replace create_stream_table_if_not_exists with new signature
-- ─────────────────────────────────────────────────────────────────────────

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
        "partition_by" TEXT DEFAULT NULL,
        "max_differential_joins" INT DEFAULT NULL,
        "max_delta_fraction" double precision DEFAULT NULL,
        "output_distribution_column" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_if_not_exists_wrapper';

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 3: Replace create_or_replace_stream_table with new signature
-- ─────────────────────────────────────────────────────────────────────────

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
        "partition_by" TEXT DEFAULT NULL,
        "max_differential_joins" INT DEFAULT NULL,
        "max_delta_fraction" double precision DEFAULT NULL,
        "output_distribution_column" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_or_replace_stream_table_wrapper';
