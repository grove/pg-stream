//! Change Data Capture via row-level triggers.
//!
//! Tracks DML changes to base tables referenced by stream tables using
//! AFTER INSERT/UPDATE/DELETE triggers that write directly into change
//! buffer tables.
//!
//! # Prior Art
//!
//! Row-level AFTER triggers for change capture are a well-established
//! PostgreSQL technique predating all relevant patents. Equivalent
//! implementations appear in:
//!
//! - **Debezium** (Red Hat, open source since 2016): trigger-based CDC for
//!   PostgreSQL and other databases.
//! - **`pgaudit` extension** (2015): captures DML via AFTER row-level
//!   triggers for audit logging.
//! - Various ETL tools using PostgreSQL trigger-based CDC since the 1990s.
//! - "Trigger-based Change Data Capture in PostgreSQL", PostgreSQL wiki.
//!
//! The `pgtrickle_changes` schema and buffer-table pattern is a standard
//! change-capture approach documented in PostgreSQL community literature.
//!
//! # Architecture
//!
//! - One PL/pgSQL trigger function + trigger per tracked base table
//! - Changes are written into `pgtrickle_changes.changes_<oid>` buffer tables
//! - Buffer tables are append-only; consumed changes are deleted after refresh
//!
//! # Compared to logical replication slots:
//!
//! - Works within a single transaction (no slot creation restrictions)
//! - Does not require `wal_level = logical`
//! - Captures changes at statement-execution time (visible after commit)

use pgrx::prelude::*;
use std::collections::HashMap;

use crate::config;
use crate::error::PgTrickleError;

