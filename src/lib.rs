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
mod ivm;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitWarningKind {
    MissingSharedPreload,
    AutoCdcWithoutLogicalWal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InitDecision {
    should_init_runtime: bool,
    warning: Option<InitWarningKind>,
}

fn build_init_decision(in_shared_preload: bool, cdc_mode: &str, wal_level: i32) -> InitDecision {
    if !in_shared_preload {
        return InitDecision {
            should_init_runtime: false,
            warning: Some(InitWarningKind::MissingSharedPreload),
        };
    }

    let warning = if cdc_mode.eq_ignore_ascii_case("auto")
        && wal_level != pg_sys::WalLevel::WAL_LEVEL_LOGICAL as i32
    {
        Some(InitWarningKind::AutoCdcWithoutLogicalWal)
    } else {
        None
    };

    InitDecision {
        should_init_runtime: true,
        warning,
    }
}

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
    let cdc_mode = config::pg_trickle_cdc_mode();
    let init_decision = if in_shared_preload {
        // SAFETY: `pg_sys::wal_level` is a PostgreSQL global written from
        // postgresql.conf before shared_preload_libraries are processed.
        let current_wal_level = unsafe { pg_sys::wal_level };
        build_init_decision(in_shared_preload, &cdc_mode, current_wal_level)
    } else {
        build_init_decision(in_shared_preload, &cdc_mode, 0)
    };

    if init_decision.should_init_runtime {
        // Register shared memory allocations
        shmem::init_shared_memory();

        // Register the launcher background worker.
        // The launcher auto-discovers all databases on this server and
        // spawns a per-database scheduler for each one with pg_trickle installed.
        scheduler::register_launcher_worker();

        // ERG-B: Warn if cdc_mode='auto' but wal_level is not 'logical'.
        // In this state the extension silently stays in TRIGGER-only CDC mode,
        // which is correct but may surprise users who expect WAL-based CDC.
        // SAFETY: `pg_sys::wal_level` is a PostgreSQL global written from
        // postgresql.conf before shared_preload_libraries are processed.
        if init_decision.warning == Some(InitWarningKind::AutoCdcWithoutLogicalWal) {
            warning!(
                "pg_trickle: cdc_mode='auto' but wal_level is not 'logical'. \
                 WAL-based CDC will not activate until wal_level = logical is \
                 set in postgresql.conf and PostgreSQL is restarted. \
                 The extension will use trigger-based CDC in the meantime."
            );
        }

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

#[cfg(test)]
mod tests {
    use super::{InitDecision, InitWarningKind, build_init_decision};
    use pgrx::pg_sys;

    #[test]
    fn test_build_init_decision_requires_shared_preload() {
        assert_eq!(
            build_init_decision(false, "auto", pg_sys::WalLevel::WAL_LEVEL_LOGICAL as i32),
            InitDecision {
                should_init_runtime: false,
                warning: Some(InitWarningKind::MissingSharedPreload),
            }
        );
    }

    #[test]
    fn test_build_init_decision_warns_for_auto_cdc_without_logical_wal() {
        assert_eq!(
            build_init_decision(true, "auto", pg_sys::WalLevel::WAL_LEVEL_REPLICA as i32),
            InitDecision {
                should_init_runtime: true,
                warning: Some(InitWarningKind::AutoCdcWithoutLogicalWal),
            }
        );
    }

    #[test]
    fn test_build_init_decision_accepts_logical_wal_for_auto_cdc() {
        assert_eq!(
            build_init_decision(true, "AUTO", pg_sys::WalLevel::WAL_LEVEL_LOGICAL as i32),
            InitDecision {
                should_init_runtime: true,
                warning: None,
            }
        );
    }

    #[test]
    fn test_build_init_decision_does_not_warn_for_explicit_cdc_modes() {
        assert_eq!(
            build_init_decision(true, "trigger", pg_sys::WalLevel::WAL_LEVEL_REPLICA as i32),
            InitDecision {
                should_init_runtime: true,
                warning: None,
            }
        );
        assert_eq!(
            build_init_decision(true, "wal", pg_sys::WalLevel::WAL_LEVEL_REPLICA as i32),
            InitDecision {
                should_init_runtime: true,
                warning: None,
            }
        );
    }
}

#[cfg(any(test, feature = "pg_test"))]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    pub fn postgresql_conf_options() -> Vec<&'static str> {
        Vec::new()
    }
}

// ── SQL migration for catalog tables ──────────────────────────────────

