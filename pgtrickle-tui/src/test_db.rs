//! Test infrastructure: Testcontainers-backed `tokio_postgres::Client`
//! with pgtrickle stub functions installed.
//!
//! Each stub has the exact column names and types that the command files
//! reference.  If a stub signature drifts from the real extension, the
//! corresponding command test will fail — which is the point.
//!
//! Only compiled in `#[cfg(test)]` context (see `main.rs`).

use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio_postgres::{Client, NoTls};

/// Minimal pgtrickle stub functions.  Column names and types must match
/// exactly what each `commands/*.rs` file reads via positional `.get(N)`.
const STUB_SQL: &str = r#"
CREATE SCHEMA IF NOT EXISTS pgtrickle;

-- ── st_refresh_stats() ─────────────────────────────────────────────────────
-- Used by: list, status, watch
CREATE OR REPLACE FUNCTION pgtrickle.st_refresh_stats()
RETURNS TABLE (
    pgt_name              text,
    pgt_schema            text,
    status                text,
    refresh_mode          text,
    is_populated          bool,
    consecutive_errors    bigint,
    schedule              text,
    staleness_secs        float8,
    total_refreshes       bigint,
    successful_refreshes  bigint,
    failed_refreshes      bigint,
    avg_duration_ms       float8,
    last_refresh_at       text,
    stale                 bool
) LANGUAGE sql STABLE AS $$
    SELECT
        'test_table'::text,
        'public'::text,
        'ACTIVE'::text,
        'DIFFERENTIAL'::text,
        true,
        0::bigint,
        NULL::text,
        NULL::float8,
        5::bigint,
        5::bigint,
        0::bigint,
        42.0::float8,
        '2026-01-01 00:00:00'::text,
        false
$$;

-- ── change_buffer_sizes() ──────────────────────────────────────────────────
-- Used by: cdc
CREATE OR REPLACE FUNCTION pgtrickle.change_buffer_sizes()
RETURNS TABLE (
    stream_table  text,
    source_table  text,
    cdc_mode      text,
    pending_rows  bigint,
    buffer_bytes  bigint
) LANGUAGE sql STABLE AS $$
    SELECT
        'test_table'::text,
        'source_table'::text,
        'trigger'::text,
        0::bigint,
        0::bigint
$$;

-- ── dependency_tree() ─────────────────────────────────────────────────────
-- Used by: graph
CREATE OR REPLACE FUNCTION pgtrickle.dependency_tree()
RETURNS TABLE (
    tree_line     text,
    node          text,
    node_type     text,
    depth         int,
    status        text,
    refresh_mode  text
) LANGUAGE sql STABLE AS $$
    SELECT
        '└─ test_table'::text,
        'test_table'::text,
        'STREAM_TABLE'::text,
        0::int,
        'ACTIVE'::text,
        'DIFFERENTIAL'::text
$$;

-- ── health_check() ────────────────────────────────────────────────────────
-- Used by: health
CREATE OR REPLACE FUNCTION pgtrickle.health_check()
RETURNS TABLE (check_name text, severity text, detail text)
LANGUAGE sql STABLE AS $$
    SELECT
        'scheduler_running'::text,
        'ok'::text,
        'Scheduler is running'::text
$$;

-- ── fuse_status() ─────────────────────────────────────────────────────────
-- Used by: fuse
-- Full signature (8 cols); command reads: stream_table, fuse_mode, fuse_state,
--   blown_at, blow_reason  (positional 0-4 after ::text casts in SELECT list)
CREATE OR REPLACE FUNCTION pgtrickle.fuse_status()
RETURNS TABLE (
    stream_table      text,
    fuse_mode         text,
    fuse_state        text,
    fuse_ceiling      int,
    effective_ceiling int,
    fuse_sensitivity  text,
    blown_at          text,
    blow_reason       text
) LANGUAGE sql STABLE AS $$
    SELECT
        'test_table'::text,
        'off'::text,
        'armed'::text,
        5::int,
        5::int,
        'standard'::text,
        NULL::text,
        NULL::text
$$;

