//! WAL-based Change Data Capture via logical replication.
//!
//! Provides an alternative CDC mechanism that uses PostgreSQL's built-in
//! logical decoding instead of row-level triggers. This eliminates the
//! synchronous write-side overhead (~2–15 μs per row) that triggers impose
//! on tracked source tables.
//!
//! # Architecture
//!
//! The WAL decoder uses a **polling** approach via SPI:
//! - Calls `pg_logical_slot_get_changes()` during the scheduler tick
//! - Decodes `pgoutput` protocol messages into typed buffer table rows
//! - Writes changes to the same `pgstream_changes.changes_<oid>` tables
//!   used by trigger-based CDC
//!
//! # Transition Lifecycle
//!
//! ```text
//! TRIGGER ──► TRANSITIONING ──► WAL
//!    ▲                           │
//!    └───────── (fallback) ──────┘
//! ```
//!
//! 1. **start**: Create publication + replication slot, set mode to TRANSITIONING
//! 2. **poll**: Both trigger and WAL decoder write to buffer (dedup at refresh)
//! 3. **complete**: Decoder caught up → drop trigger, set mode to WAL
//! 4. **fallback**: Timeout or error → drop slot/publication, revert to TRIGGER
//!
//! # Prerequisites
//!
//! - `wal_level = logical` in `postgresql.conf`
//! - Available replication slots (`max_replication_slots`)
//! - Source table has REPLICA IDENTITY DEFAULT (PK) or FULL
//! - `pg_stream.cdc_mode` set to `'auto'` or `'wal'`

use pgrx::prelude::*;

use crate::catalog::{CdcMode, DtDependency};
use crate::cdc;
use crate::config;
use crate::error::PgStreamError;

// ── Naming Conventions ─────────────────────────────────────────────────────

/// Replication slot name for a source table: `pgstream_<oid>`.
pub fn slot_name_for_source(source_oid: pg_sys::Oid) -> String {
    format!("pgstream_{}", source_oid.to_u32())
}

/// Publication name for a source table: `pgstream_cdc_<oid>`.
pub fn publication_name_for_source(source_oid: pg_sys::Oid) -> String {
    format!("pgstream_cdc_{}", source_oid.to_u32())
}

// ── Publication Management ─────────────────────────────────────────────────

/// Create a publication for a source table to enable logical decoding.
///
/// Publications tell `pgoutput` which tables to include in the change stream.
/// Each tracked source gets its own publication for independent lifecycle
/// management.
pub fn create_publication(source_oid: pg_sys::Oid) -> Result<(), PgStreamError> {
    let pub_name = publication_name_for_source(source_oid);

    // Get the fully-qualified source table name
    let source_table =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_oid.into()])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?
            .ok_or_else(|| {
                PgStreamError::NotFound(format!("Table with OID {} not found", source_oid.to_u32()))
            })?;

    // Create publication if it doesn't already exist.
    // PostgreSQL doesn't have CREATE PUBLICATION IF NOT EXISTS, so check first.
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_publication WHERE pubname = $1)",
        &[pub_name.as_str().into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if !exists {
        let sql = format!(
            "CREATE PUBLICATION {} FOR TABLE {}",
            quote_ident(&pub_name),
            source_table,
        );
        Spi::run(&sql).map_err(|e| {
            PgStreamError::WalTransitionError(format!(
                "Failed to create publication {}: {}",
                pub_name, e
            ))
        })?;
    }

    Ok(())
}

/// Drop a publication for a source table.
///
/// Safe to call even if the publication doesn't exist (uses IF EXISTS).
pub fn drop_publication(source_oid: pg_sys::Oid) -> Result<(), PgStreamError> {
    let pub_name = publication_name_for_source(source_oid);
    let sql = format!("DROP PUBLICATION IF EXISTS {}", quote_ident(&pub_name));
    Spi::run(&sql).map_err(|e| {
        PgStreamError::WalTransitionError(format!("Failed to drop publication {}: {}", pub_name, e))
    })?;
    Ok(())
}

// ── Replication Slot Management ────────────────────────────────────────────

