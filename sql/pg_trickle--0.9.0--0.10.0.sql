-- pg_trickle 0.9.0 -> 0.10.0 upgrade script
--
-- Changes:
--   PB2: Add pooler_compatibility_mode column and update API function signatures.
--   Refresh groups: New create_refresh_group, drop_refresh_group, refresh_groups functions.

-- ── Schema Changes ─────────────────────────────────────────────────────────

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS pooler_compatibility_mode BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_refresh_groups (
    group_id    SERIAL PRIMARY KEY,
    group_name  TEXT NOT NULL UNIQUE,
    member_oids OID[] NOT NULL,
    isolation   TEXT NOT NULL DEFAULT 'read_committed'
                CHECK (isolation IN ('read_committed', 'repeatable_read')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Updated API Functions (new pooler_compatibility_mode parameter) ─────────

-- The old 9-arg signatures must be dropped before creating the 10-arg versions.
-- PostgreSQL treats different arities as different functions.

DROP FUNCTION IF EXISTS pgtrickle."create_stream_table"(TEXT, TEXT, TEXT, TEXT, bool, TEXT, TEXT, TEXT, bool);

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
    "pooler_compatibility_mode" bool DEFAULT false
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_wrapper';

DROP FUNCTION IF EXISTS pgtrickle."create_stream_table_if_not_exists"(TEXT, TEXT, TEXT, TEXT, bool, TEXT, TEXT, TEXT, bool);

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
    "pooler_compatibility_mode" bool DEFAULT false
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_if_not_exists_wrapper';

DROP FUNCTION IF EXISTS pgtrickle."create_or_replace_stream_table"(TEXT, TEXT, TEXT, TEXT, bool, TEXT, TEXT, TEXT, bool);

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
    "pooler_compatibility_mode" bool DEFAULT false
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_or_replace_stream_table_wrapper';

DROP FUNCTION IF EXISTS pgtrickle."alter_stream_table"(TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, bool);

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
    "pooler_compatibility_mode" bool DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'alter_stream_table_wrapper';

-- ── New Functions ──────────────────────────────────────────────────────────

CREATE FUNCTION pgtrickle."create_refresh_group"(
    "group_name" TEXT,
    "members" TEXT[],
    "isolation" TEXT DEFAULT 'read_committed'
) RETURNS INT
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_refresh_group_wrapper';

CREATE FUNCTION pgtrickle."drop_refresh_group"(
    "group_name" TEXT
) RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'drop_refresh_group_wrapper';

CREATE FUNCTION pgtrickle."refresh_groups"() RETURNS TABLE (
    "group_id" INT,
    "group_name" TEXT,
    "member_count" INT,
    "isolation" TEXT,
    "created_at" timestamp with time zone
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'refresh_groups_fn_wrapper';
