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
// SAF-3: Deny .unwrap() in non-test production code.
// Tests are explicitly exempt (cfg(test) blocks allow free use of unwrap).
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use pgrx::prelude::*;

mod api;
mod catalog;
mod cdc;
mod config;
pub mod dag;
mod diagnostics;
pub mod dvm;
pub mod error;
mod hash;
mod hooks;
mod ivm;
pub(crate) mod metrics_server;
mod monitor;
mod refresh;
pub mod scheduler;
mod shmem;
mod template_cache;
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
    diamond_consistency TEXT NOT NULL DEFAULT 'atomic'
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
    max_differential_joins   INT,
    max_delta_fraction       DOUBLE PRECISION,
    pooler_compatibility_mode BOOLEAN NOT NULL DEFAULT FALSE,
    refresh_tier    TEXT NOT NULL DEFAULT 'hot'
                     CHECK (refresh_tier IN ('hot', 'warm', 'cold', 'frozen')),
    effective_refresh_mode TEXT,
    fuse_mode       TEXT NOT NULL DEFAULT 'off'
                     CHECK (fuse_mode IN ('off', 'on', 'auto')),
    fuse_state      TEXT NOT NULL DEFAULT 'armed'
                     CHECK (fuse_state IN ('armed', 'blown', 'disabled')),
    fuse_ceiling    BIGINT,
    fuse_sensitivity INT,
    blown_at        TIMESTAMPTZ,
    blow_reason     TEXT,
    last_error_message TEXT,
    last_error_at   TIMESTAMPTZ,
    downstream_publication_name TEXT,
    freshness_deadline_ms BIGINT,
    st_partition_key TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pgt_status ON pgtrickle.pgt_stream_tables (status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_pgt_name ON pgtrickle.pgt_stream_tables (pgt_schema, pgt_name);
-- PERF-4: Scheduler hot‐path lookup by relation OID.
CREATE INDEX IF NOT EXISTS idx_pgt_relid ON pgtrickle.pgt_stream_tables (pgt_relid);

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
-- PERF-4: Fast lookup by pgt_id (non‐PK prefix for multi‐column PK).
CREATE INDEX IF NOT EXISTS idx_deps_pgt_id ON pgtrickle.pgt_dependencies (pgt_id);

-- Refresh history / audit log
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_refresh_history (
    refresh_id      BIGSERIAL PRIMARY KEY,
    pgt_id           BIGINT NOT NULL
                     REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    data_timestamp  TIMESTAMPTZ NOT NULL,
    start_time      TIMESTAMPTZ NOT NULL,
    end_time        TIMESTAMPTZ,
    action          TEXT NOT NULL
                     CHECK (action IN ('NO_DATA', 'FULL', 'DIFFERENTIAL', 'REINITIALIZE', 'SKIP')),
    rows_inserted   BIGINT DEFAULT 0,
    rows_deleted    BIGINT DEFAULT 0,
    delta_row_count BIGINT DEFAULT 0,
    merge_strategy_used TEXT,
    was_full_fallback BOOLEAN NOT NULL DEFAULT FALSE,
    error_message   TEXT,
    status          TEXT NOT NULL
                     CHECK (status IN ('RUNNING', 'COMPLETED', 'FAILED', 'SKIPPED')),
    initiated_by    TEXT
                     CHECK (initiated_by IN ('SCHEDULER', 'MANUAL', 'INITIAL', 'SELF_MONITOR')),
    freshness_deadline TIMESTAMPTZ,
    tick_watermark_lsn PG_LSN,
    fixpoint_iteration INT
);

CREATE INDEX IF NOT EXISTS idx_hist_pgt_ts ON pgtrickle.pgt_refresh_history (pgt_id, data_timestamp);
-- PERF-1: Fast lookup by (pgt_id, start_time) for self-monitoring and scheduler_overhead queries.
CREATE INDEX IF NOT EXISTS idx_hist_pgt_start ON pgtrickle.pgt_refresh_history (pgt_id, start_time);

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
                     CHECK (unit_kind IN ('singleton', 'atomic_group', 'immediate_closure', 'cyclic_scc', 'repeatable_read_group', 'fused_chain')),
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

-- DB-3: Schema version tracking table.
-- Records which schema migration versions have been applied to this database.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_schema_version (
    version     TEXT PRIMARY KEY,
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    description TEXT
);
INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('0.19.0', 'Initial schema version tracking')
ON CONFLICT (version) DO NOTHING;

SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_stream_tables', '');
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_dependencies', '');
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_source_gates', '');
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_watermarks', '');
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_watermark_groups', '');

-- G14-SHC: Shared template cache (catalog-backed, UNLOGGED)
CREATE UNLOGGED TABLE IF NOT EXISTS pgtrickle.pgt_template_cache (
    pgt_id       BIGINT PRIMARY KEY
                 REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    query_hash   BIGINT NOT NULL,
    delta_sql    TEXT NOT NULL,
    columns      TEXT[] NOT NULL,
    source_oids  INTEGER[] NOT NULL,
    is_dedup     BOOLEAN NOT NULL DEFAULT FALSE,
    key_changed  BOOLEAN NOT NULL DEFAULT FALSE,
    all_algebraic BOOLEAN NOT NULL DEFAULT FALSE,
    cached_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);


"#,
    name = "pg_trickle_catalog",
    bootstrap,
);

