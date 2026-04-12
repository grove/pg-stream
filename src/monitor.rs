//! Monitoring, observability, and alerting for pgtrickle.
//!
//! # Statistics
//!
//! Per-ST statistics are tracked in shared memory via atomic counters and
//! exposed through the `pgtrickle.st_refresh_stats()` table-returning function
//! which aggregates from `pgtrickle.pgt_refresh_history`.
//!
//! The `pgtrickle.pg_stat_stream_tables` view combines catalog metadata with
//! runtime stats for a single-query operational overview.
//!
//! # NOTIFY Alerting
//!
//! Operational events are emitted via PostgreSQL `NOTIFY` on the
//! `pg_trickle_alert` channel. Clients can `LISTEN pg_trickle_alert;` to receive
//! JSON-formatted events:
//! - `stale_data` — scheduler is behind *and* data_timestamp is old (warning)
//! - `no_upstream_changes` — scheduler is healthy but source tables have no new writes (info)
//! - `auto_suspended` — ST suspended due to consecutive errors
//! - `reinitialize_needed` — upstream DDL change detected
//! - `buffer_growth_warning` — trigger-mode change buffers are growing
//! - `slot_lag_warning` — replication slot WAL retention growing

use pgrx::prelude::*;

use crate::catalog::{CdcMode, StDependency};
use crate::config;
use crate::error::PgTrickleError;
use crate::shmem;
use crate::wal_decoder;

// ── NOTIFY Alerting ────────────────────────────────────────────────────────

/// Alert event types emitted on the `pg_trickle_alert` NOTIFY channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertEvent {
    /// data staleness exceeds 2× schedule.
    StaleData,
    /// ST suspended after consecutive errors.
    AutoSuspended,
    /// ST resumed after suspension.
    Resumed,
    /// Upstream DDL change requires reinitialize.
    ReinitializeNeeded,
    /// Trigger-mode change buffers are growing.
    BufferGrowthWarning,
    /// WAL replication slot retention is growing.
    SlotLagWarning,
    /// Refresh completed successfully.
    RefreshCompleted,
    /// Refresh failed.
    RefreshFailed,
    /// EC-11: Refresh duration is approaching the schedule interval.
    SchedulerFallingBehind,
    /// NS-2: append_only flag automatically reverted after DELETE/UPDATE detected.
    AppendOnlyReverted,
    /// FUSE: Periodic reminder that a fuse is still blown.
    FuseBlownReminder,
    /// G-7: Stream table skipped because its tier is Frozen.
    FrozenTierSkip,
    /// CDC trigger missing or disabled on a source table.
    CdcTriggerDisabled,
    /// Change buffer cleanup persistently failing for a source OID.
    CleanupFailure,
    /// Scheduler is running normally but no upstream source rows have changed
    /// — data_timestamp is frozen because there is genuinely nothing new.
    NoUpstreamChanges,
    /// STAB-3 / PH-E2: Delta MERGE spilled to temp files, exceeding the
    /// configured `pg_trickle.spill_threshold_blocks` for consecutive refreshes.
    SpillThresholdExceeded,
}

impl AlertEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertEvent::StaleData => "stale_data",
            AlertEvent::AutoSuspended => "auto_suspended",
            AlertEvent::Resumed => "resumed",
            AlertEvent::ReinitializeNeeded => "reinitialize_needed",
            AlertEvent::BufferGrowthWarning => "buffer_growth_warning",
            AlertEvent::SlotLagWarning => "slot_lag_warning",
            AlertEvent::RefreshCompleted => "refresh_completed",
            AlertEvent::RefreshFailed => "refresh_failed",
            AlertEvent::SchedulerFallingBehind => "scheduler_falling_behind",
            AlertEvent::AppendOnlyReverted => "append_only_reverted",
            AlertEvent::FuseBlownReminder => "fuse_blown_reminder",
            AlertEvent::FrozenTierSkip => "frozen_tier_skip",
            AlertEvent::CdcTriggerDisabled => "cdc_trigger_disabled",
            AlertEvent::CleanupFailure => "cleanup_failure",
            AlertEvent::NoUpstreamChanges => "no_upstream_changes",
            AlertEvent::SpillThresholdExceeded => "spill_threshold_exceeded",
        }
    }
}

/// Emit a NOTIFY on the `pg_trickle_alert` channel with a JSON payload.
///
/// The payload is a JSON object with at minimum an `event` field.
/// Callers can add arbitrary key-value pairs for context.
///
/// PB2: When `skip_notify` is true, the NOTIFY is silently suppressed
/// (for stream tables in pooler compatibility mode).
pub fn emit_alert(
    event: AlertEvent,
    pgt_schema: &str,
    pgt_name: &str,
    extra: &str,
    skip_notify: bool,
) {
    if skip_notify {
        return;
    }
    let safe_payload = build_alert_payload(event, pgt_schema, pgt_name, extra);

    // Escape single quotes for SQL
    let escaped = safe_payload.replace('\'', "''");
    let sql = format!("NOTIFY pg_trickle_alert, '{}'", escaped);

    if let Err(e) = Spi::run(&sql) {
        pgrx::warning!("pg_trickle: failed to emit alert {}: {}", event.as_str(), e);
    }
}

