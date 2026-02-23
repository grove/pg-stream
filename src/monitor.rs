//! Monitoring, observability, and alerting for pgstream.
//!
//! # Statistics
//!
//! Per-ST statistics are tracked in shared memory via atomic counters and
//! exposed through the `pgstream.dt_refresh_stats()` table-returning function
//! which aggregates from `pgstream.pgs_refresh_history`.
//!
//! The `pgstream.pg_stat_stream_tables` view combines catalog metadata with
//! runtime stats for a single-query operational overview.
//!
//! # NOTIFY Alerting
//!
//! Operational events are emitted via PostgreSQL `NOTIFY` on the
//! `pg_stream_alert` channel. Clients can `LISTEN pg_stream_alert;` to receive
//! JSON-formatted events:
//! - `stale` — data staleness exceeds 2× schedule
//! - `auto_suspended` — ST suspended due to consecutive errors
//! - `reinitialize_needed` — upstream DDL change detected
//! - `slot_lag_warning` — replication slot WAL retention growing

use pgrx::prelude::*;

use crate::config;
use crate::error::PgStreamError;

// ── NOTIFY Alerting ────────────────────────────────────────────────────────

/// Alert event types emitted on the `pg_stream_alert` NOTIFY channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertEvent {
    /// data staleness exceeds 2× schedule.
    StaleData,
    /// ST suspended after consecutive errors.
    AutoSuspended,
    /// Upstream DDL change requires reinitialize.
    ReinitializeNeeded,
    /// Replication slot WAL retention is growing.
    BufferGrowthWarning,
    /// Refresh completed successfully.
    RefreshCompleted,
    /// Refresh failed.
    RefreshFailed,
}

impl AlertEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertEvent::StaleData => "stale_data",
            AlertEvent::AutoSuspended => "auto_suspended",
            AlertEvent::ReinitializeNeeded => "reinitialize_needed",
            AlertEvent::BufferGrowthWarning => "buffer_growth_warning",
            AlertEvent::RefreshCompleted => "refresh_completed",
            AlertEvent::RefreshFailed => "refresh_failed",
        }
    }
}

