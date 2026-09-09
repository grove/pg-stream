-- pg_trickle 0.95.0 -> 0.96.0
-- Resource-aware defaults, bounded diagnostics, progress reporting, stable errors,
-- and predefined operator roles.

CREATE OR REPLACE FUNCTION pgtrickle.active_profile()
RETURNS TABLE (
    constraint_name TEXT,
    detected_value TEXT,
    source TEXT,
    selected_value TEXT,
    overridden BOOLEAN,
    available BOOLEAN,
    detail TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'active_profile_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle.disk_usage()
RETURNS TABLE (
    stream_table TEXT,
    relation_bytes BIGINT,
    change_buffer_bytes BIGINT,
    projected_bytes BIGINT,
    headroom_bytes BIGINT,
    pressure_state TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'disk_usage_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle.error_catalog()
RETURNS TABLE (
    error_id TEXT,
    sqlstate TEXT,
    detail TEXT,
    hint TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'error_catalog_wrapper';

CREATE OR REPLACE VIEW pgtrickle.pg_stat_progress_pgtrickle AS
SELECT
    h.refresh_id AS operation_id,
    st.pgt_schema,
    st.pgt_name,
    CASE
        WHEN h.initiated_by = 'INITIAL' THEN 'INITIAL_POPULATION'
        WHEN h.action = 'REINITIALIZE' THEN 'REPAIR'
        WHEN h.action = 'FULL' THEN 'FULL_REFRESH'
        ELSE 'DIFFERENTIAL_REFRESH'
    END AS phase,
    (COALESCE(h.rows_inserted, 0) + COALESCE(h.rows_updated, 0) +
     COALESCE(h.rows_deleted, 0))::bigint AS rows_processed,
    GREATEST(c.reltuples, 0)::bigint AS estimated_rows,
    EXTRACT(EPOCH FROM (clock_timestamp() - h.start_time))::double precision AS elapsed_seconds,
    h.start_time AS started_at,
    h.start_time AS last_progress_at,
    h.status
FROM pgtrickle.pgt_refresh_history h
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = h.pgt_id
JOIN pg_catalog.pg_class c ON c.oid = st.pgt_relid
WHERE h.status = 'RUNNING';

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'pgtrickle_reader') THEN
        CREATE ROLE pgtrickle_reader NOLOGIN NOINHERIT;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'pgtrickle_operator') THEN
        CREATE ROLE pgtrickle_operator NOLOGIN NOINHERIT;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'pgtrickle_admin') THEN
        CREATE ROLE pgtrickle_admin NOLOGIN NOINHERIT;
    END IF;
END
$$;

GRANT USAGE ON SCHEMA pgtrickle TO pgtrickle_reader, pgtrickle_operator, pgtrickle_admin;
GRANT SELECT ON pgtrickle.pg_stat_progress_pgtrickle
    TO pgtrickle_reader, pgtrickle_operator, pgtrickle_admin;
GRANT EXECUTE ON FUNCTION pgtrickle.active_profile() TO pgtrickle_reader;
GRANT EXECUTE ON FUNCTION pgtrickle.disk_usage() TO pgtrickle_reader;
GRANT EXECUTE ON FUNCTION pgtrickle.error_catalog() TO pgtrickle_reader;
GRANT pgtrickle_reader TO pgtrickle_operator;
GRANT pgtrickle_operator TO pgtrickle_admin;

GRANT EXECUTE ON FUNCTION pgtrickle.refresh_stream_table(text) TO pgtrickle_operator;
GRANT EXECUTE ON FUNCTION pgtrickle.refresh_if_stale(text, interval) TO pgtrickle_operator;
GRANT EXECUTE ON FUNCTION pgtrickle.pause_stream_table(text) TO pgtrickle_operator;
GRANT EXECUTE ON FUNCTION pgtrickle.resume_stream_table(text) TO pgtrickle_operator;
GRANT EXECUTE ON FUNCTION pgtrickle.repair_stream_table(text) TO pgtrickle_operator;
GRANT EXECUTE ON FUNCTION pgtrickle.reinitialize_stream_table(text) TO pgtrickle_operator;
GRANT EXECUTE ON FUNCTION pgtrickle.drain(integer) TO pgtrickle_operator;

GRANT EXECUTE ON FUNCTION pgtrickle.create_stream_table(
    text, text, text, text, boolean, text, text, text, boolean, boolean,
    text, integer, double precision, text, boolean, text, integer, text, text
) TO pgtrickle_admin;
GRANT EXECUTE ON FUNCTION pgtrickle.alter_stream_table(
    text, text, text, text, text, text, text, text, boolean, boolean,
    text, text, bigint, integer, text, integer, double precision, text,
    double precision, text
) TO pgtrickle_admin;
GRANT EXECUTE ON FUNCTION pgtrickle.drop_stream_table(text, boolean) TO pgtrickle_admin;

GRANT EXECUTE ON FUNCTION pgtrickle.active_profile() TO PUBLIC;
GRANT EXECUTE ON FUNCTION pgtrickle.disk_usage() TO PUBLIC;
GRANT EXECUTE ON FUNCTION pgtrickle.error_catalog() TO PUBLIC;

INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('0.96.0', 'Resource-aware defaults, bounded diagnostics, progress, errors, and roles')
ON CONFLICT (version) DO NOTHING;
