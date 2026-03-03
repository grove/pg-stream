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

-- ── Placeholder: no schema changes in this version ───────────────────
-- This migration is a no-op. Remove this line and uncomment the DDL
-- above when actual schema changes are needed for 0.2.0.

SELECT 'pg_trickle upgrade 0.1.3 → 0.2.0: no-op (template)';
