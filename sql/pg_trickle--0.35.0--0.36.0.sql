-- pg_trickle 0.35.0 → 0.36.0 upgrade migration
-- All DDL is idempotent (IF NOT EXISTS / IF EXISTS / ADD COLUMN IF NOT EXISTS).

-- ── CORR-1 (v0.36.0): Temporal IVM columns ──────────────────────────────────
-- temporal_mode: whether this ST uses two-dimensional (LSN, timestamp) frontier.
-- __pgt_valid_from / __pgt_valid_to are added to the storage table itself at
-- create_stream_table() time when temporal_mode = true. Here we just track the
-- flag in the catalog.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS temporal_mode BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.temporal_mode IS
    'v0.36.0 CORR-1: TRUE when this stream table uses temporal IVM (SCD Type 2). '
    'Storage table will carry __pgt_valid_from and __pgt_valid_to columns.';

-- ── CORR-2 (v0.36.0): Storage backend column ─────────────────────────────────
-- storage_backend: which columnar/heap backend the storage table uses.
-- 'heap' = standard PostgreSQL heap (default).
-- 'citus' = Citus columnar.
-- 'pg_mooncake' = pg_mooncake columnar tables.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS storage_backend TEXT NOT NULL DEFAULT 'heap';

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.storage_backend IS
    'v0.36.0 CORR-2: Storage backend for the stream table output. '
    '''heap'' (default), ''citus'', or ''pg_mooncake''.';

-- ── F12 (v0.36.0): Column lineage JSON ───────────────────────────────────────
-- JSON array mapping each output column to its source table and column.
-- Populated at create_stream_table() time by the DVM parser.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS column_lineage JSONB;

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.column_lineage IS
    'v0.36.0 F12: JSON array [{output_col, source_table, source_col}] '
    'recording the column lineage determined at creation time.';

-- ── New functions (v0.36.0) ───────────────────────────────────────────────────
-- pgrx requires CREATE OR REPLACE to register new C functions on ALTER EXTENSION UPDATE.

-- A35: Drain mode
CREATE OR REPLACE FUNCTION pgtrickle."drain"(
    "timeout_s" INT DEFAULT 60
) RETURNS boolean
LANGUAGE c
AS 'MODULE_PATHNAME', 'drain_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."is_drained"() RETURNS boolean
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'is_drained_wrapper';

-- A25: Bulk operations
CREATE OR REPLACE FUNCTION pgtrickle."bulk_alter_stream_tables"(
    "names" TEXT[],
    "params" JSONB
) RETURNS integer
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'bulk_alter_stream_tables_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."bulk_drop_stream_tables"(
    "names" TEXT[]
) RETURNS integer
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'bulk_drop_stream_tables_wrapper';

-- F12: Column lineage
CREATE OR REPLACE FUNCTION pgtrickle."stream_table_lineage"(
    "name" TEXT
) RETURNS TABLE (
    "output_col" TEXT,
    "source_table" TEXT,
    "source_col" TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'stream_table_lineage_wrapper';

-- F11: CREATE STREAM TABLE / DROP STREAM TABLE SQL syntax helper
CREATE OR REPLACE FUNCTION pgtrickle."exec_stream_ddl"(
    "cmd" TEXT
) RETURNS boolean
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'exec_stream_ddl_wrapper';

-- v0.36.0: Extend create_stream_table with temporal + storage_backend parameters.
-- These are optional parameters appended at the end for backward compatibility.
-- The old signature still works because PostgreSQL matches parameters by position.
-- NOTE: pgrx will generate the updated C function; this stub ensures the catalog
-- function signature is updated on ALTER EXTENSION UPDATE.
DROP FUNCTION IF EXISTS pgtrickle."create_stream_table"(
    TEXT, TEXT, TEXT, TEXT, BOOLEAN, TEXT, TEXT, TEXT, BOOLEAN, BOOLEAN,
    TEXT, INT, FLOAT8, TEXT
);
CREATE OR REPLACE FUNCTION pgtrickle."create_stream_table"(
    "name" TEXT,
    "query" TEXT,
    "schedule" TEXT DEFAULT 'calculated',
    "refresh_mode" TEXT DEFAULT 'AUTO',
    "initialize" BOOLEAN DEFAULT true,
    "diamond_consistency" TEXT DEFAULT NULL,
    "diamond_schedule_policy" TEXT DEFAULT NULL,
    "cdc_mode" TEXT DEFAULT NULL,
    "append_only" BOOLEAN DEFAULT false,
    "pooler_compatibility_mode" BOOLEAN DEFAULT false,
    "partition_by" TEXT DEFAULT NULL,
    "max_differential_joins" INT DEFAULT NULL,
    "max_delta_fraction" FLOAT8 DEFAULT NULL,
    "output_distribution_column" TEXT DEFAULT NULL,
    "temporal" BOOLEAN DEFAULT false,
    "storage_backend" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_wrapper';
