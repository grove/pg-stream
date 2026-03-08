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
//! - `stale` — data staleness exceeds 2× schedule
//! - `auto_suspended` — ST suspended due to consecutive errors
//! - `reinitialize_needed` — upstream DDL change detected
//! - `slot_lag_warning` — replication slot WAL retention growing

use pgrx::prelude::*;

use crate::catalog::{CdcMode, StDependency};
use crate::config;
use crate::error::PgTrickleError;
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
    /// Replication slot WAL retention is growing.
    BufferGrowthWarning,
    /// Refresh completed successfully.
    RefreshCompleted,
    /// Refresh failed.
    RefreshFailed,
    /// EC-11: Refresh duration is approaching the schedule interval.
    SchedulerFallingBehind,
}

impl AlertEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertEvent::StaleData => "stale_data",
            AlertEvent::AutoSuspended => "auto_suspended",
            AlertEvent::Resumed => "resumed",
            AlertEvent::ReinitializeNeeded => "reinitialize_needed",
            AlertEvent::BufferGrowthWarning => "buffer_growth_warning",
            AlertEvent::RefreshCompleted => "refresh_completed",
            AlertEvent::RefreshFailed => "refresh_failed",
            AlertEvent::SchedulerFallingBehind => "scheduler_falling_behind",
        }
    }
}

/// Emit a NOTIFY on the `pg_trickle_alert` channel with a JSON payload.
///
/// The payload is a JSON object with at minimum an `event` field.
/// Callers can add arbitrary key-value pairs for context.
pub fn emit_alert(event: AlertEvent, pgt_schema: &str, pgt_name: &str, extra: &str) {
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
pub fn alert_stale_data(pgt_schema: &str, pgt_name: &str, staleness_secs: f64, schedule_secs: f64) {
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
    );
}

