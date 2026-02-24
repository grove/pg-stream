//! DDL tracking via event triggers and object access hooks.
//!
//! Monitors schema changes on upstream tables and handles direct DROP TABLE
//! on stream table storage tables.
//!
//! ## Event trigger: `pg_stream_ddl_tracker`
//!
//! Installed via `extension_sql!()` as `ON ddl_command_end`. When any DDL
//! completes, the handler queries `pg_event_trigger_ddl_commands()` to
//! discover what changed, then checks `pgstream.pgs_dependencies` to find
//! affected stream tables.
//!
//! - **ALTER TABLE** on an upstream source → mark downstream STs
//!   `needs_reinit = true`. On the next scheduler cycle, the refresh
//!   executor will use `REINITIALIZE` instead of `DIFFERENTIAL`.
//!
//! - **DROP TABLE** on an upstream source → set downstream STs to
//!   `status = 'ERROR'` since the source no longer exists.
//!
//! - **DROP TABLE** on a ST storage table itself → clean up the
//!   catalog entry and signal a DAG rebuild.
//!
//! ## Cascade invalidation
//!
//! When ST `A` depends on base table `T`, and ST `B` depends on ST `A`,
//! an ALTER TABLE on `T` must invalidate both `A` and `B`. The cascade
//! is resolved by walking transitive dependencies in `pgstream.pgs_dependencies`.

use pgrx::prelude::*;

use crate::catalog::{CdcMode, StDependency, StreamTableMeta};
use crate::dag::StStatus;
use crate::error::PgStreamError;
use crate::shmem;
use crate::{cdc, config, wal_decoder};

// ── Event trigger handler ──────────────────────────────────────────────────

/// Handler for the `ddl_command_end` event trigger.
///
/// This function is called by PostgreSQL after any DDL statement completes.
/// It inspects the affected objects and marks downstream STs for reinit
/// or error as appropriate.
///
/// Registered via `extension_sql!()` in lib.rs as:
/// ```sql
/// CREATE FUNCTION pgstream._on_ddl_end() RETURNS event_trigger ...
/// CREATE EVENT TRIGGER pg_stream_ddl_tracker ON ddl_command_end
///     EXECUTE FUNCTION pgstream._on_ddl_end();
/// ```
#[pg_extern(schema = "pgstream", name = "_on_ddl_end", sql = false)]
fn pg_stream_on_ddl_end() {
    // Query the event trigger context for affected objects.
    // pg_event_trigger_ddl_commands() is only available inside an
    // event trigger context — calling it elsewhere will error.
    let commands = match collect_ddl_commands() {
        Ok(cmds) => cmds,
        Err(e) => {
            // Not inside an event trigger context, or SPI error.
            // This can happen during CREATE EXTENSION itself — safe to ignore.
            pgrx::debug1!("pg_stream_ddl_tracker: could not read DDL commands: {}", e);
            return;
        }
    };

    for cmd in &commands {
        handle_ddl_command(cmd);
    }
}

/// A single DDL command extracted from `pg_event_trigger_ddl_commands()`.
#[derive(Debug, Clone)]
struct DdlCommand {
    /// OID of the affected object.
    objid: pg_sys::Oid,
    /// Object type string (e.g. "table", "index").
    object_type: String,
    /// Command tag (e.g. "ALTER TABLE", "DROP TABLE", "CREATE INDEX").
    command_tag: String,
    /// Schema name of the affected object, if available.
    schema_name: Option<String>,
    /// Object identity string (e.g. "public.orders").
    object_identity: Option<String>,
}

/// Collect DDL commands from the event trigger context.
fn collect_ddl_commands() -> Result<Vec<DdlCommand>, PgStreamError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT objid, object_type, command_tag, schema_name::text, object_identity \
                 FROM pg_event_trigger_ddl_commands()",
                None,
                &[],
            )
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

        let mut commands = Vec::new();
        for row in table {
            let map_spi = |e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string());

            let objid = row
                .get::<pg_sys::Oid>(1)
                .map_err(map_spi)?
                .unwrap_or(pg_sys::InvalidOid);
            let object_type = row.get::<String>(2).map_err(map_spi)?.unwrap_or_default();
            let command_tag = row.get::<String>(3).map_err(map_spi)?.unwrap_or_default();
            let schema_name = row.get::<String>(4).map_err(map_spi)?;
            let object_identity = row.get::<String>(5).map_err(map_spi)?;

            commands.push(DdlCommand {
                objid,
                object_type,
                command_tag,
                schema_name,
                object_identity,
            });
        }
        Ok(commands)
    })
}

