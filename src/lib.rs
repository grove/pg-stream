//! pg_trickle — Stream Tables for PostgreSQL 18.
//!
//! This extension provides declarative Stream Tables with automated
//! schedule-driven refresh and differential view maintenance (DVM).
//!
//! # Theoretical Basis
//!
//! - **DBSP**: Budiu et al., "DBSP: Automatic Differential View Maintenance
//!   for Rich Query Languages", PVLDB 2023. <https://arxiv.org/abs/2203.16684>
//! - **Gupta & Mumick (1995)**: "Maintenance of Materialized Views: Problems,
//!   Techniques, and Applications", IEEE Data Engineering Bulletin.
//! - **PostgreSQL REFRESH MATERIALIZED VIEW CONCURRENTLY** (since 9.4, Dec 2014).
//!
//! # Safety
//! This extension uses `unsafe` code for PostgreSQL FFI calls via pgrx.
//! All unsafe blocks are documented with `// SAFETY:` comments.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(dead_code)]

use pgrx::prelude::*;

mod api;
mod catalog;
mod cdc;
mod config;
pub mod dag;
pub mod dvm;
pub mod error;
mod hash;
mod hooks;
mod monitor;
mod refresh;
mod scheduler;
mod shmem;
pub mod version;
mod wal_decoder;

::pgrx::pg_module_magic!();

// Declare the `pgtrickle` schema so pgrx's SQL entity graph recognises it
// for `#[pg_extern(schema = "pgtrickle")]` annotations.
#[pg_schema]
mod pgtrickle {}

/// Extension initialization — called when the shared library is loaded.
///
/// Registers GUC variables, shared memory, and background workers.
/// Must be loaded via `shared_preload_libraries` for full functionality.
#[allow(non_snake_case)]
#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    // Register GUC variables first (always available)
    config::register_gucs();

    // Check if loaded via shared_preload_libraries
    // SAFETY: Reading a global boolean set by PostgreSQL during startup.
    // This is safe because the value is set before any extension code runs.
    let in_shared_preload = unsafe { pg_sys::process_shared_preload_libraries_in_progress };

    if in_shared_preload {
        // Register shared memory allocations
        shmem::init_shared_memory();

        // Register the scheduler background worker
        scheduler::register_scheduler_worker();

        log!("pg_trickle: initialized (shared_preload_libraries)");
    } else {
        warning!(
            "pg_trickle: loaded without shared_preload_libraries. \
             Background scheduler and shared memory are disabled. \
             Add 'pg_trickle' to shared_preload_libraries in \
             postgresql.conf for full functionality."
        );
    }
}

// ── SQL migration for catalog tables ──────────────────────────────────

