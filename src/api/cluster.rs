//! CLUS-1/2/3 (v0.27.0): Cluster-wide worker observability.
//!
//! Provides:
//! - `cluster_worker_summary()` — per-database worker counts from shmem + pg_stat_activity
//! - Internal helpers for per-DB Prometheus label generation (CLUS-2)

use pgrx::prelude::*;

use crate::shmem;

// ── CLUS-1: cluster_worker_summary ────────────────────────────────────────

/// CLUS-1 (v0.27.0): Return a per-database summary of active pg_trickle
/// background workers visible from this PostgreSQL instance.
///
/// Reads from `pg_stat_activity` (shared catalog) so the calling role needs
/// `pg_monitor` or superuser privilege.
#[pg_extern(schema = "pgtrickle")]
#[allow(clippy::type_complexity)]
pub fn cluster_worker_summary() -> TableIterator<
    'static,
    (
        name!(db_oid, Option<i64>),
        name!(db_name, Option<String>),
        name!(active_workers, Option<i32>),
        name!(scheduler_pid, Option<i32>),
        name!(scheduler_running, Option<bool>),
        name!(total_active_workers, Option<i32>),
    ),
> {
    let rows = cluster_worker_summary_impl();
    TableIterator::new(rows)
}

#[allow(clippy::type_complexity)]
fn cluster_worker_summary_impl() -> Vec<(
    Option<i64>,
    Option<String>,
    Option<i32>,
    Option<i32>,
    Option<bool>,
    Option<i32>,
)> {
    // Global totals from shared memory (this instance)
    let total_workers = shmem::active_worker_count() as i32;
    let sched_pid = shmem::scheduler_pid();
    let sched_running = shmem::scheduler_running();

    // Per-DB worker breakdown from pg_stat_activity
    let per_db = Spi::connect(|client| {
        let result = client.select(
            "SELECT datid::bigint, datname::text, \
                    COUNT(*)::int AS worker_count \
             FROM pg_catalog.pg_stat_activity \
             WHERE application_name LIKE 'pg_trickle_%' \
               AND state IS NOT NULL \
             GROUP BY datid, datname \
             ORDER BY datname",
            None,
            &[],
        );

        match result {
            Ok(rows) => rows
                .map(|r| {
                    let db_oid = r.get::<i64>(1).ok().flatten();
                    let db_name = r.get::<String>(2).ok().flatten();
                    let worker_count = r.get::<i32>(3).ok().flatten();
                    (db_oid, db_name, worker_count)
                })
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        }
    });

    if per_db.is_empty() {
        // Return a single row with cluster-wide totals when per-DB data is unavailable
        return vec![(
            None,
            None,
            Some(total_workers),
            Some(sched_pid),
            Some(sched_running),
            Some(total_workers),
        )];
    }

    per_db
        .into_iter()
        .map(|(db_oid, db_name, active_workers)| {
            (
                db_oid,
                db_name,
                active_workers,
                Some(sched_pid),
                Some(sched_running),
                Some(total_workers),
            )
        })
        .collect()
}

// ── CLUS-2: Per-DB Prometheus label helpers ────────────────────────────────

/// CLUS-2 (v0.27.0): Return the current database OID and name for use
/// as Prometheus labels in metrics exposition.
///
/// Called by `collect_metrics_text()` in monitor.rs to add `db_oid` and
/// `db_name` labels to all per-instance metrics.
pub(crate) fn current_db_labels() -> (Option<i64>, Option<String>) {
    let db_info = Spi::get_one::<String>(
        "SELECT datid::bigint::text || '|' || datname::text \
         FROM pg_catalog.pg_stat_activity \
         WHERE pid = pg_backend_pid() \
         LIMIT 1",
    )
    .unwrap_or(None);

    match db_info {
        Some(s) => {
            if let Some((oid_s, name)) = s.split_once('|') {
                let oid = oid_s.parse::<i64>().ok();
                (oid, Some(name.to_string()))
            } else {
                (None, None)
            }
        }
        None => (None, None),
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_cluster_worker_summary_impl_no_crash() {
        // Without a DB, this must return without panicking (empty fallback)
        // We can't call SPI in unit tests; just verify the return type compiles.
        // Real behaviour tested in integration tests.
        let _: fn() -> Vec<(
            Option<i64>,
            Option<String>,
            Option<i32>,
            Option<i32>,
            Option<bool>,
            Option<i32>,
        )> = cluster_worker_summary_impl;
    }
}
