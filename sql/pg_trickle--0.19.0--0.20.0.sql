-- pg_trickle 0.19.0 → 0.20.0 upgrade migration
-- ============================================
--
-- PERF-1: Add index on pgt_refresh_history(pgt_id, start_time) for self-monitoring queries.
-- DF-G3: Add 'SELF_MONITOR' to initiated_by CHECK constraint.

-- ── PERF-1: Index on (pgt_id, start_time) ──────────────────────────────────
-- Required by all five self-monitoring stream tables which filter on
-- start_time > now() - interval '1 hour' grouped by pgt_id.
CREATE INDEX IF NOT EXISTS idx_hist_pgt_start
    ON pgtrickle.pgt_refresh_history (pgt_id, start_time);

-- ── DF-G3: Extend initiated_by CHECK to include 'SELF_MONITOR' ────────────────
-- The auto-apply worker logs threshold changes with initiated_by = 'SELF_MONITOR'.
DO $$
BEGIN
    -- Drop existing CHECK constraint on initiated_by column.
    EXECUTE (
        SELECT format('ALTER TABLE pgtrickle.pgt_refresh_history DROP CONSTRAINT %I',
                       conname)
        FROM pg_constraint
        WHERE conrelid = 'pgtrickle.pgt_refresh_history'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) ILIKE '%initiated_by%'
        LIMIT 1
    );
EXCEPTION WHEN undefined_table OR undefined_object THEN
    NULL;
END $$;

ALTER TABLE pgtrickle.pgt_refresh_history
    ADD CONSTRAINT pgt_refresh_history_initiated_by_check
    CHECK (initiated_by IN ('SCHEDULER', 'MANUAL', 'INITIAL', 'SELF_MONITOR'));

-- ── New functions in 0.20.0 ────────────────────────────────────────────────
-- These functions were added in 0.19.0 (migrate, version_check, write_and_refresh)
-- and 0.20.0 (self-monitoring API). Use CREATE OR REPLACE so the script is
-- idempotent for upgrade chains that already passed through 0.18→0.19.

CREATE OR REPLACE FUNCTION pgtrickle."migrate"() RETURNS TEXT
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'migrate_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."version_check"() RETURNS TEXT
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'version_check_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."write_and_refresh"(
    "sql" TEXT,
    "stream_table_name" TEXT
) RETURNS void
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'write_and_refresh_wrapper';

-- ── DF: Dog-feeding API (0.20.0) ───────────────────────────────────────────

CREATE OR REPLACE FUNCTION pgtrickle."setup_dog_feeding"() RETURNS void
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'setup_dog_feeding_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."teardown_dog_feeding"() RETURNS void
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'teardown_dog_feeding_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."dog_feeding_status"() RETURNS TABLE (
    "st_name" TEXT,
    "exists" bool,
    "status" TEXT,
    "refresh_mode" TEXT,
    "last_refresh_at" TEXT,
    "total_refreshes" bigint
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'dog_feeding_status_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."scheduler_overhead"() RETURNS TABLE (
    "total_refreshes_1h" bigint,
    "df_refreshes_1h" bigint,
    "df_refresh_fraction" float8,
    "avg_refresh_ms" float8,
    "avg_df_refresh_ms" float8,
    "total_refresh_time_s" float8,
    "df_refresh_time_s" float8
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'scheduler_overhead_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."explain_dag"(
    "format" TEXT DEFAULT 'mermaid'
) RETURNS TEXT
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'explain_dag_wrapper';