/// Emit an auto-suspended alert.
pub fn alert_auto_suspended(pgt_schema: &str, pgt_name: &str, error_count: i32) {
    emit_alert(
        AlertEvent::AutoSuspended,
        pgt_schema,
        pgt_name,
        &format!(r#""consecutive_errors":{}"#, error_count),
    );
}

/// Emit a resumed alert (ST cleared from SUSPENDED back to ACTIVE).
pub fn alert_resumed(pgt_schema: &str, pgt_name: &str) {
    emit_alert(
        AlertEvent::Resumed,
        pgt_schema,
        pgt_name,
        r#""previous_status":"SUSPENDED""#,
    );
}

/// Emit a reinitialize-needed alert.
pub fn alert_reinitialize_needed(pgt_schema: &str, pgt_name: &str, reason: &str) {
    emit_alert(
        AlertEvent::ReinitializeNeeded,
        pgt_schema,
        pgt_name,
        &format!(r#""reason":"{}""#, reason.replace('"', r#"\""#)),
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
        pgrx::warning!("pg_trickle: failed to emit slot_lag_warning: {}", e);
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
) {
    emit_alert(
        AlertEvent::RefreshCompleted,
        pgt_schema,
        pgt_name,
        &format!(
            r#""action":"{}","rows_inserted":{},"rows_deleted":{},"duration_ms":{}"#,
            action, rows_inserted, rows_deleted, duration_ms,
        ),
    );
}

/// Emit a refresh-failed alert.
pub fn alert_refresh_failed(pgt_schema: &str, pgt_name: &str, action: &str, error: &str) {
    emit_alert(
        AlertEvent::RefreshFailed,
        pgt_schema,
        pgt_name,
        &format!(
            r#""action":"{}","error":"{}""#,
            action,
            error.replace('"', r#"\""#),
        ),
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
) {
    emit_alert(
        AlertEvent::SchedulerFallingBehind,
        pgt_schema,
        pgt_name,
        &format!(
            r#""elapsed_ms":{},"schedule_ms":{},"ratio":{:.2}"#,
            elapsed_ms, schedule_ms, ratio,
        ),
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
                             THEN EXTRACT(EPOCH FROM (now() - st.data_timestamp)) >
                                  pgtrickle.parse_duration_seconds(st.schedule)
                        END,
                    false)
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
            .map_err(|e| pgrx::error!("st_refresh_stats: SPI select failed: {e}"))
            .expect("unreachable after error!()");

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
            .map_err(|e| pgrx::error!("get_refresh_history: SPI select failed: {e}"))
            .expect("unreachable after error!()");

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
    let mut rows = Vec::new();

    // Trigger-mode sources from change_tracking
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
            .map_err(|e| pgrx::error!("slot_health: SPI select failed: {e}"))
            .expect("unreachable after error!()");

        let mut out = Vec::new();
        for row in result {
            let slot = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let relid = row.get::<i64>(2).unwrap_or(None).unwrap_or(0);
            out.push((slot, relid));
        }
        out
    });

    // Collect source OIDs that have WAL-mode deps (to avoid duplicating)
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
            // Source is WAL or transitioning — get real slot info
            let slot_name = wal_decoder::slot_name_for_source(pg_sys::Oid::from(source_oid_u32));
            let lag = wal_decoder::get_slot_lag_bytes(&slot_name).unwrap_or(0);
            rows.push((slot_name, relid, true, lag, mode.as_str().to_lowercase()));
        } else {
            // Trigger-mode source
            rows.push((slot, relid, true, 0, "trigger".to_string()));
        }
    }

    // Any remaining WAL sources not in change_tracking (shouldn't happen
    // in practice, but handle for robustness)
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

    TableIterator::new(rows)
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

    Ok(props)
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
    ),
> {
    let all_deps = StDependency::get_all().unwrap_or_default();
    let mut rows = Vec::new();
    let mut seen_sources = std::collections::HashSet::new();

    const LAG_ALERT_BYTES: i64 = 1_073_741_824; // 1 GB

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
                ));
            }
            CdcMode::Wal | CdcMode::Transitioning => {
                let slot = dep
                    .slot_name
                    .clone()
                    .unwrap_or_else(|| wal_decoder::slot_name_for_source(dep.source_relid));
                let lag = wal_decoder::get_slot_lag_bytes(&slot).unwrap_or(0);
                let lsn = dep.decoder_confirmed_lsn.clone();

                let alert = if lag > LAG_ALERT_BYTES {
                    Some(format!("slot_lag_exceeds_threshold: {} bytes", lag))
                } else {
                    // Check if the slot still exists
                    let slot_exists = Spi::get_one_with_args::<bool>(
                        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
                        &[slot.as_str().into()],
                    )
                    .unwrap_or(Some(false))
                    .unwrap_or(false);

                    if !slot_exists && dep.cdc_mode == CdcMode::Wal {
                        Some("replication_slot_missing".to_string())
                    } else {
                        None
                    }
                };

                rows.push((
                    oid_u32 as i64,
                    source_name,
                    mode_str,
                    Some(slot),
                    Some(lag),
                    lsn,
                    alert,
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

/// Check all tracked replication slots and emit alerts for any with
/// excessive WAL retention. Called from the scheduler loop.
///
/// Threshold: warn if retained WAL exceeds 1 GB.
pub fn check_slot_health_and_alert() {
    // With trigger-based CDC, we check pending change buffer size instead
    // of replication slot WAL retention. Alert if buffer tables grow too large.
    let change_schema = config::pg_trickle_change_buffer_schema();

    let sources = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT ct.slot_name, ct.source_relid::bigint \
                 FROM pgtrickle.pgt_change_tracking ct",
                None,
                &[],
            )
            .map_err(|e| pgrx::error!("slot_health: SPI select failed: {e}"))
            .expect("unreachable after error!()");

        let mut out = Vec::new();
        for row in result {
            let trigger = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let relid = row.get::<i64>(2).unwrap_or(None).unwrap_or(0);
            out.push((trigger, relid));
        }
        out
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
            .map_err(|e| pgrx::error!("change_buffer_sizes: SPI select failed: {e}"))
            .expect("unreachable after error!()");

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
            .map_err(|e| pgrx::error!("list_sources: SPI select failed: {e}"))
            .expect("unreachable after error!()");

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
            .map_err(|e| pgrx::error!("dependency_tree: failed to load stream tables: {e}"))
            .expect("unreachable after error!()");

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
            .map_err(|e| pgrx::error!("dependency_tree: failed to load dependencies: {e}"))
            .expect("unreachable after error!()");

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
/// - `stale_tables`         — any stream tables with staleness > 2× schedule
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
                 WHERE application_name = 'pg_trickle scheduler'",
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
                    "{} stale stream table(s) (staleness > 2× schedule): {}",
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
        let lagging: Vec<String> = client
            .select(
                "SELECT slot_name || ' (' || pg_size_pretty(retained_wal_bytes) || ')' \
                 FROM pgtrickle.slot_health() \
                 WHERE retained_wal_bytes > 100 * 1024 * 1024 \
                 ORDER BY retained_wal_bytes DESC",
                None,
                &[],
            )
            .map(|r| {
                r.filter_map(|row| row.get::<String>(1).unwrap_or(None))
                    .collect()
            })
            .unwrap_or_default();

        let (sev, detail) = if lagging.is_empty() {
            (
                "OK".to_string(),
                "All WAL replication slots within normal range".to_string(),
            )
        } else {
            (
                "WARN".to_string(),
                format!(
                    "{} WAL slot(s) retaining > 100 MB — risk of slot invalidation: {}",
                    lagging.len(),
                    lagging.join(", ")
                ),
            )
        };
        rows.push(("slot_lag".to_string(), sev, detail));
    });

    TableIterator::new(rows)
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
            .map_err(|e| pgrx::error!("refresh_timeline: SPI select failed: {e}"))
            .expect("unreachable after error!()");

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
            .map_err(|e| pgrx::error!("trigger_inventory: SPI select failed: {e}"))
            .expect("unreachable after error!()");

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
        assert_eq!(AlertEvent::RefreshCompleted.as_str(), "refresh_completed");
        assert_eq!(AlertEvent::RefreshFailed.as_str(), "refresh_failed");
        assert_eq!(
            AlertEvent::SchedulerFallingBehind.as_str(),
            "scheduler_falling_behind"
        );
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
            AlertEvent::RefreshCompleted,
            AlertEvent::RefreshFailed,
            AlertEvent::SchedulerFallingBehind,
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
}