extension_sql!(
    r#"
-- Extension schemas
CREATE SCHEMA IF NOT EXISTS pgtrickle;
CREATE SCHEMA IF NOT EXISTS pgtrickle_changes;

-- F51: Restrict change buffer schema access to prevent unauthorized
-- injection of bogus changes that would be applied on next refresh.
REVOKE ALL ON SCHEMA pgtrickle_changes FROM PUBLIC;

-- User-declared refresh groups for snapshot consistency
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_refresh_groups (
    group_id    SERIAL PRIMARY KEY,
    group_name  TEXT NOT NULL UNIQUE,
    member_oids OID[] NOT NULL,
    isolation   TEXT NOT NULL DEFAULT 'read_committed'
                CHECK (isolation IN ('read_committed', 'repeatable_read')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

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
                     CHECK (refresh_mode IN ('FULL', 'DIFFERENTIAL', 'IMMEDIATE')),
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
    topk_limit      INT,
    topk_order_by   TEXT,
    topk_offset     INT,
    diamond_consistency TEXT NOT NULL DEFAULT 'none'
                     CHECK (diamond_consistency IN ('none', 'atomic')),
    diamond_schedule_policy TEXT NOT NULL DEFAULT 'fastest'
                     CHECK (diamond_schedule_policy IN ('fastest', 'slowest')),
    has_keyless_source BOOLEAN NOT NULL DEFAULT FALSE,
    function_hashes TEXT,
    requested_cdc_mode TEXT
                     CHECK (requested_cdc_mode IN ('auto', 'trigger', 'wal')),
    is_append_only  BOOLEAN NOT NULL DEFAULT FALSE,
    scc_id          INT,
    last_fixpoint_iterations INT,
    pooler_compatibility_mode BOOLEAN NOT NULL DEFAULT FALSE,
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
    delta_row_count BIGINT DEFAULT 0,
    merge_strategy_used TEXT,
    was_full_fallback BOOLEAN NOT NULL DEFAULT FALSE,
    error_message   TEXT,
    status          TEXT NOT NULL
                     CHECK (status IN ('RUNNING', 'COMPLETED', 'FAILED', 'SKIPPED')),
    initiated_by    TEXT
                     CHECK (initiated_by IN ('SCHEDULER', 'MANUAL', 'INITIAL')),
    freshness_deadline TIMESTAMPTZ,
    tick_watermark_lsn PG_LSN,
    fixpoint_iteration INT
);

CREATE INDEX IF NOT EXISTS idx_hist_pgt_ts ON pgtrickle.pgt_refresh_history (pgt_id, data_timestamp);

-- Per-source CDC slot tracking
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_change_tracking (
    source_relid        OID PRIMARY KEY,
    slot_name           TEXT NOT NULL,
    last_consumed_lsn   PG_LSN,
    tracked_by_pgt_ids   BIGINT[]
);

-- Scheduler job table for parallel refresh dispatch
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_scheduler_jobs (
    job_id          BIGSERIAL PRIMARY KEY,
    dag_version     BIGINT NOT NULL,
    unit_key        TEXT NOT NULL,
    unit_kind       TEXT NOT NULL
                     CHECK (unit_kind IN ('singleton', 'atomic_group', 'immediate_closure', 'cyclic_scc')),
    member_pgt_ids  BIGINT[] NOT NULL,
    root_pgt_id     BIGINT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'QUEUED'
                     CHECK (status IN ('QUEUED', 'RUNNING', 'SUCCEEDED',
                                       'RETRYABLE_FAILED', 'PERMANENT_FAILED', 'CANCELLED')),
    scheduler_pid   INT NOT NULL,
    worker_pid      INT,
    attempt_no      INT NOT NULL DEFAULT 1,
    enqueued_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at      TIMESTAMPTZ,
    finished_at     TIMESTAMPTZ,
    outcome_detail  TEXT,
    retryable       BOOLEAN
);

CREATE INDEX IF NOT EXISTS idx_sched_jobs_status_enqueued
    ON pgtrickle.pgt_scheduler_jobs (status, enqueued_at);
CREATE INDEX IF NOT EXISTS idx_sched_jobs_unit_status
    ON pgtrickle.pgt_scheduler_jobs (unit_key, status);
CREATE INDEX IF NOT EXISTS idx_sched_jobs_finished
    ON pgtrickle.pgt_scheduler_jobs (finished_at)
    WHERE finished_at IS NOT NULL;

-- Bootstrap source gates (v0.5.0, Phase 3)
-- Records which source tables are currently "gated" (bootstrapping in progress).
-- When a source is gated, all stream tables that depend on it are skipped by
-- the scheduler until pgtrickle.ungate_source() is called.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_source_gates (
    source_relid    OID PRIMARY KEY,
    gated           BOOLEAN NOT NULL DEFAULT true,
    gated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    ungated_at      TIMESTAMPTZ,
    gated_by        TEXT
);

-- Per-source watermark state: tracks how far each external source has been loaded.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_watermarks (
    source_relid       OID PRIMARY KEY,
    watermark          TIMESTAMPTZ NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    advanced_by        TEXT,
    wal_lsn_at_advance TEXT
);

-- Watermark groups: declare that a set of sources must be temporally aligned.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_watermark_groups (
    group_id           SERIAL PRIMARY KEY,
    group_name         TEXT UNIQUE NOT NULL,
    source_relids      OID[] NOT NULL,
    tolerance_secs     DOUBLE PRECISION NOT NULL DEFAULT 0,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_stream_tables', '');
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_dependencies', '');
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_source_gates', '');
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_watermarks', '');
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_watermark_groups', '');


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
       END AS stale,
       CASE WHEN st.topk_limit IS NOT NULL THEN TRUE ELSE FALSE END AS is_topk
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
    stats.last_status,
    (SELECT array_agg(DISTINCT d.cdc_mode ORDER BY d.cdc_mode)
     FROM pgtrickle.pgt_dependencies d
     WHERE d.pgt_id = st.pgt_id AND d.source_type = 'TABLE') AS cdc_modes,
    st.scc_id,
    st.last_fixpoint_iterations
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

-- Per-source CDC status view (G5): exposes cdc_mode, slot names, and
-- transition timestamps for every TABLE dependency of a stream table.
CREATE OR REPLACE VIEW pgtrickle.pgt_cdc_status AS
SELECT
    st.pgt_schema,
    st.pgt_name,
    d.source_relid,
    c.relname        AS source_name,
    n.nspname        AS source_schema,
    d.cdc_mode,
    d.slot_name,
    d.decoder_confirmed_lsn,
    d.transition_started_at
FROM pgtrickle.pgt_dependencies d
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id
JOIN pg_class c ON c.oid = d.source_relid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE d.source_type = 'TABLE';
"#,
    name = "pg_trickle_monitoring_views",
    requires = [parse_duration_seconds],
);

// ── Quick health overview (ERG-E) ─────────────────────────────────────

extension_sql!(
    r#"
-- ERG-E: One-row health summary for dashboards and alerting.
CREATE OR REPLACE VIEW pgtrickle.quick_health AS
SELECT
    (SELECT count(*) FROM pgtrickle.pgt_stream_tables)::bigint
        AS total_stream_tables,
    (SELECT count(*) FROM pgtrickle.pgt_stream_tables
     WHERE status = 'ERROR' OR consecutive_errors > 0)::bigint
        AS error_tables,
    (SELECT count(*) FROM pgtrickle.pgt_stream_tables
     WHERE schedule IS NOT NULL
       AND schedule !~ '[\s@]'
       AND data_timestamp IS NOT NULL
       AND EXTRACT(EPOCH FROM (now() - data_timestamp)) >
           pgtrickle.parse_duration_seconds(schedule))::bigint
        AS stale_tables,
    (SELECT count(*) > 0 FROM pg_stat_activity
     WHERE backend_type = 'pg_trickle scheduler')
        AS scheduler_running,
    CASE
        WHEN (SELECT count(*) FROM pgtrickle.pgt_stream_tables) = 0 THEN 'EMPTY'
        WHEN (SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE status = 'SUSPENDED') > 0 THEN 'CRITICAL'
        WHEN (SELECT count(*) FROM pgtrickle.pgt_stream_tables WHERE status = 'ERROR' OR consecutive_errors > 0) > 0 THEN 'WARNING'
        WHEN (SELECT count(*) FROM pgtrickle.pgt_stream_tables
              WHERE schedule IS NOT NULL
                AND schedule !~ '[\s@]'
                AND data_timestamp IS NOT NULL
                AND EXTRACT(EPOCH FROM (now() - data_timestamp)) >
                    pgtrickle.parse_duration_seconds(schedule)) > 0 THEN 'WARNING'
        ELSE 'OK'
    END AS status;
"#,
    name = "pg_trickle_quick_health_view",
    requires = [parse_duration_seconds],
);

// ── Launcher notification (must be last) ──────────────────────────────
//
// Signal the launcher background worker to re-probe this database.
// Without this, the launcher applies a 5-minute skip TTL for databases
// where pg_trickle was not installed on first probe. Bumping the shared
// memory signal at the end of CREATE EXTENSION ensures the launcher
// detects the new install within its next 10-second wake cycle.

extension_sql!(
    r#"
SELECT pgtrickle._signal_launcher_rescan();
"#,
    name = "pg_trickle_launcher_notify",
    requires = [_signal_launcher_rescan],
    finalize,
);