-- ── watermark_groups() ────────────────────────────────────────────────────
-- Used by: watermarks
CREATE OR REPLACE FUNCTION pgtrickle.watermark_groups()
RETURNS TABLE (
    group_name     text,
    source_count   int,
    tolerance_secs float8,
    created_at     text
) LANGUAGE sql STABLE AS $$
    SELECT
        'grp1'::text,
        2::int,
        60.0::float8,
        '2026-01-01 00:00:00'::text
$$;

-- ── worker_pool_status() ──────────────────────────────────────────────────
-- Used by: workers (summary row)
CREATE OR REPLACE FUNCTION pgtrickle.worker_pool_status()
RETURNS TABLE (
    active_workers  int,
    max_workers     int,
    per_db_cap      int,
    parallel_mode   text
) LANGUAGE sql STABLE AS $$
    SELECT 0::int, 4::int, 8::int, 'on'::text
$$;

-- ── parallel_job_status() ─────────────────────────────────────────────────
-- Used by: workers (job rows)
-- Command reads: job_id(bigint), unit_key, unit_kind, status,
--   enqueued_at, started_at, duration_ms(float8) — positional 0-6
CREATE OR REPLACE FUNCTION pgtrickle.parallel_job_status()
RETURNS TABLE (
    job_id         bigint,
    unit_key       text,
    unit_kind      text,
    status         text,
    member_count   int,
    attempt_no     int,
    scheduler_pid  int,
    worker_pid     int,
    enqueued_at    text,
    started_at     text,
    finished_at    text,
    duration_ms    float8
) LANGUAGE sql STABLE AS $$
    SELECT
        NULL::bigint, NULL::text, NULL::text, NULL::text,
        NULL::int,    NULL::int,  NULL::int,  NULL::int,
        NULL::text,   NULL::text, NULL::text, NULL::float8
    WHERE false
$$;

-- ── explain_delta() ───────────────────────────────────────────────────────
-- Used by: explain  (returns SETOF text)
-- IMPORTANT: The real function requires a schema-qualified name (e.g.
-- 'public.test_table'). Return empty when given a bare name so tests
-- catch regressions where the TUI forgets to qualify.
CREATE OR REPLACE FUNCTION pgtrickle.explain_delta(st_name text, format text DEFAULT 'text')
RETURNS SETOF text LANGUAGE sql STABLE AS $$
    SELECT 'Seq Scan on test_table  (cost=0.00..0.01 rows=1 width=4)'::text
    WHERE st_name LIKE '%.%'
$$;

-- ── recommend_refresh_mode() ──────────────────────────────────────────────
-- Used by: diag
-- Command reads: pgt_schema(0), pgt_name(1), current_mode(2),
--   recommended_mode(3), confidence(4), reason(5)
-- Full signature also includes effective_mode and signals columns.
CREATE OR REPLACE FUNCTION pgtrickle.recommend_refresh_mode(
    st_name text DEFAULT NULL
)
RETURNS TABLE (
    pgt_schema        text,
    pgt_name          text,
    current_mode      text,
    effective_mode    text,
    recommended_mode  text,
    confidence        text,
    reason            text,
    signals           jsonb
) LANGUAGE sql STABLE AS $$
    SELECT
        'public'::text,
        'test_table'::text,
        'DIFFERENTIAL'::text,
        'DIFFERENTIAL'::text,
        'DIFFERENTIAL'::text,
        'high'::text,
        'No issues detected'::text,
        '{}'::jsonb
$$;

-- ── export_definition() ───────────────────────────────────────────────────
-- Used by: export (CLI command only — TUI now reads pgt_stream_tables directly)
CREATE OR REPLACE FUNCTION pgtrickle.export_definition(st_name text)
RETURNS text LANGUAGE sql STABLE AS $$
    SELECT format(
        'SELECT pgtrickle.create_stream_table(name := %L, query := ''SELECT 1'');',
        st_name
    )
$$;

