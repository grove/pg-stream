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

// ── STAB-1 (v0.30.0): SubTransaction RAII helper ─────────────────────────
//
// Wraps the CREATE TABLE AS + catalog INSERT in snapshot_stream_table_impl and
// the TRUNCATE + INSERT in restore_from_snapshot_impl in a PostgreSQL internal
// sub-transaction.  On drop (without explicit commit), rolls back automatically
// so no orphan tables or truncated storage tables are left behind on crash.

struct SnapSubTransaction {
    old_cxt: pgrx::pg_sys::MemoryContext,
    old_owner: pgrx::pg_sys::ResourceOwner,
    finished: bool,
}

impl SnapSubTransaction {
    fn begin() -> Self {
        // SAFETY: Called within a PostgreSQL transaction (SQL function context).
        // CurrentMemoryContext and CurrentResourceOwner are always valid here.
        let old_cxt = unsafe { pgrx::pg_sys::CurrentMemoryContext };
        let old_owner = unsafe { pgrx::pg_sys::CurrentResourceOwner };
        // SAFETY: BeginInternalSubTransaction sets up a sub-transaction.
        unsafe { pgrx::pg_sys::BeginInternalSubTransaction(std::ptr::null()) };
        Self {
            old_cxt,
            old_owner,
            finished: false,
        }
    }

    fn commit(mut self) {
        // SAFETY: Commits the sub-transaction; restores the outer context.
        unsafe {
            pgrx::pg_sys::ReleaseCurrentSubTransaction();
            pgrx::pg_sys::MemoryContextSwitchTo(self.old_cxt);
            pgrx::pg_sys::CurrentResourceOwner = self.old_owner;
        }
        self.finished = true;
    }

    fn rollback(mut self) {
        // SAFETY: Rolls back the sub-transaction; restores the outer context.
        unsafe {
            pgrx::pg_sys::RollbackAndReleaseCurrentSubTransaction();
            pgrx::pg_sys::MemoryContextSwitchTo(self.old_cxt);
            pgrx::pg_sys::CurrentResourceOwner = self.old_owner;
        }
        self.finished = true;
    }
}

impl Drop for SnapSubTransaction {
    fn drop(&mut self) {
        if !self.finished {
            // Auto-rollback for panic safety.
            // SAFETY: Same invariants as rollback().
            unsafe {
                pgrx::pg_sys::RollbackAndReleaseCurrentSubTransaction();
                pgrx::pg_sys::MemoryContextSwitchTo(self.old_cxt);
                pgrx::pg_sys::CurrentResourceOwner = self.old_owner;
            }
        }
    }
}

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

