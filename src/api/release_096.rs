//! v0.96.0 operational contracts: resource evidence, disk forecasts, and
//! stable refresh diagnostics.

use pgrx::prelude::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
struct Setting {
    value: String,
    source: String,
}

fn settings() -> BTreeMap<String, Setting> {
    Spi::connect(|client| {
        client
            .select(
                "SELECT name::text, setting::text, source::text \
                   FROM pg_catalog.pg_settings \
                  WHERE name IN ('shared_buffers', 'max_connections', \
                                 'max_worker_processes', 'pg_trickle.memory_budget_mb', \
                                 'pg_trickle.max_concurrent_refreshes') \
                  ORDER BY name",
                None,
                &[],
            )
            .map(|rows| {
                rows.filter_map(|row| {
                    Some((
                        row.get::<String>(1).ok().flatten()?,
                        Setting {
                            value: row.get::<String>(2).ok().flatten()?,
                            source: row.get::<String>(3).ok().flatten()?,
                        },
                    ))
                })
                .collect()
            })
            .unwrap_or_default()
    })
}

fn visible_memory_bytes() -> Option<u64> {
    [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ]
    .iter()
    .filter_map(|path| std::fs::read_to_string(path).ok())
    .filter_map(|value| {
        let value = value.trim();
        if value == "max" {
            None
        } else {
            value.parse::<u64>().ok().filter(|bytes| *bytes > 0)
        }
    })
    .next()
    .or_else(|| {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("MemTotal:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|kb| kb.parse::<u64>().ok())
                    .map(|kb| kb.saturating_mul(1024))
            })
    })
}

fn auto_workers(max_worker_processes: Option<i32>) -> i32 {
    let cpu = std::thread::available_parallelism()
        .map(|value| value.get() as i32)
        .unwrap_or(1);
    let postgres = max_worker_processes.unwrap_or(cpu).max(1);
    cpu.min(postgres).clamp(1, 32)
}

fn memory_mb(bytes: Option<u64>) -> Option<u64> {
    bytes.map(|value| value / (1024 * 1024))
}

/// Report detected constraints and the values pg_trickle selected from them.
#[pg_extern(schema = "pgtrickle", name = "active_profile")]
#[allow(clippy::type_complexity)]
pub fn active_profile() -> TableIterator<
    'static,
    (
        name!(constraint_name, String),
        name!(detected_value, Option<String>),
        name!(source, String),
        name!(selected_value, String),
        name!(overridden, bool),
        name!(available, bool),
        name!(detail, String),
    ),