-- ── pgt_stream_tables catalog ─────────────────────────────────────────────
-- Used by: TUI FetchDdl (reads defining_query, schedule, refresh_mode)
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_stream_tables (
    pgt_id          bigserial PRIMARY KEY,
    pgt_relid       oid NOT NULL UNIQUE DEFAULT 0,
    pgt_name        text NOT NULL,
    pgt_schema      text NOT NULL,
    defining_query  text NOT NULL,
    schedule        text,
    refresh_mode    text NOT NULL DEFAULT 'DIFFERENTIAL',
    status          text NOT NULL DEFAULT 'ACTIVE',
    is_populated    bool NOT NULL DEFAULT true,
    consecutive_errors int NOT NULL DEFAULT 0
);
INSERT INTO pgtrickle.pgt_stream_tables (pgt_name, pgt_schema, defining_query, refresh_mode)
VALUES ('test_table', 'public', 'SELECT id, val FROM source_table', 'DIFFERENTIAL')
ON CONFLICT DO NOTHING;

-- ── create_stream_table() ─────────────────────────────────────────────────
-- Used by: create
CREATE OR REPLACE FUNCTION pgtrickle.create_stream_table(
    name         text,
    query        text,
    initialize   bool    DEFAULT true,
    schedule     text    DEFAULT NULL,
    refresh_mode text    DEFAULT NULL,
    topk_limit   int     DEFAULT NULL,
    topk_order_by text   DEFAULT NULL
) RETURNS bigint LANGUAGE sql AS $$
    SELECT 1::bigint
$$;

-- ── drop_stream_table() ───────────────────────────────────────────────────
-- Used by: drop
CREATE OR REPLACE FUNCTION pgtrickle.drop_stream_table(name text)
RETURNS void LANGUAGE sql AS $$
    SELECT
$$;

-- ── refresh_stream_table() ────────────────────────────────────────────────
-- Used by: refresh (single table)
CREATE OR REPLACE FUNCTION pgtrickle.refresh_stream_table(name text)
RETURNS void LANGUAGE sql AS $$
    SELECT
$$;
-- ── refresh_all() ─────────────────────────────────────────────────────────
-- Used by: CLI refresh --all command
CREATE OR REPLACE FUNCTION pgtrickle.refresh_all()
RETURNS void LANGUAGE sql AS $$
    SELECT
$$;
-- ── refresh_all_stream_tables() ────────────────────────────────────────────
-- Used by: TUI RefreshAll action
CREATE OR REPLACE FUNCTION pgtrickle.refresh_all_stream_tables()
RETURNS void LANGUAGE sql AS $$
    SELECT
$$;

-- ── reset_fuse() ──────────────────────────────────────────────────────────
-- Used by: TUI fuse reset action
CREATE OR REPLACE FUNCTION pgtrickle.reset_fuse(name text, strategy text)
RETURNS void LANGUAGE sql AS $$
    SELECT
$$;

-- ── repair_stream_table() ─────────────────────────────────────────────────
-- Used by: TUI repair action
CREATE OR REPLACE FUNCTION pgtrickle.repair_stream_table(name text)
RETURNS void LANGUAGE sql AS $$
    SELECT
$$;

-- ── gate_source() / ungate_source() ──────────────────────────────────────
-- Used by: TUI watermark gate/ungate actions
CREATE OR REPLACE FUNCTION pgtrickle.gate_source(name text)
RETURNS void LANGUAGE sql AS $$
    SELECT
$$;
CREATE OR REPLACE FUNCTION pgtrickle.ungate_source(name text)
RETURNS void LANGUAGE sql AS $$
    SELECT
$$;

-- ── validate_query() ──────────────────────────────────────────────────────
-- Used by: TUI :validate command
CREATE OR REPLACE FUNCTION pgtrickle.validate_query(query text)
RETURNS TABLE (check_name text, result text, severity text) LANGUAGE sql STABLE AS $$
    SELECT 'syntax'::text, 'OK'::text, 'info'::text
$$;

-- ── diagnose_errors() ─────────────────────────────────────────────────────
-- Used by: TUI Detail view enrichment
CREATE OR REPLACE FUNCTION pgtrickle.diagnose_errors(st_name text)
RETURNS TABLE (event_time text, error_type text, error_message text, remediation text)
LANGUAGE sql STABLE AS $$
    SELECT
        '2026-04-01 12:00:00'::text,
        'query_error'::text,
        'division by zero'::text,
        'Check source data for nulls'::text
    WHERE st_name LIKE '%.%'
$$;

