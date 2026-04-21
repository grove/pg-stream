//! METR-3 (v0.27.0): `metrics_summary()` SQL function.
//!
//! Returns a cluster-wide aggregation of key pg_trickle counters from
//! `pgtrickle.pgt_stream_tables` and `pgt_refresh_history`.
//! Designed as the data source for the Grafana cluster-overview dashboard.
//!
//! # Grafana query example
//! ```sql
//! SELECT * FROM pgtrickle.metrics_summary();
//! ```

use pgrx::prelude::*;

// ── METR-3: metrics_summary ────────────────────────────────────────────────

/// METR-3 (v0.27.0): Cluster-wide metrics summary for the Grafana
/// overview dashboard.
///
/// Aggregates refresh counts, error counts, and worker utilisation from
/// all stream tables registered in this database. Includes `db_name` so
/// multi-database Grafana panels can use this as a single data source.
#[pg_extern(schema = "pgtrickle")]
#[allow(clippy::type_complexity)]
pub fn metrics_summary() -> TableIterator<
    'static,
    (
        name!(db_name, Option<String>),
        name!(total_stream_tables, Option<i64>),
        name!(active_stream_tables, Option<i64>),
        name!(suspended_stream_tables, Option<i64>),
        name!(total_refreshes, Option<i64>),
        name!(successful_refreshes, Option<i64>),
        name!(failed_refreshes, Option<i64>),
        name!(total_rows_processed, Option<i64>),
        name!(active_workers, Option<i32>),
    ),
> {
    let rows = metrics_summary_impl();
    TableIterator::new(rows)
}

#[allow(clippy::type_complexity)]
fn metrics_summary_impl() -> Vec<(
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i32>,
)> {
    let active_workers = crate::shmem::active_worker_count() as i32;

    let row = Spi::connect(|client| {
        let result = client.select(
            "SELECT \
               current_database()::text                                             AS db_name, \
               COUNT(*)                                                              AS total, \
               COUNT(*) FILTER (WHERE status = 'ACTIVE')                            AS active, \
               COUNT(*) FILTER (WHERE status = 'SUSPENDED')                         AS suspended, \
               COALESCE(SUM(h.refresh_count), 0)                                     AS total_refreshes, \
               COALESCE(SUM(h.success_count), 0)                                     AS successful_refreshes, \
               COALESCE(SUM(h.fail_count), 0)                                        AS failed_refreshes, \
               COALESCE(SUM(h.total_rows), 0)                                        AS total_rows \
             FROM pgtrickle.pgt_stream_tables s \
             LEFT JOIN LATERAL ( \
               SELECT \
                 COUNT(*)                                          AS refresh_count, \
                 COUNT(*) FILTER (WHERE status = 'COMPLETED')     AS success_count, \
                 COUNT(*) FILTER (WHERE status = 'FAILED')        AS fail_count, \
                 COALESCE(SUM( \
                   COALESCE(rows_inserted, 0) + COALESCE(rows_deleted, 0) \
                 ), 0)                                             AS total_rows \
               FROM pgtrickle.pgt_refresh_history \
               WHERE pgt_id = s.pgt_id \
             ) h ON true",
            None,
            &[],
        );

        match result {
            Ok(rows) => rows.into_iter().next().map(|row| {
                let db_name = row.get::<String>(1).ok().flatten();
                let total = row.get::<i64>(2).ok().flatten();
                let active = row.get::<i64>(3).ok().flatten();
                let suspended = row.get::<i64>(4).ok().flatten();
                let total_refreshes = row.get::<i64>(5).ok().flatten();
                let successful = row.get::<i64>(6).ok().flatten();
                let failed = row.get::<i64>(7).ok().flatten();
                let rows_processed = row.get::<i64>(8).ok().flatten();
                (
                    db_name,
                    total,
                    active,
                    suspended,
                    total_refreshes,
                    successful,
                    failed,
                    rows_processed,
                )
            }),
            Err(_) => None,
        }
    });

    match row {
        Some((db, total, active, susp, tr, sr, fr, rp)) => {
            vec![(
                db,
                total,
                active,
                susp,
                tr,
                sr,
                fr,
                rp,
                Some(active_workers),
            )]
        }
        None => Vec::new(),
    }
}