/// Process a single DDL command: check for upstream/ST impact and react.
fn handle_ddl_command(cmd: &DdlCommand) {
    match (cmd.object_type.as_str(), cmd.command_tag.as_str()) {
        // ── Table DDL ─────────────────────────────────────────────────
        ("table", "ALTER TABLE") => {
            let identity = cmd.object_identity.as_deref().unwrap_or("unknown");
            handle_alter_table(cmd.objid, identity);
        }
        ("table", "CREATE TABLE") => {
            // New tables can't be upstream of any existing ST yet.
        }

        // ── CREATE TRIGGER on a stream table → warning ────────────────
        ("trigger", "CREATE TRIGGER") => {
            handle_create_trigger(cmd);
        }

        _ => {}
    }
}

// ── ALTER TABLE handling ───────────────────────────────────────────────────

/// Handle ALTER TABLE on an object that may be an upstream dependency or
/// a ST storage table itself.
fn handle_alter_table(objid: pg_sys::Oid, identity: &str) {
    // Check if this OID is an upstream source of any ST.
    let affected_pgs_ids = match find_downstream_pgs_ids(objid) {
        Ok(ids) => ids,
        Err(e) => {
            pgrx::warning!(
                "pg_stream_ddl_tracker: failed to query dependencies for {}: {}",
                identity,
                e
            );
            return;
        }
    };

    if affected_pgs_ids.is_empty() {
        // Not an upstream of any ST — might be a ST storage table being altered.
        // That's allowed (e.g., adding indexes), so ignore.
        return;
    }

    // Classify the schema change and only reinitialize for column-affecting
    // changes.  Benign DDL (adding indexes, comments, statistics) and
    // constraint-only changes skip reinit when column tracking is populated.
    let mut reinit_pgs_ids = Vec::new();
    for pgs_id in &affected_pgs_ids {
        let kind = match detect_schema_change_kind(objid, *pgs_id) {
            Ok(k) => k,
            Err(e) => {
                // On error, fall back to conservative reinit.
                pgrx::debug1!(
                    "pg_stream_ddl_tracker: schema change detection failed for ST {}: {}, \
                     falling back to reinit",
                    pgs_id,
                    e,
                );
                SchemaChangeKind::ColumnChange
            }
        };

        match kind {
            SchemaChangeKind::Benign => {
                pgrx::debug1!(
                    "pg_stream_ddl_tracker: ALTER TABLE on {} is benign for ST {} — skipping reinit",
                    identity,
                    pgs_id,
                );
            }
            SchemaChangeKind::ConstraintChange => {
                // Constraint-only change (e.g., adding/dropping a PK or unique
                // constraint). Currently treat same as benign — the row_id
                // strategy was chosen at creation time based on the PK that
                // existed then. A future enhancement (C-5 keyless tables)
                // could reinit here if the row_id strategy depends on PK.
                pgrx::debug1!(
                    "pg_stream_ddl_tracker: ALTER TABLE on {} is constraint-only for ST {} \
                     — skipping reinit",
                    identity,
                    pgs_id,
                );
            }
            SchemaChangeKind::ColumnChange => {
                // S8: When block_source_ddl GUC is enabled, ERROR instead of reinit.
                if config::pg_stream_block_source_ddl() {
                    pgrx::error!(
                        "pg_stream: ALTER TABLE on {} blocked — column-affecting DDL is not \
                         allowed on source tables tracked by stream tables when \
                         pg_stream.block_source_ddl = true. Set pg_stream.block_source_ddl = \
                         false to allow schema changes (triggers reinitialization).",
                        identity,
                    );
                }

                if let Err(e) = StreamTableMeta::mark_for_reinitialize(*pgs_id) {
                    pgrx::warning!(
                        "pg_stream_ddl_tracker: failed to mark ST {} for reinit: {}",
                        pgs_id,
                        e,
                    );
                }
                reinit_pgs_ids.push(*pgs_id);
            }
        }
    }

    // Cascade: find STs that depend on the affected STs (transitive).
    // Only cascade from STs that were actually marked for reinit.
    let cascade_ids = if reinit_pgs_ids.is_empty() {
        Vec::new()
    } else {
        match find_transitive_downstream_sts(&reinit_pgs_ids) {
            Ok(ids) => ids,
            Err(e) => {
                pgrx::warning!("pg_stream_ddl_tracker: failed to cascade reinit: {}", e);
                Vec::new()
            }
        }
    };

    for pgs_id in &cascade_ids {
        if let Err(e) = StreamTableMeta::mark_for_reinitialize(*pgs_id) {
            pgrx::warning!(
                "pg_stream_ddl_tracker: failed to cascade reinit to ST {}: {}",
                pgs_id,
                e,
            );
        }
    }

    // Rebuild the CDC trigger function to reflect the current column set.
    // When a column is dropped from the source table, the old trigger function
    // still references NEW."<dropped_col>" — any subsequent DML on the source
    // will fail with "record 'new' has no field '<dropped_col>'".
    // CREATE OR REPLACE replaces only the function body; the trigger binding and
    // change buffer table are unaffected.
    //
    // Always rebuild — even for benign changes — to stay in sync with the
    // catalog. The cost is negligible (single CREATE OR REPLACE FUNCTION).
    let change_schema = config::pg_stream_change_buffer_schema();
    if let Err(e) = cdc::rebuild_cdc_trigger_function(objid, &change_schema) {
        pgrx::warning!(
            "pg_stream_ddl_tracker: failed to rebuild CDC trigger function for {}: {}",
            identity,
            e,
        );
    }

    // If any dependency on this source uses WAL-based CDC, abort the
    // transition and fall back to triggers. The schema change invalidates
    // the WAL decoder's column mapping — pgoutput will send a new Relation
    // message, but it's safer to reinitialize from triggers.
    if !reinit_pgs_ids.is_empty() {
        handle_alter_table_wal_fallback(objid, identity, &change_schema);
    }

    let total = reinit_pgs_ids.len() + cascade_ids.len();
    if total > 0 {
        log!(
            "pg_stream_ddl_tracker: ALTER TABLE on {} → {} ST(s) marked for reinitialize",
            identity,
            total,
        );
    } else {
        log!(
            "pg_stream_ddl_tracker: ALTER TABLE on {} → benign for all {} dependent ST(s), \
             no reinitialize needed",
            identity,
            affected_pgs_ids.len(),
        );
    }
}