/// Build a JSON alert payload for a NOTIFY event.
///
/// The payload is truncated to 7900 bytes if it exceeds PostgreSQL's
/// NOTIFY payload limit (~8000 bytes).
fn build_alert_payload(event: AlertEvent, pgt_schema: &str, pgt_name: &str, extra: &str) -> String {
    let payload = format!(
        r#"{{"event":"{}","pgt_schema":"{}","pgt_name":"{}","st":"{}",{}}}"#,
        event.as_str(),
        pgt_schema.replace('"', r#"\""#),
        pgt_name.replace('"', r#"\""#),
        format!("{}.{}", pgt_schema, pgt_name).replace('"', r#"\""#),
        extra,
    );

    // NOTIFY payloads are limited to ~8000 bytes; truncate if needed
    if payload.len() > 7900 {
        format!("{}...}}", &payload[..7890])
    } else {
        payload
    }
}

/// Emit a stale-data alert.
pub fn alert_stale_data(
    pgt_schema: &str,
    pgt_name: &str,
    staleness_secs: f64,
    schedule_secs: f64,
    skip_notify: bool,
) {
    emit_alert(
        AlertEvent::StaleData,
        pgt_schema,
        pgt_name,
        &format!(
            r#""staleness_seconds":{:.1},"schedule_seconds":{:.1},"ratio":{:.2}"#,
            staleness_secs,
            schedule_secs,
            if schedule_secs > 0.0 {
                staleness_secs / schedule_secs
            } else {
                0.0
            },
        ),
        skip_notify,
    );
}

/// Emit a no-upstream-changes informational event.
///
/// Fired when the scheduler is healthy (last_refresh_at is recent) but
/// data_timestamp has not advanced because no source rows changed.  This
/// is distinct from a genuine staleness problem.
pub fn alert_no_upstream_changes(
    pgt_schema: &str,
    pgt_name: &str,
    idle_secs: f64,
    skip_notify: bool,
) {
    emit_alert(
        AlertEvent::NoUpstreamChanges,
        pgt_schema,
        pgt_name,
        &format!(r#""idle_seconds":{:.1}"#, idle_secs),
        skip_notify,
    );
}

/// Emit an auto-suspended alert.
pub fn alert_auto_suspended(pgt_schema: &str, pgt_name: &str, error_count: i32, skip_notify: bool) {
    emit_alert(
        AlertEvent::AutoSuspended,
        pgt_schema,
        pgt_name,
        &format!(r#""consecutive_errors":{}"#, error_count),
        skip_notify,
    );
}

/// Emit a resumed alert (ST cleared from SUSPENDED back to ACTIVE).
pub fn alert_resumed(pgt_schema: &str, pgt_name: &str, skip_notify: bool) {
    emit_alert(
        AlertEvent::Resumed,
        pgt_schema,
        pgt_name,
        r#""previous_status":"SUSPENDED""#,
        skip_notify,
    );
}

/// Emit a reinitialize-needed alert.
pub fn alert_reinitialize_needed(
    pgt_schema: &str,
    pgt_name: &str,
    reason: &str,
    skip_notify: bool,
) {
    emit_alert(
        AlertEvent::ReinitializeNeeded,
        pgt_schema,
        pgt_name,
        &format!(r#""reason":"{}""#, reason.replace('"', r#"\""#)),
        skip_notify,
    );
}

/// Emit a buffer growth warning.
pub fn alert_buffer_growth(slot_name: &str, pending_bytes: i64) {
    let payload = format!(
        r#"{{"event":"buffer_growth_warning","slot_name":"{}","pending_bytes":{}}}"#,
        slot_name.replace('"', r#"\""#),
        pending_bytes,
    );
    let escaped = payload.replace('\'', "''");
    let sql = format!("NOTIFY pg_trickle_alert, '{}'", escaped);
    if let Err(e) = Spi::run(&sql) {
        pgrx::warning!("pg_trickle: failed to emit buffer_growth_warning: {}", e);
    }
}

/// Emit a WAL slot lag warning.
pub fn alert_slot_lag(slot_name: &str, retained_wal_bytes: i64, threshold_bytes: i64) {
    let payload = format!(
        r#"{{"event":"slot_lag_warning","slot_name":"{}","retained_wal_bytes":{},"threshold_bytes":{}}}"#,
        slot_name.replace('"', r#"\""#),
        retained_wal_bytes,
        threshold_bytes,
    );
    let escaped = payload.replace('\'', "''");
    let sql = format!("NOTIFY pg_trickle_alert, '{}'", escaped);
    if let Err(e) = Spi::run(&sql) {
        pgrx::warning!("pg_trickle: failed to emit slot_lag_warning: {}", e);
    }
}

fn collect_slot_health_rows() -> Vec<(String, i64, bool, i64, String)> {
    let mut rows = Vec::new();

    let trigger_rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    ct.slot_name,
                    ct.source_relid::bigint
                FROM pgtrickle.pgt_change_tracking ct",
                None,
                &[],
            )
            .unwrap_or_else(|e| pgrx::error!("slot_health: SPI select failed: {e}"));

        let mut out = Vec::new();
        for row in result {
            let slot = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let relid = row.get::<i64>(2).unwrap_or(None).unwrap_or(0);
            out.push((slot, relid));
        }
        out
    });

    let all_deps = StDependency::get_all().unwrap_or_default();
    let mut wal_sources = std::collections::HashMap::new();
    for dep in &all_deps {
        if matches!(dep.cdc_mode, CdcMode::Wal | CdcMode::Transitioning) {
            wal_sources
                .entry(dep.source_relid.to_u32())
                .or_insert((dep.cdc_mode, dep.slot_name.clone()));
        }
    }

    for (slot, relid) in trigger_rows {
        let source_oid_u32 = relid as u32;
        if let Some((mode, _)) = wal_sources.remove(&source_oid_u32) {
            let slot_name = wal_decoder::slot_name_for_source(pg_sys::Oid::from(source_oid_u32));
            let lag = wal_decoder::get_slot_lag_bytes(&slot_name).unwrap_or(0);
            rows.push((slot_name, relid, true, lag, mode.as_str().to_lowercase()));
        } else {
            rows.push((slot, relid, true, 0, "trigger".to_string()));
        }
    }

    for (oid_u32, (mode, slot_opt)) in wal_sources {
        let slot_name = slot_opt
            .unwrap_or_else(|| wal_decoder::slot_name_for_source(pg_sys::Oid::from(oid_u32)));
        let lag = wal_decoder::get_slot_lag_bytes(&slot_name).unwrap_or(0);
        rows.push((
            slot_name,
            oid_u32 as i64,
            true,
            lag,
            mode.as_str().to_lowercase(),
        ));
    }

    rows
}

fn build_cdc_health_alert(
    lag_bytes: i64,
    threshold_bytes: i64,
    slot_exists: bool,
    cdc_mode: CdcMode,
) -> Option<String> {
    if lag_bytes > threshold_bytes {
        Some(format!(
            "slot_lag_exceeds_threshold: {} bytes > {} bytes",
            lag_bytes, threshold_bytes
        ))
    } else if !slot_exists && cdc_mode == CdcMode::Wal {
        Some("replication_slot_missing".to_string())
    } else {
        None
    }
}

fn build_slot_lag_health_detail(lagging: &[String], threshold_bytes: i64) -> (String, String) {
    if lagging.is_empty() {
        (
            "OK".to_string(),
            "All WAL replication slots within normal range".to_string(),
        )
    } else {
        (
            "WARN".to_string(),
            format!(
                "{} WAL slot(s) retaining more than {} bytes: {}",
                lagging.len(),
                threshold_bytes,
                lagging.join(", ")
            ),
        )
    }
}

/// Emit a refresh-completed alert.
pub fn alert_refresh_completed(
    pgt_schema: &str,
    pgt_name: &str,
    action: &str,
    rows_inserted: i64,
    rows_deleted: i64,
    duration_ms: i64,
    skip_notify: bool,
) {
    emit_alert(
        AlertEvent::RefreshCompleted,
        pgt_schema,
        pgt_name,
        &format!(
            r#""action":"{}","rows_inserted":{},"rows_deleted":{},"duration_ms":{}"#,
            action, rows_inserted, rows_deleted, duration_ms,
        ),
        skip_notify,
    );
}

/// Emit a refresh-failed alert.
pub fn alert_refresh_failed(
    pgt_schema: &str,
    pgt_name: &str,
    action: &str,
    error: &str,
    skip_notify: bool,
) {
    emit_alert(
        AlertEvent::RefreshFailed,
        pgt_schema,
        pgt_name,
        &format!(
            r#""action":"{}","error":"{}""#,
            action,
            error.replace('"', r#"\""#),
        ),
        skip_notify,
    );
}

/// EC-11: Emit a scheduler-falling-behind alert when refresh duration
/// exceeds 80% of the schedule interval.
pub fn alert_falling_behind(
    pgt_schema: &str,
    pgt_name: &str,
    elapsed_ms: i64,
    schedule_ms: i64,
    ratio: f64,
    skip_notify: bool,
) {
    emit_alert(
        AlertEvent::SchedulerFallingBehind,
        pgt_schema,
        pgt_name,
        &format!(
            r#""elapsed_ms":{},"schedule_ms":{},"ratio":{:.2}"#,
            elapsed_ms, schedule_ms, ratio,
        ),
        skip_notify,
    );
}

/// STAB-3 / PH-E2: Emit a spill-threshold-exceeded alert when a refresh
/// spills more than `pg_trickle.spill_threshold_blocks` temp blocks to disk
/// for more than `consecutive` consecutive cycles.
pub fn alert_spill_threshold_exceeded(
    pgt_schema: &str,
    pgt_name: &str,
    temp_blks_written: i64,
    threshold_blocks: i64,
    consecutive: i32,
    limit: i32,
    skip_notify: bool,
) {
    emit_alert(
        AlertEvent::SpillThresholdExceeded,
        pgt_schema,
        pgt_name,
        &format!(
            r#""temp_blks_written":{},"threshold_blocks":{},"consecutive":{},"limit":{}"#,
            temp_blks_written, threshold_blocks, consecutive, limit,
        ),
        skip_notify,
    );
}

// ── SQL-exposed monitoring functions ───────────────────────────────────────

/// Return per-ST refresh statistics aggregated from the refresh history table.
///
/// This is the primary monitoring function, exposed as `pgtrickle.st_refresh_stats()`.
#[pg_extern(schema = "pgtrickle", name = "st_refresh_stats")]
#[allow(clippy::type_complexity)]
fn st_refresh_stats() -> TableIterator<
    'static,
    (
        name!(pgt_name, String),
        name!(pgt_schema, String),
        name!(status, String),
        name!(refresh_mode, String),
        name!(is_populated, bool),
        name!(total_refreshes, i64),
        name!(successful_refreshes, i64),
        name!(failed_refreshes, i64),
        name!(total_rows_inserted, i64),
        name!(total_rows_deleted, i64),
        name!(avg_duration_ms, f64),
        name!(last_refresh_action, Option<String>),
        name!(last_refresh_status, Option<String>),
        name!(last_refresh_at, Option<TimestampWithTimeZone>),
        name!(staleness_secs, Option<f64>),
        name!(stale, bool),
        name!(consecutive_errors, i32),
        name!(schedule, Option<String>),
        name!(refresh_tier, String),
        name!(last_error_message, Option<String>),
    ),
> {
    let rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    st.pgt_name,
                    st.pgt_schema,
                    st.status,
                    st.refresh_mode,
                    st.is_populated,
                    COALESCE(stats.total_refreshes, 0)::bigint,
                    COALESCE(stats.successful_refreshes, 0)::bigint,
                    COALESCE(stats.failed_refreshes, 0)::bigint,
                    COALESCE(stats.total_rows_inserted, 0)::bigint,
                    COALESCE(stats.total_rows_deleted, 0)::bigint,
                    COALESCE(stats.avg_duration_ms, 0)::float8,
                    last_hist.action,
                    last_hist.status,
                    st.last_refresh_at,
                    EXTRACT(EPOCH FROM (now() - st.data_timestamp))::float8,
                    COALESCE(
                        CASE WHEN st.schedule IS NOT NULL AND st.data_timestamp IS NOT NULL
                                  AND st.schedule NOT LIKE '% %'
                                  AND st.schedule NOT LIKE '@%'
                             -- Only stale when BOTH data_timestamp AND last_refresh_at
                             -- are old: scheduler itself is falling behind.
                             -- If last_refresh_at is recent the scheduler is healthy;
                             -- data_timestamp is frozen because there is nothing new
                             -- to refresh (no_upstream_changes), not a real problem.
                             THEN EXTRACT(EPOCH FROM (now() - st.data_timestamp)) >
                                      pgtrickle.parse_duration_seconds(st.schedule)
                                  AND EXTRACT(EPOCH FROM (now() - st.last_refresh_at)) >
                                      pgtrickle.parse_duration_seconds(st.schedule) * 3
                        END,
                    false),
                    st.consecutive_errors::integer,
                    st.schedule::text,
                    COALESCE(st.refresh_tier, 'hot')::text,
                    st.last_error_message::text
                FROM pgtrickle.pgt_stream_tables st
                LEFT JOIN LATERAL (
                    SELECT
                        count(*) AS total_refreshes,
                        count(*) FILTER (WHERE h.status = 'COMPLETED') AS successful_refreshes,
                        count(*) FILTER (WHERE h.status = 'FAILED') AS failed_refreshes,
                        COALESCE(sum(h.rows_inserted), 0) AS total_rows_inserted,
                        COALESCE(sum(h.rows_deleted), 0) AS total_rows_deleted,
                        CASE WHEN count(*) FILTER (WHERE h.end_time IS NOT NULL) > 0
                             THEN avg(EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000)
                                  FILTER (WHERE h.end_time IS NOT NULL)
                             ELSE 0
                        END AS avg_duration_ms
                    FROM pgtrickle.pgt_refresh_history h
                    WHERE h.pgt_id = st.pgt_id
                ) stats ON true
                LEFT JOIN LATERAL (
                    SELECT h2.action, h2.status
                    FROM pgtrickle.pgt_refresh_history h2
                    WHERE h2.pgt_id = st.pgt_id
                    ORDER BY h2.refresh_id DESC
                    LIMIT 1
                ) last_hist ON true
                ORDER BY st.pgt_schema, st.pgt_name",
                None,
                &[],
            )
            .unwrap_or_else(|e| pgrx::error!("st_refresh_stats: SPI select failed: {e}"));

        let mut out = Vec::new();
        for row in result {
            let pgt_name = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let pgt_schema = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let status = row.get::<String>(3).unwrap_or(None).unwrap_or_default();
            let refresh_mode = row.get::<String>(4).unwrap_or(None).unwrap_or_default();
            let is_populated = row.get::<bool>(5).unwrap_or(None).unwrap_or(false);
            let total_refreshes = row.get::<i64>(6).unwrap_or(None).unwrap_or(0);
            let successful = row.get::<i64>(7).unwrap_or(None).unwrap_or(0);
            let failed = row.get::<i64>(8).unwrap_or(None).unwrap_or(0);
            let rows_inserted = row.get::<i64>(9).unwrap_or(None).unwrap_or(0);
            let rows_deleted = row.get::<i64>(10).unwrap_or(None).unwrap_or(0);
            let avg_duration = row.get::<f64>(11).unwrap_or(None).unwrap_or(0.0);
            let last_action = row.get::<String>(12).unwrap_or(None);
            let last_status = row.get::<String>(13).unwrap_or(None);
            let last_refresh_at = row.get::<TimestampWithTimeZone>(14).unwrap_or(None);
            let staleness = row.get::<f64>(15).unwrap_or(None);
            let stale = row.get::<bool>(16).unwrap_or(None).unwrap_or(false);
            let consecutive_errors = row.get::<i32>(17).unwrap_or(None).unwrap_or(0);
            let schedule = row.get::<String>(18).unwrap_or(None);
            let refresh_tier = row
                .get::<String>(19)
                .unwrap_or(None)
                .unwrap_or_else(|| "hot".to_string());
            let last_error_message = row.get::<String>(20).unwrap_or(None);

            out.push((
                pgt_name,
                pgt_schema,
                status,
                refresh_mode,
                is_populated,
                total_refreshes,
                successful,
                failed,
                rows_inserted,
                rows_deleted,
                avg_duration,
                last_action,
                last_status,
                last_refresh_at,
                staleness,
                stale,
                consecutive_errors,
                schedule,
                refresh_tier,
                last_error_message,
            ));
        }
        out
    });

    TableIterator::new(rows)
}

/// Return refresh history for a specific ST, most recent first.
///
/// Exposed as `pgtrickle.get_refresh_history(name, limit)`.
#[pg_extern(schema = "pgtrickle", name = "get_refresh_history")]
#[allow(clippy::type_complexity)]
fn get_refresh_history(
    name: &str,
    max_rows: default!(i32, 20),
) -> TableIterator<
    'static,
    (
        name!(refresh_id, i64),
        name!(data_timestamp, TimestampWithTimeZone),
        name!(start_time, TimestampWithTimeZone),
        name!(end_time, Option<TimestampWithTimeZone>),
        name!(action, String),
        name!(status, String),
        name!(rows_inserted, i64),
        name!(rows_deleted, i64),
        name!(duration_ms, Option<f64>),
        name!(error_message, Option<String>),
    ),
> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    let (schema, table_name) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        ("public", parts[0])
    };

    let rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    h.refresh_id,
                    h.data_timestamp,
                    h.start_time,
                    h.end_time,
                    h.action,
                    h.status,
                    COALESCE(h.rows_inserted, 0)::bigint,
                    COALESCE(h.rows_deleted, 0)::bigint,
                    CASE WHEN h.end_time IS NOT NULL
                         THEN EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000
                         ELSE NULL
                    END::float8,
                    h.error_message
                FROM pgtrickle.pgt_refresh_history h
                JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = h.pgt_id
                WHERE st.pgt_schema = $1 AND st.pgt_name = $2
                ORDER BY h.refresh_id DESC
                LIMIT $3",
                None,
                &[schema.into(), table_name.into(), max_rows.into()],
            )
            .unwrap_or_else(|e| pgrx::error!("get_refresh_history: SPI select failed: {e}"));

        let mut out = Vec::new();
        let epoch_zero = TimestampWithTimeZone::try_from(0i64).unwrap_or_else(|_| {
            // This should never fail, but if it does, fall through gracefully.
            pgrx::error!("get_refresh_history: failed to construct epoch timestamp")
        });
        for row in result {
            let refresh_id = row.get::<i64>(1).unwrap_or(None).unwrap_or(0);
            let data_ts = row
                .get::<TimestampWithTimeZone>(2)
                .unwrap_or(None)
                .unwrap_or(epoch_zero);
            let start = row
                .get::<TimestampWithTimeZone>(3)
                .unwrap_or(None)
                .unwrap_or(epoch_zero);
            let end = row.get::<TimestampWithTimeZone>(4).unwrap_or(None);
            let action = row.get::<String>(5).unwrap_or(None).unwrap_or_default();
            let status = row.get::<String>(6).unwrap_or(None).unwrap_or_default();
            let ins = row.get::<i64>(7).unwrap_or(None).unwrap_or(0);
            let del = row.get::<i64>(8).unwrap_or(None).unwrap_or(0);
            let dur = row.get::<f64>(9).unwrap_or(None);
            let err = row.get::<String>(10).unwrap_or(None);

            out.push((
                refresh_id, data_ts, start, end, action, status, ins, del, dur, err,
            ));
        }
        out
    });

    TableIterator::new(rows)
}

/// Get the current staleness in seconds for a specific ST.
///
/// Returns NULL if the ST has never been refreshed.
/// Exposed as `pgtrickle.get_staleness(name)`.
/// Return the effective adaptive threshold for a stream table.
///
/// Returns the per-ST `auto_threshold` if set, otherwise the global
/// `pg_trickle.differential_max_change_ratio` GUC. Exposed as
/// `pgtrickle.st_auto_threshold(name)`.
#[pg_extern(schema = "pgtrickle", name = "st_auto_threshold")]
fn st_auto_threshold(name: &str) -> Option<f64> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    let (schema, table_name) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        ("public", parts[0])
    };

    let per_st = Spi::get_one_with_args::<f64>(
        "SELECT auto_threshold FROM pgtrickle.pgt_stream_tables \
         WHERE pgt_schema = $1 AND pgt_name = $2",
        &[schema.into(), table_name.into()],
    )
    .unwrap_or(None);

    per_st.or(Some(config::pg_trickle_differential_max_change_ratio()))
}

#[pg_extern(schema = "pgtrickle", name = "get_staleness")]
fn get_staleness(name: &str) -> Option<f64> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    let (schema, table_name) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        ("public", parts[0])
    };

    Spi::get_one_with_args::<f64>(
        "SELECT EXTRACT(EPOCH FROM (now() - data_timestamp))::float8 \
         FROM pgtrickle.pgt_stream_tables \
         WHERE pgt_schema = $1 AND pgt_name = $2 AND data_timestamp IS NOT NULL",
        &[schema.into(), table_name.into()],
    )
    .unwrap_or(None)
}

/// Check CDC trigger health for all tracked sources.
///
/// Returns trigger/slot name, source table, active status, retained WAL bytes,
/// and the CDC mode (`trigger`, `wal`, or `transitioning`).
/// Exposed as `pgtrickle.slot_health()` (kept for API compatibility).
#[pg_extern(schema = "pgtrickle", name = "slot_health")]
fn slot_health() -> TableIterator<
    'static,
    (
        name!(slot_name, String),
        name!(source_relid, i64),
        name!(active, bool),
        name!(retained_wal_bytes, i64),
        name!(wal_status, String),
    ),