/// Emit a NOTIFY on the `pg_stream_alert` channel with a JSON payload.
///
/// The payload is a JSON object with at minimum an `event` field.
/// Callers can add arbitrary key-value pairs for context.
pub fn emit_alert(event: AlertEvent, pgs_schema: &str, pgs_name: &str, extra: &str) {
    let payload = format!(
        r#"{{"event":"{}","pgs_schema":"{}","pgs_name":"{}","dt":"{}",{}}}"#,
        event.as_str(),
        pgs_schema.replace('"', r#"\""#),
        pgs_name.replace('"', r#"\""#),
        format!("{}.{}", pgs_schema, pgs_name).replace('"', r#"\""#),
        extra,
    );

    // NOTIFY payloads are limited to ~8000 bytes; truncate if needed
    let safe_payload = if payload.len() > 7900 {
        format!("{}...}}", &payload[..7890])
    } else {
        payload
    };

    // Escape single quotes for SQL
    let escaped = safe_payload.replace('\'', "''");
    let sql = format!("NOTIFY pg_stream_alert, '{}'", escaped);

    if let Err(e) = Spi::run(&sql) {
        pgrx::warning!("pg_stream: failed to emit alert {}: {}", event.as_str(), e);
    }
}

/// Emit a stale-data alert.
pub fn alert_stale_data(pgs_schema: &str, pgs_name: &str, staleness_secs: f64, schedule_secs: f64) {
    emit_alert(
        AlertEvent::StaleData,
        pgs_schema,
        pgs_name,
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
pub fn alert_auto_suspended(pgs_schema: &str, pgs_name: &str, error_count: i32) {
    emit_alert(
        AlertEvent::AutoSuspended,
        pgs_schema,
        pgs_name,
        &format!(r#""consecutive_errors":{}"#, error_count),
    );
}

/// Emit a reinitialize-needed alert.
pub fn alert_reinitialize_needed(pgs_schema: &str, pgs_name: &str, reason: &str) {
    emit_alert(
        AlertEvent::ReinitializeNeeded,
        pgs_schema,
        pgs_name,
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
    let sql = format!("NOTIFY pg_stream_alert, '{}'", escaped);
    if let Err(e) = Spi::run(&sql) {
        pgrx::warning!("pg_stream: failed to emit slot_lag_warning: {}", e);
    }
}

/// Emit a refresh-completed alert.
pub fn alert_refresh_completed(
    pgs_schema: &str,
    pgs_name: &str,
    action: &str,
    rows_inserted: i64,
    rows_deleted: i64,
    duration_ms: i64,
) {
    emit_alert(
        AlertEvent::RefreshCompleted,
        pgs_schema,
        pgs_name,
        &format!(
            r#""action":"{}","rows_inserted":{},"rows_deleted":{},"duration_ms":{}"#,
            action, rows_inserted, rows_deleted, duration_ms,
        ),
    );
}

/// Emit a refresh-failed alert.
pub fn alert_refresh_failed(pgs_schema: &str, pgs_name: &str, action: &str, error: &str) {
    emit_alert(
        AlertEvent::RefreshFailed,
        pgs_schema,
        pgs_name,
        &format!(
            r#""action":"{}","error":"{}""#,
            action,
            error.replace('"', r#"\""#),
        ),
    );
}

// ── SQL-exposed monitoring functions ───────────────────────────────────────

/// Return per-ST refresh statistics aggregated from the refresh history table.
///
/// This is the primary monitoring function, exposed as `pgstream.dt_refresh_stats()`.
#[pg_extern(schema = "pgstream", name = "dt_refresh_stats")]
#[allow(clippy::type_complexity)]
fn dt_refresh_stats() -> TableIterator<
    'static,
    (
        name!(pgs_name, String),
        name!(pgs_schema, String),
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
                    dt.pgs_name,
                    dt.pgs_schema,
                    dt.status,
                    dt.refresh_mode,
                    dt.is_populated,
                    COALESCE(stats.total_refreshes, 0)::bigint,
                    COALESCE(stats.successful_refreshes, 0)::bigint,
                    COALESCE(stats.failed_refreshes, 0)::bigint,
                    COALESCE(stats.total_rows_inserted, 0)::bigint,
                    COALESCE(stats.total_rows_deleted, 0)::bigint,
                    COALESCE(stats.avg_duration_ms, 0)::float8,
                    last_hist.action,
                    last_hist.status,
                    dt.last_refresh_at,
                    EXTRACT(EPOCH FROM (now() - dt.data_timestamp))::float8,
                    COALESCE(
                        CASE WHEN dt.schedule IS NOT NULL AND dt.data_timestamp IS NOT NULL
                                  AND dt.schedule NOT LIKE '% %'
                                  AND dt.schedule NOT LIKE '@%'
                             THEN EXTRACT(EPOCH FROM (now() - dt.data_timestamp)) >
                                  pgstream.parse_duration_seconds(dt.schedule)
                        END,
                    false)
                FROM pgstream.pgs_stream_tables dt
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
                    FROM pgstream.pgs_refresh_history h
                    WHERE h.pgs_id = dt.pgs_id
                ) stats ON true
                LEFT JOIN LATERAL (
                    SELECT h2.action, h2.status
                    FROM pgstream.pgs_refresh_history h2
                    WHERE h2.pgs_id = dt.pgs_id
                    ORDER BY h2.refresh_id DESC
                    LIMIT 1
                ) last_hist ON true
                ORDER BY dt.pgs_schema, dt.pgs_name",
                None,
                &[],
            )
            .unwrap();

        let mut out = Vec::new();
        for row in result {
            let pgs_name = row.get::<String>(1).unwrap().unwrap_or_default();
            let pgs_schema = row.get::<String>(2).unwrap().unwrap_or_default();
            let status = row.get::<String>(3).unwrap().unwrap_or_default();
            let refresh_mode = row.get::<String>(4).unwrap().unwrap_or_default();
            let is_populated = row.get::<bool>(5).unwrap().unwrap_or(false);
            let total_refreshes = row.get::<i64>(6).unwrap().unwrap_or(0);
            let successful = row.get::<i64>(7).unwrap().unwrap_or(0);
            let failed = row.get::<i64>(8).unwrap().unwrap_or(0);
            let rows_inserted = row.get::<i64>(9).unwrap().unwrap_or(0);
            let rows_deleted = row.get::<i64>(10).unwrap().unwrap_or(0);
            let avg_duration = row.get::<f64>(11).unwrap().unwrap_or(0.0);
            let last_action = row.get::<String>(12).unwrap();
            let last_status = row.get::<String>(13).unwrap();
            let last_refresh_at = row.get::<TimestampWithTimeZone>(14).unwrap();
            let staleness = row.get::<f64>(15).unwrap();
            let stale = row.get::<bool>(16).unwrap().unwrap_or(false);

            out.push((
                pgs_name,
                pgs_schema,
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
/// Exposed as `pgstream.get_refresh_history(name, limit)`.
#[pg_extern(schema = "pgstream", name = "get_refresh_history")]
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
                FROM pgstream.pgs_refresh_history h
                JOIN pgstream.pgs_stream_tables dt ON dt.pgs_id = h.pgs_id
                WHERE dt.pgs_schema = $1 AND dt.pgs_name = $2
                ORDER BY h.refresh_id DESC
                LIMIT $3",
                None,
                &[schema.into(), table_name.into(), max_rows.into()],
            )
            .unwrap();

        let mut out = Vec::new();
        for row in result {
            let refresh_id = row.get::<i64>(1).unwrap().unwrap_or(0);
            let data_ts = row
                .get::<TimestampWithTimeZone>(2)
                .unwrap()
                .unwrap_or_else(|| TimestampWithTimeZone::try_from(0i64).unwrap());
            let start = row
                .get::<TimestampWithTimeZone>(3)
                .unwrap()
                .unwrap_or_else(|| TimestampWithTimeZone::try_from(0i64).unwrap());
            let end = row.get::<TimestampWithTimeZone>(4).unwrap();
            let action = row.get::<String>(5).unwrap().unwrap_or_default();
            let status = row.get::<String>(6).unwrap().unwrap_or_default();
            let ins = row.get::<i64>(7).unwrap().unwrap_or(0);
            let del = row.get::<i64>(8).unwrap().unwrap_or(0);
            let dur = row.get::<f64>(9).unwrap();
            let err = row.get::<String>(10).unwrap();

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
/// Exposed as `pgstream.get_staleness(name)`.
#[pg_extern(schema = "pgstream", name = "get_staleness")]
fn get_staleness(name: &str) -> Option<f64> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    let (schema, table_name) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        ("public", parts[0])
    };

    Spi::get_one_with_args::<f64>(
        "SELECT EXTRACT(EPOCH FROM (now() - data_timestamp))::float8 \
         FROM pgstream.pgs_stream_tables \
         WHERE pgs_schema = $1 AND pgs_name = $2 AND data_timestamp IS NOT NULL",
        &[schema.into(), table_name.into()],
    )
    .unwrap_or(None)
}

/// Check CDC trigger health for all tracked sources.
///
/// Returns trigger name, source table, and pending change count.
/// Exposed as `pgstream.slot_health()` (kept for API compatibility).
#[pg_extern(schema = "pgstream", name = "slot_health")]
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
    let rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    ct.slot_name,
                    ct.source_relid::bigint,
                    true,
                    0::bigint,
                    'trigger'
                FROM pgstream.pgs_change_tracking ct",
                None,
                &[],
            )
            .unwrap();

        let mut out = Vec::new();
        for row in result {
            let slot = row.get::<String>(1).unwrap().unwrap_or_default();
            let relid = row.get::<i64>(2).unwrap().unwrap_or(0);
            let active = row.get::<bool>(3).unwrap().unwrap_or(true);
            let retained = row.get::<i64>(4).unwrap().unwrap_or(0);
            let wal_status = row
                .get::<String>(5)
                .unwrap()
                .unwrap_or_else(|| "trigger".to_string());
            out.push((slot, relid, active, retained, wal_status));
        }
        out
    });

    TableIterator::new(rows)
}

/// Explain the DVM plan for a stream table's defining query.
///
/// Returns whether the query supports differential refresh,
/// lists the operators found, and shows the generated delta query.
/// Exposed as `pgstream.explain_dt(name)`.
#[pg_extern(schema = "pgstream", name = "explain_dt")]
fn explain_dt(
    name: &str,
) -> TableIterator<'static, (name!(property, String), name!(value, String))> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    let (schema, table_name) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        ("public", parts[0])
    };

    let rows = explain_dt_impl(schema, table_name)
        .unwrap_or_else(|e| vec![("error".to_string(), e.to_string())]);

    TableIterator::new(rows)
}

fn explain_dt_impl(schema: &str, table_name: &str) -> Result<Vec<(String, String)>, PgStreamError> {
    use crate::catalog::StreamTableMeta;
    use crate::dvm;

    let dt = StreamTableMeta::get_by_name(schema, table_name)?;

    let mut props = Vec::new();

    props.push((
        "pgs_name".to_string(),
        format!("{}.{}", dt.pgs_schema, dt.pgs_name),
    ));
    props.push(("defining_query".to_string(), dt.defining_query.clone()));
    props.push((
        "refresh_mode".to_string(),
        dt.refresh_mode.as_str().to_string(),
    ));
    props.push(("status".to_string(), dt.status.as_str().to_string()));
    props.push(("is_populated".to_string(), dt.is_populated.to_string()));

    // Parse the defining query to check DVM support
    match dvm::parse_defining_query(&dt.defining_query) {
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
                &dt.defining_query,
                &prev_frontier,
                &new_frontier,
                &dt.pgs_schema,
                &dt.pgs_name,
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
    if let Some(ref frontier) = dt.frontier {
        if let Ok(json) = frontier.to_json() {
            props.push(("frontier".to_string(), json));
        }
    } else {
        props.push(("frontier".to_string(), "null".to_string()));
    }

    Ok(props)
}

// ── Slot Health Monitoring (used by scheduler) ─────────────────────────────

/// Check all tracked replication slots and emit alerts for any with
/// excessive WAL retention. Called from the scheduler loop.
///
/// Threshold: warn if retained WAL exceeds 1 GB.
pub fn check_slot_health_and_alert() {
    // With trigger-based CDC, we check pending change buffer size instead
    // of replication slot WAL retention. Alert if buffer tables grow too large.
    let change_schema = config::pg_stream_change_buffer_schema();

    let sources = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT ct.slot_name, ct.source_relid::bigint \
                 FROM pgstream.pgs_change_tracking ct",
                None,
                &[],
            )
            .unwrap();

        let mut out = Vec::new();
        for row in result {
            let trigger = row.get::<String>(1).unwrap().unwrap_or_default();
            let relid = row.get::<i64>(2).unwrap().unwrap_or(0);
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

        // Alert if more than 1 million pending changes
        if pending > 1_000_000 {
            alert_buffer_growth(&trigger_name, pending);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_event_as_str() {
        assert_eq!(AlertEvent::StaleData.as_str(), "stale_data");
        assert_eq!(AlertEvent::AutoSuspended.as_str(), "auto_suspended");
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
            AlertEvent::ReinitializeNeeded,
            AlertEvent::BufferGrowthWarning,
            AlertEvent::RefreshCompleted,
            AlertEvent::RefreshFailed,
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
}