/// When ALTER TABLE is detected on a source using WAL-based CDC, abort the
/// WAL transition and fall back to trigger-based CDC.
///
/// `pgoutput` sends a Relation message when the schema changes, but our WAL
/// decoder may not yet handle dynamic column remapping. It's safer to fall
/// back to triggers, reinitialize the downstream STs, and let the transition
/// restart once the reinitialize completes.
fn handle_alter_table_wal_fallback(source_oid: pg_sys::Oid, identity: &str, change_schema: &str) {
    let deps = match StDependency::get_all() {
        Ok(d) => d,
        Err(_) => return,
    };

    for dep in &deps {
        if dep.source_relid != source_oid {
            continue;
        }
        match dep.cdc_mode {
            CdcMode::Wal | CdcMode::Transitioning => {
                if let Err(e) =
                    wal_decoder::abort_wal_transition(dep.source_relid, dep.pgs_id, change_schema)
                {
                    pgrx::warning!(
                        "pg_stream_ddl_tracker: failed to abort WAL transition for {} (pgs_id={}): {}",
                        identity,
                        dep.pgs_id,
                        e,
                    );
                } else {
                    log!(
                        "pg_stream_ddl_tracker: ALTER TABLE on {} — \
                         aborted WAL transition (pgs_id={}), reverted to triggers",
                        identity,
                        dep.pgs_id,
                    );
                }
            }
            CdcMode::Trigger => {
                // Already on triggers — nothing to do
            }
        }
    }
}