> {
    TableIterator::new(collect_slot_health_rows())
}

// ── UX-1 / CACHE-OBS: Template cache observability ─────────────────────────

/// Return template cache statistics from shared memory.
///
/// Reports L1 (thread-local) hits, L2 (catalog table) hits, full misses
/// (DVM re-parse), evictions (generation flushes), and the current L1
/// cache size for this backend.
///
/// Exposed as `pgtrickle.cache_stats()`.
#[pg_extern(schema = "pgtrickle", name = "cache_stats")]
#[allow(clippy::type_complexity)]
fn cache_stats() -> TableIterator<
    'static,
    (
        name!(l1_hits, i64),
        name!(l2_hits, i64),
        name!(misses, i64),
        name!(evictions, i64),
        name!(l1_size, i32),
    ),
> {
    let (l1_hits, l2_hits, misses, evictions) = if crate::shmem::is_shmem_available() {
        (
            crate::shmem::TEMPLATE_CACHE_L1_HITS
                .get()
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
            crate::shmem::TEMPLATE_CACHE_L2_HITS
                .get()
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
            crate::shmem::TEMPLATE_CACHE_MISSES
                .get()
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
            crate::shmem::TEMPLATE_CACHE_EVICTIONS
                .get()
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
        )
    } else {
        (0, 0, 0, 0)
    };

    let l1_size = crate::dvm::delta_cache_size() as i32;

    TableIterator::once((l1_hits, l2_hits, misses, evictions, l1_size))
}

/// Explain the DVM plan for a stream table's defining query.
///
/// Returns whether the query supports differential refresh,
/// lists the operators found, and shows the generated delta query.
/// Exposed as `pgtrickle.explain_st(name)`.
#[pg_extern(schema = "pgtrickle", name = "explain_st")]
fn explain_st(
    name: &str,
) -> TableIterator<'static, (name!(property, String), name!(value, String))> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    let (schema, table_name) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        ("public", parts[0])
    };

    let rows = explain_st_impl(schema, table_name)
        .unwrap_or_else(|e| vec![("error".to_string(), e.to_string())]);

    TableIterator::new(rows)
}

