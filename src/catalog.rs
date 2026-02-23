//! Catalog layer — metadata tables and CRUD operations for stream tables.
//!
//! All catalog access goes through PostgreSQL's SPI interface. This module
//! provides typed Rust abstractions over the `pgstream.pgs_stream_tables`,
//! `pgstream.pgs_dependencies`, and `pgstream.pgs_refresh_history` tables.

use pgrx::prelude::*;
use pgrx::spi::{SpiHeapTupleData, SpiTupleTable};

use crate::dag::{DtStatus, RefreshMode};
use crate::error::PgStreamError;
use crate::version::Frontier;

/// Metadata for a stream table, mirrors `pgstream.pgs_stream_tables`.
#[derive(Debug, Clone)]
pub struct StreamTableMeta {
    pub pgs_id: i64,
    pub pgs_relid: pg_sys::Oid,
    pub pgs_name: String,
    pub pgs_schema: String,
    pub defining_query: String,
    pub schedule: Option<String>,
    pub refresh_mode: RefreshMode,
    pub status: DtStatus,
    pub is_populated: bool,
    pub data_timestamp: Option<TimestampWithTimeZone>,
    pub consecutive_errors: i32,
    pub needs_reinit: bool,
    /// Per-ST adaptive fallback threshold. None means use global GUC.
    pub auto_threshold: Option<f64>,
    /// Last observed FULL refresh execution time in milliseconds.
    pub last_full_ms: Option<f64>,
    /// Serialized frontier (JSONB). None means never refreshed.
    pub frontier: Option<Frontier>,
}

/// A dependency edge from a stream table to one of its upstream sources.
#[derive(Debug, Clone)]
pub struct DtDependency {
    pub pgs_id: i64,
    pub source_relid: pg_sys::Oid,
    pub source_type: String,
    pub columns_used: Option<Vec<String>>,
}

/// A refresh history record.
#[derive(Debug, Clone)]
pub struct RefreshRecord {
    pub refresh_id: i64,
    pub pgs_id: i64,
    pub data_timestamp: TimestampWithTimeZone,
    pub start_time: TimestampWithTimeZone,
    pub end_time: Option<TimestampWithTimeZone>,
    pub action: String,
    pub rows_inserted: i64,
    pub rows_deleted: i64,
    pub error_message: Option<String>,
    pub status: String,
    /// What triggered this refresh: SCHEDULER, MANUAL, or INITIAL.
    pub initiated_by: Option<String>,
    /// SLA deadline at the time of refresh (duration-based schedules only).
    pub freshness_deadline: Option<TimestampWithTimeZone>,
}

// ── StreamTableMeta CRUD ──────────────────────────────────────────────────

impl StreamTableMeta {
    /// Insert a new stream table record. Returns the assigned `pgs_id`.
    pub fn insert(
        pgs_relid: pg_sys::Oid,
        pgs_name: &str,
        pgs_schema: &str,
        defining_query: &str,
        schedule: Option<String>,
        refresh_mode: RefreshMode,
    ) -> Result<i64, PgStreamError> {
        Spi::connect_mut(|client| {
            let row = client
                .update(
                    "INSERT INTO pgstream.pgs_stream_tables \
                     (pgs_relid, pgs_name, pgs_schema, defining_query, schedule, refresh_mode) \
                     VALUES ($1, $2, $3, $4, $5, $6) \
                     RETURNING pgs_id",
                    None,
                    &[
                        pgs_relid.into(),
                        pgs_name.into(),
                        pgs_schema.into(),
                        defining_query.into(),
                        schedule.into(),
                        refresh_mode.as_str().into(),
                    ],
                )
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?
                .first();

            row.get_one::<i64>()
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?
                .ok_or_else(|| PgStreamError::InternalError("INSERT did not return pgs_id".into()))
        })
    }