// ── CREATE TRIGGER warning ─────────────────────────────────────────────────

/// Handle CREATE TRIGGER: if the trigger is on a stream table, emit a
/// warning about trigger behavior during refresh.
fn handle_create_trigger(cmd: &DdlCommand) {
    // The event trigger's objid is the trigger OID (pg_trigger.oid), not the
    // table OID. Look up the table via pg_trigger.tgrelid.
    let tgrelid = match Spi::get_one::<pg_sys::Oid>(&format!(
        "SELECT tgrelid FROM pg_trigger WHERE oid = {}",
        cmd.objid.to_u32(),
    )) {
        Ok(Some(oid)) => oid,
        _ => return, // Can't resolve — ignore silently
    };

    // Check if the table is a stream table.
    if !is_dt_storage_table(tgrelid) {
        return;
    }

    let trigger_identity = cmd
        .object_identity
        .as_deref()
        .unwrap_or("(unknown trigger)");
    let user_triggers_mode = config::pg_stream_user_triggers();

    if user_triggers_mode == "off" {
        pgrx::warning!(
            "pg_stream: trigger {} is on a stream table, but pg_stream.user_triggers = 'off'. \
             This trigger will NOT fire correctly during refresh. \
             Set pg_stream.user_triggers = 'auto' or 'on' to enable trigger support.",
            trigger_identity,
        );
    } else {
        pgrx::notice!(
            "pg_stream: trigger {} is on a stream table. \
             It will fire during DIFFERENTIAL refresh with correct TG_OP/OLD/NEW. \
             Note: row-level triggers do NOT fire during FULL refresh. \
             Use REFRESH MODE DIFFERENTIAL to ensure triggers fire on every change.",
            trigger_identity,
        );
    }
}

// ── DROP TABLE handling (via SQL event trigger for dropped objects) ─────

/// Handler for the `sql_drop` event trigger.
///
/// Detects when upstream source tables or ST storage tables themselves
/// are dropped and reacts accordingly.
#[pg_extern(schema = "pgstream", name = "_on_sql_drop", sql = false)]
fn pg_stream_on_sql_drop() {
    let dropped = match collect_dropped_objects() {
        Ok(objs) => objs,
        Err(e) => {
            pgrx::debug1!(
                "pg_stream_ddl_tracker: could not read dropped objects: {}",
                e
            );
            return;
        }
    };

    for obj in &dropped {
        if obj.object_type != "table" {
            continue;
        }
        handle_dropped_table(obj);
    }
}

/// A dropped object from `pg_event_trigger_dropped_objects()`.
#[derive(Debug, Clone)]
struct DroppedObject {
    objid: pg_sys::Oid,
    object_type: String,
    schema_name: Option<String>,
    object_name: Option<String>,
    object_identity: Option<String>,
}

/// Collect dropped objects from the event trigger context.
fn collect_dropped_objects() -> Result<Vec<DroppedObject>, PgStreamError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT objid, object_type, schema_name::text, object_name::text, object_identity \
                 FROM pg_event_trigger_dropped_objects()",
                None,
                &[],
            )
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

        let mut objects = Vec::new();
        for row in table {
            let map_spi = |e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string());

            let objid = row
                .get::<pg_sys::Oid>(1)
                .map_err(map_spi)?
                .unwrap_or(pg_sys::InvalidOid);
            let object_type = row.get::<String>(2).map_err(map_spi)?.unwrap_or_default();
            let schema_name = row.get::<String>(3).map_err(map_spi)?;
            let object_name = row.get::<String>(4).map_err(map_spi)?;
            let object_identity = row.get::<String>(5).map_err(map_spi)?;

            objects.push(DroppedObject {
                objid,
                object_type,
                schema_name,
                object_name,
                object_identity,
            });
        }
        Ok(objects)
    })
}