fn explain_st_impl(
    schema: &str,
    table_name: &str,
) -> Result<Vec<(String, String)>, PgTrickleError> {
    use crate::catalog::StreamTableMeta;
    use crate::dvm;

    let st = StreamTableMeta::get_by_name(schema, table_name)?;

    let mut props = Vec::new();

    props.push((
        "pgt_name".to_string(),
        format!("{}.{}", st.pgt_schema, st.pgt_name),
    ));
    props.push(("defining_query".to_string(), st.defining_query.clone()));
    props.push((
        "refresh_mode".to_string(),
        st.refresh_mode.as_str().to_string(),
    ));
    props.push(("status".to_string(), st.status.as_str().to_string()));
    props.push(("is_populated".to_string(), st.is_populated.to_string()));

    // Parse the defining query to check DVM support
    match dvm::parse_defining_query(&st.defining_query) {
        Ok(op_tree) => {
            props.push(("dvm_supported".to_string(), "true".to_string()));
            props.push(("operator_tree".to_string(), format!("{:?}", op_tree)));

            let columns = op_tree.output_columns();
            props.push(("output_columns".to_string(), columns.join(", ")));

            let sources = op_tree.source_oids();
            props.push((
                "source_oids".to_string(),
                sources
                    .iter()
                    .map(|o| o.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));

            // G12-AGG: Expose per-aggregate maintenance strategies.
            let strategies = op_tree.aggregate_strategies();
            if !strategies.is_empty() {
                let strategy_json: Vec<String> = strategies
                    .iter()
                    .map(|(alias, strategy)| format!("\"{}\": \"{}\"", alias, strategy))
                    .collect();
                props.push((
                    "aggregate_strategies".to_string(),
                    format!("{{{}}}", strategy_json.join(", ")),
                ));
            }

            // Try generating delta query
            let prev_frontier = crate::version::Frontier::new();
            let new_frontier = crate::version::Frontier::new();
            match dvm::generate_delta_query(
                &st.defining_query,
                &prev_frontier,
                &new_frontier,
                &st.pgt_schema,
                &st.pgt_name,
            ) {
                Ok(result) => {
                    props.push(("delta_query".to_string(), result.delta_sql));
                }
                Err(e) => {
                    props.push(("delta_query_error".to_string(), e.to_string()));
                }
            }
        }
        Err(e) => {
            props.push(("dvm_supported".to_string(), "false".to_string()));
            props.push(("dvm_error".to_string(), e.to_string()));
        }
    }

    // Frontier info
    if let Some(ref frontier) = st.frontier {
        if let Ok(json) = frontier.to_json() {
            props.push(("frontier".to_string(), json));
        }
    } else {
        props.push(("frontier".to_string(), "null".to_string()));
    }

    // DAG-3: Amplification metrics from recent refresh history.
    // Query the last 20 DIFFERENTIAL refreshes and compute min/max/avg/latest
    // amplification ratio from existing columns (delta_row_count as input,
    // rows_inserted + rows_deleted as output).
    if let Ok(stats) = amplification_stats(st.pgt_id) {
        props.push(("amplification_stats".to_string(), stats));
    }

    // EXPL-ENH: Refresh timing stats from pgt_refresh_history.
    if let Ok(timing) = refresh_timing_stats(st.pgt_id) {
        props.push(("refresh_timing_stats".to_string(), timing));
    }

    // EXPL-ENH: Source partition info for partitioned table sources.
    if let Ok(partitions) = source_partition_info(&st)
        && !partitions.is_empty()
    {
        props.push(("source_partitions".to_string(), partitions));
    }

    // EXPL-ENH: Dependency sub-graph in DOT format.
    if let Ok(dot) = dependency_subgraph(st.pgt_id, &format!("{}.{}", st.pgt_schema, st.pgt_name)) {
        props.push(("dependency_graph_dot".to_string(), dot));
    }

    // PH-D1: Merge strategy GUC value.
    props.push((
        "merge_strategy".to_string(),
        crate::config::pg_trickle_merge_strategy()
            .as_str()
            .to_string(),
    ));

    // A-3-AO: Append-only mode.
    // "explicit" = user set append_only => true at creation/alter
    // "heuristic" = auto-promoted because buffer was insert-only
    // "disabled" = not using append-only INSERT path
    // We derive the mode from `is_append_only`. When the flag was set by the
    // user at creation it shows as "explicit"; when the heuristic promoted it
    // (no user intervention) we report "heuristic". We approximate this by
    // checking if effective_refresh_mode was ever APPEND_ONLY.
    let append_only_mode = if st.is_append_only { "on" } else { "off" };
    props.push(("append_only_mode".to_string(), append_only_mode.to_string()));

    // B-1: Aggregate fast-path status.
    // Detect whether the defining query has all-algebraic aggregates.
    let agg_fast_path_guc = crate::config::pg_trickle_aggregate_fast_path();
    let is_all_algebraic = if matches!(
        st.refresh_mode,
        crate::dag::RefreshMode::Differential | crate::dag::RefreshMode::Immediate
    ) {
        crate::dvm::parse_defining_query_full(&st.defining_query)
            .map(|pr| pr.tree.is_all_algebraic_agg())
            .unwrap_or(false)
    } else {
        false
    };
    let aggregate_path = if is_all_algebraic && agg_fast_path_guc {
        "explicit_dml"
    } else if is_all_algebraic {
        "merge (fast-path disabled)"
    } else {
        "merge"
    };
    props.push(("aggregate_path".to_string(), aggregate_path.to_string()));

    // C-4: Compaction threshold from GUC.
    let compact_threshold = crate::config::pg_trickle_compact_threshold();
    props.push((
        "compact_threshold".to_string(),
        compact_threshold.to_string(),
    ));

    // PH-E2: Live temp file spill info from pg_stat_statements.
    let spill_threshold = crate::config::pg_trickle_spill_threshold_blocks();
    if spill_threshold > 0 {
        let name = format!("{}.{}", st.pgt_schema, st.pgt_name);
        match query_temp_file_usage(&name) {
            Some((read_blks, written_blks)) => {
                let exceeds = written_blks > spill_threshold as i64;
                let info = format!(
                    "{{\"temp_blks_read\":{},\"temp_blks_written\":{},\"threshold\":{},\"exceeds_threshold\":{}}}",
                    read_blks, written_blks, spill_threshold, exceeds
                );
                props.push(("spill_info".to_string(), info));
            }
            None => {
                props.push((
                    "spill_info".to_string(),
                    "{\"status\":\"pg_stat_statements not available or no matching statement\"}"
                        .to_string(),
                ));
            }
        }
    }

    // G14-SHC: Template cache stats.
    props.push((
        "template_cache".to_string(),
        crate::config::pg_trickle_template_cache_enabled().to_string(),
    ));
    if crate::shmem::is_shmem_available() {
        let l2_hits = crate::shmem::TEMPLATE_CACHE_L2_HITS
            .get()
            .load(std::sync::atomic::Ordering::Relaxed);
        let misses = crate::shmem::TEMPLATE_CACHE_MISSES
            .get()
            .load(std::sync::atomic::Ordering::Relaxed);
        props.push((
            "template_cache_stats".to_string(),
            format!("{{\"l2_hits\":{},\"full_misses\":{}}}", l2_hits, misses),
        ));
    }

    Ok(props)
}

// ── DAG-3: Amplification Statistics ─────────────────────────────────────

/// Query the last 20 DIFFERENTIAL refreshes for a stream table and compute
/// amplification ratio statistics (min, max, avg, latest).
///
/// Returns a JSON string like:
/// `{"samples":15,"min":1.0,"max":42.5,"avg":8.3,"latest":12.1,"threshold":100.0}`
///
/// Returns an error if no DIFFERENTIAL refreshes are recorded yet.
fn amplification_stats(pgt_id: i64) -> Result<String, PgTrickleError> {
    let threshold = crate::config::pg_trickle_delta_amplification_threshold();

    let sql = format!(
        "SELECT delta_row_count, rows_inserted, rows_deleted \
         FROM pgtrickle.pgt_refresh_history \
         WHERE pgt_id = {pgt_id} \
           AND action = 'DIFFERENTIAL' \
           AND status = 'COMPLETED' \
           AND delta_row_count > 0 \
         ORDER BY refresh_id DESC \
         LIMIT 20"
    );

    let mut ratios: Vec<f64> = Vec::new();
    Spi::connect(|client| {
        let cursor = client.select(&sql, None, &[]).map_err(|e| {
            PgTrickleError::SpiError(format!("amplification_stats query failed: {e}"))
        })?;
        for row in cursor {
            let input: i64 = row.get::<i64>(1).unwrap_or(Some(0)).unwrap_or(0);
            let inserted: i64 = row.get::<i64>(2).unwrap_or(Some(0)).unwrap_or(0);
            let deleted: i64 = row.get::<i64>(3).unwrap_or(Some(0)).unwrap_or(0);
            let output = inserted + deleted;
            let ratio = crate::refresh::compute_amplification_ratio(input, output);
            ratios.push(ratio);
        }
        Ok::<(), PgTrickleError>(())
    })?;

    if ratios.is_empty() {
        return Err(PgTrickleError::InternalError(
            "no DIFFERENTIAL refresh history available".to_string(),
        ));
    }

    let min = ratios.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = ratios.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let avg = ratios.iter().sum::<f64>() / ratios.len() as f64;
    let latest = ratios[0]; // first row = most recent

    Ok(format!(
        "{{\"samples\":{},\"min\":{:.2},\"max\":{:.2},\"avg\":{:.2},\"latest\":{:.2},\"threshold\":{:.1}}}",
        ratios.len(),
        min,
        max,
        avg,
        latest,
        threshold,
    ))
}

// ── EXPL-ENH: Refresh Timing Statistics ─────────────────────────────────

/// Query the last 20 completed refreshes (any action) and compute duration
/// statistics in milliseconds.
///
/// Returns a JSON string like:
/// `{"samples":10,"min_ms":12.3,"max_ms":450.0,"avg_ms":85.7,"latest_ms":42.1,"latest_action":"DIFFERENTIAL"}`
fn refresh_timing_stats(pgt_id: i64) -> Result<String, PgTrickleError> {
    let sql = format!(
        "SELECT action::text, \
                EXTRACT(EPOCH FROM (end_time - start_time)) * 1000 AS duration_ms \
         FROM pgtrickle.pgt_refresh_history \
         WHERE pgt_id = {pgt_id} \
           AND status = 'COMPLETED' \
           AND end_time IS NOT NULL \
         ORDER BY refresh_id DESC \
         LIMIT 20"
    );

    let mut durations: Vec<f64> = Vec::new();
    let mut latest_action = String::new();

    Spi::connect(|client| {
        let cursor = client.select(&sql, None, &[]).map_err(|e| {
            PgTrickleError::SpiError(format!("refresh_timing_stats query failed: {e}"))
        })?;
        for row in cursor {
            let action: String = row
                .get::<String>(1)
                .unwrap_or(Some(String::new()))
                .unwrap_or_default();
            let ms: f64 = row.get::<f64>(2).unwrap_or(Some(0.0)).unwrap_or(0.0);
            if durations.is_empty() {
                latest_action = action;
            }
            durations.push(ms);
        }
        Ok::<(), PgTrickleError>(())
    })?;

    if durations.is_empty() {
        return Err(PgTrickleError::InternalError(
            "no completed refresh history available".to_string(),
        ));
    }

    let min = durations.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = durations.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let avg = durations.iter().sum::<f64>() / durations.len() as f64;
    let latest = durations[0];

    Ok(format!(
        "{{\"samples\":{},\"min_ms\":{:.1},\"max_ms\":{:.1},\"avg_ms\":{:.1},\"latest_ms\":{:.1},\"latest_action\":\"{}\"}}",
        durations.len(),
        min,
        max,
        avg,
        latest,
        latest_action,
    ))
}

// ── EXPL-ENH: Source Partition Info ──────────────────────────────────────

/// For each source table, check if it is partitioned and report the
/// partition strategy and key.
///
/// Returns a JSON array like:
/// `[{"source":"public.orders","strategy":"RANGE","key":"order_date","partitions":12}]`
fn source_partition_info(st: &crate::catalog::StreamTableMeta) -> Result<String, PgTrickleError> {
    let sql = format!(
        "SELECT d.source_relid, \
                c.relkind, \
                c.relnamespace::regnamespace::text AS schema, \
                c.relname::text AS name, \
                pg_catalog.pg_get_partkeydef(c.oid) AS partkey, \
                (SELECT count(*) FROM pg_inherits i WHERE i.inhparent = c.oid) AS nparts \
         FROM pgtrickle.pgt_dependencies d \
         JOIN pg_class c ON c.oid = d.source_relid \
         WHERE d.pgt_id = {} \
           AND c.relkind = 'p'",
        st.pgt_id
    );

    let mut entries: Vec<String> = Vec::new();
    Spi::connect(|client| {
        let cursor = client.select(&sql, None, &[]).map_err(|e| {
            PgTrickleError::SpiError(format!("source_partition_info query failed: {e}"))
        })?;
        for row in cursor {
            let schema: String = row
                .get::<String>(3)
                .unwrap_or(Some(String::new()))
                .unwrap_or_default();
            let name: String = row
                .get::<String>(4)
                .unwrap_or(Some(String::new()))
                .unwrap_or_default();
            let partkey: String = row
                .get::<String>(5)
                .unwrap_or(Some(String::new()))
                .unwrap_or_default();
            let nparts: i64 = row.get::<i64>(6).unwrap_or(Some(0)).unwrap_or(0);
            entries.push(format!(
                "{{\"source\":\"{}.{}\",\"partition_key\":\"{}\",\"partitions\":{}}}",
                schema, name, partkey, nparts,
            ));
        }
        Ok::<(), PgTrickleError>(())
    })?;

    Ok(format!("[{}]", entries.join(",")))
}

// ── EXPL-ENH: Dependency Sub-graph ──────────────────────────────────────

/// Build a DOT-format dependency sub-graph for a stream table showing its
/// immediate upstream sources and downstream dependents.
fn dependency_subgraph(pgt_id: i64, st_name: &str) -> Result<String, PgTrickleError> {
    use crate::dag::{NodeId, StDag};

    let fallback_secs = crate::config::pg_trickle_min_schedule_seconds();
    let dag = StDag::build_from_catalog(fallback_secs)?;

    let node = NodeId::StreamTable(pgt_id);
    let upstream = dag.get_upstream(node);
    let downstream = dag.get_downstream(node);

    let mut dot = String::from("digraph dependency_subgraph {\n");
    dot.push_str(&format!(
        "  \"{}\" [shape=box, style=filled, fillcolor=lightblue];\n",
        st_name
    ));

    for up in &upstream {
        let label = dag.node_label(up);
        let shape = match up {
            NodeId::BaseTable(_) => "ellipse",
            NodeId::StreamTable(_) => "box",
        };
        dot.push_str(&format!("  \"{}\" [shape={}];\n", label, shape));
        dot.push_str(&format!("  \"{}\" -> \"{}\";\n", label, st_name));
    }

    for down in &downstream {
        let label = dag.node_label(down);
        dot.push_str(&format!("  \"{}\" [shape=box];\n", label));
        dot.push_str(&format!("  \"{}\" -> \"{}\";\n", st_name, label));
    }

    dot.push('}');
    Ok(dot)
}

// ── CDC Health Monitoring ───────────────────────────────────────────────────

/// Check CDC health for all tracked sources.
///
/// Returns per-source health status including CDC mode, estimated lag,
/// last confirmed LSN, and whether the slot lag exceeds a threshold.
///
/// Exposed as `pgtrickle.check_cdc_health()`.
#[pg_extern(schema = "pgtrickle", name = "check_cdc_health")]
#[allow(clippy::type_complexity)]
fn check_cdc_health() -> TableIterator<
    'static,
    (
        name!(source_relid, i64),
        name!(source_table, String),
        name!(cdc_mode, String),
        name!(slot_name, Option<String>),
        name!(lag_bytes, Option<i64>),
        name!(confirmed_lsn, Option<String>),
        name!(alert, Option<String>),
        name!(selective_capture, bool),
    ),
> {
    let all_deps = StDependency::get_all().unwrap_or_default();
    let mut rows = Vec::new();
    let mut seen_sources = std::collections::HashSet::new();
    let lag_alert_threshold = config::pg_trickle_slot_lag_critical_threshold_bytes();

    for dep in &all_deps {
        if dep.source_type != "TABLE" {
            continue;
        }
        let oid_u32 = dep.source_relid.to_u32();
        if !seen_sources.insert(oid_u32) {
            continue;
        }

        // Resolve source table name
        let source_name = Spi::get_one_with_args::<String>(
            "SELECT $1::oid::regclass::text",
            &[dep.source_relid.into()],
        )
        .unwrap_or(None)
        .unwrap_or_else(|| format!("oid:{}", oid_u32));

        let mode_str = dep.cdc_mode.as_str().to_string();

        // F15: is selective column capture active for this source?
        let selective = crate::cdc::is_selective_capture_active(dep.source_relid);

        match dep.cdc_mode {
            CdcMode::Trigger => {
                rows.push((
                    oid_u32 as i64,
                    source_name,
                    mode_str,
                    None,
                    None,
                    None,
                    None,
                    selective,
                ));
            }
            CdcMode::Wal | CdcMode::Transitioning => {
                let slot = dep
                    .slot_name
                    .clone()
                    .unwrap_or_else(|| wal_decoder::slot_name_for_source(dep.source_relid));
                let lag = wal_decoder::get_slot_lag_bytes(&slot).unwrap_or(0);
                let lsn = dep.decoder_confirmed_lsn.clone();

                let slot_exists = Spi::get_one_with_args::<bool>(
                    "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
                    &[slot.as_str().into()],
                )
                .unwrap_or(Some(false))
                .unwrap_or(false);

                let alert =
                    build_cdc_health_alert(lag, lag_alert_threshold, slot_exists, dep.cdc_mode);

                rows.push((
                    oid_u32 as i64,
                    source_name,
                    mode_str,
                    Some(slot),
                    Some(lag),
                    lsn,
                    alert,
                    selective,
                ));
            }
        }
    }

    TableIterator::new(rows)
}

// ── CDC Transition NOTIFY ──────────────────────────────────────────────────

/// Emit a `NOTIFY pg_trickle_cdc_transition` with a JSON payload when a
/// source transitions between CDC modes.
///
/// Payload includes source table name, old mode, new mode, and slot name.
pub fn emit_cdc_transition_notify(
    source_oid: pg_sys::Oid,
    old_mode: CdcMode,
    new_mode: CdcMode,
    slot_name: Option<&str>,
) {
    let source_name =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_oid.into()])
            .unwrap_or(None)
            .unwrap_or_else(|| format!("oid:{}", source_oid.to_u32()));

    let payload = format!(
        r#"{{"event":"cdc_transition","source_table":"{}","old_mode":"{}","new_mode":"{}","slot_name":{}}}"#,
        source_name.replace('"', r#"\""#),
        old_mode.as_str(),
        new_mode.as_str(),
        match slot_name {
            Some(s) => format!("\"{}\"", s.replace('"', r#"\""#)),
            None => "null".to_string(),
        },
    );

    let escaped = payload.replace('\'', "''");
    let sql = format!("NOTIFY pg_trickle_cdc_transition, '{}'", escaped);

    if let Err(e) = Spi::run(&sql) {
        pgrx::warning!("pg_trickle: failed to emit cdc_transition NOTIFY: {}", e);
    }
}

// ── Slot Health Monitoring (used by scheduler) ─────────────────────────────

/// Check all tracked change buffers and WAL replication slots and emit alerts
/// when either exceeds the configured threshold. Called from the scheduler loop.
///
/// This function must be called in its own clean transaction (separate from the
/// Phase 1 WAL poll transaction). If Phase 1's WAL poll for a missing slot
/// leaves the SPI session in an inconsistent state, the EC-34 slot-existence
/// check inside this function would fail silently and the TRIGGER fallback
/// would never fire. Running here in a fresh transaction guarantees SPI works.
pub fn check_slot_health_and_alert() {
    // With trigger-based CDC, we check pending change buffer size instead
    // of replication slot WAL retention. Alert if buffer tables grow too large.
    let change_schema = config::pg_trickle_change_buffer_schema();

    // Gracefully handle SPI failures (e.g. if called during transaction
    // recovery or in a degraded state) — skip rather than panic.
    let sources = Spi::connect(|client| -> Vec<(String, i64)> {
        match client.select(
            "SELECT ct.slot_name, ct.source_relid::bigint \
             FROM pgtrickle.pgt_change_tracking ct",
            None,
            &[],
        ) {
            Ok(result) => {
                let mut out = Vec::new();
                for row in result {
                    let trigger = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
                    let relid = row.get::<i64>(2).unwrap_or(None).unwrap_or(0);
                    out.push((trigger, relid));
                }
                out
            }
            Err(_) => {
                // SPI error in this query is non-fatal — skip buffer check.
                Vec::new()
            }
        }
    });

    for (trigger_name, relid) in sources {
        // Check buffer table row count as a proxy for staleness
        let pending = Spi::get_one::<i64>(&format!(
            "SELECT count(*)::bigint FROM {}.changes_{}",
            change_schema, relid
        ))
        .unwrap_or(Some(0))
        .unwrap_or(0);

        // F46 (G9.3): Alert if more than the configured threshold of pending changes
        let threshold = config::pg_trickle_buffer_alert_threshold();
        if pending > threshold {
            alert_buffer_growth(&trigger_name, pending);
        }
    }

    let lag_warning_threshold = config::pg_trickle_slot_lag_warning_threshold_bytes();
    for (slot_name, _source_relid, _active, retained_wal_bytes, wal_status) in
        collect_slot_health_rows()
    {
        if wal_status != "trigger" && retained_wal_bytes > lag_warning_threshold {
            alert_slot_lag(&slot_name, retained_wal_bytes, lag_warning_threshold);
        }
    }

    // EC-34: Check that WAL-mode dependencies still have their replication
    // slots. If a slot was dropped (e.g., by a DBA), fall back to TRIGGER
    // mode and emit a WARNING so the operator knows.
    check_wal_slot_existence();
}

/// EC-34: Verify that WAL-mode dependencies still have their replication
/// slots. If a slot is missing, fall back to TRIGGER mode and warn.
fn check_wal_slot_existence() {
    use crate::catalog::{CdcMode, StDependency};

    let deps = match StDependency::get_all() {
        Ok(d) => d,
        Err(_) => return,
    };

    for dep in &deps {
        if dep.cdc_mode != CdcMode::Wal {
            continue;
        }

        let slot_name = match &dep.slot_name {
            Some(name) => name.clone(),
            None => crate::wal_decoder::slot_name_for_source(dep.source_relid),
        };

        // Check if the replication slot exists
        let slot_exists = Spi::get_one_with_args::<bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
            &[slot_name.as_str().into()],
        )
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if !slot_exists {
            pgrx::warning!(
                "pg_trickle: replication slot '{}' for source OID {} is missing. \
                 Falling back to trigger-based CDC. The slot may have been dropped \
                 manually or by a management tool.",
                slot_name,
                dep.source_relid.to_u32(),
            );

            // Fall back to TRIGGER mode — abort_wal_transition handles
            // slot cleanup, publication removal, catalog update, AND
            // trigger recreation.
            let change_schema = crate::config::pg_trickle_change_buffer_schema();
            if let Err(e) =
                wal_decoder::abort_wal_transition(dep.source_relid, dep.pgt_id, &change_schema)
            {
                pgrx::warning!(
                    "pg_trickle: failed to fall back to TRIGGER mode for source OID {}: {}",
                    dep.source_relid.to_u32(),
                    e,
                );
            }
        }
    }
}

/// Periodic check for disabled or missing CDC triggers on source tables.
///
/// Called from the scheduler main loop every ~60s.  Detects source tables
/// whose pg_trickle CDC trigger is either:
///   - missing entirely (dropped without DDL hook firing)
///   - explicitly disabled via `ALTER TABLE … DISABLE TRIGGER`
///
/// Emits a `cdc_trigger_disabled` NOTIFY alert for each affected source
/// so operators are notified before data staleness goes undetected.
pub fn check_cdc_trigger_health() {
    use crate::catalog::StDependency;

    let deps = match StDependency::get_all() {
        Ok(d) => d,
        Err(_) => return,
    };

    // Collect unique source OIDs that use trigger-mode CDC.
    let mut checked = std::collections::HashSet::new();
    for dep in &deps {
        if dep.cdc_mode != crate::catalog::CdcMode::Trigger {
            continue;
        }
        let oid = dep.source_relid;
        if !checked.insert(oid) {
            continue; // already checked
        }

        let oid_u32 = oid.to_u32();

        // Check if any DML CDC trigger exists AND is enabled for this source.
        // tgenabled values: 'O' = origin (enabled), 'D' = disabled,
        // 'R' = replica, 'A' = always.  Anything other than 'D' is enabled.
        let trigger_ok = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS( \
               SELECT 1 FROM pg_trigger \
               WHERE tgrelid = {oid_u32}::oid \
                 AND tgname IN ( \
                     'pg_trickle_cdc_{oid_u32}', \
                     'pg_trickle_cdc_ins_{oid_u32}', \
                     'pg_trickle_cdc_upd_{oid_u32}', \
                     'pg_trickle_cdc_del_{oid_u32}' \
                 ) \
                 AND tgenabled != 'D' \
             )",
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if !trigger_ok {
            // Look up the source table name for a useful alert message.
            let source_name = Spi::get_one::<String>(&format!(
                "SELECT n.nspname::text || '.' || c.relname::text \
                 FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.oid = {oid_u32}::oid",
            ))
            .unwrap_or(None)
            .unwrap_or_else(|| format!("oid={}", oid_u32));

            emit_alert(
                AlertEvent::CdcTriggerDisabled,
                "", // no single ST schema — source-level alert
                &source_name,
                &format!(r#""source_oid":{}"#, oid_u32),
                false, // always emit, even in pooler mode
            );
        }
    }
}

// ── Temp File / Memory Usage Tracking (F45: G9.2) ──────────────────────────

/// Query `pg_stat_statements` for the temp-file metrics of a recently executed
/// MERGE (or delta query) containing the specified table name.
///
/// Returns `(temp_blks_read, temp_blks_written)` if `pg_stat_statements` is
/// available and a matching statement was found. Returns `None` if the
/// extension is not installed or no match is found.
///
/// This provides post-hoc visibility into whether large deltas spilled to
/// temporary files, which may indicate `work_mem` is too low.
pub fn query_temp_file_usage(table_name: &str) -> Option<(i64, i64)> {
    // Check if pg_stat_statements is available
    let available = Spi::get_one::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !available {
        return None;
    }

    // Look for the most recent MERGE statement referencing this table
    let escaped = table_name.replace('\'', "''");
    let result = Spi::get_two::<i64, i64>(&format!(
        "SELECT temp_blks_read::bigint, temp_blks_written::bigint \
         FROM pg_stat_statements \
         WHERE query LIKE '%MERGE%{escaped}%' \
         ORDER BY total_exec_time DESC LIMIT 1",
    ));

    match result {
        Ok((Some(read), Some(written))) => {
            if written > 0 {
                pgrx::log!(
                    "pg_trickle: MERGE for {} used {} temp blocks read, {} written \
                     — consider increasing work_mem or lowering differential_max_change_ratio",
                    table_name,
                    read,
                    written,
                );
            }
            Some((read, written))
        }
        _ => None,
    }
}

/// Show pending change counts and estimated disk sizes for all CDC-tracked
/// source tables.
///
/// Returns one row per `(stream_table, source_table)` pair.
/// `pending_rows` is the number of CDC rows not yet consumed by a differential
/// refresh; `buffer_bytes` is the estimated on-disk size of the change buffer.
///
/// Exposed as `pgtrickle.change_buffer_sizes()`.
#[pg_extern(schema = "pgtrickle", name = "change_buffer_sizes")]
#[allow(clippy::type_complexity)]
fn change_buffer_sizes() -> TableIterator<
    'static,
    (
        name!(stream_table, String),
        name!(source_table, String),
        name!(source_oid, i64),
        name!(cdc_mode, String),
        name!(pending_rows, i64),
        name!(buffer_bytes, i64),
    ),
> {
    let rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    st.pgt_schema || '.' || st.pgt_name        AS stream_table,
                    n.nspname::text || '.' || c.relname::text  AS source_table,
                    d.source_relid::bigint,
                    d.cdc_mode,
                    COALESCE(s.n_live_tup, 0)::bigint          AS pending_rows,
                    COALESCE(pg_total_relation_size(cb.oid), 0)::bigint
                                                               AS buffer_bytes
                FROM pgtrickle.pgt_dependencies d
                JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id
                JOIN pg_class c                     ON c.oid = d.source_relid
                JOIN pg_namespace n                 ON n.oid = c.relnamespace
                LEFT JOIN pg_class cb
                    ON  cb.relname = 'changes_' || d.source_relid::text
                    AND cb.relnamespace = (
                            SELECT oid FROM pg_namespace
                            WHERE  nspname = COALESCE(
                                current_setting('pg_trickle.change_buffer_schema', true),
                                'pgtrickle_changes'))
                LEFT JOIN pg_stat_user_tables s ON s.relid = cb.oid
                ORDER BY stream_table, source_table",
                None,
                &[],
            )
            .unwrap_or_else(|e| pgrx::error!("change_buffer_sizes: SPI select failed: {e}"));

        let mut out = Vec::new();
        for row in result {
            let stream_table = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let source_table = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let source_oid = row.get::<i64>(3).unwrap_or(None).unwrap_or(0);
            let cdc_mode = row.get::<String>(4).unwrap_or(None).unwrap_or_default();
            let pending_rows = row.get::<i64>(5).unwrap_or(None).unwrap_or(0);
            let buffer_bytes = row.get::<i64>(6).unwrap_or(None).unwrap_or(0);
            out.push((
                stream_table,
                source_table,
                source_oid,
                cdc_mode,
                pending_rows,
                buffer_bytes,
            ));
        }
        out
    });

    TableIterator::new(rows)
}

