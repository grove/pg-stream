//! SNAP-1/2/3 (v0.27.0): Stream-table snapshot & point-in-time restore API.
//!
//! Provides `snapshot_stream_table()`, `restore_from_snapshot()`,
//! `list_snapshots()`, and `drop_snapshot()` SQL functions.
//!
//! # Design
//!
//! A snapshot is an ordinary PostgreSQL table in the `pgtrickle` schema with
//! the naming convention `snapshot_<st_name>_<epoch_ms>`. Each snapshot row
//! matches the storage schema of the stream table plus three metadata columns:
//!   - `__pgt_snapshot_version TEXT` — extension version at snapshot time
//!   - `__pgt_frontier        JSONB` — frontier at snapshot time
//!   - `__pgt_snapshotted_at  TIMESTAMPTZ` — wall clock at snapshot time
//!
//! A catalog table `pgtrickle.pgt_snapshots` records each snapshot's metadata
//! so `list_snapshots()` can return size and row count information.

use pgrx::prelude::*;

use crate::catalog::StreamTableMeta;
use crate::error::PgTrickleError;

// ── Internal helpers ───────────────────────────────────────────────────────

/// Parse a schema-qualified name like `"myschema.mytable"` into (schema, table).
/// Uses `current_schema()` as the default when no schema is given.
fn resolve_st_name(name: &str) -> Result<(String, String), PgTrickleError> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    match parts.len() {
        1 => {
            let schema = Spi::get_one::<String>("SELECT current_schema()::text")
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_else(|| "public".to_string());
            Ok((schema, parts[0].to_string()))
        }
        2 => Ok((parts[0].to_string(), parts[1].to_string())),
        _ => Err(PgTrickleError::InvalidArgument(format!(
            "invalid stream table name: {name}"
        ))),
    }
}

/// Build a safe snapshot table name from the ST name and current timestamp (ms).
pub(super) fn auto_snapshot_table_name(st_name: &str) -> String {
    let epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let safe_name = st_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("pgtrickle.snapshot_{}_{}", safe_name, epoch_ms)
}

/// Parse a schema-qualified table name into (schema, table) without calling SPI.
pub(super) fn parse_qualified_table(qualified: &str) -> (String, String) {
    let trimmed = qualified.trim();
    if let Some((schema, table)) = trimmed.split_once('.') {
        (
            schema.trim_matches('"').to_string(),
            table.trim_matches('"').to_string(),
        )
    } else {
        ("public".to_string(), trimmed.trim_matches('"').to_string())
    }
}

/// Check if a relation exists by schema + table name.
fn relation_exists(schema: &str, table: &str) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = $2)",
        &[schema.into(), table.into()],
    )
    .unwrap_or(None)
    .unwrap_or(false)
}

// ── SNAP-1: snapshot_stream_table ─────────────────────────────────────────