/// Handle a dropped table: either an upstream source or a ST storage table.
fn handle_dropped_table(obj: &DroppedObject) {
    let identity = obj.object_identity.as_deref().unwrap_or("unknown");

    // Case 1: Check if the dropped table is a ST storage table.
    let is_dt = is_dt_storage_table(obj.objid);
    if is_dt {
        handle_dt_storage_dropped(obj.objid, identity);
        return;
    }

    // Case 2: Check if the dropped table is an upstream source of any ST.
    let affected_pgs_ids = match find_downstream_pgs_ids(obj.objid) {
        Ok(ids) => ids,
        Err(e) => {
            pgrx::warning!(
                "pg_stream_ddl_tracker: failed to query deps for dropped {}: {}",
                identity,
                e,
            );
            return;
        }
    };

    if affected_pgs_ids.is_empty() {
        return;
    }

    // Mark affected STs as ERROR — their source is gone.
    for pgs_id in &affected_pgs_ids {
        if let Err(e) = StreamTableMeta::update_status(*pgs_id, StStatus::Error) {
            pgrx::warning!(
                "pg_stream_ddl_tracker: failed to set ST {} to ERROR: {}",
                pgs_id,
                e,
            );
        }
    }

    // Cascade: STs depending on now-errored STs also go to ERROR.
    let cascade_ids = match find_transitive_downstream_sts(&affected_pgs_ids) {
        Ok(ids) => ids,
        Err(e) => {
            pgrx::warning!("pg_stream_ddl_tracker: failed to cascade error: {}", e);
            Vec::new()
        }
    };

    for pgs_id in &cascade_ids {
        if let Err(e) = StreamTableMeta::update_status(*pgs_id, StStatus::Error) {
            pgrx::warning!(
                "pg_stream_ddl_tracker: failed to cascade ERROR to ST {}: {}",
                pgs_id,
                e,
            );
        }
    }

    let total = affected_pgs_ids.len() + cascade_ids.len();
    log!(
        "pg_stream_ddl_tracker: DROP TABLE {} → {} ST(s) set to ERROR",
        identity,
        total,
    );
}

/// Handle the case where a ST's own storage table was dropped.
///
/// Clean up the catalog entry and signal a DAG rebuild.
fn handle_dt_storage_dropped(relid: pg_sys::Oid, identity: &str) {
    // Find and delete the ST catalog entry.
    let st = match StreamTableMeta::get_by_relid(relid) {
        Ok(st) => st,
        Err(_) => return, // Already cleaned up or not found
    };

    if let Err(e) = StreamTableMeta::delete(st.pgs_id) {
        pgrx::warning!(
            "pg_stream_ddl_tracker: failed to clean up catalog for dropped ST {}: {}",
            identity,
            e,
        );
        return;
    }

    // Signal the scheduler to rebuild the DAG.
    shmem::signal_dag_rebuild();

    log!(
        "pg_stream_ddl_tracker: ST storage table {} dropped → catalog cleaned, DAG rebuild signaled",
        identity,
    );
}

// ── Dependency queries ─────────────────────────────────────────────────────

/// Find ST IDs that directly depend on a given source OID.
fn find_downstream_pgs_ids(source_oid: pg_sys::Oid) -> Result<Vec<i64>, PgStreamError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT pgs_id FROM pgstream.pgs_dependencies WHERE source_relid = $1",
                None,
                &[source_oid.into()],
            )
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

        let mut ids = Vec::new();
        for row in table {
            if let Ok(Some(id)) = row.get::<i64>(1) {
                ids.push(id);
            }
        }
        Ok(ids)
    })
}