/// List the source tables that a stream table depends on.
///
/// Returns one row per source, including its CDC mode and any column-level
/// usage metadata recorded at creation time.
///
/// Exposed as `pgtrickle.list_sources(name)`.
#[pg_extern(schema = "pgtrickle", name = "list_sources")]
#[allow(clippy::type_complexity)]
fn list_sources(
    name: &str,
) -> TableIterator<
    'static,
    (
        name!(source_table, String),
        name!(source_oid, i64),
        name!(source_type, String),
        name!(cdc_mode, String),
        name!(columns_used, Option<String>),
    ),
> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    let (schema, table_name) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        ("public", parts[0])
    };

    let rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    n.nspname::text || '.' || c.relname::text AS source_table,
                    d.source_relid::bigint,
                    d.source_type,
                    d.cdc_mode,
                    d.columns_used::text
                FROM pgtrickle.pgt_dependencies d
                JOIN pgtrickle.pgt_stream_tables st
                    ON  st.pgt_id     = d.pgt_id
                    AND st.pgt_schema = $1
                    AND st.pgt_name   = $2
                JOIN pg_class     c ON c.oid = d.source_relid
                JOIN pg_namespace n ON n.oid = c.relnamespace
                ORDER BY source_table",
                None,
                &[schema.into(), table_name.into()],
            )
            .unwrap_or_else(|e| pgrx::error!("list_sources: SPI select failed: {e}"));

        let mut out = Vec::new();
        for row in result {
            let source_table = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let source_oid = row.get::<i64>(2).unwrap_or(None).unwrap_or(0);
            let source_type = row.get::<String>(3).unwrap_or(None).unwrap_or_default();
            let cdc_mode = row.get::<String>(4).unwrap_or(None).unwrap_or_default();
            let columns_used = row.get::<String>(5).unwrap_or(None);
            out.push((
                source_table,
                source_oid,
                source_type,
                cdc_mode,
                columns_used,
            ));
        }
        out
    });

    TableIterator::new(rows)
}

/// Return a visual ASCII tree of all stream table dependencies.
///
/// Each row represents one stream table node. The `tree_line` column contains
/// the indented tree rendering (using `├──` / `└──` / `│` box-drawing
/// characters). Roots (stream tables with no stream-table parents) are shown
/// at depth 0; each dependent is indented beneath its parent.
///
/// Source tables (ordinary tables, views) that feed into a stream table are
/// shown as leaf nodes with a `[src]` tag so the full data lineage is visible.
///
/// Exposed as `pgtrickle.dependency_tree()`.
#[pg_extern(schema = "pgtrickle", name = "dependency_tree")]
#[allow(clippy::type_complexity)]
fn dependency_tree() -> TableIterator<
    'static,
    (
        name!(tree_line, String),
        name!(node, String),
        name!(node_type, String),
        name!(depth, i32),
        name!(status, Option<String>),
        name!(refresh_mode, Option<String>),
    ),
> {
    // ── 1. Load all stream tables ───────────────────────────────────────────
    // Map qualified_name -> (status, refresh_mode)
    let st_info: std::collections::HashMap<String, (String, String)> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    pgt_schema || '.' || pgt_name,
                    status::text,
                    refresh_mode::text
                 FROM pgtrickle.pgt_stream_tables",
                None,
                &[],
            )
            .unwrap_or_else(|e| pgrx::error!("dependency_tree: failed to load stream tables: {e}"));

        let mut m = std::collections::HashMap::new();
        for row in result {
            let name = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let status = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let mode = row.get::<String>(3).unwrap_or(None).unwrap_or_default();
            if !name.is_empty() {
                m.insert(name, (status, mode));
            }
        }
        m
    });

    // ── 2. Load all dependency edges ───────────────────────────────────────
    // Each ST may depend on: (a) other stream tables, or (b) plain source tables.
    // We collect both kinds. For plain sources we only store them as leaf
    // display nodes attached to their consumer ST.
    //
    // st_children:  parent_st -> Vec<child_st>   (ST-to-ST edges)
    // st_sources:   consumer_st -> Vec<source_qualified_name>  (ST-to-plain-table)
    let mut st_children: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut st_sources: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    // Initialise child lists for all known STs so roots are discoverable.
    for name in st_info.keys() {
        st_children.entry(name.clone()).or_default();
        st_sources.entry(name.clone()).or_default();
    }

    Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    st.pgt_schema || '.' || st.pgt_name   AS consumer,
                    n.nspname::text || '.' || c.relname::text AS source,
                    -- is the source itself a stream table?
                    EXISTS (
                        SELECT 1 FROM pgtrickle.pgt_stream_tables st2
                        WHERE st2.pgt_schema = n.nspname::text
                          AND st2.pgt_name   = c.relname::text
                    ) AS is_st
                 FROM pgtrickle.pgt_dependencies d
                 JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id
                 JOIN pg_class     c ON c.oid = d.source_relid
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 ORDER BY consumer, source",
                None,
                &[],
            )
            .unwrap_or_else(|e| pgrx::error!("dependency_tree: failed to load dependencies: {e}"));

        for row in result {
            let consumer = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let source = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let is_st = row.get::<bool>(3).unwrap_or(None).unwrap_or(false);
            if consumer.is_empty() || source.is_empty() {
                continue;
            }
            if is_st {
                // source is itself a stream table → ST-to-ST edge
                // Record: source_st has consumer_st as a child
                st_children
                    .entry(source.clone())
                    .or_default()
                    .push(consumer.clone());
            } else {
                // plain source table / view
                st_sources
                    .entry(consumer.clone())
                    .or_default()
                    .push(source.clone());
            }
        }
    });

    // ── 3. Find roots (stream tables that are not the child of any other ST) ─
    let output = render_dependency_tree(&st_info, &st_children, &st_sources);

    TableIterator::new(output)
}

/// Context for the dependency tree DFS traversal.
struct DagCtx<'a> {
    st_info: &'a std::collections::HashMap<String, (String, String)>,
    st_children: &'a std::collections::HashMap<String, Vec<String>>,
    st_sources: &'a std::collections::HashMap<String, Vec<String>>,
}