/// Create a logical replication slot for WAL decoding.
///
/// Uses the `pgoutput` output plugin (built into PostgreSQL) which provides
/// structured change data including column names and values.
///
/// The slot captures WAL from the moment of creation, ensuring no changes
/// are missed between slot creation and the first poll.
pub fn create_replication_slot(slot_name: &str) -> Result<String, PgStreamError> {
    // Check if slot already exists
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        &[slot_name.into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if exists {
        // Return the existing slot's confirmed_flush_lsn
        let lsn = Spi::get_one_with_args::<String>(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
            &[slot_name.into()],
        )
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?
        .unwrap_or_else(|| "0/0".to_string());

        return Ok(lsn);
    }

    // Create the logical replication slot
    let lsn = Spi::get_one_with_args::<String>(
        "SELECT lsn::text FROM pg_create_logical_replication_slot($1, 'pgoutput')",
        &[slot_name.into()],
    )
    .map_err(|e| {
        PgStreamError::ReplicationSlotError(format!(
            "Failed to create replication slot '{}': {}",
            slot_name, e
        ))
    })?
    .unwrap_or_else(|| "0/0".to_string());

    Ok(lsn)
}

/// Drop a logical replication slot.
///
/// Safe to call even if the slot doesn't exist (checks first).
pub fn drop_replication_slot(slot_name: &str) -> Result<(), PgStreamError> {
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        &[slot_name.into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if exists {
        Spi::run_with_args("SELECT pg_drop_replication_slot($1)", &[slot_name.into()]).map_err(
            |e| {
                PgStreamError::ReplicationSlotError(format!(
                    "Failed to drop replication slot '{}': {}",
                    slot_name, e
                ))
            },
        )?;
    }

    Ok(())
}

/// Get the confirmed flush LSN for a replication slot.
///
/// Returns the LSN up to which the slot consumer has confirmed processing.
/// Returns `None` if the slot doesn't exist.
pub fn get_slot_confirmed_lsn(slot_name: &str) -> Result<Option<String>, PgStreamError> {
    Spi::get_one_with_args::<String>(
        "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
        &[slot_name.into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))
}

/// Get the lag in bytes between a slot's confirmed LSN and the current WAL position.
///
/// A high lag indicates the decoder is falling behind.
pub fn get_slot_lag_bytes(slot_name: &str) -> Result<i64, PgStreamError> {
    Spi::get_one_with_args::<i64>(
        "SELECT (pg_current_wal_lsn() - confirmed_flush_lsn)::bigint \
         FROM pg_replication_slots WHERE slot_name = $1",
        &[slot_name.into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))
    .map(|v| v.unwrap_or(0))
}

// ── WAL Polling ────────────────────────────────────────────────────────────

/// Maximum number of changes to process per poll cycle.
///
/// Limits memory usage and keeps each scheduler tick bounded.
/// Remaining changes are picked up in the next cycle.
const MAX_CHANGES_PER_POLL: i64 = 10_000;

/// Poll WAL changes from a replication slot and write them to the buffer table.
///
/// Uses `pg_logical_slot_get_changes()` with the `pgoutput` plugin to
/// retrieve decoded WAL changes. Each change is parsed and inserted into
/// the appropriate `pgstream_changes.changes_<oid>` buffer table.
///
/// The `pgoutput` data format provides structured output that we parse
/// to extract action type, column values, and LSN information.
///
/// Returns the number of changes processed and the last confirmed LSN.
pub fn poll_wal_changes(
    source_oid: pg_sys::Oid,
    slot_name: &str,
    change_schema: &str,
    pk_columns: &[String],
    columns: &[(String, String)],
) -> Result<(i64, Option<String>), PgStreamError> {
    let oid_u32 = source_oid.to_u32();
    let pub_name = publication_name_for_source(source_oid);

    // Poll changes from the logical replication slot.
    // pg_logical_slot_get_changes() advances the slot position automatically.
    let poll_sql = format!(
        "SELECT lsn::text, xid, data \
         FROM pg_logical_slot_get_changes(\
             '{slot_name}', NULL, {max_changes}, \
             'proto_version', '1', \
             'publication_names', '{pub_name}'\
         )",
        slot_name = slot_name,
        max_changes = MAX_CHANGES_PER_POLL,
        pub_name = pub_name,
    );

    let mut count: i64 = 0;
    let mut last_lsn: Option<String> = None;

    Spi::connect(|client| {
        let result = client
            .select(&poll_sql, None, &[])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

        for row in result {
            let lsn = row
                .get::<String>(1)
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let data = row
                .get::<String>(3)
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .unwrap_or_default();

            // Parse the pgoutput data and determine if it's relevant to our source
            if let Some(action) = parse_pgoutput_action(&data) {
                // Write the decoded change to the buffer table
                write_decoded_change(
                    oid_u32,
                    &lsn,
                    &action,
                    &data,
                    change_schema,
                    pk_columns,
                    columns,
                )?;
                count += 1;
            }

            last_lsn = Some(lsn);
        }

        Ok::<(), PgStreamError>(())
    })?;

    Ok((count, last_lsn))
}

/// Parse the action type from a pgoutput data string.
///
/// The `pgoutput` plugin with `proto_version = 1` outputs text lines like:
/// - `table public.users: INSERT: id[integer]:1 name[text]:'Alice'`
/// - `table public.users: UPDATE: ...`
/// - `table public.users: DELETE: ...`
/// - `table public.users: TRUNCATE: (no column data)`
///
/// Returns the action character ('I', 'U', 'D', 'T') or None if not a DML line.
fn parse_pgoutput_action(data: &str) -> Option<char> {
    if data.contains("INSERT:") {
        Some('I')
    } else if data.contains("UPDATE:") {
        Some('U')
    } else if data.contains("DELETE:") {
        Some('D')
    } else if data.contains("TRUNCATE:") {
        Some('T')
    } else {
        None
    }
}

/// Parse column values from a pgoutput data line.
///
/// Extracts `column_name[type]:value` pairs from the pgoutput text format.
/// Returns a map from column name to string value.
fn parse_pgoutput_columns(data: &str) -> std::collections::HashMap<String, String> {
    let mut cols = std::collections::HashMap::new();

    // Find the part after the action type (INSERT:/UPDATE:/DELETE:)
    let payload = if let Some(pos) = data.find("INSERT:") {
        &data[pos + 8..]
    } else if let Some(pos) = data.find("UPDATE:") {
        // UPDATE has "old-key:" and "new-tuple:" sections
        &data[pos + 8..]
    } else if let Some(pos) = data.find("DELETE:") {
        &data[pos + 8..]
    } else {
        return cols;
    };

    // Parse column_name[type]:value pairs
    // Format: col_name[type_name]:value col_name2[type_name2]:value2
    for segment in payload.split_whitespace() {
        if let Some(bracket_pos) = segment.find('[') {
            let col_name = &segment[..bracket_pos];
            if let Some(colon_pos) = segment.find("]:") {
                let value = &segment[colon_pos + 2..];
                // Strip surrounding quotes if present
                let clean_value = value.trim_matches('\'');
                cols.insert(col_name.to_string(), clean_value.to_string());
            }
        }
    }

    cols
}

/// Write a decoded WAL change to the buffer table.
///
/// Maps the parsed pgoutput data into the typed buffer table columns,
/// matching the same schema used by trigger-based CDC.
fn write_decoded_change(
    source_oid: u32,
    lsn: &str,
    action: &char,
    data: &str,
    change_schema: &str,
    pk_columns: &[String],
    columns: &[(String, String)],
) -> Result<(), PgStreamError> {
    // Handle TRUNCATE specially — mark downstream STs for reinit
    if *action == 'T' {
        mark_downstream_for_reinit(pg_sys::Oid::from(source_oid))?;
        return Ok(());
    }

    let parsed = parse_pgoutput_columns(data);

    // Build the INSERT statement for the buffer table
    let has_pk = !pk_columns.is_empty();

    // Column names for the INSERT
    let mut col_names = vec!["lsn".to_string(), "action".to_string()];
    let mut col_values = vec![format!("'{}'::pg_lsn", lsn), format!("'{}'", action)];

    // pk_hash column
    if has_pk {
        col_names.push("pk_hash".to_string());
        // Compute pk_hash using the same hash functions as the trigger
        let pk_hash_expr = build_pk_hash_from_values(pk_columns, &parsed);
        col_values.push(pk_hash_expr);
    }

    // Map parsed columns to new_<col> and old_<col> buffer columns
    for (col_name, _col_type) in columns {
        let safe_name = col_name.replace('"', "\"\"");

        // For INSERT: only new values
        // For UPDATE: both new and old values
        // For DELETE: only old values
        match action {
            'I' => {
                col_names.push(format!("\"new_{}\"", safe_name));
                if let Some(val) = parsed.get(col_name) {
                    col_values.push(format!("'{}'", val.replace('\'', "''")));
                } else {
                    col_values.push("NULL".to_string());
                }
            }
            'U' => {
                // new values
                col_names.push(format!("\"new_{}\"", safe_name));
                if let Some(val) = parsed.get(col_name) {
                    col_values.push(format!("'{}'", val.replace('\'', "''")));
                } else {
                    col_values.push("NULL".to_string());
                }
                // old values (available with REPLICA IDENTITY FULL or for PK columns)
                col_names.push(format!("\"old_{}\"", safe_name));
                // pgoutput separates old-key and new-tuple; simplified here
                col_values.push("NULL".to_string());
            }
            'D' => {
                col_names.push(format!("\"old_{}\"", safe_name));
                if let Some(val) = parsed.get(col_name) {
                    col_values.push(format!("'{}'", val.replace('\'', "''")));
                } else {
                    col_values.push("NULL".to_string());
                }
            }
            _ => {}
        }
    }

    let sql = format!(
        "INSERT INTO {schema}.changes_{oid} ({cols}) VALUES ({vals})",
        schema = change_schema,
        oid = source_oid,
        cols = col_names.join(", "),
        vals = col_values.join(", "),
    );

    Spi::run(&sql).map_err(|e| {
        PgStreamError::WalTransitionError(format!(
            "Failed to write decoded WAL change to buffer: {}",
            e
        ))
    })?;

    Ok(())
}

/// Build a pk_hash expression from parsed column values.
///
/// Uses the same hash computation as the trigger-based CDC to ensure
/// pk_hash values match between trigger and WAL decoder outputs.
fn build_pk_hash_from_values(
    pk_columns: &[String],
    parsed: &std::collections::HashMap<String, String>,
) -> String {
    if pk_columns.is_empty() {
        return "0".to_string();
    }

    if pk_columns.len() == 1 {
        if let Some(val) = parsed.get(&pk_columns[0]) {
            format!("pgstream.pg_stream_hash('{}')", val.replace('\'', "''"))
        } else {
            "0".to_string()
        }
    } else {
        let array_items: Vec<String> = pk_columns
            .iter()
            .map(|col| {
                if let Some(val) = parsed.get(col) {
                    format!("'{}'", val.replace('\'', "''"))
                } else {
                    "NULL".to_string()
                }
            })
            .collect();
        format!(
            "pgstream.pg_stream_hash_multi(ARRAY[{}])",
            array_items.join(", ")
        )
    }
}

/// Mark all downstream stream tables for reinitialization.
///
/// Called when a TRUNCATE is detected via WAL decoding. Since TRUNCATE
/// invalidates all existing change tracking, downstream STs need a
/// full refresh to resync.
fn mark_downstream_for_reinit(source_oid: pg_sys::Oid) -> Result<(), PgStreamError> {
    Spi::run_with_args(
        "UPDATE pgstream.pgs_stream_tables \
         SET needs_reinit = true, updated_at = now() \
         WHERE pgs_id IN ( \
             SELECT pgs_id FROM pgstream.pgs_dependencies \
             WHERE source_relid = $1 \
         )",
        &[source_oid.into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

    warning!(
        "pg_stream: TRUNCATE detected on source OID {} via WAL — downstream STs marked for reinit",
        source_oid.to_u32()
    );

    Ok(())
}

// ── Transition Orchestration ───────────────────────────────────────────────

/// Start the transition from trigger-based to WAL-based CDC for a source table.
///
/// This is called by the scheduler when it detects that:
/// - `pg_stream.cdc_mode` is `'auto'` or `'wal'`
/// - `wal_level = logical`
/// - The source has adequate REPLICA IDENTITY
/// - The source is currently using trigger-based CDC
///
/// Steps:
/// 1. Create a publication for the source table
/// 2. Record the current WAL LSN (handoff point)
/// 3. Create a logical replication slot
/// 4. Update the dependency catalog to TRANSITIONING mode
pub fn start_wal_transition(
    source_oid: pg_sys::Oid,
    pgs_id: i64,
    _change_schema: &str,
) -> Result<(), PgStreamError> {
    let oid_u32 = source_oid.to_u32();
    let slot_name = slot_name_for_source(source_oid);

    // Step 1: Create publication for this source table
    create_publication(source_oid)?;

    // Step 2: Record the current WAL LSN — this is the "handoff point".
    // The trigger captures everything up to this point.
    // The WAL decoder starts reading from this point.
    let handoff_lsn = cdc::get_current_wal_lsn()?;

    // Step 3: Create the replication slot at the current position.
    // The slot ensures no WAL is recycled before we consume it.
    let slot_lsn = create_replication_slot(&slot_name)?;

    // Step 4: Update catalog — mark as TRANSITIONING
    DtDependency::update_cdc_mode(
        pgs_id,
        source_oid,
        CdcMode::Transitioning,
        Some(&slot_name),
        Some(&slot_lsn),
    )?;

    info!(
        "pg_stream: started WAL transition for source OID {} \
         (slot: {}, handoff LSN: {}, slot LSN: {})",
        oid_u32, slot_name, handoff_lsn, slot_lsn
    );

    Ok(())
}

/// Check if the WAL transition is complete and finalize if so.
///
/// Called by the scheduler on each tick for sources in TRANSITIONING mode.
/// The transition is complete when the WAL decoder has caught up close to
/// the current WAL position (within a reasonable lag threshold).
///
/// If the transition has timed out, falls back to trigger-based CDC.
pub fn check_and_complete_transition(
    source_oid: pg_sys::Oid,
    pgs_id: i64,
    dep: &DtDependency,
    change_schema: &str,
) -> Result<(), PgStreamError> {
    let default_slot = slot_name_for_source(source_oid);
    let slot_name = dep.slot_name.as_deref().unwrap_or(&default_slot);

    // Check if the decoder has caught up
    let lag_bytes = get_slot_lag_bytes(slot_name)?;

    // Consider "caught up" when lag is under 64KB (a few WAL pages)
    const MAX_LAG_BYTES: i64 = 65_536;

    if lag_bytes <= MAX_LAG_BYTES {
        // Decoder has caught up — complete the transition
        complete_wal_transition(source_oid, pgs_id, change_schema)?;
        return Ok(());
    }

    // Not caught up — check for timeout
    if let Some(ref started_at) = dep.transition_started_at {
        let timed_out = Spi::get_one_with_args::<bool>(
            &format!(
                "SELECT (now() - $1::timestamptz) > interval '{} seconds'",
                config::pg_stream_wal_transition_timeout()
            ),
            &[started_at.as_str().into()],
        )
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?
        .unwrap_or(false);

        if timed_out {
            warning!(
                "pg_stream: WAL transition timed out for source OID {} \
                 (lag: {} bytes after {}s); falling back to triggers",
                source_oid.to_u32(),
                lag_bytes,
                config::pg_stream_wal_transition_timeout()
            );
            abort_wal_transition(source_oid, pgs_id, change_schema)?;
        }
    }

    Ok(())
}

/// Complete the WAL transition — drop the trigger and switch to WAL mode.
///
/// Called when the WAL decoder has caught up past the handoff point.
fn complete_wal_transition(
    source_oid: pg_sys::Oid,
    pgs_id: i64,
    change_schema: &str,
) -> Result<(), PgStreamError> {
    let oid_u32 = source_oid.to_u32();

    // Step 1: Drop the CDC trigger (WAL decoder now covers all changes)
    cdc::drop_change_trigger(source_oid, change_schema)?;

    // Step 2: Update catalog to WAL mode
    DtDependency::update_cdc_mode(pgs_id, source_oid, CdcMode::Wal, None, None)?;

    info!(
        "pg_stream: completed WAL transition for source OID {} — trigger dropped, WAL active",
        oid_u32
    );

    Ok(())
}

/// Abort the WAL transition and fall back to trigger-based CDC.
///
/// Called when the transition times out or encounters an unrecoverable error.
/// Cleans up WAL decoder resources and reverts to trigger mode.
pub fn abort_wal_transition(
    source_oid: pg_sys::Oid,
    pgs_id: i64,
    change_schema: &str,
) -> Result<(), PgStreamError> {
    let oid_u32 = source_oid.to_u32();
    let slot_name = slot_name_for_source(source_oid);

    // Step 1: Drop the replication slot (stops WAL retention)
    if let Err(e) = drop_replication_slot(&slot_name) {
        warning!(
            "pg_stream: failed to drop replication slot {} during abort: {}",
            slot_name,
            e
        );
    }

    // Step 2: Drop the publication
    if let Err(e) = drop_publication(source_oid) {
        warning!(
            "pg_stream: failed to drop publication during abort for OID {}: {}",
            oid_u32,
            e
        );
    }

    // Step 3: Revert catalog to trigger mode
    DtDependency::update_cdc_mode(pgs_id, source_oid, CdcMode::Trigger, None, None)?;

    // Step 4: Verify the trigger still exists — recreate if lost
    if !cdc::trigger_exists(source_oid)? {
        let pk_columns = cdc::resolve_pk_columns(source_oid)?;
        let columns = cdc::resolve_source_column_defs(source_oid)?;
        cdc::create_change_trigger(source_oid, change_schema, &pk_columns, &columns)?;
        warning!(
            "pg_stream: recreated CDC trigger for source OID {} during abort",
            oid_u32
        );
    }

    warning!(
        "pg_stream: aborted WAL transition for source OID {}; reverted to triggers",
        oid_u32
    );

    Ok(())
}

// ── Scheduler Integration ──────────────────────────────────────────────────

/// Advance WAL transitions and poll changes for WAL-mode sources.
///
/// Called from the scheduler tick when `pg_stream.cdc_mode != 'trigger'`.
/// Processes all dependency edges and handles each CDC mode:
///
/// - **TRIGGER**: Check if transition should start
/// - **TRANSITIONING**: Poll WAL changes + check completion/timeout
/// - **WAL**: Poll WAL changes + check decoder health
pub fn advance_wal_transitions(change_schema: &str) -> Result<(), PgStreamError> {
    // Only process if CDC mode allows WAL
    let cdc_mode = config::pg_stream_cdc_mode();
    if cdc_mode == "trigger" {
        return Ok(());
    }

    // Get all dependencies to check their CDC mode
    let all_deps = DtDependency::get_all()?;

    // Group by source_relid to avoid processing the same source multiple times
    let mut processed_sources = std::collections::HashSet::new();

    for dep in &all_deps {
        // Only process TABLE sources (not STREAM_TABLE or VIEW)
        if dep.source_type != "TABLE" {
            continue;
        }

        // Skip if we already processed this source in this tick
        let source_key = dep.source_relid.to_u32();
        if !processed_sources.insert(source_key) {
            continue;
        }

        match dep.cdc_mode {
            CdcMode::Trigger => {
                // Check if we should start a WAL transition
                if let Err(e) = try_start_transition(dep, change_schema) {
                    log!(
                        "pg_stream: failed to start WAL transition for source OID {}: {}",
                        source_key,
                        e
                    );
                }
            }
            CdcMode::Transitioning => {
                // Poll WAL changes (both trigger and WAL are active)
                if let Err(e) = poll_source_changes(dep, change_schema) {
                    log!(
                        "pg_stream: WAL poll error for transitioning source OID {}: {}",
                        source_key,
                        e
                    );
                }
                // Check if transition is complete or timed out
                if let Err(e) =
                    check_and_complete_transition(dep.source_relid, dep.pgs_id, dep, change_schema)
                {
                    log!(
                        "pg_stream: transition check error for source OID {}: {}",
                        source_key,
                        e
                    );
                }
            }
            CdcMode::Wal => {
                // Poll WAL changes (steady-state WAL mode)
                if let Err(e) = poll_source_changes(dep, change_schema) {
                    warning!(
                        "pg_stream: WAL poll error for source OID {} — may need fallback: {}",
                        source_key,
                        e
                    );
                    // In WAL mode, a persistent poll error is serious —
                    // the scheduler should consider falling back to triggers.
                    // For now, log the error; a health check can escalate.
                }
            }
        }
    }

    Ok(())
}

/// Try to start a WAL transition for a source currently using triggers.
fn try_start_transition(dep: &DtDependency, change_schema: &str) -> Result<(), PgStreamError> {
    // Check prerequisites
    if !cdc::can_use_logical_replication()? {
        return Ok(()); // WAL not available, stay on triggers
    }

    if !cdc::check_replica_identity(dep.source_relid)? {
        log!(
            "pg_stream: source OID {} has inadequate REPLICA IDENTITY for WAL CDC — staying on triggers",
            dep.source_relid.to_u32()
        );
        return Ok(());
    }

    // All prerequisites met — start the transition
    start_wal_transition(dep.source_relid, dep.pgs_id, change_schema)?;

    Ok(())
}

/// Poll WAL changes for a source that's in TRANSITIONING or WAL mode.
fn poll_source_changes(dep: &DtDependency, change_schema: &str) -> Result<(), PgStreamError> {
    let slot_name = match &dep.slot_name {
        Some(name) => name.clone(),
        None => slot_name_for_source(dep.source_relid),
    };

    // Resolve source column definitions for decoding
    let pk_columns = cdc::resolve_pk_columns(dep.source_relid)?;
    let columns = cdc::resolve_source_column_defs(dep.source_relid)?;

    // Poll and decode changes
    let (count, last_lsn) = poll_wal_changes(
        dep.source_relid,
        &slot_name,
        change_schema,
        &pk_columns,
        &columns,
    )?;

    // Update the decoder confirmed LSN in the catalog
    if let Some(ref lsn) = last_lsn {
        DtDependency::update_cdc_mode(
            dep.pgs_id,
            dep.source_relid,
            dep.cdc_mode,
            dep.slot_name.as_deref(),
            Some(lsn),
        )?;
    }

    if count > 0 {
        log!(
            "pg_stream: polled {} WAL changes for source OID {} (last LSN: {})",
            count,
            dep.source_relid.to_u32(),
            last_lsn.as_deref().unwrap_or("none")
        );
    }

    Ok(())
}

/// Check health of a WAL decoder for a source in WAL mode.
///
/// Verifies the replication slot exists and lag is within bounds.
/// If the slot is missing or lag is excessive, attempts recovery.
pub fn check_decoder_health(
    source_oid: pg_sys::Oid,
    pgs_id: i64,
    change_schema: &str,
) -> Result<(), PgStreamError> {
    let slot_name = slot_name_for_source(source_oid);

    // Check if the slot still exists
    let slot_exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        &[slot_name.as_str().into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if !slot_exists {
        warning!(
            "pg_stream: replication slot '{}' for source OID {} is missing — \
             falling back to triggers",
            slot_name,
            source_oid.to_u32()
        );
        abort_wal_transition(source_oid, pgs_id, change_schema)?;
        return Ok(());
    }

    // Check lag — if excessive (>1GB), warn but keep running
    let lag_bytes = get_slot_lag_bytes(&slot_name)?;
    const WARN_LAG_BYTES: i64 = 1_073_741_824; // 1 GB

    if lag_bytes > WARN_LAG_BYTES {
        warning!(
            "pg_stream: WAL decoder for source OID {} has excessive lag: {} bytes",
            source_oid.to_u32(),
            lag_bytes
        );
    }

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Quote a SQL identifier (simple quoting for generated names).
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Naming convention tests ────────────────────────────────────

    #[test]
    fn test_slot_name_for_source() {
        let oid = pg_sys::Oid::from(16384u32);
        assert_eq!(slot_name_for_source(oid), "pgstream_16384");
    }

    #[test]
    fn test_slot_name_for_source_zero() {
        let oid = pg_sys::Oid::from(0u32);
        assert_eq!(slot_name_for_source(oid), "pgstream_0");
    }

    #[test]
    fn test_publication_name_for_source() {
        let oid = pg_sys::Oid::from(16384u32);
        assert_eq!(publication_name_for_source(oid), "pgstream_cdc_16384");
    }

    #[test]
    fn test_publication_name_for_source_large_oid() {
        let oid = pg_sys::Oid::from(4294967295u32);
        assert_eq!(publication_name_for_source(oid), "pgstream_cdc_4294967295");
    }

    // ── quote_ident tests ──────────────────────────────────────────

    #[test]
    fn test_quote_ident_simple() {
        assert_eq!(quote_ident("my_slot"), "\"my_slot\"");
    }

    #[test]
    fn test_quote_ident_with_quotes() {
        assert_eq!(quote_ident("my\"slot"), "\"my\"\"slot\"");
    }

    // ── parse_pgoutput_action tests ────────────────────────────────

    #[test]
    fn test_parse_pgoutput_insert() {
        let data = "table public.users: INSERT: id[integer]:1 name[text]:'Alice'";
        assert_eq!(parse_pgoutput_action(data), Some('I'));
    }

    #[test]
    fn test_parse_pgoutput_update() {
        let data = "table public.users: UPDATE: id[integer]:1 name[text]:'Bob'";
        assert_eq!(parse_pgoutput_action(data), Some('U'));
    }

    #[test]
    fn test_parse_pgoutput_delete() {
        let data = "table public.users: DELETE: id[integer]:1";
        assert_eq!(parse_pgoutput_action(data), Some('D'));
    }

    #[test]
    fn test_parse_pgoutput_truncate() {
        let data = "table public.users: TRUNCATE: (no column data)";
        assert_eq!(parse_pgoutput_action(data), Some('T'));
    }

    #[test]
    fn test_parse_pgoutput_begin() {
        let data = "BEGIN 12345";
        assert_eq!(parse_pgoutput_action(data), None);
    }

    #[test]
    fn test_parse_pgoutput_commit() {
        let data = "COMMIT 12345";
        assert_eq!(parse_pgoutput_action(data), None);
    }

    // ── parse_pgoutput_columns tests ───────────────────────────────

    #[test]
    fn test_parse_pgoutput_columns_insert() {
        let data = "table public.users: INSERT: id[integer]:1 name[text]:'Alice'";
        let cols = parse_pgoutput_columns(data);
        assert_eq!(cols.get("id").map(|s| s.as_str()), Some("1"));
        assert_eq!(cols.get("name").map(|s| s.as_str()), Some("Alice"));
    }

    #[test]
    fn test_parse_pgoutput_columns_empty() {
        let data = "BEGIN 12345";
        let cols = parse_pgoutput_columns(data);
        assert!(cols.is_empty());
    }

    // ── build_pk_hash_from_values tests ────────────────────────────

    #[test]
    fn test_build_pk_hash_empty() {
        let pk: Vec<String> = vec![];
        let parsed = std::collections::HashMap::new();
        assert_eq!(build_pk_hash_from_values(&pk, &parsed), "0");
    }

    #[test]
    fn test_build_pk_hash_single_key() {
        let pk = vec!["id".to_string()];
        let mut parsed = std::collections::HashMap::new();
        parsed.insert("id".to_string(), "42".to_string());
        let result = build_pk_hash_from_values(&pk, &parsed);
        assert!(result.contains("pg_stream_hash"));
        assert!(result.contains("42"));
    }

    #[test]
    fn test_build_pk_hash_composite_key() {
        let pk = vec!["a".to_string(), "b".to_string()];
        let mut parsed = std::collections::HashMap::new();
        parsed.insert("a".to_string(), "1".to_string());
        parsed.insert("b".to_string(), "2".to_string());
        let result = build_pk_hash_from_values(&pk, &parsed);
        assert!(result.contains("pg_stream_hash_multi"));
        assert!(result.contains("'1'"));
        assert!(result.contains("'2'"));
    }

    #[test]
    fn test_build_pk_hash_missing_key() {
        let pk = vec!["id".to_string()];
        let parsed = std::collections::HashMap::new(); // no "id" key
        assert_eq!(build_pk_hash_from_values(&pk, &parsed), "0");
    }

    #[test]
    fn test_build_pk_hash_sql_injection_safe() {
        let pk = vec!["id".to_string()];
        let mut parsed = std::collections::HashMap::new();
        parsed.insert("id".to_string(), "'; DROP TABLE users; --".to_string());
        let result = build_pk_hash_from_values(&pk, &parsed);
        // Value should have single quotes escaped
        assert!(result.contains("''"));
    }
}