/// Create a CDC trigger on a source table.
///
/// Creates a PL/pgSQL trigger function and an AFTER trigger that captures
/// INSERT/UPDATE/DELETE into the change buffer table using typed columns.
///
/// When `pk_columns` is non-empty, the trigger pre-computes a `pk_hash`
/// BIGINT column using `pgtrickle.pg_trickle_hash()` / `pgtrickle.pg_trickle_hash_multi()`.
/// This avoids expensive JSONB PK extraction during window-function
/// partitioning in the scan delta query.
///
/// `columns` contains the source table column definitions as
/// `(column_name, sql_type_name)` pairs. The trigger writes per-column
/// `NEW."col"` → `"new_col"` and `OLD."col"` → `"old_col"` instead of
/// `to_jsonb(NEW)` / `to_jsonb(OLD)`, eliminating JSONB serialization.
pub fn create_change_trigger(
    source_oid: pg_sys::Oid,
    change_schema: &str,
    pk_columns: &[String],
    columns: &[(String, String)],
) -> Result<String, PgTrickleError> {
    let oid_u32 = source_oid.to_u32();
    let trigger_name = format!("pg_trickle_cdc_{}", oid_u32);

    // Get the fully-qualified source table name
    let source_table =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_oid.into()])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            .ok_or_else(|| {
                PgTrickleError::NotFound(format!("Table with OID {} not found", oid_u32))
            })?;

    // Build PK hash computation expressions for each DML operation.
    // Uses the same hash functions as the scan delta so pk_hash values
    // match the window PARTITION BY grouping.
    let (pk_hash_new, pk_hash_old) = build_pk_hash_trigger_exprs(pk_columns, columns);

    // Build INSERT column list and value lists.
    // pk_hash is always populated (from PK columns or all-column content hash).
    let pk_col_decl = ", pk_hash";

    let ins_pk = format!(", {pk_hash_new}");
    let upd_pk = format!(", {pk_hash_new}");
    let del_pk = format!(", {pk_hash_old}");

    // Build per-column typed INSERT components.
    // Instead of `to_jsonb(NEW)` / `to_jsonb(OLD)`, we write each column
    // individually as `NEW."col"` → `"new_col"` and `OLD."col"` → `"old_col"`.
    let new_col_names: String = columns
        .iter()
        .map(|(name, _)| format!(", \"new_{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let old_col_names: String = columns
        .iter()
        .map(|(name, _)| format!(", \"old_{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");

    let new_vals: String = columns
        .iter()
        .map(|(name, _)| format!(", NEW.\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let old_vals: String = columns
        .iter()
        .map(|(name, _)| format!(", OLD.\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");

    // Create the trigger function
    let create_fn_sql = format!(
        "CREATE OR REPLACE FUNCTION {change_schema}.pg_trickle_cdc_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             IF TG_OP = 'INSERT' THEN
                 INSERT INTO {change_schema}.changes_{oid}
                     (lsn, action{pk_col_decl}{new_col_names})
                 VALUES (pg_current_wal_lsn(), 'I'
                         {ins_pk}{new_vals});
                 RETURN NEW;
             ELSIF TG_OP = 'UPDATE' THEN
                 INSERT INTO {change_schema}.changes_{oid}
                     (lsn, action{pk_col_decl}{new_col_names}{old_col_names})
                 VALUES (pg_current_wal_lsn(), 'U'
                         {upd_pk}{new_vals}{old_vals});
                 RETURN NEW;
             ELSIF TG_OP = 'DELETE' THEN
                 INSERT INTO {change_schema}.changes_{oid}
                     (lsn, action{pk_col_decl}{old_col_names})
                 VALUES (pg_current_wal_lsn(), 'D'
                         {del_pk}{old_vals});
                 RETURN OLD;
             END IF;
             RETURN NULL;
         END;
         $$",
        change_schema = change_schema,
        oid = oid_u32,
    );

    Spi::run(&create_fn_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create CDC trigger function: {}", e))
    })?;

    // Create the row-level trigger on the source table
    let create_trigger_sql = format!(
        "CREATE TRIGGER {trigger}
         AFTER INSERT OR UPDATE OR DELETE ON {table}
         FOR EACH ROW EXECUTE FUNCTION {change_schema}.pg_trickle_cdc_fn_{oid}()",
        trigger = trigger_name,
        table = source_table,
        change_schema = change_schema,
        oid = oid_u32,
    );

    Spi::run(&create_trigger_sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create CDC trigger on {}: {}",
            source_table, e
        ))
    })?;

    // ── TRUNCATE capture (statement-level trigger) ──────────────────
    // TRUNCATE bypasses row-level triggers entirely. A separate
    // statement-level AFTER TRUNCATE trigger writes a single marker row
    // with action='T' into the change buffer. The refresh engine
    // detects this marker and falls back to a full refresh.
    let truncate_fn_sql = format!(
        "CREATE OR REPLACE FUNCTION {change_schema}.pg_trickle_cdc_truncate_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             INSERT INTO {change_schema}.changes_{oid}
                 (lsn, action)
             VALUES (pg_current_wal_lsn(), 'T');
             RETURN NULL;
         END;
         $$",
        change_schema = change_schema,
        oid = oid_u32,
    );

    Spi::run(&truncate_fn_sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create CDC TRUNCATE trigger function: {}",
            e
        ))
    })?;

    let truncate_trigger_name = format!("pg_trickle_cdc_truncate_{}", oid_u32);
    let create_truncate_trigger_sql = format!(
        "CREATE TRIGGER {trigger}
         AFTER TRUNCATE ON {table}
         FOR EACH STATEMENT EXECUTE FUNCTION {change_schema}.pg_trickle_cdc_truncate_fn_{oid}()",
        trigger = truncate_trigger_name,
        table = source_table,
        change_schema = change_schema,
        oid = oid_u32,
    );

    Spi::run(&create_truncate_trigger_sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create CDC TRUNCATE trigger on {}: {}",
            source_table, e
        ))
    })?;

    Ok(trigger_name)
}

/// Drop a CDC trigger and its function for a source table.
pub fn drop_change_trigger(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();
    let trigger_name = format!("pg_trickle_cdc_{}", oid_u32);

    // Get the source table name for the trigger drop
    let source_table =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_oid.into()])
            .unwrap_or(None);

    // Drop the trigger (IF EXISTS to be safe)
    if let Some(ref table) = source_table {
        let drop_trigger_sql = format!("DROP TRIGGER IF EXISTS {} ON {}", trigger_name, table,);
        let _ = Spi::run(&drop_trigger_sql);

        // Drop the TRUNCATE trigger as well
        let truncate_trigger_name = format!("pg_trickle_cdc_truncate_{}", oid_u32);
        let drop_truncate_sql = format!(
            "DROP TRIGGER IF EXISTS {} ON {}",
            truncate_trigger_name, table,
        );
        let _ = Spi::run(&drop_truncate_sql);
    }

    // Drop the trigger functions (row-level + TRUNCATE)
    let drop_fn_sql = format!(
        "DROP FUNCTION IF EXISTS {}.pg_trickle_cdc_fn_{}() CASCADE",
        change_schema, oid_u32,
    );
    let _ = Spi::run(&drop_fn_sql);

    let drop_truncate_fn_sql = format!(
        "DROP FUNCTION IF EXISTS {}.pg_trickle_cdc_truncate_fn_{}() CASCADE",
        change_schema, oid_u32,
    );
    let _ = Spi::run(&drop_truncate_fn_sql);

    Ok(())
}

