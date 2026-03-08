-- pg_trickle 0.2.2 -> 0.2.3 upgrade script
--
-- CDC / refresh mode interaction gaps:
--   - G1: add requested_cdc_mode override to pgt_stream_tables
--   - G5: add cdc_modes to pg_stat_stream_tables
--   - G5: add pgt_cdc_status convenience view
--
-- Keep the monitoring views in sync for ALTER EXTENSION UPDATE.

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS requested_cdc_mode TEXT;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'pgt_stream_tables_requested_cdc_mode_check'
          AND conrelid = 'pgtrickle.pgt_stream_tables'::regclass
    ) THEN
        ALTER TABLE pgtrickle.pgt_stream_tables
            ADD CONSTRAINT pgt_stream_tables_requested_cdc_mode_check
            CHECK (requested_cdc_mode IN ('auto', 'trigger', 'wal'));
    END IF;
END
$$;

DROP FUNCTION IF EXISTS pgtrickle."create_stream_table"(text, text, text, text, bool, text, text);

CREATE FUNCTION pgtrickle."create_stream_table"(
    "name" TEXT,
    "query" TEXT,
    "schedule" TEXT DEFAULT 'calculated',
    "refresh_mode" TEXT DEFAULT 'AUTO',
    "initialize" bool DEFAULT true,
    "diamond_consistency" TEXT DEFAULT NULL,
    "diamond_schedule_policy" TEXT DEFAULT NULL,
    "cdc_mode" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_wrapper';

DROP FUNCTION IF EXISTS pgtrickle."alter_stream_table"(text, text, text, text, text, text, text);

CREATE FUNCTION pgtrickle."alter_stream_table"(
    "name" TEXT,
    "query" TEXT DEFAULT NULL,
    "schedule" TEXT DEFAULT NULL,
    "refresh_mode" TEXT DEFAULT NULL,
    "status" TEXT DEFAULT NULL,
    "diamond_consistency" TEXT DEFAULT NULL,
    "diamond_schedule_policy" TEXT DEFAULT NULL,
    "cdc_mode" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'alter_stream_table_wrapper';

CREATE OR REPLACE VIEW pgtrickle.pg_stat_stream_tables AS
SELECT
    st.pgt_id,
    st.pgt_schema,
    st.pgt_name,
    st.status,
    st.refresh_mode,
    st.is_populated,
    st.data_timestamp,
    st.schedule,
    now() - st.data_timestamp AS staleness,
    CASE WHEN st.schedule IS NOT NULL AND st.data_timestamp IS NOT NULL
              AND st.schedule !~ '[\s@]'
         THEN EXTRACT(EPOCH FROM (now() - st.data_timestamp)) >
              pgtrickle.parse_duration_seconds(st.schedule)
         ELSE NULL::boolean
    END AS stale,
    st.consecutive_errors,
    st.needs_reinit,
    st.last_refresh_at,
    COALESCE(stats.total_refreshes, 0) AS total_refreshes,
    COALESCE(stats.successful_refreshes, 0) AS successful_refreshes,
    COALESCE(stats.failed_refreshes, 0) AS failed_refreshes,
    COALESCE(stats.total_rows_inserted, 0) AS total_rows_inserted,
    COALESCE(stats.total_rows_deleted, 0) AS total_rows_deleted,
    stats.avg_duration_ms,
    stats.last_action,
    stats.last_status,
    (
        SELECT array_agg(DISTINCT d.cdc_mode ORDER BY d.cdc_mode)
        FROM pgtrickle.pgt_dependencies d
        WHERE d.pgt_id = st.pgt_id AND d.source_type = 'TABLE'
    ) AS cdc_modes
FROM pgtrickle.pgt_stream_tables st
LEFT JOIN LATERAL (
    SELECT
        count(*)::bigint AS total_refreshes,
        count(*) FILTER (WHERE h.status = 'COMPLETED')::bigint AS successful_refreshes,
        count(*) FILTER (WHERE h.status = 'FAILED')::bigint AS failed_refreshes,
        COALESCE(sum(h.rows_inserted), 0)::bigint AS total_rows_inserted,
        COALESCE(sum(h.rows_deleted), 0)::bigint AS total_rows_deleted,
        CASE WHEN count(*) FILTER (WHERE h.end_time IS NOT NULL) > 0
             THEN avg(EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000)
                  FILTER (WHERE h.end_time IS NOT NULL)
             ELSE NULL
        END::float8 AS avg_duration_ms,
        (SELECT h2.action FROM pgtrickle.pgt_refresh_history h2
         WHERE h2.pgt_id = st.pgt_id ORDER BY h2.refresh_id DESC LIMIT 1) AS last_action,
        (SELECT h2.status FROM pgtrickle.pgt_refresh_history h2
         WHERE h2.pgt_id = st.pgt_id ORDER BY h2.refresh_id DESC LIMIT 1) AS last_status,
        (SELECT h2.initiated_by FROM pgtrickle.pgt_refresh_history h2
         WHERE h2.pgt_id = st.pgt_id ORDER BY h2.refresh_id DESC LIMIT 1) AS last_initiated_by,
        (SELECT h2.freshness_deadline FROM pgtrickle.pgt_refresh_history h2
         WHERE h2.pgt_id = st.pgt_id ORDER BY h2.refresh_id DESC LIMIT 1) AS freshness_deadline
    FROM pgtrickle.pgt_refresh_history h
    WHERE h.pgt_id = st.pgt_id
) stats ON true;

CREATE OR REPLACE VIEW pgtrickle.pgt_cdc_status AS
SELECT
    st.pgt_schema,
    st.pgt_name,
    d.source_relid,
    c.relname AS source_name,
    n.nspname AS source_schema,
    d.cdc_mode,
    d.slot_name,
    d.decoder_confirmed_lsn,
    d.transition_started_at
FROM pgtrickle.pgt_dependencies d
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id
JOIN pg_class c ON c.oid = d.source_relid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE d.source_type = 'TABLE';