/// Find all transitively downstream STs: given a set of directly-affected
/// ST IDs, walk the dependency graph to find STs that depend on them.
///
/// Returns only the *additional* ST IDs (not the input set).
fn find_transitive_downstream_sts(initial_pgs_ids: &[i64]) -> Result<Vec<i64>, PgStreamError> {
    if initial_pgs_ids.is_empty() {
        return Ok(Vec::new());
    }

    // We need to find which STs have a dependency edge to the storage
    // table (pgs_relid) of any of the initial STs, and then repeat
    // transitively.
    //
    // Query: for each affected ST, get its pgs_relid, then find STs
    // that list that relid as a source.

    let mut visited: std::collections::HashSet<i64> = initial_pgs_ids.iter().copied().collect();
    let mut queue: std::collections::VecDeque<i64> = initial_pgs_ids.iter().copied().collect();
    let mut cascade_ids = Vec::new();

    while let Some(pgs_id) = queue.pop_front() {
        // Get the storage table OID for this ST.
        let relid = match get_pgs_relid(pgs_id) {
            Ok(Some(oid)) => oid,
            _ => continue,
        };

        // Find STs that depend on this ST's storage table.
        let downstream = find_downstream_pgs_ids(relid)?;
        for child_id in downstream {
            if visited.insert(child_id) {
                cascade_ids.push(child_id);
                queue.push_back(child_id);
            }
        }
    }

    Ok(cascade_ids)
}

/// Get the storage table OID (pgs_relid) for a stream table.
fn get_pgs_relid(pgs_id: i64) -> Result<Option<pg_sys::Oid>, PgStreamError> {
    Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT pgs_relid FROM pgstream.pgs_stream_tables WHERE pgs_id = $1",
        &[pgs_id.into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))
}

/// Check if a given OID is a ST storage table.
fn is_dt_storage_table(relid: pg_sys::Oid) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgstream.pgs_stream_tables WHERE pgs_relid = $1)",
        &[relid.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false)
}

// ── Schema change detection helpers ────────────────────────────────────────

/// Detect what kind of schema change occurred on a table.
///
/// This can be used to determine whether a reinitialize is truly needed
/// (e.g., column add/drop/type change) vs. a benign change (e.g., adding
/// a constraint or comment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaChangeKind {
    /// Column added, dropped, or type changed — requires reinitialize.
    ColumnChange,
    /// Constraint or index change — may not require reinitialize.
    ConstraintChange,
    /// Other DDL (comment, owner change, etc.) — no reinitialize needed.
    Benign,
}

/// Detect the kind of schema change by comparing stored column metadata
/// against the current catalog state.
///
/// When a column snapshot exists (S7), performs precise comparison:
/// - Missing columns → `ColumnChange`
/// - Type OID changed → `ColumnChange`
/// - New columns added → `ColumnChange` (may affect NATURAL JOIN, SELECT *)
/// - Same fingerprint → `Benign` (fast path)
/// - All match → `ConstraintChange` (e.g., PK/unique constraint changes)
///
/// Without snapshots, falls back to checking `columns_used` existence.
pub fn detect_schema_change_kind(
    source_oid: pg_sys::Oid,
    pgs_id: i64,
) -> Result<SchemaChangeKind, PgStreamError> {
    // Fast path: compare schema fingerprints.
    // If the stored fingerprint matches the current one, nothing column-related changed.
    if let Ok(Some(stored_fp)) = crate::catalog::get_schema_fingerprint(pgs_id, source_oid)
        && !stored_fp.is_empty()
        && let Ok((_, current_fp)) = crate::catalog::build_column_snapshot(source_oid)
        && stored_fp == current_fp
    {
        return Ok(SchemaChangeKind::Benign);
    }

    // Detailed path: compare stored column snapshot against current pg_attribute.
    if let Ok(Some(snapshot)) = crate::catalog::get_column_snapshot(pgs_id, source_oid)
        && let serde_json::Value::Array(ref entries) = snapshot.0
        && !entries.is_empty()
    {
        return detect_from_snapshot(source_oid, entries);
    }

    // Legacy fallback: no snapshot available — use columns_used presence check.
    let tracked_cols = get_tracked_columns(pgs_id, source_oid)?;

    if tracked_cols.is_empty() {
        // No column-level tracking — conservatively assume column change.
        return Ok(SchemaChangeKind::ColumnChange);
    }

    // Check if any tracked columns were altered or dropped.
    for col_name in &tracked_cols {
        let exists = Spi::get_one_with_args::<bool>(
            "SELECT EXISTS( \
                SELECT 1 FROM pg_attribute \
                WHERE attrelid = $1 AND attname = $2 \
                AND attnum > 0 AND NOT attisdropped \
            )",
            &[source_oid.into(), col_name.as_str().into()],
        )
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?
        .unwrap_or(false);

        if !exists {
            return Ok(SchemaChangeKind::ColumnChange);
        }
    }

    // All tracked columns still exist — likely a benign change.
    Ok(SchemaChangeKind::ConstraintChange)
}