-- ── explain_refresh_mode() ────────────────────────────────────────────────
-- Used by: TUI Detail view enrichment
CREATE OR REPLACE FUNCTION pgtrickle.explain_refresh_mode(name text)
RETURNS TABLE (configured_mode text, effective_mode text, downgrade_reason text)
LANGUAGE sql STABLE AS $$
    SELECT
        'AUTO'::text,
        'DIFFERENTIAL'::text,
        NULL::text
    WHERE name LIKE '%.%'
$$;

-- ── list_sources() ────────────────────────────────────────────────────────
-- Used by: TUI Detail view enrichment
CREATE OR REPLACE FUNCTION pgtrickle.list_sources(name text)
RETURNS TABLE (
    source_table text, source_oid bigint, source_type text,
    cdc_mode text, columns_used text
) LANGUAGE sql STABLE AS $$
    SELECT
        'public.source'::text, 12345::bigint, 'table'::text,
        'trigger'::text, NULL::text
    WHERE name LIKE '%.%'
$$;

-- ── get_refresh_history() ─────────────────────────────────────────────────
-- Used by: TUI Detail view enrichment
-- Column order must match the SELECT in poller.rs FetchRefreshHistory:
--   action(0), status(1), rows_inserted(2), rows_deleted(3),
--   delta_row_count(4), duration_ms(5), was_full_fallback(6),
--   start_time(7), error_message(8)
CREATE OR REPLACE FUNCTION pgtrickle.get_refresh_history(name text, max_rows int DEFAULT 20)
RETURNS TABLE (
    action text, status text, rows_inserted bigint, rows_deleted bigint,
    delta_row_count bigint, duration_ms float8, was_full_fallback bool,
    start_time text, error_message text
) LANGUAGE sql STABLE AS $$
    SELECT
        'REFRESH'::text, 'SUCCESS'::text, 10::bigint, 2::bigint,
        8::bigint, 42.0::float8, false, '2026-04-01 12:00:00'::text, NULL::text
    WHERE name LIKE '%.%'
$$;

-- ── list_auxiliary_columns() ──────────────────────────────────────────────
-- Used by: TUI Delta Inspector Auxiliary Columns tab
CREATE OR REPLACE FUNCTION pgtrickle.list_auxiliary_columns(name text)
RETURNS TABLE (column_name text, data_type text, purpose text)
LANGUAGE sql STABLE AS $$
    SELECT '_pgt_id'::text, 'bigint'::text, 'row identity tracking'::text
    WHERE name LIKE '%.%'
$$;

-- ── alter_stream_table() ──────────────────────────────────────────────────
-- Used by: alter
CREATE OR REPLACE FUNCTION pgtrickle.alter_stream_table(
    name         text,
    query        text    DEFAULT NULL,
    schedule     text    DEFAULT NULL,
    refresh_mode text    DEFAULT NULL,
    status       text    DEFAULT NULL,
    topk_limit   int     DEFAULT NULL,
    topk_order_by text   DEFAULT NULL
) RETURNS bigint LANGUAGE sql AS $$
    SELECT 1::bigint
$$;
"#;

/// A live PostgreSQL 18 container with pgtrickle stub functions installed
/// and a ready-to-use `tokio_postgres::Client`.
pub struct PgtStubDb {
    pub client: Client,
    // Keep alive until dropped
    _container: testcontainers::ContainerAsync<Postgres>,
    _conn_task: tokio::task::JoinHandle<()>,
}

impl PgtStubDb {
    /// Start a fresh Postgres 18 container, connect, and install stubs.
    pub async fn new() -> Self {
        let container = Postgres::default()
            .with_tag("18.3-alpine")
            .start()
            .await
            .expect("Failed to start Postgres container for TUI command tests");

        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get mapped port");

        let conn_str =
            format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");

        let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
            .await
            .expect("Failed to connect to stub database");

        let conn_task = tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("stub db connection error: {e}");
            }
        });

        let db = PgtStubDb {
            client,
            _container: container,
            _conn_task: conn_task,
        };

        db.client
            .batch_execute(STUB_SQL)
            .await
            .expect("Failed to install pgtrickle stub functions");

        db
    }
}
