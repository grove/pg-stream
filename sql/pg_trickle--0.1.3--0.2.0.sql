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

-- Monitoring: refresh timeline history
CREATE OR REPLACE FUNCTION pgtrickle."refresh_timeline"(
	"max_rows" INT DEFAULT 50
) RETURNS TABLE (
	"start_time" timestamp with time zone,
	"stream_table" TEXT,
	"action" TEXT,
	"status" TEXT,
	"rows_inserted" bigint,
	"rows_deleted" bigint,
	"duration_ms" double precision,
	"error_message" TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'refresh_timeline_wrapper';

-- IVM: handle TRUNCATE on source tables
CREATE OR REPLACE FUNCTION pgtrickle."pgt_ivm_handle_truncate"(
	"pgt_id" bigint
) RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pgt_ivm_handle_truncate_wrapper';

-- Monitoring: health check diagnostics
CREATE OR REPLACE FUNCTION pgtrickle."health_check"() RETURNS TABLE (
	"check_name" TEXT,
	"severity" TEXT,
	"detail" TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'health_check_wrapper';

-- IVM: apply delta changes from CDC buffers
CREATE OR REPLACE FUNCTION pgtrickle."pgt_ivm_apply_delta"(
	"pgt_id" bigint,
	"source_oid" INT,
	"has_new" bool,
	"has_old" bool
) RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pgt_ivm_apply_delta_wrapper';

-- Monitoring: trigger inventory for all source tables
CREATE OR REPLACE FUNCTION pgtrickle."trigger_inventory"() RETURNS TABLE (
	"source_table" TEXT,
	"source_oid" bigint,
	"trigger_name" TEXT,
	"trigger_type" TEXT,
	"present" bool,
	"enabled" bool
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'trigger_inventory_wrapper';

-- API: extension version string
CREATE OR REPLACE FUNCTION pgtrickle."version"() RETURNS TEXT
IMMUTABLE STRICT PARALLEL SAFE
LANGUAGE c
AS 'MODULE_PATHNAME', 'version_wrapper';

-- Monitoring: dependency tree visualization
CREATE OR REPLACE FUNCTION pgtrickle."dependency_tree"() RETURNS TABLE (
	"tree_line" TEXT,
	"node" TEXT,
	"node_type" TEXT,
	"depth" INT,
	"status" TEXT,
	"refresh_mode" TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'dependency_tree_wrapper';

-- API: diamond dependency groups
CREATE OR REPLACE FUNCTION pgtrickle."diamond_groups"() RETURNS TABLE (
	"group_id" INT,
	"member_name" TEXT,
	"member_schema" TEXT,
	"is_convergence" bool,
	"epoch" bigint,
	"schedule_policy" TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'diamond_groups_wrapper';

SELECT 'pg_trickle upgrade 0.1.3 → 0.2.0: added list_sources, change_buffer_sizes, _signal_launcher_rescan, refresh_timeline, pgt_ivm_handle_truncate, health_check, pgt_ivm_apply_delta, trigger_inventory, version, dependency_tree, diamond_groups';