// ── Status overview view (requires parse_duration_seconds) ────────────

extension_sql!(
    r#"
-- Status overview view (ERR-1d: last_error_message and last_error_at are
-- included via st.* from pgt_stream_tables)
CREATE OR REPLACE VIEW pgtrickle.stream_tables_info AS
SELECT st.*,
       now() - st.last_refresh_at AS staleness,
       CASE WHEN st.schedule IS NOT NULL
                 AND st.schedule !~ '[\s@]'
            THEN EXTRACT(EPOCH FROM (now() - st.last_refresh_at)) >
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
    now() - st.last_refresh_at AS staleness,
    CASE WHEN st.schedule IS NOT NULL AND st.last_refresh_at IS NOT NULL
              AND st.schedule !~ '[\s@]'
         THEN EXTRACT(EPOCH FROM (now() - st.last_refresh_at)) >
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
    st.last_fixpoint_iterations,
    st.refresh_tier
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
       AND last_refresh_at IS NOT NULL
       AND EXTRACT(EPOCH FROM (now() - last_refresh_at)) >
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
                AND last_refresh_at IS NOT NULL
                AND EXTRACT(EPOCH FROM (now() - last_refresh_at)) >
                    pgtrickle.parse_duration_seconds(schedule)) > 0 THEN 'WARNING'
        ELSE 'OK'
    END AS status;
"#,
    name = "pg_trickle_quick_health_view",
    requires = [parse_duration_seconds],
);

// ── OP-3: pause_all / resume_all ─────────────────────────────────────

extension_sql!(
    r#"
CREATE OR REPLACE FUNCTION pgtrickle."pause_all"()
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE pgtrickle.pgt_stream_tables
       SET status = 'PAUSED'
     WHERE status = 'ACTIVE';
    RAISE NOTICE 'pg_trickle: all stream tables paused.';
END;
$$;

COMMENT ON FUNCTION pgtrickle."pause_all"() IS
    'Pause automatic refreshes for every ACTIVE stream table. '
    'Use pgtrickle.resume_all() to re-activate them.';

CREATE OR REPLACE FUNCTION pgtrickle."resume_all"()
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE pgtrickle.pgt_stream_tables
       SET status = 'ACTIVE'
     WHERE status = 'PAUSED';
    RAISE NOTICE 'pg_trickle: all paused stream tables resumed.';
END;
$$;

COMMENT ON FUNCTION pgtrickle."resume_all"() IS
    'Re-activate all stream tables that were paused with pgtrickle.pause_all().';
"#,
    name = "pg_trickle_pause_resume",
);

