//! Catalog layer — metadata tables and CRUD operations for stream tables.
//!
//! All catalog access goes through PostgreSQL's SPI interface. This module
//! provides typed Rust abstractions over the `pgtrickle.pgt_stream_tables`,
//! `pgtrickle.pgt_dependencies`, and `pgtrickle.pgt_refresh_history` tables.

use pgrx::prelude::*;
use pgrx::spi::{SpiHeapTupleData, SpiTupleTable};

use crate::dag::{RefreshMode, StStatus};
use crate::error::PgTrickleError;
use crate::version::Frontier;

/// Metadata for a stream table, mirrors `pgtrickle.pgt_stream_tables`.
#[derive(Debug, Clone)]
pub struct StreamTableMeta {
    pub pgt_id: i64,
    pub pgt_relid: pg_sys::Oid,
    pub pgt_name: String,
    pub pgt_schema: String,
    pub defining_query: String,
    pub original_query: Option<String>,
    pub schedule: Option<String>,
    pub refresh_mode: RefreshMode,
    pub status: StStatus,
    pub is_populated: bool,
    pub data_timestamp: Option<TimestampWithTimeZone>,
    pub consecutive_errors: i32,
    pub needs_reinit: bool,
    /// Per-ST adaptive fallback threshold. None means use global GUC.
    pub auto_threshold: Option<f64>,
    /// Last observed FULL refresh execution time in milliseconds.
    pub last_full_ms: Option<f64>,
    /// Function/operator names referenced in the defining query (G8.2).
    /// Used by DDL hooks to detect `CREATE OR REPLACE FUNCTION` / `DROP FUNCTION`
    /// that may change the semantics of this stream table.
    pub functions_used: Option<Vec<String>>,
    /// Serialized frontier (JSONB). None means never refreshed.
    pub frontier: Option<Frontier>,
}

/// CDC mode for a source dependency — tracks whether change capture uses
/// row-level triggers, WAL-based logical replication, or is transitioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdcMode {
    /// Row-level AFTER trigger writes to buffer table (default).
    Trigger,
    /// Both trigger and WAL decoder are active; decoder is catching up.
    Transitioning,
    /// Only the WAL decoder populates the buffer table; trigger dropped.
    Wal,
}

impl CdcMode {
    /// Serialize to the SQL CHECK constraint value.
    pub fn as_str(&self) -> &'static str {
        match self {
            CdcMode::Trigger => "TRIGGER",
            CdcMode::Transitioning => "TRANSITIONING",
            CdcMode::Wal => "WAL",
        }
    }

    /// Deserialize from SQL string. Falls back to `Trigger` for unknown values.
    pub fn from_str(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "TRIGGER" => CdcMode::Trigger,
            "TRANSITIONING" => CdcMode::Transitioning,
            "WAL" => CdcMode::Wal,
            _ => CdcMode::Trigger,
        }
    }
}

impl std::fmt::Display for CdcMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A dependency edge from a stream table to one of its upstream sources.
#[derive(Debug, Clone)]
pub struct StDependency {
    pub pgt_id: i64,
    pub source_relid: pg_sys::Oid,
    pub source_type: String,
    pub columns_used: Option<Vec<String>>,
    /// JSONB snapshot of source columns at creation time.
    /// Array of `{"name":"col","type_oid":23,"ordinal":1}` objects.
    /// Stored as `serde_json::Value` since `pgrx::JsonB` does not implement `Clone`.
    pub column_snapshot: Option<serde_json::Value>,
    /// SHA-256 fingerprint of the serialized column snapshot for fast equality checks.
    pub schema_fingerprint: Option<String>,
    /// Current CDC mechanism for this source.
    pub cdc_mode: CdcMode,
    /// Name of the replication slot (NULL when using triggers).
    pub slot_name: Option<String>,
    /// Last LSN confirmed by the WAL decoder.
    pub decoder_confirmed_lsn: Option<String>,
    /// When the transition from triggers to WAL started (for timeout detection).
    pub transition_started_at: Option<String>,
}