/// Render a dependency tree from pre-loaded ST metadata.
///
/// Pure function: takes three `HashMap`s describing the graph topology
/// and returns formatted tree rows with box-drawing prefixes.
#[allow(clippy::type_complexity)]
fn render_dependency_tree(
    st_info: &std::collections::HashMap<String, (String, String)>,
    st_children: &std::collections::HashMap<String, Vec<String>>,
    st_sources: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<(String, String, String, i32, Option<String>, Option<String>)> {
    let all_children: std::collections::HashSet<String> = st_children
        .values()
        .flat_map(|v| v.iter().cloned())
        .collect();

    let mut roots: Vec<String> = st_info
        .keys()
        .filter(|name| !all_children.contains(*name))
        .cloned()
        .collect();
    roots.sort();

    // DFS to emit rows with proper tree-drawing prefixes.
    let mut output: Vec<(String, String, String, i32, Option<String>, Option<String>)> = Vec::new();

    let ctx = DagCtx {
        st_info,
        st_children,
        st_sources,
    };

    for (i, root) in roots.iter().enumerate() {
        let is_last_root = i == roots.len() - 1;
        let root_connector = if roots.len() == 1 {
            ""
        } else if is_last_root {
            "└── "
        } else {
            "├── "
        };
        dfs(root, 0, root_connector, "", &ctx, &mut output);
    }

    output
}

#[allow(clippy::type_complexity)]
fn dfs(
    node: &str,
    depth: i32,
    connector: &str,    // "├── " | "└── " | ""
    continuation: &str, // prefix inherited from parent
    ctx: &DagCtx<'_>,
    output: &mut Vec<(String, String, String, i32, Option<String>, Option<String>)>,
) {
    let tree_line = format!("{}{}{}", continuation, connector, node);

    let (status, mode, node_type) = if let Some((s, m)) = ctx.st_info.get(node) {
        (Some(s.clone()), Some(m.clone()), "stream_table".to_string())
    } else {
        (None, None, "source_table".to_string())
    };

    output.push((tree_line, node.to_string(), node_type, depth, status, mode));

    // Children of this node: ST dependents + plain source tables
    let mut st_kids = ctx.st_children.get(node).cloned().unwrap_or_default();
    st_kids.sort();

    let mut src_kids = ctx.st_sources.get(node).cloned().unwrap_or_default();
    src_kids.sort();

    // Plain source nodes come after ST children so the ST sub-tree
    // is rendered contiguously.
    let all_kids: Vec<(String, bool)> = st_kids
        .iter()
        .map(|n| (n.clone(), true))
        .chain(src_kids.iter().map(|n| (n.clone(), false)))
        .collect();

    let child_continuation = format!(
        "{}{}",
        continuation,
        if connector == "└── " || connector.is_empty() {
            "    "
        } else {
            "│   "
        }
    );

    let total = all_kids.len();
    for (i, (child, is_st_child)) in all_kids.iter().enumerate() {
        let is_last = i == total - 1;
        let child_connector = if is_last { "└── " } else { "├── " };

        if *is_st_child {
            dfs(
                child,
                depth + 1,
                child_connector,
                &child_continuation,
                ctx,
                output,
            );
        } else {
            // Leaf source node — emit directly without recursing into
            // its own dependencies (those are tracked by its own ST entry
            // if it is also a stream table).
            let src_label = format!("{} [src]", child);
            let src_line = format!("{}{}{}", child_continuation, child_connector, src_label);
            output.push((
                src_line,
                child.clone(),
                "source_table".to_string(),
                depth + 1,
                None,
                None,
            ));
        }
    }
}

/// A single-query health overview of the pg_trickle installation.
///
/// Returns one row per check. Each row has a `severity` of `OK`, `WARN`, or
/// `ERROR` and a human-readable `detail`. Run this to get an instant triage
/// of whether anything needs attention.
///
/// Checks performed:
/// - `scheduler_running`    — background worker is alive
/// - `error_tables`         — any stream tables in ERROR/SUSPENDED status
/// - `stale_tables`         — any stream tables where last_refresh_at age exceeds schedule (scheduler behind)
/// - `needs_reinit`         — any stream tables awaiting reinitialization
/// - `consecutive_errors`   — any stream tables accumulating errors (not yet suspended)
/// - `buffer_growth`        — any CDC change buffer with > 10 000 pending rows
/// - `slot_lag`             — any WAL replication slot retaining > 100 MB of WAL
///
/// Exposed as `pgtrickle.health_check()`.
#[pg_extern(schema = "pgtrickle", name = "health_check")]
fn health_check() -> TableIterator<
    'static,
    (
        name!(check_name, String),
        name!(severity, String),
        name!(detail, String),
    ),
> {
    let mut rows: Vec<(String, String, String)> = Vec::new();

    Spi::connect(|client| {
        // ── 1. Scheduler running ────────────────────────────────────────────
        let scheduler_count = client
            .select(
                "SELECT count(*)::int FROM pg_stat_activity \
                 WHERE backend_type = 'pg_trickle scheduler'",
                None,
                &[],
            )
            .ok()
            .and_then(|r| r.first().get::<i32>(1).unwrap_or(None))
            .unwrap_or(0);

        let (sev, detail) = if scheduler_count > 0 {
            ("OK", format!("{} worker(s) running", scheduler_count))
        } else {
            (
                "ERROR",
                "No pg_trickle scheduler background worker found in pg_stat_activity. \
                 Check that pg_trickle is in shared_preload_libraries and \
                 pg_trickle.enabled = on."
                    .to_string(),
            )
        };
        rows.push(("scheduler_running".to_string(), sev.to_string(), detail));

        // ── 2. ERROR / SUSPENDED stream tables ─────────────────────────────
        let bad_tables: Vec<String> = client
            .select(
                "SELECT pgt_schema || '.' || pgt_name \
                 FROM pgtrickle.pgt_stream_tables \
                 WHERE status IN ('ERROR', 'SUSPENDED') \
                 ORDER BY 1",
                None,
                &[],
            )
            .map(|r| {
                r.filter_map(|row| row.get::<String>(1).unwrap_or(None))
                    .collect()
            })
            .unwrap_or_default();

        let (sev, detail) = if bad_tables.is_empty() {
            (
                "OK".to_string(),
                "All stream tables are ACTIVE or INITIALIZING".to_string(),
            )
        } else {
            (
                "ERROR".to_string(),
                format!(
                    "{} stream table(s) in ERROR/SUSPENDED: {}",
                    bad_tables.len(),
                    bad_tables.join(", ")
                ),
            )
        };
        rows.push(("error_tables".to_string(), sev, detail));

        // ── 3. Stale stream tables ──────────────────────────────────────────
        let stale_tables: Vec<String> = client
            .select(
                "SELECT pgt_schema || '.' || pgt_name \
                 FROM pgtrickle.stream_tables_info \
                 WHERE stale = true \
                 ORDER BY 1",
                None,
                &[],
            )
            .map(|r| {
                r.filter_map(|row| row.get::<String>(1).unwrap_or(None))
                    .collect()
            })
            .unwrap_or_default();

        let (sev, detail) = if stale_tables.is_empty() {
            ("OK".to_string(), "No stale stream tables".to_string())
        } else {
            (
                "WARN".to_string(),
                format!(
                    "{} stale stream table(s) (scheduler behind its schedule): {}",
                    stale_tables.len(),
                    stale_tables.join(", ")
                ),
            )
        };
        rows.push(("stale_tables".to_string(), sev, detail));

        // ── 4. needs_reinit ─────────────────────────────────────────────────
        let reinit_tables: Vec<String> = client
            .select(
                "SELECT pgt_schema || '.' || pgt_name \
                 FROM pgtrickle.pgt_stream_tables \
                 WHERE needs_reinit = true \
                 ORDER BY 1",
                None,
                &[],
            )
            .map(|r| {
                r.filter_map(|row| row.get::<String>(1).unwrap_or(None))
                    .collect()
            })
            .unwrap_or_default();

        let (sev, detail) = if reinit_tables.is_empty() {
            (
                "OK".to_string(),
                "No stream tables awaiting reinitialization".to_string(),
            )
        } else {
            (
                "WARN".to_string(),
                format!(
                    "{} stream table(s) need reinitialization (DDL change detected): {}",
                    reinit_tables.len(),
                    reinit_tables.join(", ")
                ),
            )
        };
        rows.push(("needs_reinit".to_string(), sev, detail));

        // ── 5. Consecutive errors (not yet suspended) ───────────────────────
        let erroring_tables: Vec<String> = client
            .select(
                "SELECT pgt_schema || '.' || pgt_name \
                 FROM pgtrickle.pgt_stream_tables \
                 WHERE consecutive_errors > 0 AND status NOT IN ('ERROR', 'SUSPENDED') \
                 ORDER BY consecutive_errors DESC",
                None,
                &[],
            )
            .map(|r| {
                r.filter_map(|row| row.get::<String>(1).unwrap_or(None))
                    .collect()
            })
            .unwrap_or_default();

        let (sev, detail) = if erroring_tables.is_empty() {
            (
                "OK".to_string(),
                "No stream tables accumulating refresh errors".to_string(),
            )
        } else {
            (
                "WARN".to_string(),
                format!(
                    "{} stream table(s) have consecutive errors (approaching auto-suspend): {}",
                    erroring_tables.len(),
                    erroring_tables.join(", ")
                ),
            )
        };
        rows.push(("consecutive_errors".to_string(), sev, detail));

        // ── 6. CDC buffer growth (> 10 000 pending rows for any source) ─────
        let bloated: Vec<String> = client
            .select(
                "SELECT source_table || ' (' || pending_rows || ' pending)' \
                 FROM pgtrickle.change_buffer_sizes() \
                 WHERE pending_rows > 10000 \
                 ORDER BY pending_rows DESC",
                None,
                &[],
            )
            .map(|r| {
                r.filter_map(|row| row.get::<String>(1).unwrap_or(None))
                    .collect()
            })
            .unwrap_or_default();

        let (sev, detail) = if bloated.is_empty() {
            (
                "OK".to_string(),
                "All CDC buffers within normal range".to_string(),
            )
        } else {
            (
                "WARN".to_string(),
                format!(
                    "{} CDC buffer(s) exceeding 10 000 pending rows — differential refresh may be stalled: {}",
                    bloated.len(),
                    bloated.join(", ")
                ),
            )
        };
        rows.push(("buffer_growth".to_string(), sev, detail));

        // ── 7. WAL slot lag ─────────────────────────────────────────────────
        let threshold_bytes = config::pg_trickle_slot_lag_warning_threshold_bytes();
        let lagging: Vec<String> = client
            .select(
                &format!(
                    "SELECT slot_name || ' (' || pg_size_pretty(retained_wal_bytes) || ')' \
                     FROM pgtrickle.slot_health() \
                     WHERE retained_wal_bytes > {} \
                     ORDER BY retained_wal_bytes DESC",
                    threshold_bytes
                ),
                None,
                &[],
            )
            .map(|r| {
                r.filter_map(|row| row.get::<String>(1).unwrap_or(None))
                    .collect()
            })
            .unwrap_or_default();

        let (sev, detail) = build_slot_lag_health_detail(&lagging, threshold_bytes);
        rows.push(("slot_lag".to_string(), sev, detail));

        // ── 8. Worker pool utilization (parallel refresh) ───────────────────
        let mode = config::pg_trickle_parallel_refresh_mode();
        if mode != config::ParallelRefreshMode::Off {
            let active = shmem::active_worker_count();
            let max_workers = config::pg_trickle_max_dynamic_refresh_workers().max(1) as u32;

            let (sev, detail) = if active >= max_workers {
                (
                    "WARN".to_string(),
                    format!(
                        "Worker pool saturated: {}/{} tokens in use — \
                         new refresh jobs will be queued until a worker finishes. \
                         Consider increasing pg_trickle.max_dynamic_refresh_workers.",
                        active, max_workers,
                    ),
                )
            } else {
                (
                    "OK".to_string(),
                    format!(
                        "{}/{} worker tokens in use (mode={})",
                        active, max_workers, mode,
                    ),
                )
            };
            rows.push(("worker_pool".to_string(), sev, detail));

            // ── 9. Queued job backlog (parallel refresh) ────────────────────
            let queued_count = client
                .select(
                    "SELECT count(*)::int FROM pgtrickle.pgt_scheduler_jobs \
                     WHERE status = 'QUEUED'",
                    None,
                    &[],
                )
                .ok()
                .and_then(|r| r.first().get::<i32>(1).unwrap_or(None))
                .unwrap_or(0);

            let (sev, detail) = if queued_count > 10 {
                (
                    "WARN".to_string(),
                    format!(
                        "{} jobs queued — refresh work is backing up. \
                         Workers may be overloaded or failing.",
                        queued_count,
                    ),
                )
            } else if queued_count > 0 {
                (
                    "OK".to_string(),
                    format!("{} job(s) waiting in queue", queued_count),
                )
            } else {
                ("OK".to_string(), "No jobs queued".to_string())
            };
            rows.push(("job_queue".to_string(), sev, detail));
        }
    });

    TableIterator::new(rows)
}

// ── UX-4: Single-endpoint health summary ────────────────────────────────────

/// Return a single-row summary of the entire pg_trickle deployment's health.
///
/// Aggregates key metrics into one place so monitoring dashboards can
/// poll a single endpoint instead of joining multiple views.
///
/// Exposed as `pgtrickle.health_summary()`.
#[pg_extern(schema = "pgtrickle", name = "health_summary")]
#[allow(clippy::type_complexity)]
fn health_summary() -> TableIterator<
    'static,
    (
        name!(total_stream_tables, i32),
        name!(active_count, i32),
        name!(error_count, i32),
        name!(suspended_count, i32),
        name!(stale_count, i32),
        name!(reinit_pending, i32),
        name!(max_staleness_seconds, Option<f64>),
        name!(scheduler_status, String),
        name!(cache_hit_rate, Option<f64>),
    ),
> {
    let row = Spi::connect(|client| {
        // ── Stream table status counts ──────────────────────────────────
        let counts = client
            .select(
                "SELECT \
                     count(*)::int AS total, \
                     count(*) FILTER (WHERE status = 'ACTIVE')::int AS active, \
                     count(*) FILTER (WHERE status = 'ERROR')::int AS errors, \
                     count(*) FILTER (WHERE status = 'SUSPENDED')::int AS suspended, \
                     count(*) FILTER (WHERE needs_reinit = true)::int AS reinit \
                 FROM pgtrickle.pgt_stream_tables",
                None,
                &[],
            )
            .ok();

        let (total, active, errors, suspended, reinit) = counts
            .map(|r| {
                let row = r.first();
                (
                    row.get::<i32>(1).unwrap_or(None).unwrap_or(0),
                    row.get::<i32>(2).unwrap_or(None).unwrap_or(0),
                    row.get::<i32>(3).unwrap_or(None).unwrap_or(0),
                    row.get::<i32>(4).unwrap_or(None).unwrap_or(0),
                    row.get::<i32>(5).unwrap_or(None).unwrap_or(0),
                )
            })
            .unwrap_or((0, 0, 0, 0, 0));

        // ── Stale count and max staleness ───────────────────────────────
        let stale_info = client
            .select(
                "SELECT \
                     count(*) FILTER (WHERE stale = true)::int, \
                     max(staleness_seconds)::float8 \
                 FROM pgtrickle.stream_tables_info",
                None,
                &[],
            )
            .ok();

        let (stale_count, max_staleness) = stale_info
            .map(|r| {
                let row = r.first();
                (
                    row.get::<i32>(1).unwrap_or(None).unwrap_or(0),
                    row.get::<f64>(2).unwrap_or(None),
                )
            })
            .unwrap_or((0, None));

        // ── Scheduler status ────────────────────────────────────────────
        let scheduler_running = client
            .select(
                "SELECT count(*)::int FROM pg_stat_activity \
                 WHERE backend_type = 'pg_trickle scheduler'",
                None,
                &[],
            )
            .ok()
            .and_then(|r| r.first().get::<i32>(1).unwrap_or(None))
            .unwrap_or(0);

        let scheduler_status = if scheduler_running > 0 {
            "ACTIVE".to_string()
        } else if crate::shmem::is_shmem_available() {
            "STOPPED".to_string()
        } else {
            "NOT_LOADED".to_string()
        };

        // ── Cache hit rate from shared memory ───────────────────────────
        let cache_hit_rate = if crate::shmem::is_shmem_available() {
            let l1 = crate::shmem::TEMPLATE_CACHE_L1_HITS
                .get()
                .load(std::sync::atomic::Ordering::Relaxed) as f64;
            let l2 = crate::shmem::TEMPLATE_CACHE_L2_HITS
                .get()
                .load(std::sync::atomic::Ordering::Relaxed) as f64;
            let misses = crate::shmem::TEMPLATE_CACHE_MISSES
                .get()
                .load(std::sync::atomic::Ordering::Relaxed) as f64;
            let total_lookups = l1 + l2 + misses;
            if total_lookups > 0.0 {
                Some((l1 + l2) / total_lookups)
            } else {
                None
            }
        } else {
            None
        };

        (
            total,
            active,
            errors,
            suspended,
            stale_count,
            reinit,
            max_staleness,
            scheduler_status,
            cache_hit_rate,
        )
    });

    TableIterator::once(row)
}

/// Cross-stream-table refresh timeline, most recent first.
///
/// Returns up to `max_rows` refresh records across all stream tables in a
/// single chronological view. Useful for spotting refresh bursts, cascading
/// failures, or unexpected mode changes without having to query each stream
/// table individually.
///
/// Exposed as `pgtrickle.refresh_timeline(limit)`.
#[pg_extern(schema = "pgtrickle", name = "refresh_timeline")]
#[allow(clippy::type_complexity)]
fn refresh_timeline(
    max_rows: default!(i32, 50),
) -> TableIterator<
    'static,
    (
        name!(start_time, TimestampWithTimeZone),
        name!(stream_table, String),
        name!(action, String),
        name!(status, String),
        name!(rows_inserted, i64),
        name!(rows_deleted, i64),
        name!(duration_ms, Option<f64>),
        name!(error_message, Option<String>),
    ),
> {
    let epoch_zero = TimestampWithTimeZone::try_from(0i64)
        .unwrap_or_else(|_| pgrx::error!("refresh_timeline: failed to construct epoch timestamp"));

    let rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    h.start_time,
                    st.pgt_schema || '.' || st.pgt_name AS stream_table,
                    h.action,
                    h.status,
                    COALESCE(h.rows_inserted, 0)::bigint,
                    COALESCE(h.rows_deleted, 0)::bigint,
                    CASE WHEN h.end_time IS NOT NULL
                         THEN EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000
                         ELSE NULL
                    END::float8,
                    h.error_message
                 FROM pgtrickle.pgt_refresh_history h
                 JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = h.pgt_id
                 ORDER BY h.start_time DESC
                 LIMIT $1",
                None,
                &[max_rows.into()],
            )
            .unwrap_or_else(|e| pgrx::error!("refresh_timeline: SPI select failed: {e}"));

        let mut out = Vec::new();
        for row in result {
            let start = row
                .get::<TimestampWithTimeZone>(1)
                .unwrap_or(None)
                .unwrap_or(epoch_zero);
            let table = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let action = row.get::<String>(3).unwrap_or(None).unwrap_or_default();
            let status = row.get::<String>(4).unwrap_or(None).unwrap_or_default();
            let ins = row.get::<i64>(5).unwrap_or(None).unwrap_or(0);
            let del = row.get::<i64>(6).unwrap_or(None).unwrap_or(0);
            let dur = row.get::<f64>(7).unwrap_or(None);
            let err = row.get::<String>(8).unwrap_or(None);
            out.push((start, table, action, status, ins, del, dur, err));
        }
        out
    });

    TableIterator::new(rows)
}