// ── OP-4: refresh_if_stale ────────────────────────────────────────────

extension_sql!(
    r#"
CREATE OR REPLACE FUNCTION pgtrickle."refresh_if_stale"(
    p_name   text,
    p_max_age interval DEFAULT '5 minutes'::interval
)
RETURNS boolean
LANGUAGE plpgsql
AS $$
DECLARE
    v_last_end timestamp with time zone;
    v_refreshed boolean := false;
BEGIN
    SELECT MAX(end_time)
      INTO v_last_end
      FROM pgtrickle.pgt_refresh_history h
      JOIN pgtrickle.pgt_stream_tables   s USING (pgt_id)
     WHERE s.pgt_name = p_name
       AND h.status = 'COMPLETED';

    IF v_last_end IS NULL OR (now() - v_last_end) > p_max_age THEN
        PERFORM pgtrickle.refresh_stream_table(p_name);
        v_refreshed := true;
    END IF;

    RETURN v_refreshed;
END;
$$;

COMMENT ON FUNCTION pgtrickle."refresh_if_stale"(text, interval) IS
    'Refresh the named stream table only when the most recent completed '
    'refresh is older than max_age.  Returns TRUE when a refresh was '
    'triggered, FALSE when the table was fresh enough.';
"#,
    name = "pg_trickle_refresh_if_stale",
    requires = [refresh_stream_table],
);

// ── OP-5: stream_table_definition ────────────────────────────────────

extension_sql!(
    r#"
CREATE OR REPLACE FUNCTION pgtrickle."stream_table_definition"(
    p_name text
)
RETURNS text
LANGUAGE sql
STABLE
AS $$
    SELECT pgtrickle.export_definition(p_name);
$$;

COMMENT ON FUNCTION pgtrickle."stream_table_definition"(text) IS
    'Return the CREATE STREAM TABLE DDL for the named stream table. '
    'Equivalent to pgtrickle.export_definition(name) — provided as a '
    'more discoverable alias.';
"#,
    name = "pg_trickle_stream_table_definition",
    requires = [export_definition],
);

// ── OPS-1: Canary / shadow-mode helpers ──────────────────────────────

extension_sql!(
    r#"
CREATE OR REPLACE FUNCTION pgtrickle."canary_begin"(
    p_name      text,
    p_new_query text
)
RETURNS text
LANGUAGE plpgsql
AS $$
DECLARE
    v_schema  text;
    v_table   text;
    v_canary  text;
    v_dot     int;
BEGIN
    v_dot    := strpos(p_name, '.');
    IF v_dot > 0 THEN
        v_schema := substr(p_name, 1, v_dot - 1);
        v_table  := substr(p_name, v_dot + 1);
    ELSE
        v_schema := current_schema();
        v_table  := p_name;
    END IF;

    v_canary := '__pgt_canary_' || v_table;

    -- Drop any stale canary table from a previous run.
    BEGIN
        PERFORM pgtrickle.drop_stream_table(v_schema || '.' || v_canary);
    EXCEPTION WHEN OTHERS THEN
        NULL;  -- ignore if it does not exist
    END;

    -- Create the canary stream table with the new query.
    PERFORM pgtrickle.create_stream_table(
        v_schema || '.' || v_canary,
        p_new_query
    );

    RETURN format(
        'Canary stream table %I.%I created. Run pgtrickle.canary_diff(%L) to compare.',
        v_schema, v_canary, p_name
    );
END;
$$;

COMMENT ON FUNCTION pgtrickle."canary_begin"(text, text) IS
    'Start a shadow/canary test for the named stream table. '
    'Creates __pgt_canary_<name> with p_new_query and starts refreshing it. '
    'Use canary_diff(name) to inspect differences and canary_promote(name) to '
    'swap canary into production.';

CREATE OR REPLACE FUNCTION pgtrickle."canary_diff"(
    p_name text
)
RETURNS TABLE(
    row_source text,
    diff_row   text
)
LANGUAGE plpgsql
AS $$
DECLARE
    v_schema  text;
    v_table   text;
    v_canary  text;
    v_dot     int;
    v_sql     text;
BEGIN
    v_dot    := strpos(p_name, '.');
    IF v_dot > 0 THEN
        v_schema := substr(p_name, 1, v_dot - 1);
        v_table  := substr(p_name, v_dot + 1);
    ELSE
        v_schema := current_schema();
        v_table  := p_name;
    END IF;

    v_canary := '__pgt_canary_' || v_table;

    -- Return rows in live-only vs canary-only using EXCEPT (symmetric difference).
    v_sql := format(
        '(SELECT %L AS row_source, t::text AS diff_row FROM %I.%I t EXCEPT
          SELECT %L, c::text FROM %I.%I c)
         UNION ALL
         (SELECT %L, c::text FROM %I.%I c EXCEPT
          SELECT %L, t::text FROM %I.%I t)',
        'live_only',   v_schema, v_table,
        'canary_only', v_schema, v_canary,
        'canary_only', v_schema, v_canary,
        'live_only',   v_schema, v_table
    );
    RETURN QUERY EXECUTE v_sql;