    /// Look up a stream table by schema-qualified name.
    pub fn get_by_name(schema: &str, name: &str) -> Result<Self, PgStreamError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgs_id, pgs_relid, pgs_name, pgs_schema, defining_query, \
                     schedule, refresh_mode, status, is_populated, data_timestamp, \
                     consecutive_errors, needs_reinit, frontier, auto_threshold, last_full_ms \
                     FROM pgstream.pgs_stream_tables \
                     WHERE pgs_schema = $1 AND pgs_name = $2",
                    None,
                    &[schema.into(), name.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?;

            if table.is_empty() {
                return Err(PgStreamError::NotFound(format!("{}.{}", schema, name)));
            }

            Self::from_spi_table(&table.first())
        })
    }

    /// Look up a stream table by its storage table OID.
    pub fn get_by_relid(relid: pg_sys::Oid) -> Result<Self, PgStreamError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgs_id, pgs_relid, pgs_name, pgs_schema, defining_query, \
                     schedule, refresh_mode, status, is_populated, data_timestamp, \
                     consecutive_errors, needs_reinit, frontier, auto_threshold, last_full_ms \
                     FROM pgstream.pgs_stream_tables \
                     WHERE pgs_relid = $1",
                    None,
                    &[relid.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?;

            if table.is_empty() {
                return Err(PgStreamError::NotFound(format!("relid={}", relid.to_u32())));
            }

            Self::from_spi_table(&table.first())
        })
    }

    /// Get all active stream tables.
    pub fn get_all_active() -> Result<Vec<Self>, PgStreamError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgs_id, pgs_relid, pgs_name, pgs_schema, defining_query, \
                     schedule, refresh_mode, status, is_populated, data_timestamp, \
                     consecutive_errors, needs_reinit, frontier, auto_threshold, last_full_ms \
                     FROM pgstream.pgs_stream_tables \
                     WHERE status = 'ACTIVE'",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?;

            let mut result = Vec::new();
            for row in table {
                match Self::from_spi_heap_tuple(&row) {
                    Ok(meta) => result.push(meta),
                    Err(e) => {
                        pgrx::warning!("Skipping corrupted ST catalog row: {}", e);
                    }
                }
            }
            Ok(result)
        })
    }

    /// Update the status of a stream table.
    pub fn update_status(pgs_id: i64, status: DtStatus) -> Result<(), PgStreamError> {
        Spi::run_with_args(
            "UPDATE pgstream.pgs_stream_tables \
             SET status = $1, updated_at = now() \
             WHERE pgs_id = $2",
            &[status.as_str().into(), pgs_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }

    /// Mark a ST as populated with a data timestamp after refresh.
    pub fn update_after_refresh(
        pgs_id: i64,
        data_ts: TimestampWithTimeZone,
        _rows_affected: i64,
    ) -> Result<(), PgStreamError> {
        Spi::run_with_args(
            "UPDATE pgstream.pgs_stream_tables \
             SET data_timestamp = $1, is_populated = true, \
             last_refresh_at = now(), consecutive_errors = 0, \
             status = 'ACTIVE', needs_reinit = false, updated_at = now() \
             WHERE pgs_id = $2",
            &[data_ts.into(), pgs_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }

    /// Mark a ST as populated with a data timestamp and store frontier after refresh.
    pub fn update_after_refresh_with_frontier(
        pgs_id: i64,
        data_ts: TimestampWithTimeZone,
        _rows_affected: i64,
        frontier: &Frontier,
    ) -> Result<(), PgStreamError> {
        let frontier_json = serde_json::to_value(frontier).map_err(|e| {
            PgStreamError::InternalError(format!("Failed to serialize frontier: {}", e))
        })?;

        Spi::run_with_args(
            "UPDATE pgstream.pgs_stream_tables \
             SET data_timestamp = $1, is_populated = true, \
             last_refresh_at = now(), consecutive_errors = 0, \
             status = 'ACTIVE', needs_reinit = false, \
             frontier = $3, updated_at = now() \
             WHERE pgs_id = $2",
            &[
                data_ts.into(),
                pgs_id.into(),
                pgrx::JsonB(frontier_json).into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }

    /// Store frontier + mark refresh complete in a single SPI call (S3 optimization).
    ///
    /// Combines `store_frontier()` + `SELECT now()` + `update_after_refresh()`
    /// into one UPDATE ... RETURNING, saving 2 SPI round-trips.
    pub fn store_frontier_and_complete_refresh(
        pgs_id: i64,
        frontier: &Frontier,
        rows_affected: i64,
    ) -> Result<TimestampWithTimeZone, PgStreamError> {
        let frontier_json = serde_json::to_value(frontier).map_err(|e| {
            PgStreamError::InternalError(format!("Failed to serialize frontier: {}", e))
        })?;

        Spi::get_one_with_args::<TimestampWithTimeZone>(
            "UPDATE pgstream.pgs_stream_tables \
             SET data_timestamp = now(), is_populated = true, \
             last_refresh_at = now(), consecutive_errors = 0, \
             status = 'ACTIVE', needs_reinit = false, \
             frontier = $3, updated_at = now() \
             WHERE pgs_id = $1 \
             RETURNING data_timestamp",
            &[
                pgs_id.into(),
                rows_affected.into(),
                pgrx::JsonB(frontier_json).into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?
        .ok_or_else(|| PgStreamError::NotFound(format!("pgs_id={}", pgs_id)))
    }

    /// Store a frontier for a stream table.
    pub fn store_frontier(pgs_id: i64, frontier: &Frontier) -> Result<(), PgStreamError> {
        let frontier_json = serde_json::to_value(frontier).map_err(|e| {
            PgStreamError::InternalError(format!("Failed to serialize frontier: {}", e))
        })?;

        Spi::run_with_args(
            "UPDATE pgstream.pgs_stream_tables \
             SET frontier = $1, updated_at = now() \
             WHERE pgs_id = $2",
            &[pgrx::JsonB(frontier_json).into(), pgs_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }

    /// Load the frontier for a stream table. Returns None if not yet set.
    pub fn get_frontier(pgs_id: i64) -> Result<Option<Frontier>, PgStreamError> {
        let json_opt = Spi::get_one_with_args::<pgrx::JsonB>(
            "SELECT frontier FROM pgstream.pgs_stream_tables WHERE pgs_id = $1",
            &[pgs_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?;

        match json_opt {
            Some(jsonb) => {
                let frontier: Frontier = serde_json::from_value(jsonb.0).map_err(|e| {
                    PgStreamError::InternalError(format!("Failed to deserialize frontier: {}", e))
                })?;
                Ok(Some(frontier))
            }
            None => Ok(None),
        }
    }

    /// Increment the consecutive error count. Returns the new count.
    pub fn increment_errors(pgs_id: i64) -> Result<i32, PgStreamError> {
        Spi::get_one_with_args::<i32>(
            "UPDATE pgstream.pgs_stream_tables \
             SET consecutive_errors = consecutive_errors + 1, updated_at = now() \
             WHERE pgs_id = $1 \
             RETURNING consecutive_errors",
            &[pgs_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?
        .ok_or_else(|| PgStreamError::NotFound(format!("pgs_id={}", pgs_id)))
    }

    /// Delete a stream table record from the catalog.
    pub fn delete(pgs_id: i64) -> Result<(), PgStreamError> {
        Spi::run_with_args(
            "DELETE FROM pgstream.pgs_stream_tables WHERE pgs_id = $1",
            &[pgs_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }

    /// Mark a ST for reinitialization (e.g., due to upstream DDL change).
    pub fn mark_for_reinitialize(pgs_id: i64) -> Result<(), PgStreamError> {
        Spi::run_with_args(
            "UPDATE pgstream.pgs_stream_tables \
             SET needs_reinit = true, updated_at = now() \
             WHERE pgs_id = $1",
            &[pgs_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }

    /// Update the per-ST adaptive fallback threshold and last FULL refresh time.
    ///
    /// Called after each differential or adaptive-fallback refresh to track
    /// performance and auto-tune the change ratio threshold.
    ///
    /// `auto_threshold` — the new threshold (0.0–1.0), or None to reset to GUC default.
    /// `last_full_ms` — the last observed FULL refresh execution time, or None to keep existing.
    pub fn update_adaptive_threshold(
        pgs_id: i64,
        auto_threshold: Option<f64>,
        last_full_ms: Option<f64>,
    ) -> Result<(), PgStreamError> {
        Spi::run_with_args(
            "UPDATE pgstream.pgs_stream_tables \
             SET auto_threshold = $1, \
                 last_full_ms = COALESCE($2, last_full_ms), \
                 updated_at = now() \
             WHERE pgs_id = $3",
            &[auto_threshold.into(), last_full_ms.into(), pgs_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }

    // ── Private helpers ────────────────────────────────────────────────

    /// Extract a StreamTableMeta from a positioned SpiTupleTable (after first()).
    fn from_spi_table(table: &SpiTupleTable<'_>) -> Result<Self, PgStreamError> {
        let map_spi = |e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string());

        let pgs_id = table
            .get::<i64>(1)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("pgs_id is NULL".into()))?;

        let pgs_relid = table
            .get::<pg_sys::Oid>(2)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("pgs_relid is NULL".into()))?;

        let pgs_name = table
            .get::<String>(3)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("pgs_name is NULL".into()))?;

        let pgs_schema = table
            .get::<String>(4)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("pgs_schema is NULL".into()))?;

        let defining_query = table
            .get::<String>(5)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("defining_query is NULL".into()))?;

        let schedule = table.get::<String>(6).map_err(map_spi)?;

        let refresh_mode_str = table
            .get::<String>(7)
            .map_err(map_spi)?
            .unwrap_or_else(|| "DIFFERENTIAL".into());
        let refresh_mode = RefreshMode::from_str(&refresh_mode_str)?;

        let status_str = table
            .get::<String>(8)
            .map_err(map_spi)?
            .unwrap_or_else(|| "INITIALIZING".into());
        let status = DtStatus::from_str(&status_str)?;

        let is_populated = table.get::<bool>(9).map_err(map_spi)?.unwrap_or(false);

        let data_timestamp = table.get::<TimestampWithTimeZone>(10).map_err(map_spi)?;

        let consecutive_errors = table.get::<i32>(11).map_err(map_spi)?.unwrap_or(0);

        let needs_reinit = table.get::<bool>(12).map_err(map_spi)?.unwrap_or(false);

        let frontier_json = table.get::<pgrx::JsonB>(13).map_err(map_spi)?;
        let frontier = frontier_json.and_then(|j| serde_json::from_value(j.0).ok());

        let auto_threshold = table.get::<f64>(14).map_err(map_spi)?;
        let last_full_ms = table.get::<f64>(15).map_err(map_spi)?;

        Ok(StreamTableMeta {
            pgs_id,
            pgs_relid,
            pgs_name,
            pgs_schema,
            defining_query,
            schedule,
            refresh_mode,
            status,
            is_populated,
            data_timestamp,
            consecutive_errors,
            needs_reinit,
            auto_threshold,
            last_full_ms,
            frontier,
        })
    }

    /// Extract a StreamTableMeta from an SpiHeapTupleData (from iteration).
    fn from_spi_heap_tuple(row: &SpiHeapTupleData<'_>) -> Result<Self, PgStreamError> {
        let map_spi = |e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string());

        let pgs_id = row
            .get::<i64>(1)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("pgs_id is NULL".into()))?;

        let pgs_relid = row
            .get::<pg_sys::Oid>(2)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("pgs_relid is NULL".into()))?;

        let pgs_name = row
            .get::<String>(3)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("pgs_name is NULL".into()))?;

        let pgs_schema = row
            .get::<String>(4)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("pgs_schema is NULL".into()))?;

        let defining_query = row
            .get::<String>(5)
            .map_err(map_spi)?
            .ok_or_else(|| PgStreamError::InternalError("defining_query is NULL".into()))?;

        let schedule = row.get::<String>(6).map_err(map_spi)?;

        let refresh_mode_str = row
            .get::<String>(7)
            .map_err(map_spi)?
            .unwrap_or_else(|| "DIFFERENTIAL".into());
        let refresh_mode = RefreshMode::from_str(&refresh_mode_str)?;

        let status_str = row
            .get::<String>(8)
            .map_err(map_spi)?
            .unwrap_or_else(|| "INITIALIZING".into());
        let status = DtStatus::from_str(&status_str)?;

        let is_populated = row.get::<bool>(9).map_err(map_spi)?.unwrap_or(false);

        let data_timestamp = row.get::<TimestampWithTimeZone>(10).map_err(map_spi)?;

        let consecutive_errors = row.get::<i32>(11).map_err(map_spi)?.unwrap_or(0);

        let needs_reinit = row.get::<bool>(12).map_err(map_spi)?.unwrap_or(false);

        let frontier_json = row.get::<pgrx::JsonB>(13).map_err(map_spi)?;
        let frontier = frontier_json.and_then(|j| serde_json::from_value(j.0).ok());

        let auto_threshold = row.get::<f64>(14).map_err(map_spi)?;
        let last_full_ms = row.get::<f64>(15).map_err(map_spi)?;

        Ok(StreamTableMeta {
            pgs_id,
            pgs_relid,
            pgs_name,
            pgs_schema,
            defining_query,
            schedule,
            refresh_mode,
            status,
            is_populated,
            data_timestamp,
            consecutive_errors,
            needs_reinit,
            auto_threshold,
            last_full_ms,
            frontier,
        })
    }
}

// ── Dependency CRUD ────────────────────────────────────────────────────────

impl DtDependency {
    /// Insert a dependency edge.
    pub fn insert(
        pgs_id: i64,
        source_relid: pg_sys::Oid,
        source_type: &str,
    ) -> Result<(), PgStreamError> {
        Spi::run_with_args(
            "INSERT INTO pgstream.pgs_dependencies (pgs_id, source_relid, source_type) \
             VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING",
            &[pgs_id.into(), source_relid.into(), source_type.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }

    /// Get all dependencies for a stream table.
    pub fn get_for_dt(pgs_id: i64) -> Result<Vec<Self>, PgStreamError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgs_id, source_relid, source_type, columns_used \
                     FROM pgstream.pgs_dependencies WHERE pgs_id = $1",
                    None,
                    &[pgs_id.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?;

            let mut result = Vec::new();
            for row in table {
                let map_spi = |e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string());
                let pgs_id = row.get::<i64>(1).map_err(map_spi)?.unwrap_or(0);
                let source_relid = row
                    .get::<pg_sys::Oid>(2)
                    .map_err(map_spi)?
                    .unwrap_or(pg_sys::InvalidOid);
                let source_type = row.get::<String>(3).map_err(map_spi)?.unwrap_or_default();
                result.push(DtDependency {
                    pgs_id,
                    source_relid,
                    source_type,
                    columns_used: None,
                });
            }
            Ok(result)
        })
    }

    /// Get all dependencies across all STs (for building the full DAG).
    pub fn get_all() -> Result<Vec<Self>, PgStreamError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgs_id, source_relid, source_type, columns_used \
                     FROM pgstream.pgs_dependencies",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?;

            let mut result = Vec::new();
            for row in table {
                let map_spi = |e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string());
                let pgs_id = row.get::<i64>(1).map_err(map_spi)?.unwrap_or(0);
                let source_relid = row
                    .get::<pg_sys::Oid>(2)
                    .map_err(map_spi)?
                    .unwrap_or(pg_sys::InvalidOid);
                let source_type = row.get::<String>(3).map_err(map_spi)?.unwrap_or_default();
                result.push(DtDependency {
                    pgs_id,
                    source_relid,
                    source_type,
                    columns_used: None,
                });
            }
            Ok(result)
        })
    }
}

// ── Refresh history CRUD ───────────────────────────────────────────────────

impl RefreshRecord {
    /// Insert a new refresh history record. Returns the `refresh_id`.
    ///
    /// `initiated_by` indicates what triggered the refresh:
    /// - `"SCHEDULER"` — background scheduler
    /// - `"MANUAL"` — user-invoked `pgstream.refresh_stream_table()`
    /// - `"INITIAL"` — first refresh after `create_stream_table()`
    ///
    /// `freshness_deadline` is the SLA deadline for duration-based schedules
    /// (NULL for cron-based schedules).
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        pgs_id: i64,
        data_timestamp: TimestampWithTimeZone,
        action: &str,
        status: &str,
        rows_inserted: i64,
        rows_deleted: i64,
        error_message: Option<&str>,
        initiated_by: Option<&str>,
        freshness_deadline: Option<TimestampWithTimeZone>,
    ) -> Result<i64, PgStreamError> {
        Spi::get_one_with_args::<i64>(
            "INSERT INTO pgstream.pgs_refresh_history \
             (pgs_id, data_timestamp, start_time, action, status, \
              rows_inserted, rows_deleted, error_message, \
              initiated_by, freshness_deadline) \
             VALUES ($1, $2, now(), $3, $4, $5, $6, $7, $8, $9) \
             RETURNING refresh_id",
            &[
                pgs_id.into(),
                data_timestamp.into(),
                action.into(),
                status.into(),
                rows_inserted.into(),
                rows_deleted.into(),
                error_message.into(),
                initiated_by.into(),
                freshness_deadline.into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?
        .ok_or_else(|| PgStreamError::InternalError("INSERT did not return refresh_id".into()))
    }

    /// Complete a refresh record (set end_time and final status).
    pub fn complete(
        refresh_id: i64,
        status: &str,
        rows_inserted: i64,
        rows_deleted: i64,
        error_message: Option<&str>,
    ) -> Result<(), PgStreamError> {
        Spi::run_with_args(
            "UPDATE pgstream.pgs_refresh_history \
             SET end_time = now(), status = $1, rows_inserted = $2, \
             rows_deleted = $3, error_message = $4 \
             WHERE refresh_id = $5",
            &[
                status.into(),
                rows_inserted.into(),
                rows_deleted.into(),
                error_message.into(),
                refresh_id.into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))
    }
}