/// Inventory of CDC triggers installed by pg_trickle for each tracked source.
///
/// For each source in `pgt_dependencies`, reports whether the expected pg_trickle
/// CDC triggers (`pg_trickle_cdc_<oid>` for DML and `pg_trickle_cdc_truncate_<oid>`
/// for TRUNCATE) are present and enabled in `pg_catalog`. A `present = false` row
/// indicates a missing trigger that will prevent change capture for that source.
///
/// Exposed as `pgtrickle.trigger_inventory()`.
#[pg_extern(schema = "pgtrickle", name = "trigger_inventory")]
#[allow(clippy::type_complexity)]
fn trigger_inventory() -> TableIterator<
    'static,
    (
        name!(source_table, String),
        name!(source_oid, i64),
        name!(trigger_name, String),
        name!(trigger_type, String),
        name!(present, bool),
        name!(enabled, bool),
    ),
> {
    let rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    n.nspname::text || '.' || c.relname::text AS source_table,
                    d.source_relid::bigint                    AS source_oid,
                    'pg_trickle_cdc_' || d.source_relid::text AS dml_trigger,
                    'pg_trickle_cdc_truncate_' || d.source_relid::text AS trunc_trigger,
                    -- DML trigger present?
                    EXISTS (SELECT 1 FROM pg_trigger t
                            WHERE t.tgrelid = d.source_relid
                              AND t.tgname = 'pg_trickle_cdc_' || d.source_relid::text)
                        AS dml_present,
                    -- DML trigger enabled? (tgenabled: 'O'=origin, 'D'=disabled, etc.)
                    COALESCE((SELECT t.tgenabled != 'D'
                              FROM pg_trigger t
                              WHERE t.tgrelid = d.source_relid
                                AND t.tgname = 'pg_trickle_cdc_' || d.source_relid::text),
                             false) AS dml_enabled,
                    -- TRUNCATE trigger present?
                    EXISTS (SELECT 1 FROM pg_trigger t
                            WHERE t.tgrelid = d.source_relid
                              AND t.tgname = 'pg_trickle_cdc_truncate_' || d.source_relid::text)
                        AS trunc_present,
                    -- TRUNCATE trigger enabled?
                    COALESCE((SELECT t.tgenabled != 'D'
                              FROM pg_trigger t
                              WHERE t.tgrelid = d.source_relid
                                AND t.tgname = 'pg_trickle_cdc_truncate_' || d.source_relid::text),
                             false) AS trunc_enabled
                 FROM (SELECT DISTINCT source_relid FROM pgtrickle.pgt_dependencies
                       WHERE source_relid != 0) d
                 JOIN pg_class     c ON c.oid = d.source_relid
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 ORDER BY source_table",
                None,
                &[],
            )
            .unwrap_or_else(|e| pgrx::error!("trigger_inventory: SPI select failed: {e}"));

        let mut out = Vec::new();
        for row in result {
            let source_table = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let source_oid = row.get::<i64>(2).unwrap_or(None).unwrap_or(0);
            let dml_trigger = row.get::<String>(3).unwrap_or(None).unwrap_or_default();
            let trunc_trigger = row.get::<String>(4).unwrap_or(None).unwrap_or_default();
            let dml_present = row.get::<bool>(5).unwrap_or(None).unwrap_or(false);
            let dml_enabled = row.get::<bool>(6).unwrap_or(None).unwrap_or(false);
            let trunc_present = row.get::<bool>(7).unwrap_or(None).unwrap_or(false);
            let trunc_enabled = row.get::<bool>(8).unwrap_or(None).unwrap_or(false);

            // Emit one row per trigger type (DML + TRUNCATE)
            out.push((
                source_table.clone(),
                source_oid,
                dml_trigger,
                "DML".to_string(),
                dml_present,
                dml_enabled,
            ));
            out.push((
                source_table,
                source_oid,
                trunc_trigger,
                "TRUNCATE".to_string(),
                trunc_present,
                trunc_enabled,
            ));
        }
        out
    });

    TableIterator::new(rows)
}

// ── Parallel Refresh Observability (Phase 6) ──────────────────────────────

/// Worker pool utilization snapshot.
///
/// Returns a single row with:
/// - active workers (from shared memory),
/// - cluster-wide worker budget (from GUC),
/// - per-database dispatch cap (from GUC),
/// - current parallel refresh mode (from GUC).
///
/// Exposed as `pgtrickle.worker_pool_status()`.
#[pg_extern(schema = "pgtrickle", name = "worker_pool_status")]
fn worker_pool_status() -> TableIterator<
    'static,
    (
        name!(active_workers, i32),
        name!(max_workers, i32),
        name!(per_db_cap, i32),
        name!(parallel_mode, String),
    ),
> {
    let active = shmem::active_worker_count() as i32;
    let max_workers = config::pg_trickle_max_dynamic_refresh_workers();
    let per_db = config::pg_trickle_max_concurrent_refreshes();
    let mode = config::pg_trickle_parallel_refresh_mode().to_string();

    TableIterator::new(vec![(active, max_workers, per_db, mode)])
}

/// Active and recent scheduler jobs.
///
/// Returns one row per job in `pgt_scheduler_jobs` that is currently queued,
/// running, or recently completed (within the last `max_age_seconds`).
///
/// Exposed as `pgtrickle.parallel_job_status(max_age_seconds)`.
#[pg_extern(schema = "pgtrickle", name = "parallel_job_status")]
#[allow(clippy::type_complexity)]
fn parallel_job_status(
    max_age_seconds: default!(i32, 300),
) -> TableIterator<
    'static,
    (
        name!(job_id, i64),
        name!(unit_key, String),
        name!(unit_kind, String),
        name!(status, String),
        name!(member_count, i32),
        name!(attempt_no, i32),
        name!(scheduler_pid, i32),
        name!(worker_pid, Option<i32>),
        name!(enqueued_at, TimestampWithTimeZone),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(duration_ms, Option<f64>),
    ),