/// Create a change buffer table for a source table.
///
/// Uses **typed columns** (`new_col TYPE`, `old_col TYPE`) instead of
/// JSONB blobs, eliminating `to_jsonb()`/`jsonb_populate_record()` overhead.
///
/// The buffer always includes a `pk_hash BIGINT` column. When the source has
/// a primary key, pk_hash is the PK hash; for keyless tables (S10), it is
/// an all-column content hash computed by the CDC trigger.
///
/// `columns` contains the source table column definitions as
/// `(column_name, sql_type_name)` pairs from `resolve_source_column_defs()`.
pub fn create_change_buffer_table(
    source_oid: pg_sys::Oid,
    change_schema: &str,
    columns: &[(String, String)],
) -> Result<(), PgTrickleError> {
    // pk_hash is always present (PK hash or all-column content hash).
    let pk_col = ",pk_hash BIGINT";

    // Build typed column definitions: "new_col" TYPE, "old_col" TYPE
    let typed_col_defs: String = columns
        .iter()
        .map(|(name, type_name)| {
            let qname = name.replace('"', "\"\"");
            format!(",\"new_{qname}\" {type_name},\"old_{qname}\" {type_name}")
        })
        .collect::<Vec<_>>()
        .join("");

    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {schema}.changes_{oid} (\
            change_id   BIGSERIAL,\
            lsn         PG_LSN NOT NULL,\
            action      CHAR(1) NOT NULL\
            {pk_col}\
            {typed_col_defs}\
        )",
        schema = change_schema,
        oid = source_oid.to_u32(),
    );

    Spi::run(&sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create change buffer table: {}", e))
    })?;

    // AA1: Single covering index replaces the previous dual-index setup.
    //
    // Old indexes:
    //   idx_changes_<oid>_lsn_action   (lsn, action)
    //   idx_changes_<oid>_pk_hash_cid  (pk_hash, change_id)
    //
    // New index:
    //   idx_changes_<oid>_lsn_pk_cid   (lsn, pk_hash, change_id) INCLUDE (action)
    //
    // This supports:
    //   - LSN range filter: WHERE lsn > prev AND lsn <= new  → index prefix scan
    //   - pk_stats CTE:    WHERE lsn_range GROUP BY pk_hash  → sorted by pk_hash within range
    //   - Window functions: PARTITION BY pk_hash ORDER BY change_id → index-ordered within range
    //   - Action filter:   from the INCLUDE column (index-only scan)
    //
    // Reduces from 2 B-tree updates per trigger INSERT to 1, giving ~20%
    // trigger overhead reduction.
    // pk_hash is always present (PK hash or all-column content hash for keyless tables).
    let idx_sql = format!(
        "CREATE INDEX IF NOT EXISTS idx_changes_{oid}_lsn_pk_cid \
         ON {schema}.changes_{oid} (lsn, pk_hash, change_id) INCLUDE (action)",
        schema = change_schema,
        oid = source_oid.to_u32(),
    );
    Spi::run(&idx_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create change buffer index: {}", e))
    })?;

    Ok(())
}

// ── PK hash helpers ─────────────────────────────────────────────────

