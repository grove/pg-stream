//! Catalog layer — metadata tables and CRUD operations for stream tables.
//!
//! All catalog access goes through PostgreSQL's SPI interface. This module
//! provides typed Rust abstractions over the `pgtrickle.pgt_stream_tables`,
//! `pgtrickle.pgt_dependencies`, and `pgtrickle.pgt_refresh_history` tables.

use pgrx::prelude::*;
use pgrx::spi::{SpiHeapTupleData, SpiTupleTable};

use crate::dag::{DiamondConsistency, DiamondSchedulePolicy, RefreshMode, StStatus};
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
    /// TopK LIMIT value. None means this is not a TopK stream table.
    pub topk_limit: Option<i32>,
    /// TopK ORDER BY clause SQL. None means this is not a TopK stream table.
    pub topk_order_by: Option<String>,
    /// TopK OFFSET value. None means no OFFSET.
    pub topk_offset: Option<i32>,
    /// Diamond consistency mode for this ST ('none' or 'atomic').
    pub diamond_consistency: DiamondConsistency,
    /// Diamond schedule policy for this convergence node ('fastest' or 'slowest').
    pub diamond_schedule_policy: DiamondSchedulePolicy,
    /// Whether any source table lacks a PRIMARY KEY (EC-06).
    /// When true, the storage table uses a non-unique index on __pgt_row_id
    /// and the apply logic uses counted DELETE instead of MERGE.
    pub has_keyless_source: bool,
    /// Serialized JSON map of function-name → SHA-256 hash for EC-16 polling.
    /// None means no hashes have been recorded yet (baseline will be taken on
    /// the next differential refresh).
    pub function_hashes: Option<String>,
    /// User-requested CDC mode override for this stream table.
    /// None means "follow the global pg_trickle.cdc_mode GUC".
    pub requested_cdc_mode: Option<String>,
    /// Whether this stream table uses the append-only INSERT fast path.
    /// When true, differential refresh uses INSERT instead of MERGE,
    /// bypassing DELETE and UPDATE handling for better throughput.
    /// Automatically reverted to false if a DELETE/UPDATE is detected.
    pub is_append_only: bool,
    /// SCC (Strongly Connected Component) identifier for circular dependencies.
    /// `None` means this stream table is not part of a cyclic SCC.
    /// When set, all members of the same cycle share the same `scc_id`.
    pub scc_id: Option<i32>,
    /// Number of fixpoint iterations in the last SCC convergence (CYC-5).
    /// `None` for non-cyclic stream tables or if never iterated.
    pub last_fixpoint_iterations: Option<i32>,
    /// PB2: When true, the refresh engine skips `PREPARE`/`EXECUTE` for this
    /// stream table and suppresses `NOTIFY` emissions, enabling compatibility
    /// with PgBouncer transaction-mode pooling.
    pub pooler_compatibility_mode: bool,
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
    /// CSS1: WAL LSN watermark captured at scheduler tick start (NULL when feature disabled).
    pub tick_watermark_lsn: Option<String>,
    /// CYC-3: Iteration of the fixed-point loop that produced this refresh.
    /// `None` for non-cyclic refreshes.
    pub fixpoint_iteration: Option<i32>,
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
        topk_limit: Option<i32>,
        topk_order_by: Option<&str>,
        topk_offset: Option<i32>,
        diamond_consistency: DiamondConsistency,
        diamond_schedule_policy: DiamondSchedulePolicy,
        has_keyless_source: bool,
        requested_cdc_mode: Option<&str>,
        is_append_only: bool,
        pooler_compatibility_mode: bool,
    ) -> Result<i64, PgTrickleError> {
        Spi::connect_mut(|client| {
            let row = client
                .update(
                    "INSERT INTO pgtrickle.pgt_stream_tables \
                     (pgt_relid, pgt_name, pgt_schema, defining_query, original_query, schedule, refresh_mode, functions_used, topk_limit, topk_order_by, topk_offset, diamond_consistency, diamond_schedule_policy, has_keyless_source, requested_cdc_mode, is_append_only, pooler_compatibility_mode) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
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
                        topk_limit.into(),
                        topk_order_by.into(),
                        topk_offset.into(),
                        diamond_consistency.as_str().into(),
                        diamond_schedule_policy.as_str().into(),
                        has_keyless_source.into(),
                        requested_cdc_mode.into(),
                        is_append_only.into(),
                        pooler_compatibility_mode.into(),
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
                     auto_threshold, last_full_ms, functions_used, topk_limit, topk_order_by, \
                     topk_offset, diamond_consistency, diamond_schedule_policy, \
                     has_keyless_source, function_hashes, requested_cdc_mode, is_append_only, \
                     scc_id, last_fixpoint_iterations, pooler_compatibility_mode \
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
                     auto_threshold, last_full_ms, functions_used, topk_limit, topk_order_by, \
                     topk_offset, diamond_consistency, diamond_schedule_policy, \
                     has_keyless_source, function_hashes, requested_cdc_mode, is_append_only, \
                     scc_id, last_fixpoint_iterations, pooler_compatibility_mode \
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

    /// Look up a stream table by its catalog `pgt_id`.
    ///
    /// Returns `Ok(Some(meta))` if found, `Ok(None)` if the row doesn't exist.
    pub fn get_by_id(pgt_id: i64) -> Result<Option<Self>, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgt_id, pgt_relid, pgt_name, pgt_schema, defining_query, \
                     original_query, schedule, refresh_mode, status, is_populated, \
                     data_timestamp, consecutive_errors, needs_reinit, frontier, \
                     auto_threshold, last_full_ms, functions_used, topk_limit, topk_order_by, \
                     topk_offset, diamond_consistency, diamond_schedule_policy, \
                     has_keyless_source, function_hashes, requested_cdc_mode, is_append_only, \
                     scc_id, last_fixpoint_iterations, pooler_compatibility_mode \
                     FROM pgtrickle.pgt_stream_tables \
                     WHERE pgt_id = $1",
                    None,
                    &[pgt_id.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            if table.is_empty() {
                return Ok(None);
            }

            Self::from_spi_table(&table.first()).map(Some)
        })
    }

    /// Get all stream tables (including paused/broken).
    pub fn get_all() -> Result<Vec<Self>, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT pgt_id, pgt_relid, pgt_name, pgt_schema, defining_query, \
                     original_query, schedule, refresh_mode, status, is_populated, \
                     data_timestamp, consecutive_errors, needs_reinit, frontier, \
                     auto_threshold, last_full_ms, functions_used, topk_limit, topk_order_by, \
                     topk_offset, diamond_consistency, diamond_schedule_policy, \
                     has_keyless_source, function_hashes, requested_cdc_mode, is_append_only, \
                     scc_id, last_fixpoint_iterations, pooler_compatibility_mode \
                     FROM pgtrickle.pgt_stream_tables",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            let mut results = Vec::new();
            for row in table {
                match Self::from_spi_heap_tuple(&row) {
                    Ok(meta) => results.push(meta),
                    Err(e) => {
                        pgrx::warning!("Skipping corrupted ST catalog row in get_all: {}", e);
                    }
                }
            }
            Ok(results)
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
                     auto_threshold, last_full_ms, functions_used, topk_limit, topk_order_by, \
                     topk_offset, diamond_consistency, diamond_schedule_policy, \
                     has_keyless_source, function_hashes, requested_cdc_mode, is_append_only, \
                     scc_id, last_fixpoint_iterations, pooler_compatibility_mode \
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

    /// Record a "no data" refresh cycle — the table was verified up-to-date but
    /// no rows were written.  Updates `last_refresh_at` so staleness calculations
    /// see the check, but intentionally preserves `data_timestamp` so that
    /// downstream stream tables (which compare `upstream.data_timestamp > us.data_timestamp`
    /// to detect when a full refresh is needed) do not see a spurious "upstream
    /// changed" signal after a pure no-data verification pass.
    pub fn update_after_no_data_refresh(pgt_id: i64) -> Result<(), PgTrickleError> {
        // NOTE: intentionally does NOT clear needs_reinit.  A no-data refresh
        // means no rows were written — it must not overwrite a needs_reinit=true
        // flag set by EC-16 function-body-change detection or DDL hooks.  The
        // flag is cleared only by update_after_refresh / store_frontier_and_complete_refresh
        // after an actual full reinitialization succeeds.
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET is_populated = true, \
             last_refresh_at = now(), consecutive_errors = 0, \
             status = 'ACTIVE', updated_at = now() \
             WHERE pgt_id = $1",
            &[pgt_id.into()],
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

    /// Persist the function source hashes for a stream table (EC-16).
    ///
    /// `hashes_json` is a JSON text string mapping `{ "func_name": "md5hex", ... }`.
    /// Pass `None` to clear (reset) stored hashes (e.g., after a full rebase).
    pub fn update_function_hashes(
        pgt_id: i64,
        hashes_json: Option<&str>,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET function_hashes = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[hashes_json.into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Update the per-stream-table requested CDC mode override.
    pub fn update_requested_cdc_mode(
        pgt_id: i64,
        requested_cdc_mode: Option<&str>,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET requested_cdc_mode = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[requested_cdc_mode.into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Update the append-only flag for a stream table.
    ///
    /// Called by the CDC heuristic fallback when a DELETE or UPDATE is
    /// detected on an append-only stream table, reverting it to MERGE.
    pub fn update_append_only(pgt_id: i64, is_append_only: bool) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET is_append_only = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[is_append_only.into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// PB2: Update the pooler compatibility mode flag for a stream table.
    pub fn update_pooler_compatibility_mode(
        pgt_id: i64,
        enabled: bool,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET pooler_compatibility_mode = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[enabled.into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Update the SCC identifier for a stream table (CYC-3).
    ///
    /// `scc_id` — the SCC group identifier, or `None` to clear (no cycle).
    pub fn update_scc_id(pgt_id: i64, scc_id: Option<i32>) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET scc_id = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[scc_id.into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Update the last fixpoint iteration count for a stream table (CYC-5).
    ///
    /// Recorded after a successful fixpoint convergence so monitoring can
    /// track how many iterations each SCC member required.
    pub fn update_last_fixpoint_iterations(
        pgt_id: i64,
        iterations: i32,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET last_fixpoint_iterations = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[iterations.into(), pgt_id.into()],
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

    /// Get the diamond consistency mode for a stream table by pgt_id.
    pub fn get_diamond_consistency(pgt_id: i64) -> Result<DiamondConsistency, PgTrickleError> {
        let val = Spi::get_one_with_args::<String>(
            "SELECT diamond_consistency FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
            &[pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or_else(|| "none".into());
        Ok(DiamondConsistency::from_sql_str(&val))
    }

    /// Set the diamond consistency mode for a stream table.
    pub fn set_diamond_consistency(
        pgt_id: i64,
        mode: DiamondConsistency,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET diamond_consistency = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[mode.as_str().into(), pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Get the diamond schedule policy for a stream table by pgt_id.
    pub fn get_diamond_schedule_policy(
        pgt_id: i64,
    ) -> Result<DiamondSchedulePolicy, PgTrickleError> {
        let val = Spi::get_one_with_args::<String>(
            "SELECT diamond_schedule_policy FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1",
            &[pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or_else(|| "fastest".into());
        Ok(DiamondSchedulePolicy::from_sql_str(&val).unwrap_or_default())
    }

    /// Set the diamond schedule policy for a stream table.
    pub fn set_diamond_schedule_policy(
        pgt_id: i64,
        policy: DiamondSchedulePolicy,
    ) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET diamond_schedule_policy = $1, updated_at = now() \
             WHERE pgt_id = $2",
            &[policy.as_str().into(), pgt_id.into()],
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
        let topk_limit = table.get::<i32>(18).map_err(map_spi)?;
        let topk_order_by = table.get::<String>(19).map_err(map_spi)?;
        let topk_offset = table.get::<i32>(20).map_err(map_spi)?;

        let diamond_consistency_str = table
            .get::<String>(21)
            .map_err(map_spi)?
            .unwrap_or_else(|| "none".into());
        let diamond_consistency = DiamondConsistency::from_sql_str(&diamond_consistency_str);

        let diamond_schedule_policy_str = table
            .get::<String>(22)
            .map_err(map_spi)?
            .unwrap_or_else(|| "fastest".into());
        let diamond_schedule_policy =
            DiamondSchedulePolicy::from_sql_str(&diamond_schedule_policy_str).unwrap_or_default();

        let has_keyless_source = table.get::<bool>(23).map_err(map_spi)?.unwrap_or(false);
        let function_hashes = table.get::<String>(24).map_err(map_spi)?;
        let requested_cdc_mode = table.get::<String>(25).map_err(map_spi)?;
        let is_append_only = table.get::<bool>(26).map_err(map_spi)?.unwrap_or(false);
        let scc_id = table.get::<i32>(27).map_err(map_spi)?;
        let last_fixpoint_iterations = table.get::<i32>(28).map_err(map_spi)?;
        let pooler_compatibility_mode = table.get::<bool>(29).map_err(map_spi)?.unwrap_or(false);

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
            topk_limit,
            topk_order_by,
            topk_offset,
            diamond_consistency,
            diamond_schedule_policy,
            has_keyless_source,
            function_hashes,
            requested_cdc_mode,
            is_append_only,
            scc_id,
            last_fixpoint_iterations,
            pooler_compatibility_mode,
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
        let topk_limit = row.get::<i32>(18).map_err(map_spi)?;
        let topk_order_by = row.get::<String>(19).map_err(map_spi)?;
        let topk_offset = row.get::<i32>(20).map_err(map_spi)?;

        let diamond_consistency_str = row
            .get::<String>(21)
            .map_err(map_spi)?
            .unwrap_or_else(|| "none".into());
        let diamond_consistency = DiamondConsistency::from_sql_str(&diamond_consistency_str);

        let diamond_schedule_policy_str = row
            .get::<String>(22)
            .map_err(map_spi)?
            .unwrap_or_else(|| "fastest".into());
        let diamond_schedule_policy =
            DiamondSchedulePolicy::from_sql_str(&diamond_schedule_policy_str).unwrap_or_default();

        let has_keyless_source = row.get::<bool>(23).map_err(map_spi)?.unwrap_or(false);
        let function_hashes = row.get::<String>(24).map_err(map_spi)?;
        let requested_cdc_mode = row.get::<String>(25).map_err(map_spi)?;
        let is_append_only = row.get::<bool>(26).map_err(map_spi)?.unwrap_or(false);
        let scc_id = row.get::<i32>(27).map_err(map_spi)?;
        let last_fixpoint_iterations = row.get::<i32>(28).map_err(map_spi)?;
        let pooler_compatibility_mode = row.get::<bool>(29).map_err(map_spi)?.unwrap_or(false);

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
            topk_limit,
            topk_order_by,
            topk_offset,
            diamond_consistency,
            diamond_schedule_policy,
            has_keyless_source,
            function_hashes,
            requested_cdc_mode,
            is_append_only,
            scc_id,
            last_fixpoint_iterations,
            pooler_compatibility_mode,
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

    /// Delete all dependency edges for a stream table.
    pub fn delete_for_st(pgt_id: i64) -> Result<(), PgTrickleError> {
        Spi::run_with_args(
            "DELETE FROM pgtrickle.pgt_dependencies WHERE pgt_id = $1",
            &[pgt_id.into()],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
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

    /// Update the CDC mode and related fields for all dependencies of a source.
    pub fn update_cdc_mode_for_source(
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
                 WHERE source_relid = $4",
                transition_started
            ),
            &[
                cdc_mode.as_str().into(),
                slot_name.into(),
                decoder_confirmed_lsn.into(),
                source_relid.into(),
            ],
        )
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
    }

    /// Resolve the effective CDC request for a source across all deferred STs.
    ///
    /// Precedence is conservative: if any dependent ST requests `trigger`, the
    /// source remains trigger-based. Otherwise `wal` wins over `auto`.
    /// Returns `None` when no deferred TABLE dependencies exist.
    pub fn effective_requested_mode_for_source(
        source_relid: pg_sys::Oid,
    ) -> Result<Option<String>, PgTrickleError> {
        Spi::get_one_with_args::<String>(
            "SELECT CASE \
                    WHEN bool_or(lower(COALESCE(st.requested_cdc_mode, current_setting('pg_trickle.cdc_mode'))) = 'trigger') THEN 'trigger' \
                    WHEN bool_or(lower(COALESCE(st.requested_cdc_mode, current_setting('pg_trickle.cdc_mode'))) = 'wal') THEN 'wal' \
                    WHEN bool_or(lower(COALESCE(st.requested_cdc_mode, current_setting('pg_trickle.cdc_mode'))) = 'auto') THEN 'auto' \
                    ELSE NULL \
                END \
             FROM pgtrickle.pgt_dependencies d \
             JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id \
             WHERE d.source_relid = $1 \
               AND d.source_type = 'TABLE' \
               AND st.refresh_mode <> 'IMMEDIATE'",
            &[source_relid.into()],
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

    /// Return the `pgt_id`s of all stream tables that depend on the given
    /// `source_relid` as a `STREAM_TABLE` source.
    ///
    /// Used by `drop_stream_table` to implement CASCADE: dropping a stream
    /// table also drops every stream table downstream of it.
    pub fn get_downstream_pgt_ids(source_relid: pg_sys::Oid) -> Result<Vec<i64>, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT DISTINCT pgt_id \
                     FROM pgtrickle.pgt_dependencies \
                     WHERE source_relid = $1 AND source_type = 'STREAM_TABLE'",
                    None,
                    &[source_relid.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;
            let mut result = Vec::new();
            for row in table {
                let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());
                if let Some(id) = row.get::<i64>(1).map_err(map_spi)? {
                    result.push(id);
                }
            }
            Ok(result)
        })
    }

    /// Return the **union** of `columns_used` recorded across all stream tables
    /// that depend on `source_oid` as a base-table source.
    ///
    /// This is the catalog read for F15 (Selective CDC Column Capture). When
    /// every downstream ST has a non-NULL `columns_used`, the result is the
    /// minimal set of columns that must be present in the CDC change buffer.
    ///
    /// Returns `None` when *any* dependency has `columns_used = NULL` (meaning
    /// "all columns are needed" — the ST was created without AST column
    /// tracking, e.g. `SELECT *`). Callers must treat `None` as "track
    /// everything".
    pub fn union_referenced_columns_for_source(
        source_oid: pg_sys::Oid,
    ) -> Result<Option<Vec<String>>, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT columns_used \
                     FROM pgtrickle.pgt_dependencies \
                     WHERE source_relid = $1 \
                       AND source_type IN ('TABLE', 'FOREIGN_TABLE')",
                    None,
                    &[source_oid.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            let mut union: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut found_any = false;

            for row in table {
                found_any = true;
                let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());
                match row.get::<Vec<String>>(1).map_err(map_spi)? {
                    // NULL columns_used → ST needs all columns; bail out early.
                    None => return Ok(None),
                    Some(cols) => {
                        for c in cols {
                            union.insert(c);
                        }
                    }
                }
            }

            if !found_any {
                // No dependencies yet (called before the ST insert) — return None
                // so the caller falls back to full column tracking.
                return Ok(None);
            }

            Ok(Some(union.into_iter().collect()))
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

    // Include RLS state so the fingerprint changes when RLS is toggled.
    let (rls_enabled, rls_forced) = query_rls_flags(source_oid)?;

    // PT2: Include partition child count so the fingerprint changes when
    // ATTACH/DETACH PARTITION modifies the partition structure.
    let partition_child_count = query_partition_child_count(source_oid)?;

    let snapshot_obj = serde_json::json!({
        "columns": entries,
        "rls_enabled": rls_enabled,
        "rls_forced": rls_forced,
        "partition_child_count": partition_child_count,
    });

    let json_str = serde_json::to_string(&snapshot_obj)
        .map_err(|e| PgTrickleError::InternalError(format!("JSON serialization failed: {e}")))?;

    let mut hasher = Sha256::new();
    hasher.update(json_str.as_bytes());
    let fingerprint = format!("{:x}", hasher.finalize());

    let snapshot = pgrx::JsonB(snapshot_obj);
    Ok((snapshot, fingerprint))
}

/// Query the current RLS state of a table from `pg_class`.
///
/// Returns `(relrowsecurity, relforcerowsecurity)`.
#[cfg(not(test))]
pub fn query_rls_flags(source_oid: pg_sys::Oid) -> Result<(bool, bool), PgTrickleError> {
    Spi::connect(|client| {
        let sql = format!(
            "SELECT relrowsecurity, relforcerowsecurity \
             FROM pg_class WHERE oid = {}",
            source_oid.to_u32(),
        );
        let mut result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        if let Some(row) = result.next() {
            let rls: bool = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(false);
            let force: bool = row
                .get(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(false);
            return Ok((rls, force));
        }
        Ok((false, false))
    })
}

/// Test-only stub: SPI is unavailable in unit tests.
#[cfg(test)]
pub fn build_column_snapshot(
    _source_oid: pg_sys::Oid,
) -> Result<(pgrx::JsonB, String), PgTrickleError> {
    let obj = serde_json::json!({
        "columns": [],
        "rls_enabled": false,
        "rls_forced": false,
    });
    Ok((pgrx::JsonB(obj), String::new()))
}

/// Test-only stub for `query_rls_flags`.
#[cfg(test)]
pub fn query_rls_flags(_source_oid: pg_sys::Oid) -> Result<(bool, bool), PgTrickleError> {
    Ok((false, false))
}

/// Query the number of child partitions of a table.
///
/// Returns 0 for non-partitioned tables. For partitioned tables (`relkind = 'p'`),
/// returns the count of rows in `pg_inherits` where this table is the parent.
#[cfg(not(test))]
pub fn query_partition_child_count(source_oid: pg_sys::Oid) -> Result<i64, PgTrickleError> {
    Spi::get_one_with_args::<i64>(
        "SELECT count(*)::bigint FROM pg_inherits WHERE inhparent = $1",
        &[source_oid.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))
    .map(|opt| opt.unwrap_or(0))
}

/// Test-only stub for `query_partition_child_count`.
#[cfg(test)]
pub fn query_partition_child_count(_source_oid: pg_sys::Oid) -> Result<i64, PgTrickleError> {
    Ok(0)
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

/// Rebuild and persist the column snapshot + schema fingerprint for a given
/// (pgt_id, source_oid) dependency pair.
///
/// Called after Task 3.5 ADD COLUMN online extension so that the next DDL
/// event detects the correct baseline and does not spuriously trigger a reinit.
#[cfg(not(test))]
pub fn store_column_snapshot_for_pgt_id(
    pgt_id: i64,
    source_oid: pg_sys::Oid,
) -> Result<(), PgTrickleError> {
    let (snapshot, fingerprint) = build_column_snapshot(source_oid)?;
    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_dependencies \
         SET column_snapshot = $1, schema_fingerprint = $2 \
         WHERE pgt_id = $3 AND source_relid = $4",
        &[
            snapshot.into(),
            fingerprint.as_str().into(),
            pgt_id.into(),
            source_oid.into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("Failed to store column snapshot: {e}")))
}

/// Test-only stub.
#[cfg(test)]
pub fn store_column_snapshot_for_pgt_id(
    _pgt_id: i64,
    _source_oid: pg_sys::Oid,
) -> Result<(), PgTrickleError> {
    Ok(())
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
    ///
    /// `tick_watermark_lsn` is the WAL LSN watermark at tick start (CSS1; NULL when disabled).
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
        tick_watermark_lsn: Option<&str>,
    ) -> Result<i64, PgTrickleError> {
        Spi::get_one_with_args::<i64>(
            "INSERT INTO pgtrickle.pgt_refresh_history \
             (pgt_id, data_timestamp, start_time, action, status, \
              rows_inserted, rows_deleted, error_message, \
              initiated_by, freshness_deadline, \
              delta_row_count, merge_strategy_used, was_full_fallback, tick_watermark_lsn) \
             VALUES ($1, $2, now(), $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13::pg_lsn) \
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
                tick_watermark_lsn.into(),
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

// ── Source gate CRUD (v0.5.0, Phase 3 — Bootstrap Source Gating) ──────────

/// Returns the OIDs of all currently gated source relations.
///
/// Used by the scheduler once per tick to build the gated-source set before
/// deciding whether to skip a stream table refresh.
pub fn get_gated_source_oids() -> Result<Vec<pg_sys::Oid>, PgTrickleError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT source_relid \
                 FROM pgtrickle.pgt_source_gates \
                 WHERE gated = true",
                None,
                &[],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut oids: Vec<pg_sys::Oid> = Vec::new();
        for row in table {
            if let Some(oid) = row
                .get::<pg_sys::Oid>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            {
                oids.push(oid);
            }
        }
        Ok(oids)
    })
}

/// UPSERT a source gate: mark the given source OID as gated.
pub fn upsert_gate(
    source_relid: pg_sys::Oid,
    gated_by: Option<&str>,
) -> Result<(), PgTrickleError> {
    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_source_gates \
             (source_relid, gated, gated_at, ungated_at, gated_by) \
         VALUES ($1, true, now(), NULL, $2) \
         ON CONFLICT (source_relid) DO UPDATE SET \
             gated = true, gated_at = now(), ungated_at = NULL, \
             gated_by = EXCLUDED.gated_by",
        &[source_relid.into(), gated_by.into()],
    )
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
}

/// Mark a source gate as ungated (sets gated=false and records ungated_at).
pub fn set_ungated(source_relid: pg_sys::Oid) -> Result<(), PgTrickleError> {
    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_source_gates \
         SET gated = false, ungated_at = now() \
         WHERE source_relid = $1",
        &[source_relid.into()],
    )
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))
}

// ── Watermark Gating (v0.7.0) ─────────────────────────────────────────────

/// Per-source watermark state, mirrors `pgtrickle.pgt_watermarks`.
#[derive(Debug, Clone)]
pub struct WatermarkState {
    pub source_relid: pg_sys::Oid,
    pub watermark: TimestampWithTimeZone,
    pub updated_at: TimestampWithTimeZone,
    pub advanced_by: Option<String>,
    pub wal_lsn_at_advance: Option<String>,
}

/// Watermark group definition, mirrors `pgtrickle.pgt_watermark_groups`.
#[derive(Debug, Clone)]
pub struct WatermarkGroup {
    pub group_id: i32,
    pub group_name: String,
    pub source_relids: Vec<pg_sys::Oid>,
    pub tolerance_secs: f64,
    pub created_at: TimestampWithTimeZone,
}

/// Advance (or insert) the watermark for a source table.
///
/// Enforces monotonicity: the new watermark must be >= the current value.
/// Records `pg_current_wal_insert_lsn()` alongside the watermark for
/// future hold-back support.
pub fn advance_watermark(
    source_relid: pg_sys::Oid,
    watermark: TimestampWithTimeZone,
    advanced_by: Option<&str>,
) -> Result<(), PgTrickleError> {
    Spi::connect_mut(|client| {
        // Check monotonicity: reject backward movement.
        // Use client.select + into_iter().next() to safely handle zero rows.
        let table = client
            .select(
                "SELECT watermark FROM pgtrickle.pgt_watermarks WHERE source_relid = $1",
                None,
                &[source_relid.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        let current: Option<TimestampWithTimeZone> = if let Some(row) = table.into_iter().next() {
            row.get::<TimestampWithTimeZone>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        } else {
            None
        };

        if let Some(current_wm) = current {
            // Compare via SQL to use PostgreSQL's TIMESTAMPTZ comparison.
            let cmp_table = client
                .select(
                    "SELECT $1::timestamptz < $2::timestamptz AS is_backward, \
                            $1::timestamptz = $2::timestamptz AS is_equal",
                    None,
                    &[watermark.into(), current_wm.into()],
                )
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

            if let Some(cmp_row) = cmp_table.into_iter().next() {
                let is_backward = cmp_row
                    .get::<bool>(1)
                    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                    .unwrap_or(false);

                if is_backward {
                    return Err(PgTrickleError::WatermarkBackwardMovement(format!(
                        "new watermark is older than current for source OID {}",
                        source_relid.to_u32()
                    )));
                }

                let is_equal = cmp_row
                    .get::<bool>(2)
                    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                    .unwrap_or(false);

                if is_equal {
                    return Ok(());
                }
            }
        }

        client
            .update(
                "INSERT INTO pgtrickle.pgt_watermarks \
                     (source_relid, watermark, updated_at, advanced_by, wal_lsn_at_advance) \
                 VALUES ($1, $2, now(), $3, pg_current_wal_insert_lsn()::text) \
                 ON CONFLICT (source_relid) DO UPDATE SET \
                     watermark = EXCLUDED.watermark, \
                     updated_at = EXCLUDED.updated_at, \
                     advanced_by = EXCLUDED.advanced_by, \
                     wal_lsn_at_advance = EXCLUDED.wal_lsn_at_advance",
                None,
                &[source_relid.into(), watermark.into(), advanced_by.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Ok(())
    })
}

/// Get all current watermark states.
pub fn get_all_watermarks() -> Result<Vec<WatermarkState>, PgTrickleError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT source_relid, watermark, updated_at, advanced_by, wal_lsn_at_advance \
                 FROM pgtrickle.pgt_watermarks \
                 ORDER BY source_relid",
                None,
                &[],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        let mut out = Vec::new();
        for row in table {
            let source_relid = row
                .get::<pg_sys::Oid>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(pg_sys::Oid::from(0u32));
            let watermark = row
                .get::<TimestampWithTimeZone>(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .ok_or_else(|| {
                    PgTrickleError::InternalError("NULL watermark in pgt_watermarks".into())
                })?;
            let updated_at = row
                .get::<TimestampWithTimeZone>(3)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .ok_or_else(|| {
                    PgTrickleError::InternalError("NULL updated_at in pgt_watermarks".into())
                })?;
            let advanced_by = row
                .get::<String>(4)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            let wal_lsn_at_advance = row
                .get::<String>(5)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            out.push(WatermarkState {
                source_relid,
                watermark,
                updated_at,
                advanced_by,
                wal_lsn_at_advance,
            });
        }
        Ok(out)
    })
}

/// Get the watermark for a specific source OID.
pub fn get_watermark_for_source(
    source_relid: pg_sys::Oid,
) -> Result<Option<WatermarkState>, PgTrickleError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT source_relid, watermark, updated_at, advanced_by, wal_lsn_at_advance \
                 FROM pgtrickle.pgt_watermarks WHERE source_relid = $1",
                None,
                &[source_relid.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        if let Some(row) = table.into_iter().next() {
            let watermark = row
                .get::<TimestampWithTimeZone>(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .ok_or_else(|| {
                    PgTrickleError::InternalError("NULL watermark in pgt_watermarks".into())
                })?;
            let updated_at = row
                .get::<TimestampWithTimeZone>(3)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .ok_or_else(|| {
                    PgTrickleError::InternalError("NULL updated_at in pgt_watermarks".into())
                })?;
            let advanced_by = row
                .get::<String>(4)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            let wal_lsn_at_advance = row
                .get::<String>(5)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            return Ok(Some(WatermarkState {
                source_relid,
                watermark,
                updated_at,
                advanced_by,
                wal_lsn_at_advance,
            }));
        }
        Ok(None)
    })
}

/// Create a new watermark group.
pub fn create_watermark_group(
    group_name: &str,
    source_relids: &[pg_sys::Oid],
    tolerance_secs: f64,
) -> Result<i32, PgTrickleError> {
    // Check for duplicate name.
    let exists: Option<bool> = Spi::get_one_with_args(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_watermark_groups WHERE group_name = $1)",
        &[group_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    if exists == Some(true) {
        return Err(PgTrickleError::WatermarkGroupAlreadyExists(
            group_name.to_string(),
        ));
    }

    // Build an OID array literal for SQL.
    let oid_strs: Vec<String> = source_relids
        .iter()
        .map(|o| o.to_u32().to_string())
        .collect();
    let array_literal = format!("ARRAY[{}]::oid[]", oid_strs.join(","));

    let sql = format!(
        "INSERT INTO pgtrickle.pgt_watermark_groups \
             (group_name, source_relids, tolerance_secs) \
         VALUES ($1, {}, $2) \
         RETURNING group_id",
        array_literal
    );
    let group_id: i32 = Spi::get_one_with_args(&sql, &[group_name.into(), tolerance_secs.into()])
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .ok_or_else(|| {
            PgTrickleError::InternalError(
                "create_watermark_group: INSERT RETURNING returned NULL".into(),
            )
        })?;
    Ok(group_id)
}

/// Drop a watermark group by name.
pub fn drop_watermark_group(group_name: &str) -> Result<(), PgTrickleError> {
    let deleted: Option<i64> = Spi::get_one_with_args(
        "WITH d AS (\
             DELETE FROM pgtrickle.pgt_watermark_groups WHERE group_name = $1 RETURNING 1\
         ) SELECT count(*) FROM d",
        &[group_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    if deleted == Some(0) || deleted.is_none() {
        return Err(PgTrickleError::WatermarkGroupNotFound(
            group_name.to_string(),
        ));
    }
    Ok(())
}

/// Get all watermark groups.
pub fn get_all_watermark_groups() -> Result<Vec<WatermarkGroup>, PgTrickleError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT group_id, group_name, source_relids, tolerance_secs, created_at \
                 FROM pgtrickle.pgt_watermark_groups \
                 ORDER BY group_name",
                None,
                &[],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        let mut out = Vec::new();
        for row in table {
            let group_id = row
                .get::<i32>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(0);
            let group_name = row
                .get::<String>(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            // source_relids is OID[] — fetch as a Vec<pg_sys::Oid>.
            let source_relids: Vec<pg_sys::Oid> = row
                .get::<Vec<pg_sys::Oid>>(3)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let tolerance_secs = row
                .get::<f64>(4)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(0.0);
            let created_at = row
                .get::<TimestampWithTimeZone>(5)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .ok_or_else(|| {
                    PgTrickleError::InternalError("NULL created_at in pgt_watermark_groups".into())
                })?;
            out.push(WatermarkGroup {
                group_id,
                group_name,
                source_relids,
                tolerance_secs,
                created_at,
            });
        }
        Ok(out)
    })
}

/// Check watermark alignment for a stream table's source OIDs.
///
/// Returns `true` if all overlapping watermark groups are aligned (or no
/// groups apply). Returns `false` if any group's watermarks are misaligned
/// beyond tolerance.
///
/// A group is considered aligned when:
///   max(watermark) - min(watermark) <= tolerance
/// among all source OIDs that belong to both the group and the ST's source set.
///
/// Sources that have never had a watermark advanced are ignored (they don't
/// participate in gating until their first `advance_watermark()` call).
pub fn check_watermark_alignment(
    source_oids: &[pg_sys::Oid],
) -> Result<(bool, Option<String>), PgTrickleError> {
    let groups = get_all_watermark_groups()?;
    if groups.is_empty() {
        return Ok((true, None));
    }

    let source_set: std::collections::HashSet<pg_sys::Oid> = source_oids.iter().copied().collect();

    for group in &groups {
        // Find the intersection: group sources that are also ST sources.
        let overlapping: Vec<pg_sys::Oid> = group
            .source_relids
            .iter()
            .filter(|oid| source_set.contains(oid))
            .copied()
            .collect();

        // If fewer than 2 of this group's sources are in the ST's source
        // set, the group is irrelevant for this ST.
        if overlapping.len() < 2 {
            continue;
        }

        // Collect watermarks for overlapping sources.
        let mut timestamps: Vec<TimestampWithTimeZone> = Vec::new();
        let mut missing_count = 0usize;
        for oid in &overlapping {
            match get_watermark_for_source(*oid)? {
                Some(wm) => timestamps.push(wm.watermark),
                None => missing_count += 1,
            }
        }

        // If any overlapping source has no watermark yet, skip gating for
        // this group (watermarks not fully set up yet).
        if missing_count > 0 {
            continue;
        }

        // All sources have watermarks — check alignment.
        if timestamps.len() >= 2 {
            let lag_secs: Option<f64> = Spi::get_one_with_args(
                "SELECT EXTRACT(EPOCH FROM ($1::timestamptz - $2::timestamptz))::float8",
                &[timestamps[0].into(), timestamps[1].into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

            // For >2 sources, compute max and min via SQL for robustness.
            let (max_wm, min_wm) = if timestamps.len() == 2 {
                // Determine which is max and which is min.
                let first_is_greater: bool = Spi::get_one_with_args(
                    "SELECT $1::timestamptz >= $2::timestamptz",
                    &[timestamps[0].into(), timestamps[1].into()],
                )
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(true);

                if first_is_greater {
                    (timestamps[0], timestamps[1])
                } else {
                    (timestamps[1], timestamps[0])
                }
            } else {
                // For 3+ sources, build SQL to find max/min.
                let mut max = timestamps[0];
                let mut min = timestamps[0];
                for ts in &timestamps[1..] {
                    let is_greater: bool = Spi::get_one_with_args(
                        "SELECT $1::timestamptz > $2::timestamptz",
                        &[(*ts).into(), max.into()],
                    )
                    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                    .unwrap_or(false);
                    if is_greater {
                        max = *ts;
                    }
                    let is_less: bool = Spi::get_one_with_args(
                        "SELECT $1::timestamptz < $2::timestamptz",
                        &[(*ts).into(), min.into()],
                    )
                    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                    .unwrap_or(false);
                    if is_less {
                        min = *ts;
                    }
                }
                (max, min)
            };

            let lag: f64 = Spi::get_one_with_args(
                "SELECT EXTRACT(EPOCH FROM ($1::timestamptz - $2::timestamptz))::float8",
                &[max_wm.into(), min_wm.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            .unwrap_or(0.0);

            let _ = lag_secs; // used above for 2-source shortcut

            if lag > group.tolerance_secs {
                let reason = format!(
                    "watermark group '{}' misaligned: lag {:.1}s exceeds tolerance {:.1}s",
                    group.group_name, lag, group.tolerance_secs
                );
                return Ok((false, Some(reason)));
            }
        }
    }

    Ok((true, None))
}

// ── Scheduler Job (Phase 2: parallel refresh) ─────────────────────────────

/// Status of a scheduler job in the parallel refresh pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    RetryableFailed,
    PermanentFailed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "QUEUED",
            JobStatus::Running => "RUNNING",
            JobStatus::Succeeded => "SUCCEEDED",
            JobStatus::RetryableFailed => "RETRYABLE_FAILED",
            JobStatus::PermanentFailed => "PERMANENT_FAILED",
            JobStatus::Cancelled => "CANCELLED",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "QUEUED" => JobStatus::Queued,
            "RUNNING" => JobStatus::Running,
            "SUCCEEDED" => JobStatus::Succeeded,
            "RETRYABLE_FAILED" => JobStatus::RetryableFailed,
            "PERMANENT_FAILED" => JobStatus::PermanentFailed,
            "CANCELLED" => JobStatus::Cancelled,
            _ => JobStatus::Cancelled,
        }
    }

    /// Whether this status represents a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Succeeded
                | JobStatus::RetryableFailed
                | JobStatus::PermanentFailed
                | JobStatus::Cancelled
        )
    }
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A scheduler job row from `pgtrickle.pgt_scheduler_jobs`.
#[derive(Debug, Clone)]
pub struct SchedulerJob {
    pub job_id: i64,
    pub dag_version: i64,
    pub unit_key: String,
    pub unit_kind: String,
    pub member_pgt_ids: Vec<i64>,
    pub root_pgt_id: i64,
    pub status: JobStatus,
    pub scheduler_pid: i32,
    pub worker_pid: Option<i32>,
    pub attempt_no: i32,
    pub enqueued_at: TimestampWithTimeZone,
    pub started_at: Option<TimestampWithTimeZone>,
    pub finished_at: Option<TimestampWithTimeZone>,
    pub outcome_detail: Option<String>,
    pub retryable: Option<bool>,
}

impl SchedulerJob {
    /// Enqueue a new job in QUEUED status. Returns the assigned `job_id`.
    pub fn enqueue(
        dag_version: i64,
        unit_key: &str,
        unit_kind: &str,
        member_pgt_ids: &[i64],
        root_pgt_id: i64,
        scheduler_pid: i32,
        attempt_no: i32,
    ) -> Result<i64, PgTrickleError> {
        Spi::connect_mut(|client| {
            let row = client
                .update(
                    "INSERT INTO pgtrickle.pgt_scheduler_jobs \
                     (dag_version, unit_key, unit_kind, member_pgt_ids, root_pgt_id, \
                      scheduler_pid, attempt_no) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     RETURNING job_id",
                    None,
                    &[
                        dag_version.into(),
                        unit_key.into(),
                        unit_kind.into(),
                        member_pgt_ids.into(),
                        root_pgt_id.into(),
                        scheduler_pid.into(),
                        attempt_no.into(),
                    ],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
                .first();

            row.get_one::<i64>()
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
                .ok_or_else(|| {
                    PgTrickleError::InternalError("INSERT job did not return job_id".into())
                })
        })
    }

    /// Claim a QUEUED job: transition QUEUED → RUNNING and set worker_pid.
    ///
    /// Returns `Ok(true)` if the claim succeeded (row was updated),
    /// `Ok(false)` if the job was already claimed or no longer QUEUED.
    pub fn claim(job_id: i64, worker_pid: i32) -> Result<bool, PgTrickleError> {
        Spi::connect_mut(|client| {
            let result = client
                .update(
                    "UPDATE pgtrickle.pgt_scheduler_jobs \
                     SET status = 'RUNNING', worker_pid = $2, started_at = now() \
                     WHERE job_id = $1 AND status = 'QUEUED'",
                    None,
                    &[job_id.into(), worker_pid.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;
            Ok(!result.is_empty())
        })
    }

    /// Complete a job: set terminal status, outcome detail, and retryability.
    pub fn complete(
        job_id: i64,
        status: JobStatus,
        outcome_detail: Option<&str>,
        retryable: Option<bool>,
    ) -> Result<(), PgTrickleError> {
        Spi::connect_mut(|client| {
            client
                .update(
                    "UPDATE pgtrickle.pgt_scheduler_jobs \
                     SET status = $2, finished_at = now(), \
                         outcome_detail = $3, retryable = $4 \
                     WHERE job_id = $1",
                    None,
                    &[
                        job_id.into(),
                        status.as_str().into(),
                        outcome_detail.into(),
                        retryable.into(),
                    ],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;
            Ok(())
        })
    }

    /// Cancel a job (force to CANCELLED).
    pub fn cancel(job_id: i64, reason: &str) -> Result<(), PgTrickleError> {
        Self::complete(job_id, JobStatus::Cancelled, Some(reason), None)
    }

    /// Load a job by its ID. Returns `None` if not found.
    pub fn get_by_id(job_id: i64) -> Result<Option<Self>, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT job_id, dag_version, unit_key, unit_kind, member_pgt_ids, \
                     root_pgt_id, status, scheduler_pid, worker_pid, attempt_no, \
                     enqueued_at, started_at, finished_at, outcome_detail, retryable \
                     FROM pgtrickle.pgt_scheduler_jobs \
                     WHERE job_id = $1",
                    None,
                    &[job_id.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            if table.is_empty() {
                return Ok(None);
            }

            Self::from_spi_table_row(&table.first()).map(Some)
        })
    }

    /// Cancel all QUEUED/RUNNING jobs whose worker_pid or scheduler_pid is no
    /// longer alive. Used for crash recovery / orphaned job cleanup.
    ///
    /// Returns the number of jobs cancelled.
    pub fn cancel_orphaned_jobs() -> Result<i64, PgTrickleError> {
        Spi::connect_mut(|client| {
            let result = client
                .update(
                    "UPDATE pgtrickle.pgt_scheduler_jobs \
                     SET status = 'CANCELLED', \
                         finished_at = now(), \
                         outcome_detail = 'Cancelled: orphaned after crash recovery' \
                     WHERE status IN ('QUEUED', 'RUNNING') \
                       AND NOT EXISTS ( \
                           SELECT 1 FROM pg_stat_activity \
                           WHERE pid = pgt_scheduler_jobs.worker_pid \
                       ) \
                       AND NOT EXISTS ( \
                           SELECT 1 FROM pg_stat_activity \
                           WHERE pid = pgt_scheduler_jobs.scheduler_pid \
                       )",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;
            Ok(result.len() as i64)
        })
    }

    /// Prune completed/failed/cancelled jobs older than the given age.
    ///
    /// Returns the number of rows deleted.
    pub fn prune_completed(max_age_seconds: i64) -> Result<i64, PgTrickleError> {
        Spi::connect_mut(|client| {
            let result = client
                .update(
                    "DELETE FROM pgtrickle.pgt_scheduler_jobs \
                     WHERE status IN ('SUCCEEDED', 'RETRYABLE_FAILED', 'PERMANENT_FAILED', 'CANCELLED') \
                       AND finished_at < now() - make_interval(secs => $1::float8)",
                    None,
                    &[max_age_seconds.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;
            Ok(result.len() as i64)
        })
    }

    /// Check whether an in-flight (QUEUED or RUNNING) job already exists for
    /// the given unit_key.
    pub fn has_inflight_job(unit_key: &str) -> Result<bool, PgTrickleError> {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT 1 FROM pgtrickle.pgt_scheduler_jobs \
                     WHERE unit_key = $1 AND status IN ('QUEUED', 'RUNNING') \
                     LIMIT 1",
                    None,
                    &[unit_key.into()],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;
            Ok(!table.is_empty())
        })
    }

    /// Parse a job row from SPI query results (ordinal column access).
    ///
    /// Column order must match the SELECT in `get_by_id`:
    /// 1=job_id, 2=dag_version, 3=unit_key, 4=unit_kind, 5=member_pgt_ids,
    /// 6=root_pgt_id, 7=status, 8=scheduler_pid, 9=worker_pid, 10=attempt_no,
    /// 11=enqueued_at, 12=started_at, 13=finished_at, 14=outcome_detail, 15=retryable
    fn from_spi_table_row(table: &SpiTupleTable<'_>) -> Result<Self, PgTrickleError> {
        let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());

        let status_str: String = table.get::<String>(7).map_err(map_spi)?.unwrap_or_default();

        Ok(Self {
            job_id: table.get::<i64>(1).map_err(map_spi)?.unwrap_or(0),
            dag_version: table.get::<i64>(2).map_err(map_spi)?.unwrap_or(0),
            unit_key: table.get::<String>(3).map_err(map_spi)?.unwrap_or_default(),
            unit_kind: table.get::<String>(4).map_err(map_spi)?.unwrap_or_default(),
            member_pgt_ids: table
                .get::<Vec<i64>>(5)
                .map_err(map_spi)?
                .unwrap_or_default(),
            root_pgt_id: table.get::<i64>(6).map_err(map_spi)?.unwrap_or(0),
            status: JobStatus::from_str(&status_str),
            scheduler_pid: table.get::<i32>(8).map_err(map_spi)?.unwrap_or(0),
            worker_pid: table.get::<i32>(9).map_err(map_spi)?,
            attempt_no: table.get::<i32>(10).map_err(map_spi)?.unwrap_or(1),
            enqueued_at: table
                .get::<TimestampWithTimeZone>(11)
                .map_err(map_spi)?
                .ok_or_else(|| PgTrickleError::InternalError("NULL enqueued_at".into()))?,
            started_at: table.get::<TimestampWithTimeZone>(12).map_err(map_spi)?,
            finished_at: table.get::<TimestampWithTimeZone>(13).map_err(map_spi)?,
            outcome_detail: table.get::<String>(14).map_err(map_spi)?,
            retryable: table.get::<bool>(15).map_err(map_spi)?,
        })
    }
}

// ── Refresh Group Catalog (A8) ──────────────────────────────────────────────

/// Metadata for a user-declared refresh group, mirrors
/// `pgtrickle.pgt_refresh_groups`.
#[derive(Debug, Clone)]
pub struct RefreshGroupMeta {
    pub group_id: i32,
    pub group_name: String,
    pub member_oids: Vec<pg_sys::Oid>,
    pub isolation: String,
    pub created_at: TimestampWithTimeZone,
}

/// Insert a new refresh group. Returns the assigned `group_id`.
pub fn create_refresh_group(
    group_name: &str,
    member_oids: &[pg_sys::Oid],
    isolation: &str,
) -> Result<i32, PgTrickleError> {
    Spi::connect_mut(|client| {
        let row = client
            .update(
                "INSERT INTO pgtrickle.pgt_refresh_groups \
                 (group_name, member_oids, isolation) \
                 VALUES ($1, $2, $3) \
                 RETURNING group_id",
                None,
                &[
                    group_name.into(),
                    member_oids.to_vec().into(),
                    isolation.into(),
                ],
            )
            .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
            .first();

        row.get_one::<i32>()
            .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
            .ok_or_else(|| PgTrickleError::InternalError("INSERT did not return group_id".into()))
    })
}

/// Delete a refresh group by name.
pub fn drop_refresh_group(group_name: &str) -> Result<(), PgTrickleError> {
    Spi::connect_mut(|client| {
        let count = client
            .update(
                "DELETE FROM pgtrickle.pgt_refresh_groups WHERE group_name = $1",
                None,
                &[group_name.into()],
            )
            .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?
            .len();

        if count == 0 {
            return Err(PgTrickleError::NotFound(format!(
                "refresh group '{}' does not exist",
                group_name
            )));
        }
        Ok(())
    })
}

/// Return all refresh groups.
pub fn get_all_refresh_groups() -> Result<Vec<RefreshGroupMeta>, PgTrickleError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT group_id, group_name, member_oids, isolation, created_at \
                 FROM pgtrickle.pgt_refresh_groups \
                 ORDER BY group_id",
                None,
                &[],
            )
            .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

        let mut groups = Vec::new();
        for row in table {
            let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());
            groups.push(RefreshGroupMeta {
                group_id: row.get::<i32>(1).map_err(map_spi)?.unwrap_or(0),
                group_name: row.get::<String>(2).map_err(map_spi)?.unwrap_or_default(),
                member_oids: row
                    .get::<Vec<pg_sys::Oid>>(3)
                    .map_err(map_spi)?
                    .unwrap_or_default(),
                isolation: row
                    .get::<String>(4)
                    .map_err(map_spi)?
                    .unwrap_or_else(|| "read_committed".to_string()),
                created_at: row
                    .get::<TimestampWithTimeZone>(5)
                    .map_err(map_spi)?
                    .ok_or_else(|| PgTrickleError::InternalError("NULL created_at".into()))?,
            });
        }
        Ok(groups)
    })
}

/// Check whether any existing refresh group already contains the given OID.
/// Returns the conflicting group name if found.
pub fn find_group_containing_member(
    member_oid: pg_sys::Oid,
) -> Result<Option<String>, PgTrickleError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT group_name FROM pgtrickle.pgt_refresh_groups \
                 WHERE $1 = ANY(member_oids) LIMIT 1",
                None,
                &[member_oid.into()],
            )
            .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

        if table.is_empty() {
            Ok(None)
        } else {
            Ok(table
                .first()
                .get::<String>(1)
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?)
        }
    })
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

    // ── JobStatus tests ────────────────────────────────────────────────

    #[test]
    fn test_job_status_as_str() {
        assert_eq!(JobStatus::Queued.as_str(), "QUEUED");
        assert_eq!(JobStatus::Running.as_str(), "RUNNING");
        assert_eq!(JobStatus::Succeeded.as_str(), "SUCCEEDED");
        assert_eq!(JobStatus::RetryableFailed.as_str(), "RETRYABLE_FAILED");
        assert_eq!(JobStatus::PermanentFailed.as_str(), "PERMANENT_FAILED");
        assert_eq!(JobStatus::Cancelled.as_str(), "CANCELLED");
    }

    #[test]
    fn test_job_status_from_str_valid() {
        assert_eq!(JobStatus::from_str("QUEUED"), JobStatus::Queued);
        assert_eq!(JobStatus::from_str("RUNNING"), JobStatus::Running);
        assert_eq!(JobStatus::from_str("SUCCEEDED"), JobStatus::Succeeded);
        assert_eq!(
            JobStatus::from_str("RETRYABLE_FAILED"),
            JobStatus::RetryableFailed
        );
        assert_eq!(
            JobStatus::from_str("PERMANENT_FAILED"),
            JobStatus::PermanentFailed
        );
        assert_eq!(JobStatus::from_str("CANCELLED"), JobStatus::Cancelled);
    }

    #[test]
    fn test_job_status_from_str_unknown_defaults_to_cancelled() {
        assert_eq!(JobStatus::from_str(""), JobStatus::Cancelled);
        assert_eq!(JobStatus::from_str("UNKNOWN"), JobStatus::Cancelled);
    }

    #[test]
    fn test_job_status_roundtrip() {
        for status in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Succeeded,
            JobStatus::RetryableFailed,
            JobStatus::PermanentFailed,
            JobStatus::Cancelled,
        ] {
            assert_eq!(JobStatus::from_str(status.as_str()), status);
        }
    }

    #[test]
    fn test_job_status_is_terminal() {
        assert!(!JobStatus::Queued.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert!(JobStatus::Succeeded.is_terminal());
        assert!(JobStatus::RetryableFailed.is_terminal());
        assert!(JobStatus::PermanentFailed.is_terminal());
        assert!(JobStatus::Cancelled.is_terminal());
    }

    #[test]
    fn test_job_status_display() {
        assert_eq!(format!("{}", JobStatus::Queued), "QUEUED");
        assert_eq!(format!("{}", JobStatus::Running), "RUNNING");
        assert_eq!(format!("{}", JobStatus::Succeeded), "SUCCEEDED");
    }
}