END;
$$;

COMMENT ON FUNCTION pgtrickle."canary_diff"(text) IS
    'Compare the live stream table with its canary counterpart. '
    'Returns rows that exist in only one of the two tables. '
    'An empty result set indicates the new query produces the same output.';

CREATE OR REPLACE FUNCTION pgtrickle."canary_promote"(
    p_name text
)
RETURNS text
LANGUAGE plpgsql
AS $$
DECLARE
    v_schema    text;
    v_table     text;
    v_canary    text;
    v_dot       int;
    v_new_query text;
BEGIN
    v_dot    := strpos(p_name, '.');
    IF v_dot > 0 THEN
        v_schema := substr(p_name, 1, v_dot - 1);
        v_table  := substr(p_name, v_dot + 1);
    ELSE
        v_schema := current_schema();
        v_table  := p_name;
    END IF;

    v_canary := '__pgt_canary_' || v_table;

    -- Read the defining query from the canary table.
    SELECT defining_query
      INTO v_new_query
      FROM pgtrickle.pgt_stream_tables
     WHERE pgt_schema = v_schema
       AND pgt_name   = v_canary;

    IF v_new_query IS NULL THEN
        RAISE EXCEPTION 'No canary found for %. Run pgtrickle.canary_begin() first.', p_name;
    END IF;

    -- Promote: alter the live table to use the new query, then drop the canary.
    PERFORM pgtrickle.alter_stream_table(v_schema || '.' || v_table, query => v_new_query);

    BEGIN
        PERFORM pgtrickle.drop_stream_table(v_schema || '.' || v_canary);
    EXCEPTION WHEN OTHERS THEN
        NULL;
    END;

    RETURN format(
        'Canary promoted: %I.%I now uses the canary query. Canary table dropped.',
        v_schema, v_table
    );
END;
$$;

COMMENT ON FUNCTION pgtrickle."canary_promote"(text) IS
    'Promote the canary stream table to production. '
    'Calls ALTER STREAM TABLE with the canary query, then drops the canary table. '
    'Run pgtrickle.canary_diff(name) first to confirm the result set matches.';
"#,
    name = "pg_trickle_canary",
    requires = [create_stream_table, drop_stream_table, alter_stream_table],
);

// ── v0.28.0: Outbox & Inbox catalog tables ─────────────────────────────