/// Resolve all user column definitions for a source table.
///
/// Returns `(column_name, sql_type_name)` pairs using `format_type()` to
/// get the full SQL type including modifiers (e.g. `numeric`, `character varying(100)`).
///
/// Used by `create_change_buffer_table()` and `create_change_trigger()`
/// to generate typed change buffer columns and per-column trigger INSERTs.
pub fn resolve_source_column_defs(
    source_oid: pg_sys::Oid,
) -> Result<Vec<(String, String)>, PgTrickleError> {
    let sql = format!(
        "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod) \
         FROM pg_attribute a \
         WHERE a.attrelid = {} AND a.attnum > 0 AND NOT a.attisdropped \
           AND a.attgenerated = '' \
         ORDER BY a.attnum",
        source_oid.to_u32(),
    );

    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut cols = Vec::new();
        for row in result {
            let name: String = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let type_name: String = row
                .get(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_else(|| "text".to_string());
            cols.push((name, type_name));
        }
        Ok(cols)
    })
}

/// Resolve primary key column names for a source table via `pg_constraint`.
///
/// Returns columns in key order. Returns an empty Vec if no PK exists.
pub fn resolve_pk_columns(source_oid: pg_sys::Oid) -> Result<Vec<String>, PgTrickleError> {
    let sql = format!(
        "SELECT a.attname::text \
         FROM pg_constraint c \
         JOIN pg_attribute a ON a.attrelid = c.conrelid \
           AND a.attnum = ANY(c.conkey) \
         WHERE c.conrelid = {} AND c.contype = 'p' \
         ORDER BY array_position(c.conkey, a.attnum)",
        source_oid.to_u32(),
    );

    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut pk_cols = Vec::new();
        for row in result {
            let name: String = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            pk_cols.push(name);
        }
        Ok(pk_cols)
    })
}

/// Build PL/pgSQL expressions for computing `pk_hash` in a CDC trigger.
///
/// Returns `(new_expr, old_expr)` — the expression using NEW record keys
/// and the expression using OLD record keys respectively.
///
/// For a single-column PK `id`:
///   `pgtrickle.pg_trickle_hash(NEW."id"::text)`, `pgtrickle.pg_trickle_hash(OLD."id"::text)`
///
/// For a composite PK `(a, b)`:
///   `pgtrickle.pg_trickle_hash_multi(ARRAY[NEW."a"::text, NEW."b"::text])`, ...
///
/// **S10 — Keyless tables:** When `pk_columns` is empty, computes an
/// all-column content hash from `all_columns` so that every row gets a
/// meaningful `pk_hash` even without a primary key.
fn build_pk_hash_trigger_exprs(
    pk_columns: &[String],
    all_columns: &[(String, String)],
) -> (String, String) {
    // Determine effective hash columns: PK if available, otherwise all columns.
    let hash_cols: Vec<String> = if pk_columns.is_empty() {
        all_columns.iter().map(|(name, _)| name.clone()).collect()
    } else {
        pk_columns.to_vec()
    };

    if hash_cols.is_empty() {
        // Degenerate case: table with zero columns (shouldn't happen).
        return ("0".to_string(), "0".to_string());
    }

    if hash_cols.len() == 1 {
        let col = format!("\"{}\"", hash_cols[0].replace('"', "\"\""));
        (
            format!("pgtrickle.pg_trickle_hash(NEW.{col}::text)"),
            format!("pgtrickle.pg_trickle_hash(OLD.{col}::text)"),
        )
    } else {
        let new_items: Vec<String> = hash_cols
            .iter()
            .map(|c| format!("NEW.\"{}\"::text", c.replace('"', "\"\"")))
            .collect();
        let old_items: Vec<String> = hash_cols
            .iter()
            .map(|c| format!("OLD.\"{}\"::text", c.replace('"', "\"\"")))
            .collect();
        (
            format!(
                "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
                new_items.join(", ")
            ),
            format!(
                "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
                old_items.join(", ")
            ),
        )
    }
}

// ── Frontier / Position Queries ─────────────────────────────────────────

/// Get the current WAL insert LSN (the latest write position).
///
/// This represents the "now" position in the WAL and is used as the
/// upper bound of the new frontier.
pub fn get_current_wal_lsn() -> Result<String, PgTrickleError> {
    let lsn = Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text")
        .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    Ok(lsn.unwrap_or_else(|| "0/0".to_string()))
}

/// Get the current LSN positions for all source tables of a ST.
///
/// `source_oids` — the OIDs of base tables this ST depends on.
///
/// Returns a map from source OID to the latest WAL LSN.
pub fn get_slot_positions(
    source_oids: &[pg_sys::Oid],
) -> Result<HashMap<u32, String>, PgTrickleError> {
    let mut positions = HashMap::new();

    // Get the current WAL position — this is the "now" upper bound
    let current_lsn = get_current_wal_lsn()?;

    for oid in source_oids {
        positions.insert(oid.to_u32(), current_lsn.clone());
    }

    Ok(positions)
}