/// Compare a stored column snapshot against the current `pg_attribute` state.
///
/// Detects: columns dropped, columns added, type OID changed.
fn detect_from_snapshot(
    source_oid: pg_sys::Oid,
    stored_entries: &[serde_json::Value],
) -> Result<SchemaChangeKind, PgStreamError> {
    // Build a map of current columns: name → type_oid
    let current_cols: std::collections::HashMap<String, i64> = Spi::connect(|client| {
        let sql = format!(
            "SELECT attname::text, atttypid::bigint \
             FROM pg_attribute \
             WHERE attrelid = {} AND attnum > 0 AND NOT attisdropped \
             ORDER BY attnum",
            source_oid.to_u32(),
        );
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        let mut map = std::collections::HashMap::new();
        for row in result {
            let name: String = row
                .get(1)
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let type_oid: i64 = row
                .get(2)
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .unwrap_or(0);
            map.insert(name, type_oid);
        }
        Ok(map)
    })?;

    // Check each stored column still exists with the same type.
    for entry in stored_entries {
        let name = entry["name"].as_str().unwrap_or("");
        let stored_type = entry["type_oid"].as_i64().unwrap_or(0);

        match current_cols.get(name) {
            None => return Ok(SchemaChangeKind::ColumnChange), // dropped
            Some(&current_type) if current_type != stored_type => {
                return Ok(SchemaChangeKind::ColumnChange); // type changed
            }
            _ => {} // matches
        }
    }

    // Check if new columns were added (may affect NATURAL JOIN, SELECT *).
    let stored_names: std::collections::HashSet<&str> = stored_entries
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();

    for name in current_cols.keys() {
        if !stored_names.contains(name.as_str()) {
            return Ok(SchemaChangeKind::ColumnChange); // new column added
        }
    }

    // Column set is identical — this is a constraint-only change.
    Ok(SchemaChangeKind::ConstraintChange)
}

/// Get column names tracked for a given ST + source pair.
fn get_tracked_columns(pgs_id: i64, source_oid: pg_sys::Oid) -> Result<Vec<String>, PgStreamError> {
    // columns_used is stored as TEXT[] in pgs_dependencies.
    let cols = Spi::get_one_with_args::<Vec<String>>(
        "SELECT columns_used FROM pgstream.pgs_dependencies \
         WHERE pgs_id = $1 AND source_relid = $2",
        &[pgs_id.into(), source_oid.into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

    Ok(cols.unwrap_or_default())
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_change_kind_eq() {
        assert_eq!(
            SchemaChangeKind::ColumnChange,
            SchemaChangeKind::ColumnChange
        );
        assert_ne!(SchemaChangeKind::ColumnChange, SchemaChangeKind::Benign);
        assert_ne!(SchemaChangeKind::ConstraintChange, SchemaChangeKind::Benign,);
    }

    #[test]
    fn test_ddl_command_debug() {
        let cmd = DdlCommand {
            objid: pg_sys::InvalidOid,
            object_type: "table".to_string(),
            command_tag: "ALTER TABLE".to_string(),
            schema_name: Some("public".to_string()),
            object_identity: Some("public.orders".to_string()),
        };
        let debug = format!("{:?}", cmd);
        assert!(debug.contains("ALTER TABLE"));
        assert!(debug.contains("public.orders"));
    }

    #[test]
    fn test_dropped_object_debug() {
        let obj = DroppedObject {
            objid: pg_sys::InvalidOid,
            object_type: "table".to_string(),
            schema_name: Some("public".to_string()),
            object_name: Some("orders".to_string()),
            object_identity: Some("public.orders".to_string()),
        };
        let debug = format!("{:?}", obj);
        assert!(debug.contains("public.orders"));
    }
}
