-- pg_trickle upgrade migration: 0.1.3 → 0.2.0
--
-- This is a TEMPLATE migration file. It becomes active when:
--   1. The extension version in Cargo.toml is bumped to 0.2.0
--   2. This file is placed in the extension directory
--   3. `ALTER EXTENSION pg_trickle UPDATE TO '0.2.0'` is run
--
-- Naming convention: pg_trickle--<from>--<to>.sql
--
-- Guidelines (from plans/sql/PLAN_UPGRADE_MIGRATIONS.md):
--   • Use idempotent DDL (IF NOT EXISTS / DO $$ IF NOT EXISTS $$)
--   • Never touch pgtrickle_changes.* tables — they are ephemeral
--   • Keep each migration self-contained and forward-only
--   • Rollback = DROP EXTENSION + CREATE EXTENSION (destructive)

-- ── Example: add columns to catalog table ────────────────────────────
-- Uncomment and adapt when the v0.2.0 schema is finalized.

-- DO $$
-- BEGIN
--     -- Add cdc_mode column (per-ST CDC mode override)
--     IF NOT EXISTS (
--         SELECT 1 FROM information_schema.columns
--         WHERE table_schema = 'pgtrickle'
--           AND table_name = 'pgt_stream_tables'
--           AND column_name = 'cdc_mode'
--     ) THEN
--         ALTER TABLE pgtrickle.pgt_stream_tables
--             ADD COLUMN cdc_mode TEXT NOT NULL DEFAULT 'trigger'
--             CHECK (cdc_mode IN ('trigger', 'wal'));
--     END IF;
--
--     -- Add last_error column (last error message for ERROR status)
--     IF NOT EXISTS (
--         SELECT 1 FROM information_schema.columns
--         WHERE table_schema = 'pgtrickle'
--           AND table_name = 'pgt_stream_tables'
--           AND column_name = 'last_error'
--     ) THEN
--         ALTER TABLE pgtrickle.pgt_stream_tables
--             ADD COLUMN last_error TEXT;
--     END IF;
-- END
-- $$;

-- ── New functions in 0.2.0 ────────────────────────────────────────────

-- Monitoring: list source tables for a given stream table
CREATE OR REPLACE FUNCTION pgtrickle."list_sources"(
        "name" TEXT
) RETURNS TABLE (
        "source_table" TEXT,
        "source_oid" bigint,
        "source_type" TEXT,
        "cdc_mode" TEXT,
        "columns_used" TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'list_sources_wrapper';

-- Monitoring: inspect CDC change buffer sizes per stream table
CREATE OR REPLACE FUNCTION pgtrickle."change_buffer_sizes"() RETURNS TABLE (
        "stream_table" TEXT,
        "source_table" TEXT,
        "source_oid" bigint,
        "cdc_mode" TEXT,
        "pending_rows" bigint,
        "buffer_bytes" bigint
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'change_buffer_sizes_wrapper';

-- Internal: signal the launcher background worker to rescan databases
-- immediately (bypasses the skip_ttl cache after CREATE EXTENSION).
CREATE OR REPLACE FUNCTION pgtrickle."_signal_launcher_rescan"() RETURNS void
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', '_signal_launcher_rescan_wrapper';

SELECT 'pg_trickle upgrade 0.1.3 → 0.2.0: added list_sources, change_buffer_sizes, _signal_launcher_rescan';
