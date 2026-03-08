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
//! - Writes changes to the same `pgtrickle_changes.changes_<oid>` tables
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
//! - `pg_trickle.cdc_mode` set to `'auto'` or `'wal'`

use pgrx::prelude::*;

use crate::catalog::{CdcMode, StDependency};
use crate::cdc;
use crate::config;
use crate::error::PgTrickleError;
use crate::monitor;

// ── Naming Conventions ─────────────────────────────────────────────────────

/// Replication slot name for a source table: `pgtrickle_<oid>`.
pub fn slot_name_for_source(source_oid: pg_sys::Oid) -> String {
    format!("pgtrickle_{}", source_oid.to_u32())
}

/// Publication name for a source table: `pgtrickle_cdc_<oid>`.
pub fn publication_name_for_source(source_oid: pg_sys::Oid) -> String {
    format!("pgtrickle_cdc_{}", source_oid.to_u32())
}

// ── Publication Management ─────────────────────────────────────────────────

/// Create a publication for a source table to enable logical decoding.
///
/// Publications tell `pgoutput` which tables to include in the change stream.
/// Each tracked source gets its own publication for independent lifecycle
/// management.
pub fn create_publication(source_oid: pg_sys::Oid) -> Result<(), PgTrickleError> {
    let pub_name = publication_name_for_source(source_oid);

    // Get the fully-qualified source table name
    let source_table =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_oid.into()])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            .ok_or_else(|| {
                PgTrickleError::NotFound(format!(
                    "Table with OID {} not found",
                    source_oid.to_u32()
                ))
            })?;

    // Create publication if it doesn't already exist.
    // PostgreSQL doesn't have CREATE PUBLICATION IF NOT EXISTS, so check first.
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_publication WHERE pubname = $1)",
        &[pub_name.as_str().into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if !exists {
        let sql = format!(
            "CREATE PUBLICATION {} FOR TABLE {}",
            quote_ident(&pub_name),
            source_table,
        );
        Spi::run(&sql).map_err(|e| {
            PgTrickleError::WalTransitionError(format!(
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
pub fn drop_publication(source_oid: pg_sys::Oid) -> Result<(), PgTrickleError> {
    let pub_name = publication_name_for_source(source_oid);
    let sql = format!("DROP PUBLICATION IF EXISTS {}", quote_ident(&pub_name));
    Spi::run(&sql).map_err(|e| {
        PgTrickleError::WalTransitionError(format!(
            "Failed to drop publication {}: {}",
            pub_name, e
        ))
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
///
/// # Implementation Note
///
/// This uses the low-level C replication API (`ReplicationSlotCreate`,
/// `CreateInitDecodingContext`, etc.) instead of the SQL function
/// `pg_create_logical_replication_slot()`.
///
/// Both the SQL wrapper and `CreateInitDecodingContext` reject calls from
/// transactions that have an assigned XID (transaction ID).  With
/// `wal_level = logical`, even read-only SPI queries can trigger hint-bit
/// WAL writes that assign an XID.
///
/// **CRITICAL**: This function must be called in a transaction that has not
/// done ANY prior SPI queries or catalog reads.  The prerequisite checks
/// (wal_level, permissions, replica identity) must be done in a *separate,
/// earlier* transaction.  `CheckSlotPermissions` and
/// `CheckLogicalDecodingRequirements` are intentionally skipped here because
/// they access the catalog (which could assign an XID); instead the caller
/// must verify prerequisites before calling this function.
pub fn create_replication_slot_pristine(slot_name: &str) -> Result<String, PgTrickleError> {
    create_replication_slot_internal(slot_name)
}

/// Check if a replication slot already exists and return its confirmed_flush_lsn.
///
/// Returns `Some(lsn)` if the slot exists, `None` if it doesn't.
/// This function does SPI reads and must NOT be called in the same
/// transaction as `create_replication_slot_pristine`.
pub fn get_existing_slot_lsn(slot_name: &str) -> Result<Option<String>, PgTrickleError> {
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        &[slot_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if !exists {
        return Ok(None);
    }

    let lsn = Spi::get_one_with_args::<String>(
        "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
        &[slot_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or_else(|| "0/0".to_string());

    Ok(Some(lsn))
}

/// Advance a logical replication slot's `confirmed_flush_lsn` to the current
/// WAL LSN (`pg_current_wal_lsn()`).
///
/// Called after a FULL refresh to allow PostgreSQL to reclaim WAL segments
/// that the full refresh has already materialized (G3). Returns `Ok(())`
/// immediately if the slot does not exist (e.g., trigger-based sources).
pub fn advance_slot_to_current(slot_name: &str) -> Result<(), PgTrickleError> {
    // Guard against missing slot before issuing the advance,
    // which would otherwise raise a PostgreSQL ERROR.
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        &[slot_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if !exists {
        return Ok(());
    }

    Spi::run_with_args(
        "SELECT pg_replication_slot_advance($1, pg_current_wal_lsn())",
        &[slot_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("advance slot '{}': {}", slot_name, e)))
}

/// Create a logical replication slot via the PostgreSQL C API.
///
/// Replicates the logic of `pg_create_logical_replication_slot()` from
/// `replicationfuncs.c` but skips the `XactHasPerformedWrites()` guard
/// AND the catalog-touching permission/requirement checks.  Those checks
/// must be done by the caller in a prior transaction.
///
/// **CRITICAL**: Must run in a pristine transaction with NO prior SPI
/// calls or catalog access, otherwise `CreateInitDecodingContext` will
/// fail because the transaction has an assigned XID.
fn create_replication_slot_internal(slot_name: &str) -> Result<String, PgTrickleError> {
    use std::ffi::CString;

    let c_slot_name = CString::new(slot_name)
        .map_err(|e| PgTrickleError::ReplicationSlotError(format!("Invalid slot name: {}", e)))?;
    let c_plugin = CString::new("test_decoding").unwrap();

    // SAFETY: Calling PostgreSQL C API functions for replication slot management.
    // These are the same functions called by pg_create_logical_replication_slot(),
    // minus the XactHasPerformedWrites guard and minus CheckSlotPermissions /
    // CheckLogicalDecodingRequirements (which do catalog reads that would assign
    // an XID).  The caller guarantees prerequisites were checked in a prior
    // transaction.
    //
    // Sequence: ReplicationSlotCreate (ephemeral) → CreateInitDecodingContext →
    // DecodingContextFindStartpoint → persist → release.
    unsafe {
        // Create as ephemeral first — if anything fails, PG cleans up automatically.
        pg_sys::ReplicationSlotCreate(
            c_slot_name.as_ptr(),
            true, // db_specific
            pg_sys::ReplicationSlotPersistency::RS_EPHEMERAL,
            false, // two_phase
            false, // failover
            false, // synced
        );

        // Set up the XLogReaderRoutine with the standard local WAL readers.
        // We use thin wrappers because Rust edition 2024 does not implicitly
        // coerce function items across ABI boundaries.
        unsafe extern "C-unwind" fn page_read_wrapper(
            state: *mut pg_sys::XLogReaderState,
            target: pg_sys::XLogRecPtr,
            req_len: std::ffi::c_int,
            target_rec: pg_sys::XLogRecPtr,
            cur_page: *mut std::ffi::c_char,
        ) -> std::ffi::c_int {
            // SAFETY: Delegating to the PG-provided read_local_xlog_page with
            // the same arguments the caller passed.
            unsafe { pg_sys::read_local_xlog_page(state, target, req_len, target_rec, cur_page) }
        }
        unsafe extern "C-unwind" fn segment_open_wrapper(
            state: *mut pg_sys::XLogReaderState,
            next_seg_no: pg_sys::XLogSegNo,
            tli_p: *mut pg_sys::TimeLineID,
        ) {
            // SAFETY: Delegating to the PG-provided wal_segment_open.
            unsafe { pg_sys::wal_segment_open(state, next_seg_no, tli_p) }
        }
        unsafe extern "C-unwind" fn segment_close_wrapper(state: *mut pg_sys::XLogReaderState) {
            // SAFETY: Delegating to the PG-provided wal_segment_close.
            unsafe { pg_sys::wal_segment_close(state) }
        }
        let mut xl_routine = pg_sys::XLogReaderRoutine {
            page_read: Some(page_read_wrapper),
            segment_open: Some(segment_open_wrapper),
            segment_close: Some(segment_close_wrapper),
        };

        // Create the initial decoding context — this finds the starting LSN
        let ctx = pg_sys::CreateInitDecodingContext(
            c_plugin.as_ptr(),
            std::ptr::null_mut(), // output_plugin_options (NIL)
            false,                // need_full_snapshot
            pg_sys::InvalidXLogRecPtr as u64,
            &mut xl_routine,
            None, // prepare_write
            None, // do_write
            None, // update_progress
        );

        // Build the initial snapshot and find the start point
        pg_sys::DecodingContextFindStartpoint(ctx);

        // Read the confirmed_flush LSN before releasing
        let confirmed_flush = (*pg_sys::MyReplicationSlot).data.confirmed_flush;

        // Clean up the decoding context
        pg_sys::FreeDecodingContext(ctx);

        // Persist the slot (it was created as ephemeral)
        pg_sys::ReplicationSlotMarkDirty();
        pg_sys::ReplicationSlotSave();
        pg_sys::ReplicationSlotPersist();

        // Release the slot
        pg_sys::ReplicationSlotRelease();

        // Format LSN as "X/Y"
        let lsn_str = format!(
            "{:X}/{:X}",
            (confirmed_flush >> 32) as u32,
            confirmed_flush as u32
        );

        Ok(lsn_str)
    }
}

/// Drop a logical replication slot.
///
/// Safe to call even if the slot doesn't exist (checks first).
pub fn drop_replication_slot(slot_name: &str) -> Result<(), PgTrickleError> {
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        &[slot_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if exists {
        Spi::run_with_args("SELECT pg_drop_replication_slot($1)", &[slot_name.into()]).map_err(
            |e| {
                PgTrickleError::ReplicationSlotError(format!(
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
pub fn get_slot_confirmed_lsn(slot_name: &str) -> Result<Option<String>, PgTrickleError> {
    Spi::get_one_with_args::<String>(
        "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
        &[slot_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))
}

/// Get the lag in bytes between a slot's confirmed LSN and the current WAL position.
///
/// A high lag indicates the decoder is falling behind.
pub fn get_slot_lag_bytes(slot_name: &str) -> Result<i64, PgTrickleError> {
    Spi::get_one_with_args::<i64>(
        "SELECT (pg_current_wal_lsn() - confirmed_flush_lsn)::bigint \
         FROM pg_replication_slots WHERE slot_name = $1",
        &[slot_name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))
    .map(|v| v.unwrap_or(0))
}

// ── WAL Polling ────────────────────────────────────────────────────────────

/// Maximum number of changes to process per poll cycle.
///
/// Limits memory usage and keeps each scheduler tick bounded.
/// Remaining changes are picked up in the next cycle.
const MAX_CHANGES_PER_POLL: i64 = 10_000;

/// Number of consecutive WAL poll errors before automatically falling back
/// to trigger-based CDC. Prevents a permanently broken WAL decoder from
/// blocking change capture indefinitely.
const MAX_CONSECUTIVE_WAL_ERRORS: u32 = 5;

/// Poll WAL changes from a replication slot and write them to the buffer table.
///
/// Uses `pg_logical_slot_get_changes()` with the `test_decoding` plugin to
/// retrieve decoded WAL changes. Each change is parsed and inserted into
/// the appropriate `pgtrickle_changes.changes_<oid>` buffer table.
///
/// The `test_decoding` output format provides structured text output that
/// we parse to extract action type, column values, and LSN information.
/// Since `test_decoding` decodes ALL tables (not just the source), we
/// filter by matching the qualified table name in each row.
///
/// **Schema-change detection**: When the decoded column set doesn't match
/// our expected columns, this function returns `Err(WalTransitionError)`
/// so the caller can abort the WAL transition and fall back to triggers.
///
/// Returns the number of changes processed and the last confirmed LSN.
pub fn poll_wal_changes(
    source_oid: pg_sys::Oid,
    slot_name: &str,
    source_table_name: &str,
    change_schema: &str,
    pk_columns: &[String],
    columns: &[(String, String)],
) -> Result<(i64, Option<String>), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();

    // Poll changes from the logical replication slot.
    // pg_logical_slot_get_changes() advances the slot position
    // automatically.  We use test_decoding which produces text output
    // in the format: "table schema.table: ACTION: col[type]:val ..."
    let poll_sql = format!(
        "SELECT lsn::text, xid, data \
         FROM pg_logical_slot_get_changes(\
             '{slot_name}', NULL, {max_changes}\
         )",
        slot_name = slot_name,
        max_changes = MAX_CHANGES_PER_POLL,
    );

    let mut count: i64 = 0;
    let mut last_lsn: Option<String> = None;

    Spi::connect(|client| {
        let result = client
            .select(&poll_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        for row in result {
            let lsn = row
                .get::<String>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let data = row
                .get::<String>(3)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();

            // Parse the test_decoding data and determine if it's relevant to our source.
            // test_decoding decodes ALL tables, so filter by matching the
            // qualified table name (e.g. "table public.orders: INSERT: ...").
            let table_prefix = format!("table {}: ", source_table_name);
            if !data.starts_with(&table_prefix) {
                // Row is for a different table — skip but still track LSN
                last_lsn = Some(lsn);
                continue;
            }

            if let Some(action) = parse_pgoutput_action(&data) {
                // Schema-change detection: when pgoutput emits a DML message
                // whose column set doesn't match our expected columns, a DDL
                // change likely occurred. Return an error so the caller can
                // fall back to triggers.
                if action != 'T' {
                    let parsed = parse_pgoutput_columns(&data);
                    if detect_schema_mismatch(&parsed, columns) {
                        return Err(PgTrickleError::WalTransitionError(format!(
                            "Schema change detected for source OID {} — \
                             decoded columns don't match expected columns",
                            oid_u32
                        )));
                    }
                }

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

        Ok::<(), PgTrickleError>(())
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
///
/// Parses the action **positionally** rather than with `contains()` to avoid
/// false matches when a schema/table name or column value happens to contain
/// an action keyword (G2.3).
fn parse_pgoutput_action(data: &str) -> Option<char> {
    // Strip the fixed "table " prefix that prefixes all DML lines.
    let rest = data.strip_prefix("table ")?;
    // Skip over "schema.tablename" to the first ": " separator.
    let after_table_colon = rest.split_once(": ")?.1;
    // The action keyword is the next token up to the next ':'.
    let action = after_table_colon.split_once(':')?.0.trim();
    match action {
        "INSERT" => Some('I'),
        "UPDATE" => Some('U'),
        "DELETE" => Some('D'),
        "TRUNCATE" => Some('T'),
        _ => None,
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

/// Parse old-tuple column values from a pgoutput UPDATE data line.
///
/// With `REPLICA IDENTITY FULL`, pgoutput UPDATE messages include an
/// "old-key:" section before the "new-tuple:" section:
/// ```text
/// table public.t: UPDATE: old-key: id[integer]:1 name[text]:'Alice' new-tuple: id[integer]:1 name[text]:'Bob'
/// ```
///
/// This function extracts the "old-key:" portion and parses column values.
/// Returns an empty map for non-UPDATE messages or messages without an
/// "old-key:" section (i.e., REPLICA IDENTITY DEFAULT where only PK
/// columns appear in old-key).
fn parse_pgoutput_old_columns(data: &str) -> std::collections::HashMap<String, String> {
    let mut cols = std::collections::HashMap::new();

    // Find the "old-key:" section in UPDATE messages.
    let old_key_start = match data.find("old-key:") {
        Some(pos) => pos + 9, // skip past "old-key: "
        None => return cols,
    };

    // The old-key section ends at "new-tuple:" (if present) or at end of string.
    let old_key_end = data[old_key_start..]
        .find("new-tuple:")
        .map(|pos| old_key_start + pos)
        .unwrap_or(data.len());

    let old_section = &data[old_key_start..old_key_end];

    // Parse column_name[type]:value pairs from the old-key section.
    for segment in old_section.split_whitespace() {
        if let Some(bracket_pos) = segment.find('[') {
            let col_name = &segment[..bracket_pos];
            if let Some(colon_pos) = segment.find("]:") {
                let value = &segment[colon_pos + 2..];
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
) -> Result<(), PgTrickleError> {
    // Handle TRUNCATE specially — mark downstream STs for reinit
    if *action == 'T' {
        mark_downstream_for_reinit(pg_sys::Oid::from(source_oid))?;
        return Ok(());
    }

    let parsed = parse_pgoutput_columns(data);

    // G2.2: Parse old-tuple values for UPDATE events.
    // With REPLICA IDENTITY FULL, pgoutput includes the old tuple in the
    // "old-key:" section before the new tuple. Parse both sections.
    let old_parsed = if *action == 'U' {
        parse_pgoutput_old_columns(data)
    } else {
        std::collections::HashMap::new()
    };

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
                // G2.2: old values from pgoutput old-key section.
                // With REPLICA IDENTITY FULL (required by try_start_transition),
                // pgoutput includes the complete old tuple. Parse old values
                // from the "old-key:" section of the UPDATE message.
                col_names.push(format!("\"old_{}\"", safe_name));
                if let Some(val) = old_parsed.get(col_name) {
                    col_values.push(format!("'{}'", val.replace('\'', "''")));
                } else {
                    col_values.push("NULL".to_string());
                }
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
        PgTrickleError::WalTransitionError(format!(
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
            format!("pgtrickle.pg_trickle_hash('{}')", val.replace('\'', "''"))
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
            "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
            array_items.join(", ")
        )
    }
}

/// Mark all downstream stream tables for reinitialization.
///
/// Called when a TRUNCATE is detected via WAL decoding. Since TRUNCATE
/// invalidates all existing change tracking, downstream STs need a
/// full refresh to resync.
fn mark_downstream_for_reinit(source_oid: pg_sys::Oid) -> Result<(), PgTrickleError> {
    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET needs_reinit = true, updated_at = now() \
         WHERE pgt_id IN ( \
             SELECT pgt_id FROM pgtrickle.pgt_dependencies \
             WHERE source_relid = $1 \
         )",
        &[source_oid.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    warning!(
        "pg_trickle: TRUNCATE detected on source OID {} via WAL — downstream STs marked for reinit",
        source_oid.to_u32()
    );

    Ok(())
}

// ── Transition Orchestration ───────────────────────────────────────────────

/// Check if the WAL transition is complete and finalize if so.
///
/// Called by the scheduler on each tick for sources in TRANSITIONING mode.
/// The transition is complete when the WAL decoder has caught up close to
/// the current WAL position (within a reasonable lag threshold).
///
/// If the transition has timed out, falls back to trigger-based CDC.
pub fn check_and_complete_transition(
    source_oid: pg_sys::Oid,
    pgt_id: i64,
    dep: &StDependency,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let default_slot = slot_name_for_source(source_oid);
    let slot_name = dep.slot_name.as_deref().unwrap_or(&default_slot);

    // Check if the decoder has caught up
    let lag_bytes = get_slot_lag_bytes(slot_name)?;

    // Consider "caught up" when lag is under 64KB (a few WAL pages)
    const MAX_LAG_BYTES: i64 = 65_536;

    if lag_bytes <= MAX_LAG_BYTES {
        // Decoder has caught up — complete the transition
        complete_wal_transition(source_oid, pgt_id, change_schema)?;
        return Ok(());
    }

    // Not caught up — check for timeout with progressive backoff (F32: G2.4).
    // We allow up to 3× the configured timeout before aborting, logging
    // warnings at 1× and 2× to give operators visibility into slow transitions.
    if let Some(ref started_at) = dep.transition_started_at {
        let base_timeout = config::pg_trickle_wal_transition_timeout();

        // Check if we've exceeded the final deadline (3× base timeout)
        let final_deadline = base_timeout * 3;
        let exceeded_final = Spi::get_one_with_args::<bool>(
            &format!(
                "SELECT (now() - $1::timestamptz) > interval '{} seconds'",
                final_deadline
            ),
            &[started_at.as_str().into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or(false);

        if exceeded_final {
            warning!(
                "pg_trickle: WAL transition exhausted all retries for source OID {} \
                 (lag: {} bytes after {}s, max {}s); falling back to triggers",
                source_oid.to_u32(),
                lag_bytes,
                final_deadline,
                final_deadline,
            );
            abort_wal_transition(source_oid, pgt_id, change_schema)?;
            return Ok(());
        }

        // Emit warnings at intermediate checkpoints (1× and 2× base timeout)
        let exceeded_first = Spi::get_one_with_args::<bool>(
            &format!(
                "SELECT (now() - $1::timestamptz) > interval '{} seconds'",
                base_timeout
            ),
            &[started_at.as_str().into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or(false);

        if exceeded_first {
            let exceeded_second = Spi::get_one_with_args::<bool>(
                &format!(
                    "SELECT (now() - $1::timestamptz) > interval '{} seconds'",
                    base_timeout * 2
                ),
                &[started_at.as_str().into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            .unwrap_or(false);

            if exceeded_second {
                warning!(
                    "pg_trickle: WAL transition slow for source OID {} \
                     (lag: {} bytes, retry 2/3 — will abort after {}s)",
                    source_oid.to_u32(),
                    lag_bytes,
                    final_deadline,
                );
            } else {
                log!(
                    "pg_trickle: WAL transition slow for source OID {} \
                     (lag: {} bytes, retry 1/3 — extending deadline)",
                    source_oid.to_u32(),
                    lag_bytes,
                );
            }
        }
    }

    Ok(())
}

/// Complete the WAL transition — drop the trigger and switch to WAL mode.
///
/// Called when the WAL decoder has caught up past the handoff point.
fn complete_wal_transition(
    source_oid: pg_sys::Oid,
    _pgt_id: i64,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();

    // Step 1: Drop the CDC trigger (WAL decoder now covers all changes)
    cdc::drop_change_trigger(source_oid, change_schema)?;

    // Step 2: Update catalog to WAL mode
    StDependency::update_cdc_mode_for_source(source_oid, CdcMode::Wal, None, None)?;

    info!(
        "pg_trickle: completed WAL transition for source OID {} — trigger dropped, WAL active",
        oid_u32
    );

    // Emit NOTIFY for transition completion
    let slot_name = slot_name_for_source(source_oid);
    monitor::emit_cdc_transition_notify(
        source_oid,
        CdcMode::Transitioning,
        CdcMode::Wal,
        Some(&slot_name),
    );

    Ok(())
}

/// Abort the WAL transition and fall back to trigger-based CDC.
///
/// Called when the transition times out or encounters an unrecoverable error.
/// Cleans up WAL decoder resources and reverts to trigger mode.
pub fn abort_wal_transition(
    source_oid: pg_sys::Oid,
    _pgt_id: i64,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();
    let slot_name = slot_name_for_source(source_oid);

    // Step 1: Drop the replication slot (stops WAL retention)
    if let Err(e) = drop_replication_slot(&slot_name) {
        warning!(
            "pg_trickle: failed to drop replication slot {} during abort: {}",
            slot_name,
            e
        );
    }

    // Step 2: Drop the publication
    if let Err(e) = drop_publication(source_oid) {
        warning!(
            "pg_trickle: failed to drop publication during abort for OID {}: {}",
            oid_u32,
            e
        );
    }

    // Step 3: Revert catalog to trigger mode
    // Step 3: Revert catalog to trigger mode for all dependents of this source.
    StDependency::update_cdc_mode_for_source(source_oid, CdcMode::Trigger, None, None)?;

    // Step 4: Verify the trigger still exists — recreate if lost
    if !cdc::trigger_exists(source_oid)? {
        let pk_columns = cdc::resolve_pk_columns(source_oid)?;
        let columns = cdc::resolve_source_column_defs(source_oid)?;
        cdc::create_change_trigger(source_oid, change_schema, &pk_columns, &columns)?;
        warning!(
            "pg_trickle: recreated CDC trigger for source OID {} during abort",
            oid_u32
        );
    }

    warning!(
        "pg_trickle: aborted WAL transition for source OID {}; reverted to triggers",
        oid_u32
    );

    // Emit NOTIFY for transition abort (fallback to triggers)
    monitor::emit_cdc_transition_notify(source_oid, CdcMode::Wal, CdcMode::Trigger, None);

    Ok(())
}

/// Force a source back to trigger-based CDC to satisfy a conservative request.
pub fn force_source_to_trigger(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let deps = StDependency::get_all()?;
    let source_deps: Vec<_> = deps
        .into_iter()
        .filter(|dep| dep.source_relid == source_oid && dep.source_type == "TABLE")
        .collect();

    let previous_mode = if source_deps.iter().any(|dep| dep.cdc_mode == CdcMode::Wal) {
        Some(CdcMode::Wal)
    } else if source_deps
        .iter()
        .any(|dep| dep.cdc_mode == CdcMode::Transitioning)
    {
        Some(CdcMode::Transitioning)
    } else {
        None
    };

    let slot_name = slot_name_for_source(source_oid);
    if let Err(e) = drop_replication_slot(&slot_name) {
        warning!(
            "pg_trickle: failed to drop replication slot {} while forcing trigger CDC: {}",
            slot_name,
            e
        );
    }
    if let Err(e) = drop_publication(source_oid) {
        warning!(
            "pg_trickle: failed to drop publication while forcing trigger CDC for OID {}: {}",
            source_oid.to_u32(),
            e
        );
    }

    StDependency::update_cdc_mode_for_source(source_oid, CdcMode::Trigger, None, None)?;

    if !cdc::trigger_exists(source_oid)? {
        let pk_columns = cdc::resolve_pk_columns(source_oid)?;
        let columns = cdc::resolve_source_column_defs(source_oid)?;
        cdc::create_change_trigger(source_oid, change_schema, &pk_columns, &columns)?;
    }

    if let Some(prev) = previous_mode {
        monitor::emit_cdc_transition_notify(source_oid, prev, CdcMode::Trigger, None);
    }

    Ok(())
}

// ── Scheduler Integration ──────────────────────────────────────────────────

/// Pending slot creation request, collected in Phase 1 and executed in Phase 2.
pub struct PendingSlotCreation {
    pub source_relid: pg_sys::Oid,
    pub pgt_id: i64,
    pub slot_name: String,
}

/// WAL source that reached the error threshold and needs to be aborted
/// (reverted to trigger CDC) in a separate transaction.
pub struct PendingAbort {
    pub source_relid: pg_sys::Oid,
    pub pgt_id: i64,
}

/// Result from Phase 1: pending slot creations and pending aborts.
pub struct Phase1Result {
    pub pending_slots: Vec<PendingSlotCreation>,
    pub pending_aborts: Vec<PendingAbort>,
}

/// Phase 1: Check eligibility, collect pending slot creations, and handle
/// already-transitioned/WAL sources.
///
/// This phase does SPI reads (catalog, pg_replication_slots).
/// Must run in its own transaction BEFORE Phase 2.
///
/// Returns pending slot creations (Phase 2) and pending aborts (Phase 4).
/// WAL poll panics (from missing slots) are caught and counted. Sources
/// that exceed `MAX_CONSECUTIVE_WAL_ERRORS` are queued for abort in a
/// separate transaction (because the SPI connection is broken after a
/// caught panic).
pub fn advance_wal_transitions_phase1(change_schema: &str) -> Result<Phase1Result, PgTrickleError> {
    // Only process if CDC mode allows WAL
    let cdc_mode = config::pg_trickle_cdc_mode();
    if cdc_mode == "trigger" {
        return Ok(Phase1Result {
            pending_slots: vec![],
            pending_aborts: vec![],
        });
    }

    // Get all dependencies to check their CDC mode
    let all_deps = StDependency::get_all()?;

    // Group by source_relid to avoid processing the same source multiple times
    let mut processed_sources = std::collections::HashSet::new();
    let mut pending_slots = Vec::new();
    let mut pending_aborts: Vec<PendingAbort> = Vec::new();

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

        let requested_mode = StDependency::effective_requested_mode_for_source(dep.source_relid)?;
        match requested_mode.as_deref() {
            None | Some("trigger") => {
                if dep.cdc_mode != CdcMode::Trigger {
                    pending_aborts.push(PendingAbort {
                        source_relid: dep.source_relid,
                        pgt_id: dep.pgt_id,
                    });
                }
                continue;
            }
            Some("auto") | Some("wal") => {}
            Some(_) => continue,
        }

        match dep.cdc_mode {
            CdcMode::Trigger => {
                // Check if this source is eligible for WAL transition
                match check_transition_eligible(dep, requested_mode.as_deref().unwrap_or("auto")) {
                    Ok(true) => {
                        let slot_name = slot_name_for_source(dep.source_relid);
                        // Check if slot already exists (SPI read — fine in Phase 1)
                        match get_existing_slot_lsn(&slot_name)? {
                            Some(slot_lsn) => {
                                // Slot already exists — go straight to Phase 3
                                log!(
                                    "pg_trickle: slot '{}' already exists, finishing transition",
                                    slot_name
                                );
                                if let Err(e) = finish_wal_transition(
                                    dep.source_relid,
                                    dep.pgt_id,
                                    &slot_name,
                                    &slot_lsn,
                                ) {
                                    log!(
                                        "pg_trickle: failed to finish WAL transition for OID {}: {}",
                                        source_key,
                                        e
                                    );
                                }
                            }
                            None => {
                                // Slot needs creation — queue for Phase 2
                                log!(
                                    "pg_trickle: source OID {} eligible for WAL transition, queuing slot creation",
                                    source_key
                                );
                                pending_slots.push(PendingSlotCreation {
                                    source_relid: dep.source_relid,
                                    pgt_id: dep.pgt_id,
                                    slot_name,
                                });
                            }
                        }
                    }
                    Ok(false) => {
                        // Not eligible — stay on triggers
                        if cdc_mode == "auto" {
                            emit_auto_cdc_stuck_log(dep);
                        }
                    }
                    Err(e) => {
                        log!(
                            "pg_trickle: failed to check WAL transition eligibility for source OID {}: {}",
                            source_key,
                            e
                        );
                    }
                }
            }
            CdcMode::Transitioning => {
                // Poll WAL changes (both trigger and WAL are active)
                if let Err(e) = poll_source_changes(dep, change_schema) {
                    log!(
                        "pg_trickle: WAL poll error for transitioning source OID {}: {}",
                        source_key,
                        e
                    );
                }
                // Check if transition is complete or timed out
                if let Err(e) =
                    check_and_complete_transition(dep.source_relid, dep.pgt_id, dep, change_schema)
                {
                    log!(
                        "pg_trickle: transition check error for source OID {}: {}",
                        source_key,
                        e
                    );
                }
            }
            CdcMode::Wal => {
                // Poll WAL changes (steady-state WAL mode).
                // Use catch_unwind because a missing/invalid slot causes a
                // PG ERROR → Rust panic that would bypass the error counter.
                let poll_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    poll_source_changes(dep, change_schema)
                }));
                let poll_err = match poll_result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(_panic) => Some("PG error during WAL poll (likely missing slot)".into()),
                };

                if let Some(err_msg) = poll_err {
                    let count = bump_wal_error_count(source_key);
                    if count >= MAX_CONSECUTIVE_WAL_ERRORS {
                        warning!(
                            "pg_trickle: WAL poll failed {} consecutive times for source OID {} \
                             — falling back to triggers. Last error: {}",
                            count,
                            source_key,
                            err_msg
                        );
                        reset_wal_error_count(source_key);
                        // Defer abort to Phase 4 (separate transaction) because
                        // after catch_unwind of a PG ERROR the SPI connection
                        // is broken in this transaction.
                        pending_aborts.push(PendingAbort {
                            source_relid: dep.source_relid,
                            pgt_id: dep.pgt_id,
                        });
                    } else {
                        warning!(
                            "pg_trickle: WAL poll error for source OID {} ({}/{} before fallback): {}",
                            source_key,
                            count,
                            MAX_CONSECUTIVE_WAL_ERRORS,
                            err_msg
                        );
                    }
                } else {
                    reset_wal_error_count(source_key);
                    // Check decoder health periodically (slot existence, lag)
                    if let Err(e) =
                        check_decoder_health(dep.source_relid, dep.pgt_id, change_schema)
                    {
                        log!(
                            "pg_trickle: health check error for WAL source OID {}: {}",
                            source_key,
                            e
                        );
                    }
                }
            }
        }
    }

    Ok(Phase1Result {
        pending_slots,
        pending_aborts,
    })
}

/// Phase 3: Finish WAL transitions for slots that were created in Phase 2.
///
/// Creates publications and updates the catalog for each successfully created slot.
/// This phase does SPI writes and must run in its own transaction AFTER Phase 2.
pub fn advance_wal_transitions_phase3(
    created_slots: &[(PendingSlotCreation, String)],
) -> Result<(), PgTrickleError> {
    for (pending, slot_lsn) in created_slots {
        if let Err(e) = finish_wal_transition(
            pending.source_relid,
            pending.pgt_id,
            &pending.slot_name,
            slot_lsn,
        ) {
            log!(
                "pg_trickle: failed to finish WAL transition for OID {}: {}",
                pending.source_relid.to_u32(),
                e
            );
        }
    }
    Ok(())
}

fn check_transition_eligible(
    dep: &StDependency,
    requested_mode: &str,
) -> Result<bool, PgTrickleError> {
    if !cdc::can_use_logical_replication_for_mode(requested_mode)? {
        return Ok(false);
    }

    if !cdc::check_replica_identity(dep.source_relid)? {
        return Ok(false);
    }

    let pk_columns = cdc::resolve_pk_columns(dep.source_relid)?;
    if pk_columns.is_empty() {
        return Ok(false);
    }

    let identity = cdc::get_replica_identity_mode(dep.source_relid)?;
    if identity != "full" {
        return Ok(false);
    }

    Ok(true)
}

/// Finish a WAL transition after the replication slot has been created.
///
/// Creates the publication and updates the catalog to TRANSITIONING mode.
/// Called from the scheduler after slot creation succeeds in a separate
/// transaction.
pub fn finish_wal_transition(
    source_oid: pg_sys::Oid,
    _pgt_id: i64,
    slot_name: &str,
    slot_lsn: &str,
) -> Result<(), PgTrickleError> {
    // Create publication for this source table
    create_publication(source_oid)?;

    // Update catalog — mark as TRANSITIONING
    StDependency::update_cdc_mode_for_source(
        source_oid,
        CdcMode::Transitioning,
        Some(slot_name),
        Some(slot_lsn),
    )?;

    info!(
        "pg_trickle: started WAL transition for source OID {} \
         (slot: {}, slot LSN: {})",
        source_oid.to_u32(),
        slot_name,
        slot_lsn
    );

    // Emit NOTIFY so clients can track the transition
    monitor::emit_cdc_transition_notify(
        source_oid,
        CdcMode::Trigger,
        CdcMode::Transitioning,
        Some(slot_name),
    );

    Ok(())
}

// ── Consecutive WAL error tracking ─────────────────────────────────────────

use std::sync::Mutex;

/// Shared consecutive-error counters per source OID.
static WAL_ERROR_COUNTS: Mutex<Option<std::collections::HashMap<u32, u32>>> = Mutex::new(None);

/// Increment the consecutive error counter for a WAL source and return the
/// new count.
fn bump_wal_error_count(source_oid: u32) -> u32 {
    let mut guard = WAL_ERROR_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    let entry = map.entry(source_oid).or_insert(0);
    *entry += 1;
    *entry
}

/// Reset the consecutive error counter for a source after a successful poll.
fn reset_wal_error_count(source_oid: u32) {
    let mut guard = WAL_ERROR_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = guard.as_mut() {
        map.remove(&source_oid);
    }
}

/// EC-18: Rate-limited LOG explaining why `auto` CDC mode is stuck in TRIGGER
/// phase for a particular source.
///
/// Uses a simple modular counter on scheduler ticks. Only emits once every
/// ~60 invocations (approximately once per minute at the default 1s
/// scheduler interval).
fn emit_auto_cdc_stuck_log(dep: &StDependency) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TICK_COUNTER: AtomicU64 = AtomicU64::new(0);

    let tick = TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
    if !tick.is_multiple_of(60) {
        return;
    }

    let source_oid = dep.source_relid;
    let reason = match cdc::can_use_logical_replication() {
        Ok(false) | Err(_) => {
            "wal_level is not 'logical'. Set wal_level = logical in postgresql.conf and restart."
                .to_string()
        }
        Ok(true) => {
            // WAL is available, check other prerequisites
            let pk_columns = cdc::resolve_pk_columns(source_oid).unwrap_or_default();
            if pk_columns.is_empty() {
                format!(
                    "source OID {} has no PRIMARY KEY. WAL-based CDC requires a PK. \
                     Add a PRIMARY KEY or switch to cdc_mode = 'trigger'.",
                    source_oid.to_u32()
                )
            } else {
                let identity = cdc::get_replica_identity_mode(source_oid)
                    .unwrap_or_else(|_| "unknown".to_string());
                if identity != "full" {
                    format!(
                        "source OID {} has REPLICA IDENTITY '{}' (need FULL). \
                         Run: ALTER TABLE ... REPLICA IDENTITY FULL",
                        source_oid.to_u32(),
                        identity
                    )
                } else {
                    format!(
                        "source OID {} meets prerequisites but transition has not started yet. \
                         This may resolve on the next scheduler tick.",
                        source_oid.to_u32()
                    )
                }
            }
        }
    };

    log!(
        "pg_trickle: cdc_mode = 'auto' but source OID {} is still using triggers. Reason: {}",
        source_oid.to_u32(),
        reason
    );
}

/// Poll WAL changes for a source that's in TRANSITIONING or WAL mode.
fn poll_source_changes(dep: &StDependency, change_schema: &str) -> Result<(), PgTrickleError> {
    let slot_name = match &dep.slot_name {
        Some(name) => name.clone(),
        None => slot_name_for_source(dep.source_relid),
    };

    // Resolve qualified source table name for filtering test_decoding output
    let source_table_name = cdc::get_qualified_table_name(dep.source_relid)?;

    // Resolve source column definitions for decoding
    let pk_columns = cdc::resolve_pk_columns(dep.source_relid)?;
    let columns = cdc::resolve_source_column_defs(dep.source_relid)?;

    // Poll and decode changes
    let (count, last_lsn) = poll_wal_changes(
        dep.source_relid,
        &slot_name,
        &source_table_name,
        change_schema,
        &pk_columns,
        &columns,
    )?;

    // Update the decoder confirmed LSN in the catalog
    if let Some(ref lsn) = last_lsn {
        StDependency::update_cdc_mode_for_source(
            dep.source_relid,
            dep.cdc_mode,
            dep.slot_name.as_deref(),
            Some(lsn),
        )?;
    }

    if count > 0 {
        log!(
            "pg_trickle: polled {} WAL changes for source OID {} (last LSN: {})",
            count,
            dep.source_relid.to_u32(),
            last_lsn.as_deref().unwrap_or("none")
        );
    }

    Ok(())
}

/// Check health of a WAL decoder for a source in WAL mode.
///
/// Verifies the replication slot exists, `wal_level` is still `logical`,
/// and lag is within bounds.
/// If the slot is missing, `wal_level` changed, or lag is excessive,
/// attempts recovery or fallback.
pub fn check_decoder_health(
    source_oid: pg_sys::Oid,
    pgt_id: i64,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let slot_name = slot_name_for_source(source_oid);

    // Check wal_level hasn't been changed (takes effect after restart)
    let wal_level = Spi::get_one::<String>("SELECT current_setting('wal_level')")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or_default();
    if wal_level != "logical" {
        warning!(
            "pg_trickle: wal_level changed from 'logical' to '{}' — \
             WAL decoder for source OID {} will fail after next restart. \
             Falling back to triggers now.",
            wal_level,
            source_oid.to_u32()
        );
        abort_wal_transition(source_oid, pgt_id, change_schema)?;
        return Ok(());
    }

    // Check if the slot still exists
    let slot_exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        &[slot_name.as_str().into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or(false);

    if !slot_exists {
        warning!(
            "pg_trickle: replication slot '{}' for source OID {} is missing — \
             falling back to triggers",
            slot_name,
            source_oid.to_u32()
        );
        abort_wal_transition(source_oid, pgt_id, change_schema)?;
        return Ok(());
    }

    // Check lag — if excessive (>1GB), warn but keep running
    let lag_bytes = get_slot_lag_bytes(&slot_name)?;
    const WARN_LAG_BYTES: i64 = 1_073_741_824; // 1 GB

    if lag_bytes > WARN_LAG_BYTES {
        warning!(
            "pg_trickle: WAL decoder for source OID {} has excessive lag: {} bytes",
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

/// Detect a schema mismatch between decoded pgoutput columns and the
/// expected column definitions.
///
/// Returns `true` if:
/// - The decoded row contains a column that doesn't appear in the expected set
///   (e.g., a column was added via ALTER TABLE ADD COLUMN).
/// - The decoded row has at least as many columns as expected but some expected
///   columns are missing (e.g., a column was renamed). F33: G2.5.
///   The "at least as many" guard avoids false positives on partial-column
///   messages (DELETE with non-FULL replica identity sends only PK columns).
///
/// DDL event triggers in hooks.rs handle the reinitialize; this provides a
/// safety net for DDL that bypasses event triggers.
fn detect_schema_mismatch(
    parsed: &std::collections::HashMap<String, String>,
    expected_columns: &[(String, String)],
) -> bool {
    if parsed.is_empty() {
        return false;
    }
    let expected_names: std::collections::HashSet<&str> = expected_columns
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();

    // Check for unknown columns (additions)
    for col_name in parsed.keys() {
        if !expected_names.contains(col_name.as_str()) {
            return true;
        }
    }

    // Check for missing expected columns (renames) — F33
    // Only check when the decoded message has at least as many columns as
    // expected, to avoid false positives from DELETE messages that only
    // carry PK columns with non-FULL replica identity.
    if parsed.len() >= expected_columns.len() {
        let parsed_names: std::collections::HashSet<&str> =
            parsed.keys().map(|k| k.as_str()).collect();
        for expected_name in &expected_names {
            if !parsed_names.contains(*expected_name) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Naming convention tests ────────────────────────────────────

    #[test]
    fn test_slot_name_for_source() {
        let oid = pg_sys::Oid::from(16384u32);
        assert_eq!(slot_name_for_source(oid), "pgtrickle_16384");
    }

    #[test]
    fn test_slot_name_for_source_zero() {
        let oid = pg_sys::Oid::from(0u32);
        assert_eq!(slot_name_for_source(oid), "pgtrickle_0");
    }

    #[test]
    fn test_publication_name_for_source() {
        let oid = pg_sys::Oid::from(16384u32);
        assert_eq!(publication_name_for_source(oid), "pgtrickle_cdc_16384");
    }

    #[test]
    fn test_publication_name_for_source_large_oid() {
        let oid = pg_sys::Oid::from(4294967295u32);
        assert_eq!(publication_name_for_source(oid), "pgtrickle_cdc_4294967295");
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

    #[test]
    fn test_parse_pgoutput_table_named_insert_log() {
        // Table named INSERT_LOG must not be misclassified (G2.3 edge case).
        let data = "table public.INSERT_LOG: UPDATE: id[integer]:1 msg[text]:'hello'";
        assert_eq!(parse_pgoutput_action(data), Some('U'));
    }

    #[test]
    fn test_parse_pgoutput_column_value_contains_delete() {
        // Column value containing "DELETE:" must not be misclassified (G2.3 edge case).
        let data = "table audit.log: UPDATE: op[text]:'DELETE: old row' id[integer]:42";
        assert_eq!(parse_pgoutput_action(data), Some('U'));
    }

    #[test]
    fn test_parse_pgoutput_schema_named_insert() {
        // Schema named "insert" must not affect action classification.
        let data = "table insert.orders: DELETE: id[integer]:7";
        assert_eq!(parse_pgoutput_action(data), Some('D'));
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
        assert!(result.contains("pg_trickle_hash"));
        assert!(result.contains("42"));
    }

    #[test]
    fn test_build_pk_hash_composite_key() {
        let pk = vec!["a".to_string(), "b".to_string()];
        let mut parsed = std::collections::HashMap::new();
        parsed.insert("a".to_string(), "1".to_string());
        parsed.insert("b".to_string(), "2".to_string());
        let result = build_pk_hash_from_values(&pk, &parsed);
        assert!(result.contains("pg_trickle_hash_multi"));
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

    // ── detect_schema_mismatch tests ───────────────────────────────

    #[test]
    fn test_schema_mismatch_no_mismatch() {
        let expected = vec![
            ("id".to_string(), "integer".to_string()),
            ("name".to_string(), "text".to_string()),
        ];
        let mut parsed = std::collections::HashMap::new();
        parsed.insert("id".to_string(), "42".to_string());
        parsed.insert("name".to_string(), "Alice".to_string());
        assert!(!detect_schema_mismatch(&parsed, &expected));
    }

    #[test]
    fn test_schema_mismatch_new_column() {
        let expected = vec![
            ("id".to_string(), "integer".to_string()),
            ("name".to_string(), "text".to_string()),
        ];
        let mut parsed = std::collections::HashMap::new();
        parsed.insert("id".to_string(), "42".to_string());
        parsed.insert("name".to_string(), "Alice".to_string());
        parsed.insert("email".to_string(), "alice@example.com".to_string());
        assert!(detect_schema_mismatch(&parsed, &expected));
    }

    #[test]
    fn test_schema_mismatch_empty_parsed() {
        let expected = vec![("id".to_string(), "integer".to_string())];
        let parsed = std::collections::HashMap::new();
        assert!(!detect_schema_mismatch(&parsed, &expected));
    }

    #[test]
    fn test_schema_mismatch_subset_ok() {
        // Fewer decoded columns than expected is OK (e.g., DELETE only sends PK)
        let expected = vec![
            ("id".to_string(), "integer".to_string()),
            ("name".to_string(), "text".to_string()),
        ];
        let mut parsed = std::collections::HashMap::new();
        parsed.insert("id".to_string(), "42".to_string());
        assert!(!detect_schema_mismatch(&parsed, &expected));
    }

    #[test]
    fn test_schema_mismatch_column_rename() {
        // F33: Column renamed from "name" to "full_name" — same count, different names
        let expected = vec![
            ("id".to_string(), "integer".to_string()),
            ("name".to_string(), "text".to_string()),
        ];
        let mut parsed = std::collections::HashMap::new();
        parsed.insert("id".to_string(), "42".to_string());
        parsed.insert("full_name".to_string(), "Alice".to_string());
        assert!(detect_schema_mismatch(&parsed, &expected));
    }

    // ── parse_pgoutput_old_columns tests (G2.2) ───────────────────

    #[test]
    fn test_parse_old_columns_update_with_old_key() {
        let data = "table public.users: UPDATE: old-key: id[integer]:1 name[text]:'Alice' new-tuple: id[integer]:1 name[text]:'Bob'";
        let old = parse_pgoutput_old_columns(data);
        assert_eq!(old.get("id").map(|s| s.as_str()), Some("1"));
        assert_eq!(old.get("name").map(|s| s.as_str()), Some("Alice"));
    }

    #[test]
    fn test_parse_old_columns_no_old_key_section() {
        // UPDATE without REPLICA IDENTITY FULL produces no old-key section
        let data = "table public.users: UPDATE: id[integer]:1 name[text]:'Bob'";
        let old = parse_pgoutput_old_columns(data);
        assert!(old.is_empty());
    }

    #[test]
    fn test_parse_old_columns_insert_has_no_old_key() {
        let data = "table public.users: INSERT: id[integer]:1 name[text]:'Alice'";
        let old = parse_pgoutput_old_columns(data);
        assert!(old.is_empty());
    }

    #[test]
    fn test_parse_old_columns_delete_has_no_old_key() {
        let data = "table public.users: DELETE: id[integer]:1";
        let old = parse_pgoutput_old_columns(data);
        assert!(old.is_empty());
    }

    #[test]
    fn test_parse_old_columns_old_key_at_end() {
        // Edge case: old-key section without a following new-tuple marker
        let data = "table public.users: UPDATE: old-key: id[integer]:99 name[text]:'Zara'";
        let old = parse_pgoutput_old_columns(data);
        assert_eq!(old.get("id").map(|s| s.as_str()), Some("99"));
        assert_eq!(old.get("name").map(|s| s.as_str()), Some("Zara"));
    }

    #[test]
    fn test_parse_old_columns_composite_pk() {
        let data = "table public.orders: UPDATE: old-key: customer_id[integer]:5 order_id[integer]:10 new-tuple: customer_id[integer]:5 order_id[integer]:10 status[text]:'shipped'";
        let old = parse_pgoutput_old_columns(data);
        assert_eq!(old.get("customer_id").map(|s| s.as_str()), Some("5"));
        assert_eq!(old.get("order_id").map(|s| s.as_str()), Some("10"));
        assert_eq!(old.len(), 2);
    }
}