> {
    let mut rows = Vec::new();

    Spi::connect(|client| {
        let result = client.select(
            &format!(
                "SELECT job_id, unit_key, unit_kind, status, \
                        array_length(member_pgt_ids, 1), attempt_no, \
                        scheduler_pid, worker_pid, enqueued_at, started_at, \
                        finished_at, \
                        CASE WHEN started_at IS NOT NULL AND finished_at IS NOT NULL \
                             THEN EXTRACT(EPOCH FROM (finished_at - started_at)) * 1000.0 \
                        END AS duration_ms \
                 FROM pgtrickle.pgt_scheduler_jobs \
                 WHERE status IN ('QUEUED', 'RUNNING') \
                    OR (finished_at > now() - interval '{} seconds') \
                 ORDER BY enqueued_at DESC",
                max_age_seconds.max(0)
            ),
            None,
            &[],
        );

        if let Ok(tup_table) = result {
            for row in tup_table {
                let job_id = row.get::<i64>(1).unwrap_or(None).unwrap_or(0);
                let unit_key = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
                let unit_kind = row.get::<String>(3).unwrap_or(None).unwrap_or_default();
                let status = row.get::<String>(4).unwrap_or(None).unwrap_or_default();
                let member_count = row.get::<i32>(5).unwrap_or(None).unwrap_or(0);
                let attempt_no = row.get::<i32>(6).unwrap_or(None).unwrap_or(1);
                let scheduler_pid = row.get::<i32>(7).unwrap_or(None).unwrap_or(0);
                let worker_pid = row.get::<i32>(8).unwrap_or(None);
                let enqueued_at = row
                    .get::<TimestampWithTimeZone>(9)
                    .unwrap_or(None)
                    .unwrap_or_else(|| {
                        TimestampWithTimeZone::try_from(0i64).unwrap_or_else(|_| {
                            pgrx::error!("parallel_job_status: failed to construct epoch timestamp")
                        })
                    });
                let started_at = row.get::<TimestampWithTimeZone>(10).unwrap_or(None);
                let finished_at = row.get::<TimestampWithTimeZone>(11).unwrap_or(None);
                let duration_ms = row.get::<f64>(12).unwrap_or(None);

                rows.push((
                    job_id,
                    unit_key,
                    unit_kind,
                    status,
                    member_count,
                    attempt_no,
                    scheduler_pid,
                    worker_pid,
                    enqueued_at,
                    started_at,
                    finished_at,
                    duration_ms,
                ));
            }
        }
    });

    TableIterator::new(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_event_as_str() {
        assert_eq!(AlertEvent::StaleData.as_str(), "stale_data");
        assert_eq!(AlertEvent::AutoSuspended.as_str(), "auto_suspended");
        assert_eq!(AlertEvent::Resumed.as_str(), "resumed");
        assert_eq!(
            AlertEvent::ReinitializeNeeded.as_str(),
            "reinitialize_needed"
        );
        assert_eq!(
            AlertEvent::BufferGrowthWarning.as_str(),
            "buffer_growth_warning"
        );
        assert_eq!(AlertEvent::SlotLagWarning.as_str(), "slot_lag_warning");
        assert_eq!(AlertEvent::RefreshCompleted.as_str(), "refresh_completed");
        assert_eq!(AlertEvent::RefreshFailed.as_str(), "refresh_failed");
        assert_eq!(
            AlertEvent::SchedulerFallingBehind.as_str(),
            "scheduler_falling_behind"
        );
        assert_eq!(
            AlertEvent::FuseBlownReminder.as_str(),
            "fuse_blown_reminder"
        );
        assert_eq!(AlertEvent::FrozenTierSkip.as_str(), "frozen_tier_skip");
        assert_eq!(
            AlertEvent::CdcTriggerDisabled.as_str(),
            "cdc_trigger_disabled"
        );
        assert_eq!(AlertEvent::CleanupFailure.as_str(), "cleanup_failure");
    }

    #[test]
    fn test_alert_event_equality() {
        assert_eq!(AlertEvent::StaleData, AlertEvent::StaleData);
        assert_ne!(AlertEvent::StaleData, AlertEvent::AutoSuspended);
    }

    #[test]
    fn test_alert_event_all_variants_unique() {
        let variants = [
            AlertEvent::StaleData,
            AlertEvent::AutoSuspended,
            AlertEvent::Resumed,
            AlertEvent::ReinitializeNeeded,
            AlertEvent::BufferGrowthWarning,
            AlertEvent::SlotLagWarning,
            AlertEvent::RefreshCompleted,
            AlertEvent::RefreshFailed,
            AlertEvent::SchedulerFallingBehind,
            AlertEvent::AppendOnlyReverted,
            AlertEvent::FuseBlownReminder,
            AlertEvent::FrozenTierSkip,
            AlertEvent::CdcTriggerDisabled,
            AlertEvent::CleanupFailure,
        ];
        // All as_str() values should be distinct
        let strs: Vec<&str> = variants.iter().map(|v| v.as_str()).collect();
        let mut deduped = strs.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(
            strs.len(),
            deduped.len(),
            "All AlertEvent variants must have unique as_str()"
        );
    }

    #[test]
    fn test_alert_event_clone_and_copy() {
        let event = AlertEvent::RefreshFailed;
        let copied = event; // Copy
        assert_eq!(event, copied);
        // Verify Clone trait is implemented (Copy requires Clone)
        let cloned: AlertEvent = Clone::clone(&event);
        assert_eq!(event, cloned);
    }

    #[test]
    fn test_alert_event_debug_format() {
        let debug = format!("{:?}", AlertEvent::StaleData);
        assert!(
            debug.contains("StaleData"),
            "Debug should contain variant name: {debug}"
        );
    }

    // ── build_alert_payload tests ───────────────────────────────────

    #[test]
    fn test_alert_payload_basic_structure() {
        let payload = build_alert_payload(
            AlertEvent::StaleData,
            "public",
            "orders_st",
            r#""extra_key":"extra_val""#,
        );
        assert!(payload.contains(r#""event":"stale_data""#));
        assert!(payload.contains(r#""pgt_schema":"public""#));
        assert!(payload.contains(r#""pgt_name":"orders_st""#));
        assert!(payload.contains(r#""st":"public.orders_st""#));
        assert!(payload.contains(r#""extra_key":"extra_val""#));
    }

    #[test]
    fn test_alert_payload_escapes_quotes() {
        let payload = build_alert_payload(
            AlertEvent::RefreshFailed,
            r#"my"schema"#,
            r#"my"table"#,
            r#""err":"test""#,
        );
        assert!(payload.contains(r#"my\"schema"#));
        assert!(payload.contains(r#"my\"table"#));
    }

    #[test]
    fn test_alert_payload_truncation() {
        let long_extra = "x".repeat(8000);
        let payload = build_alert_payload(
            AlertEvent::BufferGrowthWarning,
            "public",
            "test",
            &format!(r#""data":"{}""#, long_extra),
        );
        assert!(
            payload.len() <= 7900,
            "payload should be truncated: len={}",
            payload.len()
        );
        assert!(
            payload.ends_with("...}"),
            "should end with truncation marker, got: ...{}",
            &payload[payload.len().saturating_sub(10)..],
        );
    }

    #[test]
    fn test_alert_payload_short_not_truncated() {
        let payload = build_alert_payload(
            AlertEvent::Resumed,
            "public",
            "test",
            r#""reason":"manual""#,
        );
        assert!(!payload.contains("...}}"));
    }

    #[test]
    fn test_build_cdc_health_alert_for_slot_lag() {
        let alert = build_cdc_health_alert(2048, 1024, true, CdcMode::Wal);
        assert_eq!(
            alert,
            Some("slot_lag_exceeds_threshold: 2048 bytes > 1024 bytes".to_string())
        );
    }

    #[test]
    fn test_build_cdc_health_alert_for_missing_slot() {
        let alert = build_cdc_health_alert(128, 1024, false, CdcMode::Wal);
        assert_eq!(alert, Some("replication_slot_missing".to_string()));
    }

    #[test]
    fn test_build_slot_lag_health_detail_warns_with_threshold() {
        let (severity, detail) = build_slot_lag_health_detail(
            &["pg_trickle_slot_123 (128 MB)".to_string()],
            104_857_600,
        );
        assert_eq!(severity, "WARN");
        assert!(detail.contains("104857600 bytes"));
        assert!(detail.contains("pg_trickle_slot_123 (128 MB)"));
    }

    // ── render_dependency_tree tests ────────────────────────────────

    #[test]
    fn test_tree_single_root_no_children() {
        let mut st_info = std::collections::HashMap::new();
        st_info.insert(
            "public.orders_st".to_string(),
            ("ACTIVE".to_string(), "DEFERRED".to_string()),
        );
        let st_children = std::collections::HashMap::new();
        let st_sources = std::collections::HashMap::new();

        let rows = render_dependency_tree(&st_info, &st_children, &st_sources);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "public.orders_st"); // tree_line
        assert_eq!(rows[0].1, "public.orders_st"); // node
        assert_eq!(rows[0].2, "stream_table"); // node_type
        assert_eq!(rows[0].3, 0); // depth
        assert_eq!(rows[0].4, Some("ACTIVE".to_string()));
    }

    #[test]
    fn test_tree_with_source_leaf() {
        let mut st_info = std::collections::HashMap::new();
        st_info.insert(
            "public.orders_st".to_string(),
            ("ACTIVE".to_string(), "DEFERRED".to_string()),
        );
        let st_children = std::collections::HashMap::new();
        let mut st_sources = std::collections::HashMap::new();
        st_sources.insert(
            "public.orders_st".to_string(),
            vec!["public.orders".to_string()],
        );

        let rows = render_dependency_tree(&st_info, &st_children, &st_sources);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "public.orders_st");
        assert_eq!(rows[1].1, "public.orders");
        assert_eq!(rows[1].2, "source_table");
        assert!(rows[1].0.contains("[src]"));
    }

    #[test]
    fn test_tree_st_chain() {
        let mut st_info = std::collections::HashMap::new();
        st_info.insert(
            "public.base_st".to_string(),
            ("ACTIVE".to_string(), "DEFERRED".to_string()),
        );
        st_info.insert(
            "public.derived_st".to_string(),
            ("ACTIVE".to_string(), "DEFERRED".to_string()),
        );
        let mut st_children = std::collections::HashMap::new();
        st_children.insert(
            "public.base_st".to_string(),
            vec!["public.derived_st".to_string()],
        );
        let st_sources = std::collections::HashMap::new();

        let rows = render_dependency_tree(&st_info, &st_children, &st_sources);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "public.base_st");
        assert_eq!(rows[0].3, 0); // depth
        assert_eq!(rows[1].1, "public.derived_st");
        assert_eq!(rows[1].3, 1); // depth
    }

    #[test]
    fn test_tree_multiple_roots_sorted() {
        let mut st_info = std::collections::HashMap::new();
        st_info.insert(
            "public.b_st".to_string(),
            ("ACTIVE".to_string(), "DEFERRED".to_string()),
        );
        st_info.insert(
            "public.a_st".to_string(),
            ("ACTIVE".to_string(), "DEFERRED".to_string()),
        );
        let st_children = std::collections::HashMap::new();
        let st_sources = std::collections::HashMap::new();

        let rows = render_dependency_tree(&st_info, &st_children, &st_sources);
        assert_eq!(rows.len(), 2);
        // Roots should be alphabetically sorted
        assert_eq!(rows[0].1, "public.a_st");
        assert_eq!(rows[1].1, "public.b_st");
    }

    #[test]
    fn test_tree_diamond_topology() {
        // base_st -> mid_a_st -> leaf_st
        // base_st -> mid_b_st -> leaf_st
        let mut st_info = std::collections::HashMap::new();
        for name in &["public.base_st", "public.mid_a_st", "public.mid_b_st"] {
            st_info.insert(
                name.to_string(),
                ("ACTIVE".to_string(), "DEFERRED".to_string()),
            );
        }
        let mut st_children = std::collections::HashMap::new();
        st_children.insert(
            "public.base_st".to_string(),
            vec!["public.mid_a_st".to_string(), "public.mid_b_st".to_string()],
        );
        let st_sources = std::collections::HashMap::new();

        let rows = render_dependency_tree(&st_info, &st_children, &st_sources);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, "public.base_st");
        // Children should be sorted
        assert_eq!(rows[1].1, "public.mid_a_st");
        assert_eq!(rows[2].1, "public.mid_b_st");
    }

    #[test]
    fn test_tree_source_not_in_st_info() {
        let mut st_info = std::collections::HashMap::new();
        st_info.insert(
            "public.my_st".to_string(),
            ("ACTIVE".to_string(), "IMMEDIATE".to_string()),
        );
        let st_children = std::collections::HashMap::new();
        let mut st_sources = std::collections::HashMap::new();
        st_sources.insert(
            "public.my_st".to_string(),
            vec!["public.raw_table".to_string()],
        );

        let rows = render_dependency_tree(&st_info, &st_children, &st_sources);
        let src_row = rows.iter().find(|r| r.1 == "public.raw_table").unwrap();
        assert_eq!(src_row.4, None); // no status for source tables
        assert_eq!(src_row.5, None); // no mode for source tables
    }

    #[test]
    fn test_tree_empty_graph() {
        let st_info = std::collections::HashMap::new();
        let st_children = std::collections::HashMap::new();
        let st_sources = std::collections::HashMap::new();

        let rows = render_dependency_tree(&st_info, &st_children, &st_sources);
        assert!(rows.is_empty());
    }

    // ── build_cdc_health_alert (missing branches) ────────────────────────────

    #[test]
    fn test_build_cdc_health_alert_ok_when_below_threshold_and_slot_present() {
        let alert = build_cdc_health_alert(512, 1024, true, CdcMode::Wal);
        assert!(
            alert.is_none(),
            "No alert when lag is below threshold and slot exists"
        );
    }

    #[test]
    fn test_build_cdc_health_alert_no_alert_for_trigger_mode_missing_slot() {
        // A missing replication slot is not relevant for trigger-based CDC
        let alert = build_cdc_health_alert(0, 1024, false, CdcMode::Trigger);
        assert!(alert.is_none());
    }

    #[test]
    fn test_build_cdc_health_alert_lag_takes_priority_over_missing_slot() {
        // Both conditions true: lag > threshold and slot is missing.
        // The lag check comes first so it should win.
        let alert = build_cdc_health_alert(2048, 1024, false, CdcMode::Wal);
        assert_eq!(
            alert,
            Some("slot_lag_exceeds_threshold: 2048 bytes > 1024 bytes".to_string())
        );
    }

    #[test]
    fn test_build_cdc_health_alert_exact_threshold_not_alerted() {
        // `>` not `>=`: equal to threshold must not trigger
        let alert = build_cdc_health_alert(1024, 1024, true, CdcMode::Wal);
        assert!(alert.is_none());
    }

    // ── build_slot_lag_health_detail (missing branches) ──────────────────────

    #[test]
    fn test_build_slot_lag_health_detail_ok_when_empty() {
        let (severity, detail) = build_slot_lag_health_detail(&[], 104_857_600);
        assert_eq!(severity, "OK");
        assert!(detail.contains("within normal range"));
    }

    #[test]
    fn test_build_slot_lag_health_detail_multiple_lagging_slots() {
        let lagging = vec!["slot_a (200 MB)".to_string(), "slot_b (300 MB)".to_string()];
        let (severity, detail) = build_slot_lag_health_detail(&lagging, 104_857_600);
        assert_eq!(severity, "WARN");
        assert!(detail.contains("2 WAL slot(s)"));
        assert!(detail.contains("slot_a (200 MB)"));
        assert!(detail.contains("slot_b (300 MB)"));
        assert!(detail.contains("104857600 bytes"));
    }
}