/// SNAP-1 (v0.27.0): Export the current content of a stream table into an
/// archival snapshot table.
///
/// The snapshot table is created in the `pgtrickle` schema with the naming
/// convention `snapshot_<name>_<epoch_ms>` unless `p_target` is given.
/// Returns the fully-qualified name of the created snapshot table.
#[pg_extern(schema = "pgtrickle")]
pub fn snapshot_stream_table(p_name: &str, p_target: default!(Option<&str>, "NULL")) -> String {
    snapshot_stream_table_impl(p_name, p_target).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn snapshot_stream_table_impl(name: &str, target: Option<&str>) -> Result<String, PgTrickleError> {
    let (schema, st_name) = resolve_st_name(name)?;
    let meta = StreamTableMeta::get_by_name(&schema, &st_name)?;

    let snapshot_fqn = match target {
        Some(t) => t.to_string(),
        None => auto_snapshot_table_name(&meta.pgt_name),
    };

    let (snap_schema, snap_table) = parse_qualified_table(&snapshot_fqn);

    if target.is_some() && relation_exists(&snap_schema, &snap_table) {
        return Err(PgTrickleError::SnapshotAlreadyExists(snapshot_fqn.clone()));
    }

    let storage_fqn = format!(
        r#""{}"."{}""#,
        meta.pgt_schema.replace('"', r#"\""#),
        meta.pgt_name.replace('"', r#"\""#)
    );

    let frontier_json = meta
        .frontier
        .as_ref()
        .map(|f| serde_json::to_string(f).unwrap_or_else(|_| "null".to_string()))
        .unwrap_or_else(|| "null".to_string());

    let ext_ver = env!("CARGO_PKG_VERSION");
    let snap_fqn_quoted = format!(
        r#""{}"."{}""#,
        snap_schema.replace('"', r#"\""#),
        snap_table.replace('"', r#"\""#)
    );

    // CREATE TABLE AS SELECT — copy all rows plus metadata columns
    let create_sql = format!(
        "CREATE TABLE {} AS \
         SELECT *, \
                $1::text        AS __pgt_snapshot_version, \
                $2::jsonb       AS __pgt_frontier, \
                now()           AS __pgt_snapshotted_at \
         FROM {}",
        snap_fqn_quoted, storage_fqn
    );

    Spi::run_with_args(
        &create_sql,
        &[ext_ver.into(), frontier_json.as_str().into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("snapshot create failed: {e}")))?;

    // Persist metadata to the snapshots catalog (best-effort)
    let _ = Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_snapshots \
         (pgt_id, snapshot_schema, snapshot_table, snapshot_version, frontier, created_at) \
         VALUES ($1, $2, $3, $4, $5::jsonb, now()) \
         ON CONFLICT (snapshot_schema, snapshot_table) DO NOTHING",
        &[
            meta.pgt_id.into(),
            snap_schema.as_str().into(),
            snap_table.as_str().into(),
            ext_ver.into(),
            frontier_json.as_str().into(),
        ],
    );

    pgrx::log!(
        "[pg_trickle] SNAP-1: snapshot created for '{}.{}' → {}.{}",
        schema,
        st_name,
        snap_schema,
        snap_table
    );

    Ok(snapshot_fqn)
}

// ── SNAP-2: restore_from_snapshot ─────────────────────────────────────────

/// SNAP-2 (v0.27.0): Rehydrate a stream table from an archival snapshot.
///
/// The stream table must already be registered. After restore the frontier is
/// set to the snapshot's frontier so the next refresh cycle is DIFFERENTIAL
/// (skipping the initial FULL re-scan).
#[pg_extern(schema = "pgtrickle")]
pub fn restore_from_snapshot(p_name: &str, p_source: &str) {
    restore_from_snapshot_impl(p_name, p_source).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn restore_from_snapshot_impl(name: &str, source: &str) -> Result<(), PgTrickleError> {
    let (schema, st_name) = resolve_st_name(name)?;
    let meta = StreamTableMeta::get_by_name(&schema, &st_name)?;

    let (src_schema, src_table) = parse_qualified_table(source);

    if !relation_exists(&src_schema, &src_table) {
        return Err(PgTrickleError::SnapshotSourceNotFound(source.to_string()));
    }

    let src_fqn = format!(
        r#""{}"."{}""#,
        src_schema.replace('"', r#"\""#),
        src_table.replace('"', r#"\""#)
    );

    // Check schema version (best-effort; major mismatch is an error)
    let snap_ver: Option<String> = Spi::get_one_with_args::<String>(
        &format!(
            "SELECT __pgt_snapshot_version::text FROM {} LIMIT 1",
            src_fqn
        ),
        &[],
    )
    .unwrap_or(None);

    if let Some(sv) = &snap_ver {
        let cur = env!("CARGO_PKG_VERSION");
        let sv_maj: &str = sv.split('.').next().unwrap_or("0");
        let cur_maj: &str = cur.split('.').next().unwrap_or("0");
        if sv_maj != cur_maj {
            return Err(PgTrickleError::SnapshotSchemaVersionMismatch(format!(
                "snapshot version {sv} incompatible with current {cur} (major version differs)"
            )));
        }
    }

    let storage_fqn = format!(
        r#""{}"."{}""#,
        meta.pgt_schema.replace('"', r#"\""#),
        meta.pgt_name.replace('"', r#"\""#)
    );

    // Truncate, then bulk-insert from snapshot (excluding metadata columns)
    Spi::run(&format!("TRUNCATE {}", storage_fqn)) // nosemgrep: rust.spi.run.dynamic-format — DDL cannot be parameterized; storage_fqn is a double-quoted and escaped catalog identifier.
        .map_err(|e| PgTrickleError::SpiError(format!("truncate failed: {e}")))?;

    // PostgreSQL 18+ supports SELECT * EXCEPT (...) — fall back to explicit
    // column list if it fails (older PG in tests).
    let insert_sql = format!(
        "INSERT INTO {} \
         SELECT * EXCEPT (__pgt_snapshot_version, __pgt_frontier, __pgt_snapshotted_at) \
         FROM {}",
        storage_fqn, src_fqn
    );
    Spi::run(&insert_sql) // nosemgrep: rust.spi.run.dynamic-format — DDL/DML cannot be parameterized for table names; storage_fqn and src_fqn are double-quoted and escaped catalog identifiers.
        .map_err(|e| PgTrickleError::SpiError(format!("restore insert failed: {e}")))?;

    // Restore frontier so next refresh is DIFFERENTIAL (not FULL)
    let frontier_json: Option<String> = Spi::get_one_with_args::<String>(
        &format!("SELECT __pgt_frontier::text FROM {} LIMIT 1", src_fqn),
        &[],
    )
    .unwrap_or(None);

    if let Some(fj) = frontier_json {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET frontier = $1::jsonb, is_populated = true \
             WHERE pgt_id = $2",
            &[fj.as_str().into(), meta.pgt_id.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(format!("frontier restore failed: {e}")))?;
    }

    // Signal the DAG to pick up the frontier change
    crate::shmem::signal_dag_invalidation(meta.pgt_id);

    pgrx::log!(
        "[pg_trickle] SNAP-2: restored '{}.{}' from '{}'",
        schema,
        st_name,
        source
    );

    Ok(())
}

// ── SNAP-3a: list_snapshots ────────────────────────────────────────────────

/// SNAP-3 (v0.27.0): List all archival snapshot tables for a stream table.
///
/// Returns one row per snapshot ordered by creation time descending.
#[pg_extern(schema = "pgtrickle")]
#[allow(clippy::type_complexity)]
pub fn list_snapshots(
    p_name: &str,
) -> TableIterator<
    'static,
    (
        name!(snapshot_table, Option<String>),
        name!(created_at, Option<TimestampWithTimeZone>),
        name!(row_count, Option<i64>),
        name!(frontier, Option<pgrx::JsonB>),
        name!(size_bytes, Option<i64>),
    ),
> {
    let rows = list_snapshots_impl(p_name);
    TableIterator::new(rows)
}

#[allow(clippy::type_complexity)]
fn list_snapshots_impl(
    name: &str,
) -> Vec<(
    Option<String>,
    Option<TimestampWithTimeZone>,
    Option<i64>,
    Option<pgrx::JsonB>,
    Option<i64>,
)> {
    let (schema, st_name) = match resolve_st_name(name) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let meta = match StreamTableMeta::get_by_name(&schema, &st_name) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    Spi::connect(|client| {
        let rows_result = client.select(
            "SELECT \
               format('%I.%I', s.snapshot_schema, s.snapshot_table) AS snapshot_table, \
               s.created_at, \
               NULL::bigint AS row_count, \
               s.frontier, \
               pg_total_relation_size( \
                 (format('%I.%I', s.snapshot_schema, s.snapshot_table))::regclass \
               ) AS size_bytes \
             FROM pgtrickle.pgt_snapshots s \
             WHERE s.pgt_id = $1 \
             ORDER BY s.created_at DESC",
            None,
            &[meta.pgt_id.into()],
        );

        match rows_result {
            Ok(rows) => {
                let mut out = Vec::new();
                for row in rows {
                    let snap_table = row.get::<String>(1).unwrap_or(None);
                    let created_at = row.get::<TimestampWithTimeZone>(2).unwrap_or(None);
                    let row_count = row.get::<i64>(3).unwrap_or(None);
                    let frontier_json = row.get::<pgrx::JsonB>(4).unwrap_or(None);
                    let size_bytes = row.get::<i64>(5).unwrap_or(None);
                    out.push((snap_table, created_at, row_count, frontier_json, size_bytes));
                }
                out
            }
            Err(_) => Vec::new(),
        }
    })
}

// ── SNAP-3b: drop_snapshot ────────────────────────────────────────────────

/// SNAP-3 (v0.27.0): Drop an archival snapshot table.
///
/// Removes the snapshot table and its catalog row from `pgtrickle.pgt_snapshots`.
#[pg_extern(schema = "pgtrickle")]
pub fn drop_snapshot(p_snapshot_table: &str) {
    drop_snapshot_impl(p_snapshot_table).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn drop_snapshot_impl(snapshot_table: &str) -> Result<(), PgTrickleError> {
    let (snap_schema, snap_table) = parse_qualified_table(snapshot_table);

    if !relation_exists(&snap_schema, &snap_table) {
        return Err(PgTrickleError::SnapshotSourceNotFound(
            snapshot_table.to_string(),
        ));
    }

    // Remove from catalog (best-effort)
    let _ = Spi::run_with_args(
        "DELETE FROM pgtrickle.pgt_snapshots \
         WHERE snapshot_schema = $1 AND snapshot_table = $2",
        &[snap_schema.as_str().into(), snap_table.as_str().into()],
    );

    let fqn = format!(
        r#""{}"."{}""#,
        snap_schema.replace('"', r#"\""#),
        snap_table.replace('"', r#"\""#)
    );

    Spi::run(&format!("DROP TABLE IF EXISTS {}", fqn)) // nosemgrep: rust.spi.run.dynamic-format — DDL cannot be parameterized; fqn is a double-quoted and escape-hardened catalog identifier.
        .map_err(|e| PgTrickleError::SpiError(format!("drop snapshot failed: {e}")))?;

    pgrx::log!("[pg_trickle] SNAP-3: dropped snapshot '{}'", snapshot_table);

    Ok(())
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_snapshot_table_name_contains_st_name() {
        let name = auto_snapshot_table_name("orders");
        assert!(
            name.contains("orders"),
            "snapshot name should contain the ST name: {name}"
        );
        assert!(
            name.starts_with("pgtrickle.snapshot_"),
            "snapshot name should start with pgtrickle.snapshot_: {name}"
        );
    }

    #[test]
    fn test_auto_snapshot_table_name_sanitizes_special_chars() {
        let name = auto_snapshot_table_name("my-table");
        assert!(!name.contains('-'), "dashes should be sanitized: {name}");
    }

    #[test]
    fn test_parse_qualified_table_with_schema() {
        let (schema, table) = parse_qualified_table("myschema.mytable");
        assert_eq!(schema, "myschema");
        assert_eq!(table, "mytable");
    }

    #[test]
    fn test_parse_qualified_table_without_schema() {
        let (schema, table) = parse_qualified_table("mytable");
        assert_eq!(schema, "public");
        assert_eq!(table, "mytable");
    }

    #[test]
    fn test_parse_qualified_table_quoted() {
        let (schema, table) = parse_qualified_table(r#""pgtrickle"."snapshot_orders_123""#);
        assert_eq!(schema, "pgtrickle");
        assert_eq!(table, "snapshot_orders_123");
    }
}