/// A refresh history record.
#[derive(Debug, Clone)]
pub struct RefreshRecord {
    pub refresh_id: i64,
    pub pgt_id: i64,
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
    /// Insert a new stream table record. Returns the assigned `pgt_id`.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        pgt_relid: pg_sys::Oid,
        pgt_name: &str,
        pgt_schema: &str,
        defining_query: &str,
        original_query: Option<&str>,
        schedule: Option<String>,
        refresh_mode: RefreshMode,
        functions_used: Option<Vec<String>>,
    ) -> Result<i64, PgTrickleError> {
        Spi::connect_mut(|client| {
            let row = client
                .update(
                    "INSERT INTO pgtrickle.pgt_stream_tables \
                     (pgt_relid, pgt_name, pgt_schema, defining_query, original_query, schedule, refresh_mode, functions_used) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                     RETURNING pgt_id",
                    None,
                    &[
                        pgt_relid.into(),
                        pgt_name.into(),
                        pgt_schema.into(),
                        defining_query.into(),
                        original_query.into(),
                        schedule.into(),
                        refresh_mode.as_str().into(),
                        functions_used.into(),
                    ],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
                .first();

            row.get_one::<i64>()
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
                .ok_or_else(|| PgTrickleError::InternalError("INSERT did not return pgt_id".into()))
        })
    }

    /// Look up a stream table by schema-qualified name.
    pub fn get_by_name(schema: &str, name: &str) -> Result<Self, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgt_id, pgt_relid, pgt_name, pgt_schema, defining_query, \
                     original_query, schedule, refresh_mode, status, is_populated, \
                     data_timestamp, consecutive_errors, needs_reinit, frontier, \
                     auto_threshold, last_full_ms, functions_used \
                     FROM pgtrickle.pgt_stream_tables \
                     WHERE pgt_schema = $1 AND pgt_name = $2",
                    None,
                    &[schema.into(), name.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            if table.is_empty() {
                return Err(PgTrickleError::NotFound(format!("{}.{}", schema, name)));
            }

            Self::from_spi_table(&table.first())
        })
    }

    /// Look up a stream table by its storage table OID.
    pub fn get_by_relid(relid: pg_sys::Oid) -> Result<Self, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgt_id, pgt_relid, pgt_name, pgt_schema, defining_query, \
                     original_query, schedule, refresh_mode, status, is_populated, \
                     data_timestamp, consecutive_errors, needs_reinit, frontier, \
                     auto_threshold, last_full_ms, functions_used \
                     FROM pgtrickle.pgt_stream_tables \
                     WHERE pgt_relid = $1",
                    None,
                    &[relid.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            if table.is_empty() {
                return Err(PgTrickleError::NotFound(format!(
                    "relid={}",
                    relid.to_u32()
                )));
            }

            Self::from_spi_table(&table.first())
        })
    }

    /// Get all active stream tables.
    pub fn get_all_active() -> Result<Vec<Self>, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgt_id, pgt_relid, pgt_name, pgt_schema, defining_query, \
                     original_query, schedule, refresh_mode, status, is_populated, \
                     data_timestamp, consecutive_errors, needs_reinit, frontier, \
                     auto_threshold, last_full_ms, functions_used \
                     FROM pgtrickle.pgt_stream_tables \
                     WHERE status = 'ACTIVE'",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

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

    /// Find pgt_ids of stream tables whose `functions_used` array contains
    /// the given function name (case-insensitive match via `@>`).
    /// Used by DDL hooks to detect which STs are affected when a function
    /// is CREATEd OR REPLACEd / ALTERed / DROPped.
    pub fn find_by_function_name(func_name: &str) -> Result<Vec<i64>, PgTrickleError> {
        let lower = func_name.to_lowercase();
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgt_id FROM pgtrickle.pgt_stream_tables \
                     WHERE functions_used @> ARRAY[$1]::text[]",
                    None,
                    &[lower.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            let mut ids = Vec::new();
            for row in table {
                if let Ok(Some(id)) = row.get::<i64>(1) {
                    ids.push(id);
                }
            }
            Ok(ids)
        })
    }

    /// Update the status of a stream table.
    pub fn update_status(pgt_id: i64, status: StStatus) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET status = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[status.as_str().into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Mark a ST as populated with a data timestamp after refresh.
    pub fn update_after_refresh(
        pgt_id: i64,
        data_ts: TimestampWithTimeZone,
        _rows_affected: i64,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET data_timestamp = $1, is_populated = true, \
             last_refresh_at = now(), consecutive_errors = 0, \
             status = 'ACTIVE', needs_reinit = false, updated_at = now() \
             WHERE pgt_id = $2",
            &[data_ts.into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Mark a ST as populated with a data timestamp and store frontier after refresh.
    pub fn update_after_refresh_with_frontier(
        pgt_id: i64,
        data_ts: TimestampWithTimeZone,
        _rows_affected: i64,
        frontier: &Frontier,
    ) -> Result<(), PgTrickleError> {
        let frontier_json = serde_json::to_value(frontier).map_err(|e| {
            PgTrickleError::InternalError(format!("Failed to serialize frontier: {}", e))
        })?;

        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET data_timestamp = $1, is_populated = true, \
             last_refresh_at = now(), consecutive_errors = 0, \
             status = 'ACTIVE', needs_reinit = false, \
             frontier = $3, updated_at = now() \
             WHERE pgt_id = $2",
            &[
                data_ts.into(),
                pgt_id.into(),
                pgrx::JsonB(frontier_json).into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Store frontier + mark refresh complete in a single SPI call (S3 optimization).
    ///
    /// Combines `store_frontier()` + `SELECT now()` + `update_after_refresh()`
    /// into one UPDATE ... RETURNING, saving 2 SPI round-trips.
    pub fn store_frontier_and_complete_refresh(
        pgt_id: i64,
        frontier: &Frontier,
        rows_affected: i64,
    ) -> Result<TimestampWithTimeZone, PgTrickleError> {
        let frontier_json = serde_json::to_value(frontier).map_err(|e| {
            PgTrickleError::InternalError(format!("Failed to serialize frontier: {}", e))
        })?;

        Spi::get_one_with_args::<TimestampWithTimeZone>(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET data_timestamp = now(), is_populated = true, \
             last_refresh_at = now(), consecutive_errors = 0, \
             status = 'ACTIVE', needs_reinit = false, \
             frontier = $3, updated_at = now() \
             WHERE pgt_id = $1 \
             RETURNING data_timestamp",
            &[
                pgt_id.into(),
                rows_affected.into(),
                pgrx::JsonB(frontier_json).into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
        .ok_or_else(|| PgTrickleError::NotFound(format!("pgt_id={}", pgt_id)))
    }

    /// Store a frontier for a stream table.
    pub fn store_frontier(pgt_id: i64, frontier: &Frontier) -> Result<(), PgTrickleError> {
        let frontier_json = serde_json::to_value(frontier).map_err(|e| {
            PgTrickleError::InternalError(format!("Failed to serialize frontier: {}", e))
        })?;

        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET frontier = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[pgrx::JsonB(frontier_json).into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Load the frontier for a stream table. Returns None if not yet set.
    pub fn get_frontier(pgt_id: i64) -> Result<Option<Frontier>, PgTrickleError> {
        let json_opt = Spi::get_one_with_args::<pgrx::JsonB>(
            "SELECT frontier FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
            &[pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

        match json_opt {
            Some(jsonb) => {
                let frontier: Frontier = serde_json::from_value(jsonb.0).map_err(|e| {
                    PgTrickleError::InternalError(format!("Failed to deserialize frontier: {}", e))
                })?;
                Ok(Some(frontier))
            }
            None => Ok(None),
        }
    }

    /// Increment the consecutive error count. Returns the new count.
    pub fn increment_errors(pgt_id: i64) -> Result<i32, PgTrickleError> {
        Spi::get_one_with_args::<i32>(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET consecutive_errors = consecutive_errors + 1, updated_at = now() \
             WHERE pgt_id = $1 \
             RETURNING consecutive_errors",
            &[pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
        .ok_or_else(|| PgTrickleError::NotFound(format!("pgt_id={}", pgt_id)))
    }

    /// Delete a stream table record from the catalog.
    pub fn delete(pgt_id: i64) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "DELETE FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
            &[pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Mark a ST for reinitialization (e.g., due to upstream DDL change).
    pub fn mark_for_reinitialize(pgt_id: i64) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET needs_reinit = true, updated_at = now() \
             WHERE pgt_id = $1",
            &[pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Update the per-ST adaptive fallback threshold and last FULL refresh time.
    ///
    /// Called after each differential or adaptive-fallback refresh to track
    /// performance and auto-tune the change ratio threshold.
    ///
    /// `auto_threshold` — the new threshold (0.0–1.0), or None to reset to GUC default.
    /// `last_full_ms` — the last observed FULL refresh execution time, or None to keep existing.
    pub fn update_adaptive_threshold(
        pgt_id: i64,
        auto_threshold: Option<f64>,
        last_full_ms: Option<f64>,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET auto_threshold = $1, \
                 last_full_ms = COALESCE($2, last_full_ms), \
                 updated_at = now() \
             WHERE pgt_id = $3",
            &[auto_threshold.into(), last_full_ms.into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    // ── Private helpers ────────────────────────────────────────────────

    /// Extract a StreamTableMeta from a positioned SpiTupleTable (after first()).
    fn from_spi_table(table: &SpiTupleTable<'_>) -> Result<Self, PgTrickleError> {
        let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());

        let pgt_id = table
            .get::<i64>(1)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("pgt_id is NULL".into()))?;

        let pgt_relid = table
            .get::<pg_sys::Oid>(2)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("pgt_relid is NULL".into()))?;

        let pgt_name = table
            .get::<String>(3)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("pgt_name is NULL".into()))?;

        let pgt_schema = table
            .get::<String>(4)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("pgt_schema is NULL".into()))?;

        let defining_query = table
            .get::<String>(5)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("defining_query is NULL".into()))?;

        let original_query = table.get::<String>(6).map_err(map_spi)?;

        let schedule = table.get::<String>(7).map_err(map_spi)?;

        let refresh_mode_str = table
            .get::<String>(8)
            .map_err(map_spi)?
            .unwrap_or_else(|| "DIFFERENTIAL".into());
        let refresh_mode = RefreshMode::from_str(&refresh_mode_str)?;

        let status_str = table
            .get::<String>(9)
            .map_err(map_spi)?
            .unwrap_or_else(|| "INITIALIZING".into());
        let status = StStatus::from_str(&status_str)?;

        let is_populated = table.get::<bool>(10).map_err(map_spi)?.unwrap_or(false);

        let data_timestamp = table.get::<TimestampWithTimeZone>(11).map_err(map_spi)?;

        let consecutive_errors = table.get::<i32>(12).map_err(map_spi)?.unwrap_or(0);

        let needs_reinit = table.get::<bool>(13).map_err(map_spi)?.unwrap_or(false);

        let frontier_json = table.get::<pgrx::JsonB>(14).map_err(map_spi)?;
        let frontier = frontier_json.and_then(|j| serde_json::from_value(j.0).ok());

        let auto_threshold = table.get::<f64>(15).map_err(map_spi)?;
        let last_full_ms = table.get::<f64>(16).map_err(map_spi)?;
        let functions_used = table.get::<Vec<String>>(17).map_err(map_spi)?;

        Ok(StreamTableMeta {
            pgt_id,
            pgt_relid,
            pgt_name,
            pgt_schema,
            defining_query,
            original_query,
            schedule,
            refresh_mode,
            status,
            is_populated,
            data_timestamp,
            consecutive_errors,
            needs_reinit,
            auto_threshold,
            last_full_ms,
            functions_used,
            frontier,
        })
    }

    /// Extract a StreamTableMeta from an SpiHeapTupleData (from iteration).
    fn from_spi_heap_tuple(row: &SpiHeapTupleData<'_>) -> Result<Self, PgTrickleError> {
        let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());

        let pgt_id = row
            .get::<i64>(1)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("pgt_id is NULL".into()))?;

        let pgt_relid = row
            .get::<pg_sys::Oid>(2)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("pgt_relid is NULL".into()))?;

        let pgt_name = row
            .get::<String>(3)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("pgt_name is NULL".into()))?;

        let pgt_schema = row
            .get::<String>(4)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("pgt_schema is NULL".into()))?;

        let defining_query = row
            .get::<String>(5)
            .map_err(map_spi)?
            .ok_or_else(|| PgTrickleError::InternalError("defining_query is NULL".into()))?;

        let original_query = row.get::<String>(6).map_err(map_spi)?;

        let schedule = row.get::<String>(7).map_err(map_spi)?;

        let refresh_mode_str = row
            .get::<String>(8)
            .map_err(map_spi)?
            .unwrap_or_else(|| "DIFFERENTIAL".into());
        let refresh_mode = RefreshMode::from_str(&refresh_mode_str)?;

        let status_str = row
            .get::<String>(9)
            .map_err(map_spi)?
            .unwrap_or_else(|| "INITIALIZING".into());
        let status = StStatus::from_str(&status_str)?;

        let is_populated = row.get::<bool>(10).map_err(map_spi)?.unwrap_or(false);

        let data_timestamp = row.get::<TimestampWithTimeZone>(11).map_err(map_spi)?;

        let consecutive_errors = row.get::<i32>(12).map_err(map_spi)?.unwrap_or(0);

        let needs_reinit = row.get::<bool>(13).map_err(map_spi)?.unwrap_or(false);

        let frontier_json = row.get::<pgrx::JsonB>(14).map_err(map_spi)?;
        let frontier = frontier_json.and_then(|j| serde_json::from_value(j.0).ok());

        let auto_threshold = row.get::<f64>(15).map_err(map_spi)?;
        let last_full_ms = row.get::<f64>(16).map_err(map_spi)?;
        let functions_used = row.get::<Vec<String>>(17).map_err(map_spi)?;

        Ok(StreamTableMeta {
            pgt_id,
            pgt_relid,
            pgt_name,
            pgt_schema,
            defining_query,
            original_query,
            schedule,
            refresh_mode,
            status,
            is_populated,
            data_timestamp,
            consecutive_errors,
            needs_reinit,
            auto_threshold,
            last_full_ms,
            functions_used,
            frontier,
        })
    }
}

// ── Dependency CRUD ────────────────────────────────────────────────────────

impl StDependency {
    /// Insert a dependency edge.
    pub fn insert(
        pgt_id: i64,
        source_relid: pg_sys::Oid,
        source_type: &str,
        columns_used: Option<Vec<String>>,
    ) -> Result<(), PgTrickleError> {
        Self::insert_with_snapshot(pgt_id, source_relid, source_type, columns_used, None, None)
    }

    /// Insert a dependency edge with column snapshot and schema fingerprint.
    pub fn insert_with_snapshot(
        pgt_id: i64,
        source_relid: pg_sys::Oid,
        source_type: &str,
        columns_used: Option<Vec<String>>,
        column_snapshot: Option<pgrx::JsonB>,
        schema_fingerprint: Option<String>,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "INSERT INTO pgtrickle.pgt_dependencies \
             (pgt_id, source_relid, source_type, cdc_mode, columns_used, \
              column_snapshot, schema_fingerprint) \
             VALUES ($1, $2, $3, 'TRIGGER', $4, $5, $6) \
             ON CONFLICT DO NOTHING",
            &[
                pgt_id.into(),
                source_relid.into(),
                source_type.into(),
                columns_used.into(),
                column_snapshot.into(),
                schema_fingerprint.into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Update the CDC mode and related fields for a dependency.
    pub fn update_cdc_mode(
        pgt_id: i64,
        source_relid: pg_sys::Oid,
        cdc_mode: CdcMode,
        slot_name: Option<&str>,
        decoder_confirmed_lsn: Option<&str>,
    ) -> Result<(), PgTrickleError> {
        let transition_started = if cdc_mode == CdcMode::Transitioning {
            "now()"
        } else {
            "NULL"
        };
        Spi::run_with_args(
            &format!(
                "UPDATE pgtrickle.pgt_dependencies \
                 SET cdc_mode = $1, slot_name = $2, decoder_confirmed_lsn = $3::pg_lsn, \
                     transition_started_at = {} \
                 WHERE pgt_id = $4 AND source_relid = $5",
                transition_started
            ),
            &[
                cdc_mode.as_str().into(),
                slot_name.into(),
                decoder_confirmed_lsn.into(),
                pgt_id.into(),
                source_relid.into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Get all dependencies for a stream table.
    pub fn get_for_st(pgt_id: i64) -> Result<Vec<Self>, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgt_id, source_relid, source_type, columns_used, \
                            cdc_mode, slot_name, decoder_confirmed_lsn::text, \
                            transition_started_at::text, column_snapshot, \
                            schema_fingerprint \
                     FROM pgtrickle.pgt_dependencies WHERE pgt_id = $1",
                    None,
                    &[pgt_id.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            let mut result = Vec::new();
            for row in table {
                let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());
                let pgt_id = row.get::<i64>(1).map_err(map_spi)?.unwrap_or(0);
                let source_relid = row
                    .get::<pg_sys::Oid>(2)
                    .map_err(map_spi)?
                    .unwrap_or(pg_sys::InvalidOid);
                let source_type = row.get::<String>(3).map_err(map_spi)?.unwrap_or_default();
                let columns_used = row.get::<Vec<String>>(4).map_err(map_spi)?;
                let cdc_mode_str = row.get::<String>(5).map_err(map_spi)?.unwrap_or_default();
                let slot_name = row.get::<String>(6).map_err(map_spi)?;
                let decoder_confirmed_lsn = row.get::<String>(7).map_err(map_spi)?;
                let transition_started_at = row.get::<String>(8).map_err(map_spi)?;
                let column_snapshot = row.get::<pgrx::JsonB>(9).map_err(map_spi)?.map(|jb| jb.0);
                let schema_fingerprint = row.get::<String>(10).map_err(map_spi)?;
                result.push(StDependency {
                    pgt_id,
                    source_relid,
                    source_type,
                    columns_used,
                    column_snapshot,
                    schema_fingerprint,
                    cdc_mode: CdcMode::from_str(&cdc_mode_str),
                    slot_name,
                    decoder_confirmed_lsn,
                    transition_started_at,
                });
            }
            Ok(result)
        })
    }

    /// Get all dependencies across all STs (for building the full DAG).
    pub fn get_all() -> Result<Vec<Self>, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgt_id, source_relid, source_type, columns_used, \
                            cdc_mode, slot_name, decoder_confirmed_lsn::text, \
                            transition_started_at::text, column_snapshot, \
                            schema_fingerprint \
                     FROM pgtrickle.pgt_dependencies",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            let mut result = Vec::new();
            for row in table {
                let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());
                let pgt_id = row.get::<i64>(1).map_err(map_spi)?.unwrap_or(0);
                let source_relid = row
                    .get::<pg_sys::Oid>(2)
                    .map_err(map_spi)?
                    .unwrap_or(pg_sys::InvalidOid);
                let source_type = row.get::<String>(3).map_err(map_spi)?.unwrap_or_default();
                let columns_used = row.get::<Vec<String>>(4).map_err(map_spi)?;
                let cdc_mode_str = row.get::<String>(5).map_err(map_spi)?.unwrap_or_default();
                let slot_name = row.get::<String>(6).map_err(map_spi)?;
                let decoder_confirmed_lsn = row.get::<String>(7).map_err(map_spi)?;
                let transition_started_at = row.get::<String>(8).map_err(map_spi)?;
                let column_snapshot = row.get::<pgrx::JsonB>(9).map_err(map_spi)?.map(|jb| jb.0);
                let schema_fingerprint = row.get::<String>(10).map_err(map_spi)?;
                result.push(StDependency {
                    pgt_id,
                    source_relid,
                    source_type,
                    columns_used,
                    column_snapshot,
                    schema_fingerprint,
                    cdc_mode: CdcMode::from_str(&cdc_mode_str),
                    slot_name,
                    decoder_confirmed_lsn,
                    transition_started_at,
                });
            }
            Ok(result)
        })
    }
}

// ── Column snapshot helpers ────────────────────────────────────────────────

/// Build a JSONB column snapshot and SHA-256 fingerprint for a source table.
///
/// Queries `pg_attribute` for the current column set (name, type OID, ordinal
/// position) and returns `(snapshot_jsonb, sha256_hex)`.
///
/// F49: Generated (STORED/VIRTUAL) columns are excluded to align with
/// `resolve_source_column_defs()` which also filters `attgenerated != ''`.
/// This ensures the snapshot matches the columns tracked in the change
/// buffer table, preventing false schema-change alerts.
///
/// The snapshot is a JSON array of objects:
/// ```json
/// [{"name":"id","type_oid":23,"ordinal":1},{"name":"val","type_oid":25,"ordinal":2}]
/// ```
///
/// Used at creation time to record the source schema in `pgt_dependencies`
/// so `detect_schema_change_kind()` can compare against the current catalog.
#[cfg(not(test))]
pub fn build_column_snapshot(
    source_oid: pg_sys::Oid,
) -> Result<(pgrx::JsonB, String), PgTrickleError> {
    use sha2::{Digest, Sha256};

    let sql = format!(
        "SELECT attname::text, atttypid::int, attnum::int \
         FROM pg_attribute \
         WHERE attrelid = {} AND attnum > 0 AND NOT attisdropped \
           AND attgenerated = '' \
         ORDER BY attnum",
        source_oid.to_u32(),
    );

    let entries: Vec<serde_json::Value> = Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut out = Vec::new();
        for row in result {
            let name: String = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let type_oid: i32 = row
                .get(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(0);
            let ordinal: i32 = row
                .get(3)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(0);
            out.push(serde_json::json!({
                "name": name,
                "type_oid": type_oid,
                "ordinal": ordinal,
            }));
        }
        Ok(out)
    })?;

    let json_str = serde_json::to_string(&entries)
        .map_err(|e| PgTrickleError::InternalError(format!("JSON serialization failed: {e}")))?;

    let mut hasher = Sha256::new();
    hasher.update(json_str.as_bytes());
    let fingerprint = format!("{:x}", hasher.finalize());

    let snapshot = pgrx::JsonB(serde_json::Value::Array(entries));
    Ok((snapshot, fingerprint))
}

/// Test-only stub: SPI is unavailable in unit tests.
#[cfg(test)]
pub fn build_column_snapshot(
    _source_oid: pg_sys::Oid,
) -> Result<(pgrx::JsonB, String), PgTrickleError> {
    let empty = serde_json::Value::Array(vec![]);
    Ok((pgrx::JsonB(empty), String::new()))
}

/// Get the stored column snapshot for a dependency pair.
///
/// Returns `None` if no snapshot is stored.
pub fn get_column_snapshot(
    pgt_id: i64,
    source_oid: pg_sys::Oid,
) -> Result<Option<pgrx::JsonB>, PgTrickleError> {
    Spi::get_one_with_args::<pgrx::JsonB>(
        "SELECT column_snapshot FROM pgtrickle.pgt_dependencies \
         WHERE pgt_id = $1 AND source_relid = $2",
        &[pgt_id.into(), source_oid.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))
}

/// Get the stored schema fingerprint for a dependency pair.
pub fn get_schema_fingerprint(
    pgt_id: i64,
    source_oid: pg_sys::Oid,
) -> Result<Option<String>, PgTrickleError> {
    Spi::get_one_with_args::<String>(
        "SELECT schema_fingerprint FROM pgtrickle.pgt_dependencies \
         WHERE pgt_id = $1 AND source_relid = $2",
        &[pgt_id.into(), source_oid.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))
}

// ── Refresh history CRUD ───────────────────────────────────────────────────

impl RefreshRecord {
    /// Insert a new refresh history record. Returns the `refresh_id`.
    ///
    /// `initiated_by` indicates what triggered the refresh:
    /// - `"SCHEDULER"` — background scheduler
    /// - `"MANUAL"` — user-invoked `pgtrickle.refresh_stream_table()`
    /// - `"INITIAL"` — first refresh after `create_stream_table()`
    ///
    /// `freshness_deadline` is the SLA deadline for duration-based schedules
    /// (NULL for cron-based schedules).
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        pgt_id: i64,
        data_timestamp: TimestampWithTimeZone,
        action: &str,
        status: &str,
        rows_inserted: i64,
        rows_deleted: i64,
        error_message: Option<&str>,
        initiated_by: Option<&str>,
        freshness_deadline: Option<TimestampWithTimeZone>,
        delta_row_count: i64,
        merge_strategy_used: Option<&str>,
        was_full_fallback: bool,
    ) -> Result<i64, PgTrickleError> {
        Spi::get_one_with_args::<i64>(
            "INSERT INTO pgtrickle.pgt_refresh_history \
             (pgt_id, data_timestamp, start_time, action, status, \
              rows_inserted, rows_deleted, error_message, \
              initiated_by, freshness_deadline, \
              delta_row_count, merge_strategy_used, was_full_fallback) \
             VALUES ($1, $2, now(), $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             RETURNING refresh_id",
            &[
                pgt_id.into(),
                data_timestamp.into(),
                action.into(),
                status.into(),
                rows_inserted.into(),
                rows_deleted.into(),
                error_message.into(),
                initiated_by.into(),
                freshness_deadline.into(),
                delta_row_count.into(),
                merge_strategy_used.into(),
                was_full_fallback.into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
        .ok_or_else(|| PgTrickleError::InternalError("INSERT did not return refresh_id".into()))
    }

    /// Complete a refresh record (set end_time and final status).
    #[allow(clippy::too_many_arguments)]
    pub fn complete(
        refresh_id: i64,
        status: &str,
        rows_inserted: i64,
        rows_deleted: i64,
        error_message: Option<&str>,
        delta_row_count: i64,
        merge_strategy_used: Option<&str>,
        was_full_fallback: bool,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_refresh_history \
             SET end_time = now(), status = $1, rows_inserted = $2, \
             rows_deleted = $3, error_message = $4, \
             delta_row_count = $5, merge_strategy_used = $6, \
             was_full_fallback = $7 \
             WHERE refresh_id = $8",
            &[
                status.into(),
                rows_inserted.into(),
                rows_deleted.into(),
                error_message.into(),
                delta_row_count.into(),
                merge_strategy_used.into(),
                was_full_fallback.into(),
                refresh_id.into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CdcMode tests ──────────────────────────────────────────────────

    #[test]
    fn test_cdc_mode_as_str() {
        assert_eq!(CdcMode::Trigger.as_str(), "TRIGGER");
        assert_eq!(CdcMode::Transitioning.as_str(), "TRANSITIONING");
        assert_eq!(CdcMode::Wal.as_str(), "WAL");
    }

    #[test]
    fn test_cdc_mode_from_str_valid() {
        assert_eq!(CdcMode::from_str("TRIGGER"), CdcMode::Trigger);
        assert_eq!(CdcMode::from_str("TRANSITIONING"), CdcMode::Transitioning);
        assert_eq!(CdcMode::from_str("WAL"), CdcMode::Wal);
    }

    #[test]
    fn test_cdc_mode_from_str_case_insensitive() {
        assert_eq!(CdcMode::from_str("trigger"), CdcMode::Trigger);
        assert_eq!(CdcMode::from_str("Transitioning"), CdcMode::Transitioning);
        assert_eq!(CdcMode::from_str("wal"), CdcMode::Wal);
        assert_eq!(CdcMode::from_str("Wal"), CdcMode::Wal);
    }

    #[test]
    fn test_cdc_mode_from_str_unknown_defaults_to_trigger() {
        assert_eq!(CdcMode::from_str(""), CdcMode::Trigger);
        assert_eq!(CdcMode::from_str("unknown"), CdcMode::Trigger);
        assert_eq!(CdcMode::from_str("LOGICAL"), CdcMode::Trigger);
    }

    #[test]
    fn test_cdc_mode_display() {
        assert_eq!(format!("{}", CdcMode::Trigger), "TRIGGER");
        assert_eq!(format!("{}", CdcMode::Transitioning), "TRANSITIONING");
        assert_eq!(format!("{}", CdcMode::Wal), "WAL");
    }

    #[test]
    fn test_cdc_mode_roundtrip() {
        for mode in [CdcMode::Trigger, CdcMode::Transitioning, CdcMode::Wal] {
            assert_eq!(CdcMode::from_str(mode.as_str()), mode);
        }
    }

    #[test]
    fn test_cdc_mode_equality() {
        assert_eq!(CdcMode::Trigger, CdcMode::Trigger);
        assert_ne!(CdcMode::Trigger, CdcMode::Wal);
        assert_ne!(CdcMode::Transitioning, CdcMode::Wal);
    }

    #[test]
    fn test_cdc_mode_clone_copy() {
        let mode = CdcMode::Wal;
        let cloned = mode;
        assert_eq!(mode, cloned);
    }
}