> {
    let current = settings();
    let max_workers = current
        .get("max_worker_processes")
        .and_then(|setting| setting.value.parse::<i32>().ok());
    let host_memory = visible_memory_bytes();
    let configured_budget = current
        .get("pg_trickle.memory_budget_mb")
        .and_then(|setting| setting.value.parse::<u64>().ok())
        .unwrap_or(256);
    let auto_budget = memory_mb(host_memory)
        .map(|mb| (mb / 10).clamp(16, 1_048_576))
        .unwrap_or(configured_budget);
    let configured_workers = current
        .get("pg_trickle.max_concurrent_refreshes")
        .and_then(|setting| setting.value.parse::<i32>().ok())
        .unwrap_or(4);
    let auto_worker_count = auto_workers(max_workers);

    let row = |name: &str,
               detected: Option<String>,
               source: &str,
               selected: String,
               overridden: bool,
               available: bool,
               detail: &str| {
        (
            name.to_string(),
            detected,
            source.to_string(),
            selected,
            overridden,
            available,
            detail.to_string(),
        )
    };

    let memory_source = current
        .get("pg_trickle.memory_budget_mb")
        .map(|setting| setting.source.clone())
        .unwrap_or_else(|| "default".to_string());
    let worker_source = current
        .get("pg_trickle.max_concurrent_refreshes")
        .map(|setting| setting.source.clone())
        .unwrap_or_else(|| "default".to_string());

    TableIterator::new(vec![
        row(
            "host_memory",
            memory_mb(host_memory).map(|value| format!("{value} MiB")),
            "cgroup/procfs",
            format!("{} MiB budget", auto_budget),
            current
                .get("pg_trickle.memory_budget_mb")
                .is_some_and(|setting| setting.source != "default"),
            host_memory.is_some(),
            "The selected extension memory budget is capped at 10% of visible memory unless the GUC is overridden.",
        ),
        row(
            "max_worker_processes",
            max_workers.map(|value| value.to_string()),
            "pg_settings",
            auto_worker_count.to_string(),
            current
                .get("max_worker_processes")
                .is_some_and(|setting| setting.source != "default"),
            max_workers.is_some(),
            "Worker admission is throttled; PostgreSQL's global worker limit remains authoritative.",
        ),
        row(
            "cpu_parallelism",
            std::thread::available_parallelism()
                .ok()
                .map(|value| value.get().to_string()),
            "host",
            auto_worker_count.to_string(),
            current
                .get("pg_trickle.max_concurrent_refreshes")
                .is_some_and(|setting| setting.source != "default"),
            std::thread::available_parallelism().is_ok(),
            "CPU is used as an admission hint; committed changes are never discarded.",
        ),
        row(
            "max_connections",
            current
                .get("max_connections")
                .map(|setting| setting.value.clone()),
            "pg_settings",
            current
                .get("max_connections")
                .map(|setting| setting.value.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            current
                .get("max_connections")
                .is_some_and(|setting| setting.source != "default"),
            current.contains_key("max_connections"),
            "Connection pressure is forecast from PostgreSQL's configured limit.",
        ),
        row(
            "shared_buffers",
            current
                .get("shared_buffers")
                .map(|setting| setting.value.clone()),
            "pg_settings",
            current
                .get("shared_buffers")
                .map(|setting| setting.value.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            current
                .get("shared_buffers")
                .is_some_and(|setting| setting.source != "default"),
            current.contains_key("shared_buffers"),
            "Shared buffers are evidence only; PostgreSQL owns their enforcement.",
        ),
        row(
            "pg_trickle_memory_budget_mb",
            Some(configured_budget.to_string()),
            &memory_source,
            auto_budget.to_string(),
            current
                .get("pg_trickle.memory_budget_mb")
                .is_some_and(|setting| setting.source != "default"),
            true,
            "Hard bound for extension-managed in-process state; storage growth is reported separately.",
        ),
        row(
            "pg_trickle_max_concurrent_refreshes",
            Some(configured_workers.to_string()),
            &worker_source,
            auto_worker_count.to_string(),
            current
                .get("pg_trickle.max_concurrent_refreshes")
                .is_some_and(|setting| setting.source != "default"),
            true,
            "Throttled admission limit for per-database refresh concurrency.",
        ),
    ])
}

/// Report stream-table storage, pending CDC storage, and configured disk
/// headroom. A forecast is intentionally advisory: PostgreSQL and source
/// activity can continue to grow beyond it.
#[pg_extern(schema = "pgtrickle", name = "disk_usage")]
#[allow(clippy::type_complexity)]
pub fn disk_usage() -> TableIterator<
    'static,
    (
        name!(stream_table, String),
        name!(relation_bytes, i64),
        name!(change_buffer_bytes, i64),
        name!(projected_bytes, i64),
        name!(headroom_bytes, i64),
        name!(pressure_state, String),
    ),
> {
    let headroom = crate::config::pg_trickle_disk_headroom_bytes() as i64;
    let rows: Vec<_> = Spi::connect(|client| {
        client
            .select(
                "SELECT st.pgt_schema::text || '.' || st.pgt_name::text, \
                        pg_total_relation_size(st.pgt_relid)::bigint, \
                        COALESCE(( \
                          SELECT sum(pg_total_relation_size(to_regclass( \
                              format('%I.%I', current_setting('pg_trickle.change_buffer_schema'), \
                                     'changes_' || COALESCE(NULLIF(ct.source_stable_name, ''), dep.source_relid::text))))) \
                            FROM pgtrickle.pgt_dependencies dep \
                            LEFT JOIN pgtrickle.pgt_change_tracking ct \
                              ON ct.source_relid = dep.source_relid \
                           WHERE dep.pgt_id = st.pgt_id \
                             AND dep.source_type <> 'STREAM_TABLE'), 0)::bigint \
                   FROM pgtrickle.pgt_stream_tables st \
                  ORDER BY st.pgt_schema, st.pgt_name",
                None,
                &[],
            )
            .map(|result| {
                result
                    .filter_map(|row| {
                        let stream_table = row.get::<String>(1).ok().flatten()?;
                        let relation_bytes = row.get::<i64>(2).ok().flatten().unwrap_or(0).max(0);
                        let buffer_bytes = row.get::<i64>(3).ok().flatten().unwrap_or(0).max(0);
                        let projected = relation_bytes.saturating_add(buffer_bytes);
                        let pressure = if headroom == 0 {
                            "DISABLED"
                        } else if projected >= headroom {
                            "OVER_HEADROOM"
                        } else if projected.saturating_mul(100) >= headroom.saturating_mul(80) {
                            "WARN"
                        } else {
                            "OK"
                        };
                        Some((
                            stream_table,
                            relation_bytes,
                            buffer_bytes,
                            projected,
                            (headroom - projected).max(0),
                            pressure.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    TableIterator::new(rows)
}

/// Stable operational error identifiers and their PostgreSQL SQLSTATEs.
#[pg_extern(schema = "pgtrickle", name = "error_catalog")]
pub fn error_catalog() -> TableIterator<
    'static,
    (
        name!(error_id, String),
        name!(sqlstate, String),
        name!(detail, String),
        name!(hint, String),
    ),
> {
    TableIterator::new(vec![
        (
            "LOCK_TIMEOUT".to_string(),
            "55P03".to_string(),
            "A refresh could not acquire its required lock before the deadline.".to_string(),
            "Retry after the competing transaction finishes or increase lock_timeout.".to_string(),
        ),
        (
            "STATEMENT_TIMEOUT".to_string(),
            "57014".to_string(),
            "A refresh exceeded its transaction-local statement deadline.".to_string(),
            "Reduce the refresh scope or raise statement_timeout for the operation.".to_string(),
        ),
        (
            "DEADLOCK".to_string(),
            "40P01".to_string(),
            "PostgreSQL aborted the refresh because it detected a deadlock.".to_string(),
            "Retry the operation and inspect concurrent lock ordering.".to_string(),
        ),
        (
            "SERIALIZATION".to_string(),
            "40001".to_string(),
            "PostgreSQL aborted the refresh to preserve serializable correctness.".to_string(),
            "Retry the operation; committed changes are retained.".to_string(),
        ),
        (
            "OUT_OF_MEMORY".to_string(),
            "53200".to_string(),
            "The refresh exceeded available PostgreSQL memory.".to_string(),
            "Lower the pg_trickle memory budget or reduce the refresh workload.".to_string(),
        ),
        (
            "PERMANENT".to_string(),
            "XX000".to_string(),
            "The refresh failed for a non-retryable operational condition.".to_string(),
            "Inspect the stream-table error detail and apply its remediation.".to_string(),
        ),
    ])
}

/// Return the v0.96 disk forecast rows used by health_check().
pub(crate) fn disk_health_rows() -> Vec<(String, String, String)> {
    let headroom = crate::config::pg_trickle_disk_headroom_bytes() as i64;
    if headroom == 0 {
        return vec![(
            "disk_forecast".to_string(),
            "OK".to_string(),
            "Disk forecast is disabled by pg_trickle.disk_headroom_mb = 0.".to_string(),
        )];
    }

    let max_projected = Spi::get_one::<i64>(
        "SELECT COALESCE(max(pg_total_relation_size(st.pgt_relid) + \
                COALESCE((SELECT sum(pg_total_relation_size(to_regclass( \
                    format('%I.%I', current_setting('pg_trickle.change_buffer_schema'), \
                           'changes_' || COALESCE(NULLIF(ct.source_stable_name, ''), dep.source_relid::text))))) \
                  FROM pgtrickle.pgt_dependencies dep \
                  LEFT JOIN pgtrickle.pgt_change_tracking ct ON ct.source_relid = dep.source_relid \
                 WHERE dep.pgt_id = st.pgt_id AND dep.source_type <> 'STREAM_TABLE'), 0)), 0)::bigint \
           FROM pgtrickle.pgt_stream_tables st",
    )
    .unwrap_or(None)
    .unwrap_or(0)
    .max(0);
    let severity = if max_projected >= headroom {
        "WARN"
    } else {
        "OK"
    };
    vec![(
        "disk_forecast".to_string(),
        severity.to_string(),
        format!(
            "maximum projected stream-table footprint={max_projected} bytes; configured headroom={headroom} bytes; enforcement=FORECAST_AND_REACT"
        ),
    )]
}

#[cfg(test)]
mod tests {
    use super::auto_workers;

    #[test]
    fn worker_admission_respects_cpu_and_postgres_bounds() {
        assert_eq!(auto_workers(Some(2)), 2);
        let cpu = std::thread::available_parallelism()
            .map(|value| value.get() as i32)
            .unwrap_or(1);
        assert_eq!(auto_workers(Some(128)), cpu.clamp(1, 32));
        assert!((1..=32).contains(&auto_workers(None)));
    }
}