/// No-op: with trigger-based CDC, changes are written directly to buffer
/// tables by the trigger. No "consumption" step needed.
///
/// Returns the count of pending changes (for informational purposes).
///
/// **Deprecated:** This function performs a full `SELECT count(*)`
/// on the change buffer table which is wasteful. It is no longer called
/// from the refresh pipeline. Kept for potential diagnostic use only.
#[allow(dead_code)]
pub fn consume_slot_changes(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<i64, PgTrickleError> {
    // With triggers, changes are already in the buffer table.
    // Just return how many uncommitted changes exist (informational).
    let count = Spi::get_one::<i64>(&format!(
        "SELECT count(*)::bigint FROM {schema}.changes_{oid}",
        schema = change_schema,
        oid = source_oid.to_u32(),
    ))
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    Ok(count.unwrap_or(0))
}

/// Delete consumed changes from the buffer table up to a given LSN.
///
/// Called after a successful differential refresh to clean up processed changes.
pub fn delete_consumed_changes(
    source_oid: pg_sys::Oid,
    change_schema: &str,
    up_to_lsn: &str,
) -> Result<i64, PgTrickleError> {
    let count = Spi::get_one_with_args::<i64>(
        &format!(
            "WITH deleted AS (\
                DELETE FROM {schema}.changes_{oid} \
                WHERE lsn <= $1::pg_lsn \
                RETURNING 1\
            ) SELECT count(*)::bigint FROM deleted",
            schema = change_schema,
            oid = source_oid.to_u32(),
        ),
        &[up_to_lsn.into()],
    )
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    Ok(count.unwrap_or(0))
}

/// Rebuild the CDC trigger function for a source table after a schema change.
///
/// Recreates only the PL/pgSQL trigger function body (using `CREATE OR REPLACE`)
/// with the **current** column set from the source table. The trigger itself and
/// the change buffer table are left untouched — existing buffer rows retain their
/// typed columns (columns for dropped source columns will simply be NULL in new
/// rows, which is harmless since the delta queries only read columns referenced
/// by the defining query).
///
/// Called from the DDL event handler when an upstream source table is altered
/// (e.g., `ALTER TABLE ... DROP COLUMN`) to ensure the trigger function no
/// longer references columns that no longer exist.
pub fn rebuild_cdc_trigger_function(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let pk_columns = resolve_pk_columns(source_oid)?;
    let columns = resolve_source_column_defs(source_oid)?;

    // Nothing to rebuild if the table has no user columns.
    if columns.is_empty() {
        return Ok(());
    }

    let oid_u32 = source_oid.to_u32();
    let (pk_hash_new, pk_hash_old) = build_pk_hash_trigger_exprs(&pk_columns, &columns);

    // pk_hash is always populated (from PK columns or all-column content hash).
    let pk_col_decl = ", pk_hash";
    let ins_pk = format!(", {pk_hash_new}");
    let upd_pk = format!(", {pk_hash_new}");
    let del_pk = format!(", {pk_hash_old}");

    let new_col_names: String = columns
        .iter()
        .map(|(name, _)| format!(", \"new_{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let old_col_names: String = columns
        .iter()
        .map(|(name, _)| format!(", \"old_{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let new_vals: String = columns
        .iter()
        .map(|(name, _)| format!(", NEW.\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let old_vals: String = columns
        .iter()
        .map(|(name, _)| format!(", OLD.\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");

    let create_fn_sql = format!(
        "CREATE OR REPLACE FUNCTION {change_schema}.pg_trickle_cdc_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             IF TG_OP = 'INSERT' THEN
                 INSERT INTO {change_schema}.changes_{oid}
                     (lsn, action{pk_col_decl}{new_col_names})
                 VALUES (pg_current_wal_lsn(), 'I'
                         {ins_pk}{new_vals});
                 RETURN NEW;
             ELSIF TG_OP = 'UPDATE' THEN
                 INSERT INTO {change_schema}.changes_{oid}
                     (lsn, action{pk_col_decl}{new_col_names}{old_col_names})
                 VALUES (pg_current_wal_lsn(), 'U'
                         {upd_pk}{new_vals}{old_vals});
                 RETURN NEW;
             ELSIF TG_OP = 'DELETE' THEN
                 INSERT INTO {change_schema}.changes_{oid}
                     (lsn, action{pk_col_decl}{old_col_names})
                 VALUES (pg_current_wal_lsn(), 'D'
                         {del_pk}{old_vals});
                 RETURN OLD;
             END IF;
             RETURN NULL;
         END;
         $$",
        change_schema = change_schema,
        oid = oid_u32,
    );

    Spi::run(&create_fn_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to rebuild CDC trigger function: {}", e))
    })?;

    // Sync change buffer table schema: add any columns that are present in
    // the current source but missing from the buffer (e.g. after ADD COLUMN).
    sync_change_buffer_columns(source_oid, change_schema, &columns)?;

    Ok(())
}

/// Sync the change buffer table schema to match the current source columns.
///
/// When a column is added to the source table (ALTER TABLE … ADD COLUMN),
/// the CDC trigger function is rebuilt to write into `"new_<col>"` and
/// `"old_<col>"` columns — but those columns don't exist in the buffer
/// table yet. This function adds any missing `new_*` / `old_*` columns
/// using `ALTER TABLE … ADD COLUMN IF NOT EXISTS`.
///
/// Columns that were dropped from the source are NOT removed from the
/// buffer here; they become harmlessly NULL-populated by the trigger and
/// are cleaned up when the ST is reinitialized (FULL refresh).
fn sync_change_buffer_columns(
    source_oid: pg_sys::Oid,
    change_schema: &str,
    columns: &[(String, String)],
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();
    let buffer_table = format!("{}.changes_{}", change_schema, oid_u32);

    // Fetch existing column names from the change buffer table.
    let existing_sql = format!(
        "SELECT attname::text \
         FROM pg_attribute \
         WHERE attrelid = '{buffer_table}'::regclass \
           AND attnum > 0 AND NOT attisdropped",
    );

    let existing_cols: std::collections::HashSet<String> = Spi::connect(|client| {
        let result = client
            .select(&existing_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut set = std::collections::HashSet::new();
        for row in result {
            if let Some(name) = row
                .get::<String>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            {
                set.insert(name);
            }
        }
        Ok(set)
    })?;

    // For each source column, add new_<col> and old_<col> if missing.
    for (col_name, col_type) in columns {
        let new_col = format!("new_{}", col_name);
        let old_col = format!("old_{}", col_name);

        if !existing_cols.contains(&new_col) {
            let sql = format!(
                "ALTER TABLE {buffer_table} ADD COLUMN IF NOT EXISTS \"{new_col}\" {col_type}"
            );
            Spi::run(&sql).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to add column \"{new_col}\" to change buffer: {e}"
                ))
            })?;
            pgrx::debug1!(
                "pg_trickle_cdc: added column \"{}\" to {}",
                new_col,
                buffer_table
            );
        }

        if !existing_cols.contains(&old_col) {
            let sql = format!(
                "ALTER TABLE {buffer_table} ADD COLUMN IF NOT EXISTS \"{old_col}\" {col_type}"
            );
            Spi::run(&sql).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to add column \"{old_col}\" to change buffer: {e}"
                ))
            })?;
            pgrx::debug1!(
                "pg_trickle_cdc: added column \"{}\" to {}",
                old_col,
                buffer_table
            );
        }
    }

    Ok(())
}