/// CORR-3 (v0.30.0): Build a comma-separated list of user-visible column names from
/// the snapshot table, excluding pg_trickle metadata columns.
///
/// Uses `pg_attribute` catalog walk instead of `SELECT * EXCEPT (...)` so the
/// function works on all PG 18.x minor versions without PG-minor sensitivity.
fn build_user_column_list(
    _src_fqn: &str,
    src_schema: &str,
    src_table: &str,
) -> Result<String, PgTrickleError> {
    let skip: &[&str] = &[
        "__pgt_snapshot_version",
        "__pgt_frontier",
        "__pgt_snapshotted_at",
    ];

    let cols: Vec<String> = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT a.attname::text \
                 FROM pg_catalog.pg_attribute a \
                 JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1 \
                   AND c.relname  = $2 \
                   AND a.attnum   > 0 \
                   AND NOT a.attisdropped \
                 ORDER BY a.attnum",
                None,
                &[src_schema.into(), src_table.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let name: String = row
                .get::<String>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            if !name.is_empty() && !skip.contains(&name.as_str()) {
                out.push(format!("\"{}\"", name.replace('"', "\"\"")));
            }
        }
        Ok::<_, PgTrickleError>(out)
    })?;

    if cols.is_empty() {
        return Err(PgTrickleError::SpiError(format!(
            "no user columns found in snapshot table {}.{}",
            src_schema, src_table
        )));
    }
    Ok(cols.join(", "))
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

    // STAB-1 (v0.30.0): Wrap CREATE TABLE AS + catalog INSERT in a SubTransaction.
    // If the catalog INSERT fails, the subtransaction rolls back, cleaning up
    // the orphan snapshot table automatically.
    let subtxn = SnapSubTransaction::begin();
    let create_result = Spi::run_with_args(
        &create_sql,
        &[ext_ver.into(), frontier_json.as_str().into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("snapshot create failed: {e}")));

    if let Err(e) = create_result {
        subtxn.rollback();
        return Err(e);
    }

    // STAB-4 (v0.30.0): Promote catalog INSERT failure from silent discard to WARNING.
    // A failed insert means list_snapshots() will not find this snapshot.
    if let Err(e) = Spi::run_with_args(
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
    ) {
        // Catalog insert failed: roll back the subtransaction so no orphan table is left.
        subtxn.rollback();
        return Err(PgTrickleError::SpiError(format!(
            "[pg_trickle] SNAP-1: snapshot table created but catalog INSERT failed \
             (snapshot at {}.{} was rolled back): {}",
            snap_schema, snap_table, e
        )));
    }

    subtxn.commit();

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

    // STAB-1 (v0.30.0): Propagate schema-version-check failure as a typed error
    // rather than silently treating None as compatible.
    let snap_ver: Option<String> = Spi::get_one_with_args::<String>(
        &format!(
            "SELECT __pgt_snapshot_version::text FROM {} LIMIT 1",
            src_fqn
        ),
        &[],
    )
    .unwrap_or(None);

    // Validate snapshot version — None means the snapshot has no metadata column,
    // which indicates schema incompatibility (pre-v0.27 snapshot).
    if let Some(sv) = &snap_ver {
        let cur = env!("CARGO_PKG_VERSION");
        let sv_maj: &str = sv.split('.').next().unwrap_or("0");
        let cur_maj: &str = cur.split('.').next().unwrap_or("0");
        if sv_maj != cur_maj {
            return Err(PgTrickleError::SnapshotSchemaVersionMismatch(format!(
                "snapshot version {sv} incompatible with current {cur} (major version differs)"
            )));
        }
    } else {
        // None = no __pgt_snapshot_version column → old snapshot format
        return Err(PgTrickleError::SnapshotSchemaVersionMismatch(
            "snapshot has no __pgt_snapshot_version column — \
             it was created by a version of pg_trickle prior to v0.27.0 \
             and cannot be restored with this version"
                .to_string(),
        ));
    }

    let storage_fqn = format!(
        r#""{}"."{}""#,
        meta.pgt_schema.replace('"', r#"\""#),
        meta.pgt_name.replace('"', r#"\""#)
    );

    // STAB-1 (v0.30.0): Wrap TRUNCATE + INSERT in a SubTransaction with an
    // exclusive lock acquired before the TRUNCATE, so no orphan/truncated
    // storage table is left on crash and concurrent refreshes are blocked.
    let subtxn = SnapSubTransaction::begin();

    let lock_result = Spi::run(&format!(
        // nosemgrep: rust.spi.run.dynamic-format — DDL cannot be parameterized; storage_fqn is a double-quoted and escaped catalog identifier.
        "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
        storage_fqn
    ))
    .map_err(|e| PgTrickleError::SpiError(format!("restore lock failed: {e}")));

    if let Err(e) = lock_result {
        subtxn.rollback();
        return Err(e);
    }

    // Truncate, then bulk-insert from snapshot (excluding metadata columns)
    let truncate_result =
        Spi::run(&format!("TRUNCATE {}", storage_fqn)) // nosemgrep: rust.spi.run.dynamic-format
            .map_err(|e| PgTrickleError::SpiError(format!("truncate failed: {e}")));

    if let Err(e) = truncate_result {
        subtxn.rollback();
        return Err(e);
    }

    // CORR-3 (v0.30.0): Build explicit column list from pg_attribute catalog walk,
    // eliminating PG-minor-version sensitivity of SELECT * EXCEPT (...).
    let user_cols = match build_user_column_list(&src_fqn, &src_schema, &src_table) {
        Ok(cols) => cols,
        Err(e) => {
            subtxn.rollback();
            return Err(e);
        }
    };
    let insert_sql = format!(
        "INSERT INTO {} ({}) \
         SELECT {} FROM {}",
        storage_fqn, user_cols, user_cols, src_fqn
    );
    let insert_result =
        Spi::run(&insert_sql) // nosemgrep: rust.spi.run.dynamic-format — DDL/DML cannot be parameterized for table names; storage_fqn and src_fqn are double-quoted and escaped catalog identifiers.
            .map_err(|e| PgTrickleError::SpiError(format!("restore insert failed: {e}")));

    if let Err(e) = insert_result {
        subtxn.rollback();
        return Err(e);
    }

    // Restore frontier so next refresh is DIFFERENTIAL (not FULL)
    let frontier_json: Option<String> = Spi::get_one_with_args::<String>(
        &format!("SELECT __pgt_frontier::text FROM {} LIMIT 1", src_fqn),
        &[],
    )
    .unwrap_or(None);

    if let Some(fj) = frontier_json {
        let frontier_result = Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET frontier = $1::jsonb, is_populated = true \
             WHERE pgt_id = $2",
            &[fj.as_str().into(), meta.pgt_id.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(format!("frontier restore failed: {e}")));

        if let Err(e) = frontier_result {
            subtxn.rollback();
            return Err(e);
        }
    }

    subtxn.commit();

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