extension_sql!(
    r#"
-- OUTBOX-1 (v0.28.0): Per-stream-table outbox configuration.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_outbox_config (
    stream_table_oid   OID         NOT NULL PRIMARY KEY,
    stream_table_name  TEXT        NOT NULL,
    outbox_table_name  TEXT        NOT NULL,
    retention_hours    INT         NOT NULL DEFAULT 24,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_drained_at    TIMESTAMPTZ,
    last_drained_count BIGINT      NOT NULL DEFAULT 0,
    CONSTRAINT uq_outbox_table_name UNIQUE (outbox_table_name)
);

CREATE INDEX IF NOT EXISTS idx_pgt_outbox_config_name
    ON pgtrickle.pgt_outbox_config (stream_table_name);

COMMENT ON TABLE pgtrickle.pgt_outbox_config IS
    'OUTBOX-1 (v0.28.0): Catalog of stream tables with the transactional outbox pattern enabled.';

-- INBOX-1 (v0.28.0): Named inbox configurations.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_inbox_config (
    inbox_name             TEXT        NOT NULL PRIMARY KEY,
    inbox_schema           TEXT        NOT NULL DEFAULT 'pgtrickle',
    max_retries            INT         NOT NULL DEFAULT 3,
    schedule               TEXT        NOT NULL DEFAULT '1s',
    with_dead_letter       BOOL        NOT NULL DEFAULT true,
    with_stats             BOOL        NOT NULL DEFAULT true,
    retention_hours        INT         NOT NULL DEFAULT 72,
    id_column              TEXT        NOT NULL DEFAULT 'event_id',
    processed_at_column    TEXT        NOT NULL DEFAULT 'processed_at',
    retry_count_column     TEXT        NOT NULL DEFAULT 'retry_count',
    error_column           TEXT        NOT NULL DEFAULT 'error',
    received_at_column     TEXT        NOT NULL DEFAULT 'received_at',
    event_type_column      TEXT        NOT NULL DEFAULT 'event_type',
    is_managed             BOOL        NOT NULL DEFAULT true,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE pgtrickle.pgt_inbox_config IS
    'INBOX-1 (v0.28.0): Catalog of named transactional inbox configurations.';

-- OUTBOX-B1 (v0.28.0): Consumer groups for the outbox pattern.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_consumer_groups (
    group_name         TEXT        NOT NULL PRIMARY KEY,
    outbox_name        TEXT        NOT NULL,
    auto_offset_reset  TEXT        NOT NULL DEFAULT 'latest',
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_consumer_group_auto_offset_reset
        CHECK (auto_offset_reset IN ('earliest', 'latest'))
);

CREATE INDEX IF NOT EXISTS idx_pgt_consumer_groups_outbox
    ON pgtrickle.pgt_consumer_groups (outbox_name);

COMMENT ON TABLE pgtrickle.pgt_consumer_groups IS
    'OUTBOX-B1 (v0.28.0): Named consumer groups that track consumption progress on an outbox.';

-- OUTBOX-B2 (v0.28.0): Per-consumer committed offsets within a group.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_consumer_offsets (
    group_name        TEXT        NOT NULL
                      REFERENCES pgtrickle.pgt_consumer_groups(group_name) ON DELETE CASCADE,
    consumer_id       TEXT        NOT NULL,
    committed_offset  BIGINT      NOT NULL DEFAULT 0,
    last_committed_at TIMESTAMPTZ,
    last_heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_name, consumer_id)
);

COMMENT ON TABLE pgtrickle.pgt_consumer_offsets IS
    'OUTBOX-B2 (v0.28.0): Per-consumer committed offsets and heartbeat tracking.';

-- OUTBOX-B3 (v0.28.0): Visibility leases granted by poll_outbox().
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_consumer_leases (
    group_name     TEXT        NOT NULL,
    consumer_id    TEXT        NOT NULL,
    batch_start    BIGINT      NOT NULL,
    batch_end      BIGINT      NOT NULL,
    lease_expires  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (group_name, consumer_id),
    FOREIGN KEY (group_name, consumer_id)
        REFERENCES pgtrickle.pgt_consumer_offsets(group_name, consumer_id) ON DELETE CASCADE
);

COMMENT ON TABLE pgtrickle.pgt_consumer_leases IS
    'OUTBOX-B3 (v0.28.0): Visibility leases for in-flight outbox message batches.';

-- INBOX-B1 (v0.28.0): Per-inbox ordering configuration.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_inbox_ordering_config (
    inbox_name        TEXT NOT NULL PRIMARY KEY
                      REFERENCES pgtrickle.pgt_inbox_config(inbox_name) ON DELETE CASCADE,
    aggregate_id_col  TEXT NOT NULL,
    sequence_num_col  TEXT NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE pgtrickle.pgt_inbox_ordering_config IS
    'INBOX-B1 (v0.28.0): Ordering configuration for per-aggregate sequenced inbox processing.';

-- INBOX-B2 (v0.28.0): Per-inbox priority tiers configuration.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_inbox_priority_config (
    inbox_name    TEXT NOT NULL PRIMARY KEY
                  REFERENCES pgtrickle.pgt_inbox_config(inbox_name) ON DELETE CASCADE,
    priority_col  TEXT NOT NULL,
    tiers         JSONB,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE pgtrickle.pgt_inbox_priority_config IS
    'INBOX-B2 (v0.28.0): Priority tier configuration for inbox message processing.';
"#,
    name = "pg_trickle_outbox_inbox_catalog",
    requires = [],
);

// ── Relay catalog tables + SQL API (v0.29.0) ──────────────────────────
extension_sql!(
    r#"
-- RELAY-CAT (v0.29.0): Forward pipelines — outbox → external sink.
CREATE TABLE IF NOT EXISTS pgtrickle.relay_outbox_config (
    name     TEXT    NOT NULL PRIMARY KEY,
    enabled  BOOLEAN NOT NULL DEFAULT true,
    config   JSONB   NOT NULL
);

COMMENT ON TABLE pgtrickle.relay_outbox_config IS
    'RELAY-CAT (v0.29.0): Forward relay pipeline definitions (outbox → external sink).';

-- RELAY-CAT (v0.29.0): Reverse pipelines — external source → pg-trickle inbox.
CREATE TABLE IF NOT EXISTS pgtrickle.relay_inbox_config (
    name     TEXT    NOT NULL PRIMARY KEY,
    enabled  BOOLEAN NOT NULL DEFAULT true,
    config   JSONB   NOT NULL
);

COMMENT ON TABLE pgtrickle.relay_inbox_config IS
    'RELAY-CAT (v0.29.0): Reverse relay pipeline definitions (external source → inbox).';

-- RELAY-CAT (v0.29.0): Durable per-pipeline offset tracking.
CREATE TABLE IF NOT EXISTS pgtrickle.relay_consumer_offsets (
    relay_group_id  TEXT        NOT NULL,
    pipeline_id     TEXT        NOT NULL,
    last_change_id  BIGINT      NOT NULL DEFAULT 0,
    worker_id       TEXT,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (relay_group_id, pipeline_id)
);

COMMENT ON TABLE pgtrickle.relay_consumer_offsets IS
    'RELAY-CAT (v0.29.0): Durable per-pipeline offset tracking for the relay binary.';

-- RELAY-CAT (v0.29.0): Shared trigger function for config hot-reload.
CREATE OR REPLACE FUNCTION pgtrickle.relay_config_notify()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
BEGIN
    PERFORM pg_notify(
        'pgtrickle_relay_config',
        json_build_object(
            'direction', TG_TABLE_NAME,
            'event',     TG_OP,
            'name',      COALESCE(NEW.name, OLD.name),
            'enabled',   COALESCE(NEW.enabled, OLD.enabled)
        )::text
    );
    RETURN NULL;
END;
$$;

DROP TRIGGER IF EXISTS relay_outbox_config_notify ON pgtrickle.relay_outbox_config;
CREATE TRIGGER relay_outbox_config_notify
    AFTER INSERT OR UPDATE OR DELETE ON pgtrickle.relay_outbox_config
    FOR EACH ROW EXECUTE FUNCTION pgtrickle.relay_config_notify();

DROP TRIGGER IF EXISTS relay_inbox_config_notify ON pgtrickle.relay_inbox_config;
CREATE TRIGGER relay_inbox_config_notify
    AFTER INSERT OR UPDATE OR DELETE ON pgtrickle.relay_inbox_config
    FOR EACH ROW EXECUTE FUNCTION pgtrickle.relay_config_notify();

-- RELAY-CAT (v0.29.0): set_relay_outbox — upsert a forward pipeline.
CREATE OR REPLACE FUNCTION pgtrickle.set_relay_outbox(
    p_name            TEXT,
    p_outbox          TEXT,
    p_group           TEXT,
    p_sink            JSONB,
    p_retention_hours INT     DEFAULT 24,
    p_enabled         BOOLEAN DEFAULT true
) RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    v_sink_type TEXT;
    v_config    JSONB;
BEGIN
    v_sink_type := p_sink ->> 'type';
    IF v_sink_type IS NULL OR v_sink_type = '' THEN
        RAISE EXCEPTION 'relay.invalid_config: sink JSONB must contain a "type" key. Got: %', p_sink
            USING ERRCODE = 'raise_exception';
    END IF;
    IF p_outbox IS NULL OR p_outbox = '' THEN
        RAISE EXCEPTION 'relay.invalid_config: outbox name must not be empty'
            USING ERRCODE = 'raise_exception';
    END IF;
    v_config := jsonb_build_object(
        'source_type', 'outbox',
        'source',      jsonb_build_object('outbox', p_outbox, 'group', p_group, 'retention_hours', p_retention_hours),
        'sink_type',   v_sink_type,
        'sink',        p_sink
    );
    INSERT INTO pgtrickle.relay_outbox_config (name, enabled, config)
    VALUES (p_name, p_enabled, v_config)
    ON CONFLICT (name) DO UPDATE SET enabled = EXCLUDED.enabled, config = EXCLUDED.config;
END;
$$;

-- RELAY-CAT (v0.29.0): set_relay_inbox — upsert a reverse pipeline.
CREATE OR REPLACE FUNCTION pgtrickle.set_relay_inbox(
    p_name             TEXT,
    p_inbox            TEXT,
    p_source           JSONB,
    p_max_retries      INT     DEFAULT 3,
    p_schedule         TEXT    DEFAULT '1s',
    p_with_dead_letter BOOLEAN DEFAULT true,
    p_retention_hours  INT     DEFAULT 24,
    p_enabled          BOOLEAN DEFAULT true
) RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    v_source_type TEXT;
    v_config      JSONB;
BEGIN
    v_source_type := p_source ->> 'type';
    IF v_source_type IS NULL OR v_source_type = '' THEN
        RAISE EXCEPTION 'relay.invalid_config: source JSONB must contain a "type" key. Got: %', p_source
            USING ERRCODE = 'raise_exception';
    END IF;
    IF p_inbox IS NULL OR p_inbox = '' THEN
        RAISE EXCEPTION 'relay.invalid_config: inbox name must not be empty'
            USING ERRCODE = 'raise_exception';
    END IF;
    v_config := jsonb_build_object(
        'source_type', v_source_type,
        'source',      p_source,
        'sink_type',   'pg-inbox',
        'sink',        jsonb_build_object('inbox', p_inbox, 'max_retries', p_max_retries,
                           'schedule', p_schedule, 'with_dead_letter', p_with_dead_letter,
                           'retention_hours', p_retention_hours)
    );
    INSERT INTO pgtrickle.relay_inbox_config (name, enabled, config)
    VALUES (p_name, p_enabled, v_config)
    ON CONFLICT (name) DO UPDATE SET enabled = EXCLUDED.enabled, config = EXCLUDED.config;
END;
$$;

-- RELAY-CAT (v0.29.0): enable_relay — enable a named pipeline.
CREATE OR REPLACE FUNCTION pgtrickle.enable_relay(p_name TEXT) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE v_o INT; v_i INT;
BEGIN
    UPDATE pgtrickle.relay_outbox_config SET enabled = true WHERE name = p_name;
    GET DIAGNOSTICS v_o = ROW_COUNT;
    UPDATE pgtrickle.relay_inbox_config  SET enabled = true WHERE name = p_name;
    GET DIAGNOSTICS v_i = ROW_COUNT;
    IF v_o + v_i = 0 THEN
        RAISE EXCEPTION 'relay: pipeline "%" not found', p_name USING ERRCODE = 'no_data_found';
    END IF;
END;
$$;

-- RELAY-CAT (v0.29.0): disable_relay — disable a named pipeline.
CREATE OR REPLACE FUNCTION pgtrickle.disable_relay(p_name TEXT) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE v_o INT; v_i INT;
BEGIN
    UPDATE pgtrickle.relay_outbox_config SET enabled = false WHERE name = p_name;
    GET DIAGNOSTICS v_o = ROW_COUNT;
    UPDATE pgtrickle.relay_inbox_config  SET enabled = false WHERE name = p_name;
    GET DIAGNOSTICS v_i = ROW_COUNT;
    IF v_o + v_i = 0 THEN
        RAISE EXCEPTION 'relay: pipeline "%" not found', p_name USING ERRCODE = 'no_data_found';
    END IF;
END;
$$;

-- RELAY-CAT (v0.29.0): delete_relay — delete a named pipeline.
CREATE OR REPLACE FUNCTION pgtrickle.delete_relay(p_name TEXT) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE v_o INT; v_i INT;
BEGIN
    DELETE FROM pgtrickle.relay_outbox_config WHERE name = p_name;
    GET DIAGNOSTICS v_o = ROW_COUNT;
    DELETE FROM pgtrickle.relay_inbox_config  WHERE name = p_name;
    GET DIAGNOSTICS v_i = ROW_COUNT;
    IF v_o + v_i = 0 THEN
        RAISE EXCEPTION 'relay: pipeline "%" not found', p_name USING ERRCODE = 'no_data_found';
    END IF;
END;
$$;

-- RELAY-CAT (v0.29.0): get_relay_config — fetch config for a single pipeline.
CREATE OR REPLACE FUNCTION pgtrickle.get_relay_config(p_name TEXT)
RETURNS TABLE (name TEXT, direction TEXT, enabled BOOLEAN, config JSONB)
LANGUAGE plpgsql SECURITY DEFINER AS $$
BEGIN
    RETURN QUERY
        SELECT r.name, 'forward'::TEXT, r.enabled, r.config
          FROM pgtrickle.relay_outbox_config r WHERE r.name = p_name
        UNION ALL
        SELECT r.name, 'reverse'::TEXT, r.enabled, r.config
          FROM pgtrickle.relay_inbox_config  r WHERE r.name = p_name;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'relay: pipeline "%" not found', p_name USING ERRCODE = 'no_data_found';
    END IF;
END;
$$;

-- RELAY-CAT (v0.29.0): list_relay_configs — list all pipelines.
CREATE OR REPLACE FUNCTION pgtrickle.list_relay_configs()
RETURNS TABLE (name TEXT, direction TEXT, enabled BOOLEAN, config JSONB)
LANGUAGE plpgsql SECURITY DEFINER AS $$
BEGIN
    RETURN QUERY
        SELECT r.name, 'forward'::TEXT, r.enabled, r.config
          FROM pgtrickle.relay_outbox_config r
        UNION ALL
        SELECT r.name, 'reverse'::TEXT, r.enabled, r.config
          FROM pgtrickle.relay_inbox_config  r
        ORDER BY name;
END;
$$;

-- Create the relay role if it does not exist.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pgtrickle_relay') THEN
        CREATE ROLE pgtrickle_relay NOLOGIN;
    END IF;
END;
$$;
"#,
    name = "pg_trickle_relay_catalog",
    requires = ["pg_trickle_outbox_inbox_catalog"],
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