/// Check if a CDC trigger exists for a source table.
pub fn trigger_exists(source_oid: pg_sys::Oid) -> Result<bool, PgTrickleError> {
    let trigger_name = trigger_name_for_source(source_oid);
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(
            SELECT 1 FROM pg_trigger
            WHERE tgname = $1 AND tgrelid = $2
        )",
        &[trigger_name.as_str().into(), source_oid.into()],
    )
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    Ok(exists.unwrap_or(false))
}

/// Get the trigger name for a source OID.
pub fn trigger_name_for_source(source_oid: pg_sys::Oid) -> String {
    format!("pg_trickle_cdc_{}", source_oid.to_u32())
}

// ── WAL Availability Detection ─────────────────────────────────────────────

/// Check if the server supports logical replication for CDC.
///
/// Returns `true` if ALL of:
/// - `pg_trickle.cdc_mode` is `"auto"` or `"wal"` (not `"trigger"`)
/// - `wal_level` is `'logical'`
/// - `max_replication_slots` > currently used slots
///
/// When `cdc_mode = "wal"` and `wal_level != logical`, returns an error
/// instead of `false` (hard requirement).
pub fn can_use_logical_replication() -> Result<bool, PgTrickleError> {
    let cdc_mode = config::pg_trickle_cdc_mode();
    if cdc_mode == "trigger" {
        return Ok(false);
    }

    let wal_level = Spi::get_one::<String>("SELECT current_setting('wal_level')")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or_default();

    if wal_level != "logical" {
        if cdc_mode == "wal" {
            return Err(PgTrickleError::InvalidArgument(
                "pg_trickle.cdc_mode = 'wal' requires wal_level = logical".into(),
            ));
        }
        return Ok(false);
    }

    // Check available replication slots
    let available = Spi::get_one::<i64>(
        "SELECT current_setting('max_replication_slots')::bigint \
         - (SELECT count(*) FROM pg_replication_slots)",
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or(0);

    Ok(available > 0)
}

/// Check if a source table has adequate REPLICA IDENTITY for logical decoding.
///
/// Returns `true` if REPLICA IDENTITY is `DEFAULT` (has PK) or `FULL`.
/// Returns `false` if `NOTHING` or if using an index that doesn't cover
/// all needed columns.
///
/// The `relreplident` column in `pg_class` encodes:
/// - `'d'` → DEFAULT (PK-based, sufficient for UPDATE/DELETE old values)
/// - `'f'` → FULL (always includes all columns)
/// - `'n'` → NOTHING (no old values for UPDATE/DELETE)
/// - `'i'` → INDEX (uses a specific index; may or may not be sufficient)
pub fn check_replica_identity(source_oid: pg_sys::Oid) -> Result<bool, PgTrickleError> {
    let identity = Spi::get_one_with_args::<String>(
        "SELECT CASE relreplident \
           WHEN 'd' THEN 'default' \
           WHEN 'f' THEN 'full' \
           WHEN 'n' THEN 'nothing' \
           WHEN 'i' THEN 'index' \
         END FROM pg_class WHERE oid = $1",
        &[source_oid.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or_else(|| "nothing".into());

    // 'default' works if the table has a PK (which pg_trickle already checks)
    // 'full' always works
    // 'nothing' doesn't provide OLD values for UPDATE/DELETE
    // 'index' may work but needs further validation in later phases
    Ok(identity == "default" || identity == "full")
}

/// Return the REPLICA IDENTITY mode as a string for a source table.
///
/// Returns one of: `"default"`, `"full"`, `"nothing"`, `"index"`.
/// Used by the WAL transition guard (G2.2) to require REPLICA IDENTITY FULL.
pub fn get_replica_identity_mode(source_oid: pg_sys::Oid) -> Result<String, PgTrickleError> {
    let identity = Spi::get_one_with_args::<String>(
        "SELECT CASE relreplident \
           WHEN 'd' THEN 'default' \
           WHEN 'f' THEN 'full' \
           WHEN 'n' THEN 'nothing' \
           WHEN 'i' THEN 'index' \
         END FROM pg_class WHERE oid = $1",
        &[source_oid.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or_else(|| "nothing".into());

    Ok(identity)
}

/// Returns true if the relation has any user-defined row-level triggers
/// (excluding internal triggers and pg_trickle's own CDC triggers).
///
/// Used by the refresh executor to decide whether to use the explicit DML
/// path (which fires triggers with correct `TG_OP` / `OLD` / `NEW`) instead
/// of the single-pass MERGE path.
///
/// This is a lightweight query — single index scan on `pg_trigger(tgrelid)`.
pub fn has_user_triggers(st_relid: pg_sys::Oid) -> Result<bool, PgTrickleError> {
    Spi::get_one::<bool>(&format!(
        "SELECT EXISTS(\
           SELECT 1 FROM pg_trigger \
           WHERE tgrelid = {}::oid \
             AND tgisinternal = false \
             AND tgname NOT LIKE 'pgt_%' \
         )",
        st_relid.to_u32(),
    ))
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))
    .map(|v| v.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── trigger_name_for_source tests ───────────────────────────────

    #[test]
    fn test_trigger_name_for_source_basic() {
        let oid = pgrx::pg_sys::Oid::from(12345u32);
        assert_eq!(trigger_name_for_source(oid), "pg_trickle_cdc_12345");
    }

    #[test]
    fn test_trigger_name_for_source_zero() {
        let oid = pgrx::pg_sys::Oid::from(0u32);
        assert_eq!(trigger_name_for_source(oid), "pg_trickle_cdc_0");
    }

    #[test]
    fn test_trigger_name_for_source_large_oid() {
        let oid = pgrx::pg_sys::Oid::from(4294967295u32); // u32::MAX
        assert_eq!(trigger_name_for_source(oid), "pg_trickle_cdc_4294967295");
    }

    // ── build_pk_hash_trigger_exprs tests ────────────────────────────

    #[test]
    fn test_build_pk_hash_single_column() {
        let pk = vec!["id".to_string()];
        let all = vec![("id".to_string(), "integer".to_string())];
        let (new_expr, old_expr) = build_pk_hash_trigger_exprs(&pk, &all);
        assert_eq!(new_expr, r#"pgtrickle.pg_trickle_hash(NEW."id"::text)"#);
        assert_eq!(old_expr, r#"pgtrickle.pg_trickle_hash(OLD."id"::text)"#);
    }

    #[test]
    fn test_build_pk_hash_composite_key() {
        let pk = vec!["a".to_string(), "b".to_string()];
        let all = vec![
            ("a".to_string(), "integer".to_string()),
            ("b".to_string(), "text".to_string()),
        ];
        let (new_expr, old_expr) = build_pk_hash_trigger_exprs(&pk, &all);
        assert!(new_expr.contains("pgtrickle.pg_trickle_hash_multi"));
        assert!(new_expr.contains(r#"NEW."a"::text"#));
        assert!(new_expr.contains(r#"NEW."b"::text"#));
        assert!(old_expr.contains(r#"OLD."a"::text"#));
        assert!(old_expr.contains(r#"OLD."b"::text"#));
    }

    #[test]
    fn test_build_pk_hash_empty_pk_falls_back_to_all_columns() {
        // S10: Keyless table — should hash all columns, not return "0".
        let pk: Vec<String> = vec![];
        let all = vec![
            ("name".to_string(), "text".to_string()),
            ("value".to_string(), "integer".to_string()),
        ];
        let (new_expr, old_expr) = build_pk_hash_trigger_exprs(&pk, &all);
        assert!(
            new_expr.contains("pgtrickle.pg_trickle_hash_multi"),
            "Got: {new_expr}",
        );
        assert!(new_expr.contains(r#"NEW."name"::text"#), "Got: {new_expr}");
        assert!(new_expr.contains(r#"NEW."value"::text"#), "Got: {new_expr}",);
        assert!(old_expr.contains(r#"OLD."name"::text"#), "Got: {old_expr}");
        assert!(old_expr.contains(r#"OLD."value"::text"#), "Got: {old_expr}",);
    }

    #[test]
    fn test_build_pk_hash_empty_pk_single_all_column() {
        // Keyless table with a single column — uses hash() not hash_multi().
        let pk: Vec<String> = vec![];
        let all = vec![("val".to_string(), "text".to_string())];
        let (new_expr, old_expr) = build_pk_hash_trigger_exprs(&pk, &all);
        assert_eq!(new_expr, r#"pgtrickle.pg_trickle_hash(NEW."val"::text)"#);
        assert_eq!(old_expr, r#"pgtrickle.pg_trickle_hash(OLD."val"::text)"#);
    }

    #[test]
    fn test_build_pk_hash_special_chars() {
        let pk = vec![r#"col"name"#.to_string()];
        let all = vec![(r#"col"name"#.to_string(), "text".to_string())];
        let (new_expr, old_expr) = build_pk_hash_trigger_exprs(&pk, &all);
        // The embedded quote should be doubled
        assert!(new_expr.contains(r#"col""name"#), "Got: {new_expr}");
        assert!(old_expr.contains(r#"col""name"#), "Got: {old_expr}");
    }
}