extension_sql!(
    r#"
-- Extension schemas
CREATE SCHEMA IF NOT EXISTS pgtrickle;
CREATE SCHEMA IF NOT EXISTS pgtrickle_changes;

-- Core ST metadata
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_stream_tables (
    pgt_id           BIGSERIAL PRIMARY KEY,
    pgt_relid        OID NOT NULL UNIQUE,
    pgt_name         TEXT NOT NULL,
    pgt_schema       TEXT NOT NULL,
    defining_query  TEXT NOT NULL,
    original_query  TEXT,
    schedule      TEXT,
    refresh_mode    TEXT NOT NULL DEFAULT 'DIFFERENTIAL'
                     CHECK (refresh_mode IN ('FULL', 'DIFFERENTIAL', 'DIFFERENTIAL')),
    status          TEXT NOT NULL DEFAULT 'INITIALIZING'
                     CHECK (status IN ('INITIALIZING', 'ACTIVE', 'SUSPENDED', 'ERROR')),
    is_populated    BOOLEAN NOT NULL DEFAULT FALSE,
    data_timestamp  TIMESTAMPTZ,
    frontier        JSONB,
    last_refresh_at TIMESTAMPTZ,
    consecutive_errors INT NOT NULL DEFAULT 0,
    needs_reinit    BOOLEAN NOT NULL DEFAULT FALSE,
    auto_threshold  DOUBLE PRECISION,
    last_full_ms    DOUBLE PRECISION,
    functions_used  TEXT[],
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pgt_status ON pgtrickle.pgt_stream_tables (status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_pgt_name ON pgtrickle.pgt_stream_tables (pgt_schema, pgt_name);

-- DAG edges
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_dependencies (
    pgt_id        BIGINT NOT NULL REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    source_relid OID NOT NULL,
    source_type  TEXT NOT NULL CHECK (source_type IN ('TABLE', 'STREAM_TABLE', 'VIEW', 'MATVIEW', 'FOREIGN_TABLE')),
    columns_used TEXT[],
    column_snapshot JSONB,
    schema_fingerprint TEXT,
    cdc_mode     TEXT NOT NULL DEFAULT 'TRIGGER'
                  CHECK (cdc_mode IN ('TRIGGER', 'TRANSITIONING', 'WAL')),
    slot_name    TEXT,
    decoder_confirmed_lsn PG_LSN,
    transition_started_at TIMESTAMPTZ,
    PRIMARY KEY (pgt_id, source_relid)
);

CREATE INDEX IF NOT EXISTS idx_deps_source ON pgtrickle.pgt_dependencies (source_relid);

-- Refresh history / audit log
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_refresh_history (
    refresh_id      BIGSERIAL PRIMARY KEY,
    pgt_id           BIGINT NOT NULL,
    data_timestamp  TIMESTAMPTZ NOT NULL,
    start_time      TIMESTAMPTZ NOT NULL,
    end_time        TIMESTAMPTZ,
    action          TEXT NOT NULL
                     CHECK (action IN ('NO_DATA', 'FULL', 'DIFFERENTIAL', 'DIFFERENTIAL', 'REINITIALIZE', 'SKIP')),
    rows_inserted   BIGINT DEFAULT 0,
    rows_deleted    BIGINT DEFAULT 0,
    error_message   TEXT,
    status          TEXT NOT NULL
                     CHECK (status IN ('RUNNING', 'COMPLETED', 'FAILED', 'SKIPPED')),
    initiated_by    TEXT
                     CHECK (initiated_by IN ('SCHEDULER', 'MANUAL', 'INITIAL')),
    freshness_deadline TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_hist_pgt_ts ON pgtrickle.pgt_refresh_history (pgt_id, data_timestamp);

-- Per-source CDC slot tracking
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_change_tracking (
    source_relid        OID PRIMARY KEY,
    slot_name           TEXT NOT NULL,
    last_consumed_lsn   PG_LSN,
    tracked_by_pgt_ids   BIGINT[]
);

"#,
    name = "pg_trickle_catalog",
    bootstrap,
);

// ── Status overview view (requires parse_duration_seconds) ────────────

extension_sql!(
    r#"
-- Status overview view
CREATE OR REPLACE VIEW pgtrickle.stream_tables_info AS
SELECT st.*,
       now() - st.data_timestamp AS staleness,
       CASE WHEN st.schedule IS NOT NULL
                 AND st.schedule !~ '[\s@]'
            THEN EXTRACT(EPOCH FROM (now() - st.data_timestamp)) >
                 pgtrickle.parse_duration_seconds(st.schedule)
            ELSE NULL::boolean
       END AS stale
FROM pgtrickle.pgt_stream_tables st;
"#,
    name = "pg_trickle_info_view",
    requires = [parse_duration_seconds],
);

// ── DDL event triggers (Phase 7) ──────────────────────────────────────

extension_sql!(
    r#"
-- Create event trigger functions with correct RETURNS event_trigger type.
-- pgrx's #[pg_extern] generates RETURNS void, which PostgreSQL rejects for
-- event triggers. We create them manually here with the correct return type.
CREATE FUNCTION pgtrickle."_on_ddl_end"()
    RETURNS event_trigger
    LANGUAGE c
    AS 'MODULE_PATHNAME', 'pg_trickle_on_ddl_end_wrapper';

CREATE FUNCTION pgtrickle."_on_sql_drop"()
    RETURNS event_trigger
    LANGUAGE c
    AS 'MODULE_PATHNAME', 'pg_trickle_on_sql_drop_wrapper';

-- Event trigger: track ALTER TABLE on upstream sources
CREATE EVENT TRIGGER pg_trickle_ddl_tracker
    ON ddl_command_end
    EXECUTE FUNCTION pgtrickle._on_ddl_end();

-- Event trigger: track DROP TABLE on upstream sources / ST storage tables
CREATE EVENT TRIGGER pg_trickle_drop_tracker
    ON sql_drop
    EXECUTE FUNCTION pgtrickle._on_sql_drop();
"#,
    name = "pg_trickle_event_triggers",
);

// ── Monitoring views (Phase 9) ────────────────────────────────────────

extension_sql!(
    r#"
-- Convenience view: pg_stat_stream_tables
-- Combines catalog metadata with aggregate refresh statistics.
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
    stats.last_status
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
"#,
    name = "pg_trickle_monitoring_views",
    requires = [parse_duration_seconds],
);
