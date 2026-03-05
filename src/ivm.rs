//! Transactional IVM (Incremental View Maintenance) for IMMEDIATE mode.
//!
//! This module implements statement-level AFTER trigger-based IVM that
//! maintains stream tables synchronously within the same transaction as
//! the base table DML.
//!
//! ## Architecture
//!
//! Instead of row-level CDC triggers that write to change buffer tables
//! (used by DIFFERENTIAL mode), IMMEDIATE mode uses:
//!
//! 1. **Statement-level BEFORE triggers** on each base table.
//!    These acquire an `ExclusiveLock` on the stream table to prevent
//!    concurrent conflicting updates.
//!
//! 2. **Statement-level AFTER triggers** with transition tables
//!    (`REFERENCING NEW TABLE AS ... OLD TABLE AS ...`).
//!    The PL/pgSQL trigger body copies transition table data into
//!    temp tables (`__pgt_newtable_<oid>` / `__pgt_oldtable_<oid>`),
//!    then calls the Rust `pgt_ivm_apply_delta` function.
//!
//! 3. **Delta computation** reuses the existing DVM engine with
//!    `DeltaSource::TransitionTable` — the `Scan` operator reads
//!    from the temp tables instead of change buffer tables.
//!
//! 4. **Delta application** uses the same explicit DML path (DELETE +
//!    UPDATE + INSERT) as the deferred mode's user-trigger path.
//!
//! ## Current Limitations
//!
//! - TRUNCATE on a base table causes a full refresh of the stream table.
//! - Uses temp tables for transition table access (ENR-based access is a
//!   future optimization).
//! - Recursive CTEs emit a warning about potential stack-depth issues.
//! - TopK tables use micro-refresh (recompute top-K on each DML) gated by
//!   `pg_trickle.ivm_topk_max_limit`.

use crate::error::PgTrickleError;
use pgrx::prelude::*;

// ── Lock Mode Analysis ─────────────────────────────────────────────────

/// The type of lock acquired by BEFORE triggers on the stream table.
///
/// Determines the concurrency strategy for IMMEDIATE mode:
/// - `Exclusive` — advisory lock prevents all concurrent IVM updates.
///   Required for queries with aggregates, DISTINCT, or multi-table joins
///   where concurrent modifications could produce incorrect results.
/// - `RowExclusive` — lighter lock allowing concurrent inserts to proceed
///   in parallel when safe. Suitable for simple single-table SELECT queries
///   without aggregates or DISTINCT, where each row_id is independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IvmLockMode {
    /// ExclusiveLock equivalent — serializes all IVM updates.
    Exclusive,
    /// RowExclusiveLock equivalent — allows concurrent non-conflicting inserts.
    RowExclusive,
}

impl IvmLockMode {
    /// Analyze a defining query to determine the appropriate lock mode.
    ///
    /// Uses `RowExclusive` only when the query is a simple single-table
    /// projection/filter without aggregates or DISTINCT. All other queries
    /// use `Exclusive` to prevent incorrect delta computation from
    /// concurrent modifications.
    pub fn for_query(defining_query: &str) -> Self {
        // Try to parse the defining query. If parsing fails, default to Exclusive.
        let result = match crate::dvm::parse_defining_query(defining_query) {
            Ok(tree) => tree,
            Err(_) => return IvmLockMode::Exclusive,
        };

        if Self::is_simple_scan_chain(&result) {
            IvmLockMode::RowExclusive
        } else {
            IvmLockMode::Exclusive
        }
    }

    /// Check if an OpTree is a simple scan chain:
    /// Scan → optional Filter → optional Project
    /// (no joins, no aggregates, no distinct, no subqueries, etc.)
    fn is_simple_scan_chain(tree: &crate::dvm::parser::OpTree) -> bool {
        use crate::dvm::parser::OpTree;
        match tree {
            OpTree::Scan { .. } => true,
            OpTree::Filter { child, .. } | OpTree::Project { child, .. } => {
                Self::is_simple_scan_chain(child)
            }
            _ => false,
        }
    }
}

// ── Delta SQL Template Cache ───────────────────────────────────────────

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Cache key for IVM delta templates: (pgt_id, source_oid, has_new, has_old).
///
/// The delta SQL varies by which transition tables are present:
/// - INSERT: has_new=true, has_old=false
/// - UPDATE: has_new=true, has_old=true
/// - DELETE: has_new=false, has_old=true
#[derive(Hash, PartialEq, Eq, Clone)]
struct IvmCacheKey {
    pgt_id: i64,
    source_oid: u32,
    has_new: bool,
    has_old: bool,
}

/// Cached IVM delta template — stores the pre-computed delta SQL,
/// output columns, and the hash of the defining query for invalidation.
#[derive(Clone)]
struct CachedIvmDelta {
    /// Hash of the defining query — for staleness detection.
    defining_query_hash: u64,
    /// Pre-computed delta SQL (references transition temp tables).
    delta_sql: String,
    /// User-facing column names from the delta result.
    user_columns: Vec<String>,
}

fn hash_str(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

thread_local! {
    /// Per-session cache of IVM delta SQL templates.
    /// Keyed by (pgt_id, source_oid, has_new, has_old).
    /// Invalidated when the defining query hash changes or the
    /// shared cache generation counter advances.
    static IVM_DELTA_CACHE: RefCell<HashMap<IvmCacheKey, CachedIvmDelta>> =
        RefCell::new(HashMap::new());

    /// Local snapshot of the shared cache generation counter.
    static LOCAL_IVM_CACHE_GEN: Cell<u64> = const { Cell::new(0) };
}

/// Invalidate all cached IVM delta templates for a given pgt_id.
pub fn invalidate_ivm_delta_cache(pgt_id: i64) {
    IVM_DELTA_CACHE.with(|cache| {
        cache.borrow_mut().retain(|k, _| k.pgt_id != pgt_id);
    });
}

/// Install IVM triggers on a source table for an IMMEDIATE-mode stream table.
///
/// Creates statement-level BEFORE and AFTER triggers for INSERT, UPDATE,
/// DELETE, and TRUNCATE operations. The AFTER triggers use `REFERENCING`
/// clauses to capture transition tables.
///
/// # Arguments
///
/// * `source_oid` — OID of the base table to install triggers on.
/// * `pgt_id` — The stream table's `pgt_id` (catalog primary key).
/// * `st_relid` — OID of the stream table (for locking).
/// * `lock_mode` — The lock mode to use for BEFORE triggers.
pub fn setup_ivm_triggers(
    source_oid: pg_sys::Oid,
    pgt_id: i64,
    st_relid: pg_sys::Oid,
    lock_mode: IvmLockMode,
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();
    let st_oid_u32 = st_relid.to_u32();

    // Resolve the fully-qualified source table name.
    let source_table =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_oid.into()])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            .ok_or_else(|| {
                PgTrickleError::NotFound(format!("Table with OID {} not found", oid_u32))
            })?;

    // ── BEFORE triggers (statement-level) ────────────────────────────
    // Acquire a lock on the stream table to prevent concurrent conflicting
    // updates. The lock mode depends on the query complexity:
    // - Exclusive (advisory lock): for queries with aggregates, DISTINCT,
    //   or multi-table joins where concurrent writes can conflict.
    // - RowExclusive: for simple single-table scans where row_ids are
    //   independent and concurrent inserts are safe.
    let lock_body = match lock_mode {
        IvmLockMode::Exclusive => format!("PERFORM pg_advisory_xact_lock({st_oid_u32}::BIGINT);"),
        IvmLockMode::RowExclusive => {
            format!("PERFORM pg_try_advisory_xact_lock({st_oid_u32}::BIGINT);")
        }
    };

    for op in &["INSERT", "UPDATE", "DELETE"] {
        let trigger_name = format!(
            "pgt_ivm_before_{op}_{pgt_id}",
            op = op.to_lowercase(),
            pgt_id = pgt_id,
        );

        // The BEFORE trigger function acquires a lock on the stream table.
        let fn_name = format!("pgtrickle.pgt_ivm_before_fn_{pgt_id}_{oid_u32}");

        let create_fn_sql = format!(
            "CREATE OR REPLACE FUNCTION {fn_name}()
             RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 -- Lock stream table for IVM ({lock_mode:?} mode).
                 {lock_body}
                 RETURN NULL;
             END;
             $$"
        );
        Spi::run(&create_fn_sql).map_err(|e| {
            PgTrickleError::SpiError(format!(
                "Failed to create IVM BEFORE trigger function: {}",
                e
            ))
        })?;

        let create_trigger_sql = format!(
            "CREATE TRIGGER {trigger_name}
             BEFORE {op} ON {source_table}
             FOR EACH STATEMENT
             EXECUTE FUNCTION {fn_name}()"
        );
        Spi::run(&create_trigger_sql).map_err(|e| {
            PgTrickleError::SpiError(format!(
                "Failed to create IVM BEFORE trigger on {}: {}",
                source_table, e
            ))
        })?;
    }

    // ── BEFORE TRUNCATE trigger ──────────────────────────────────────
    let trunc_before_trigger = format!("pgt_ivm_before_trunc_{pgt_id}");
    let trunc_before_fn = format!("pgtrickle.pgt_ivm_before_fn_{pgt_id}_{oid_u32}");
    // Reuse the same lock function for TRUNCATE.
    let create_trunc_before_sql = format!(
        "CREATE TRIGGER {trunc_before_trigger}
         BEFORE TRUNCATE ON {source_table}
         FOR EACH STATEMENT
         EXECUTE FUNCTION {trunc_before_fn}()"
    );
    Spi::run(&create_trunc_before_sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create IVM BEFORE TRUNCATE trigger on {}: {}",
            source_table, e
        ))
    })?;

    // ── AFTER triggers (statement-level, with transition tables) ─────
    // Each DML operation has its own AFTER trigger with the appropriate
    // REFERENCING clause. The PL/pgSQL body:
    // 1. Creates temp tables from the transition tables
    // 2. Calls the Rust pgt_ivm_apply_delta function
    // 3. Drops the temp tables

    // AFTER INSERT: only NEW table
    let after_ins_fn = format!("pgtrickle.pgt_ivm_after_ins_fn_{pgt_id}_{oid_u32}");
    let create_after_ins_fn = format!(
        "CREATE OR REPLACE FUNCTION {after_ins_fn}()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             -- Copy transition table to temp table for SPI access
             CREATE TEMP TABLE __pgt_newtable_{oid_u32} ON COMMIT DROP AS
                 SELECT * FROM __pgt_newtable;

             -- Apply delta via the DVM engine
             PERFORM pgtrickle.pgt_ivm_apply_delta({pgt_id}, {oid_u32}, true, false);

             -- Clean up temp table
             DROP TABLE IF EXISTS __pgt_newtable_{oid_u32};
             RETURN NULL;
         END;
         $$"
    );
    Spi::run(&create_after_ins_fn).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create IVM AFTER INSERT function: {}", e))
    })?;

    let after_ins_trigger = format!("pgt_ivm_after_ins_{pgt_id}");
    let create_after_ins_trigger = format!(
        "CREATE TRIGGER {after_ins_trigger}
         AFTER INSERT ON {source_table}
         REFERENCING NEW TABLE AS __pgt_newtable
         FOR EACH STATEMENT
         EXECUTE FUNCTION {after_ins_fn}()"
    );
    Spi::run(&create_after_ins_trigger).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create IVM AFTER INSERT trigger on {}: {}",
            source_table, e
        ))
    })?;

    // AFTER UPDATE: both OLD and NEW tables
    let after_upd_fn = format!("pgtrickle.pgt_ivm_after_upd_fn_{pgt_id}_{oid_u32}");
    let create_after_upd_fn = format!(
        "CREATE OR REPLACE FUNCTION {after_upd_fn}()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             CREATE TEMP TABLE __pgt_newtable_{oid_u32} ON COMMIT DROP AS
                 SELECT * FROM __pgt_newtable;
             CREATE TEMP TABLE __pgt_oldtable_{oid_u32} ON COMMIT DROP AS
                 SELECT * FROM __pgt_oldtable;

             PERFORM pgtrickle.pgt_ivm_apply_delta({pgt_id}, {oid_u32}, true, true);

             DROP TABLE IF EXISTS __pgt_newtable_{oid_u32};
             DROP TABLE IF EXISTS __pgt_oldtable_{oid_u32};
             RETURN NULL;
         END;
         $$"
    );
    Spi::run(&create_after_upd_fn).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create IVM AFTER UPDATE function: {}", e))
    })?;

    let after_upd_trigger = format!("pgt_ivm_after_upd_{pgt_id}");
    let create_after_upd_trigger = format!(
        "CREATE TRIGGER {after_upd_trigger}
         AFTER UPDATE ON {source_table}
         REFERENCING OLD TABLE AS __pgt_oldtable NEW TABLE AS __pgt_newtable
         FOR EACH STATEMENT
         EXECUTE FUNCTION {after_upd_fn}()"
    );
    Spi::run(&create_after_upd_trigger).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create IVM AFTER UPDATE trigger on {}: {}",
            source_table, e
        ))
    })?;

    // AFTER DELETE: only OLD table
    let after_del_fn = format!("pgtrickle.pgt_ivm_after_del_fn_{pgt_id}_{oid_u32}");
    let create_after_del_fn = format!(
        "CREATE OR REPLACE FUNCTION {after_del_fn}()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             CREATE TEMP TABLE __pgt_oldtable_{oid_u32} ON COMMIT DROP AS
                 SELECT * FROM __pgt_oldtable;

             PERFORM pgtrickle.pgt_ivm_apply_delta({pgt_id}, {oid_u32}, false, true);

             DROP TABLE IF EXISTS __pgt_oldtable_{oid_u32};
             RETURN NULL;
         END;
         $$"
    );
    Spi::run(&create_after_del_fn).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create IVM AFTER DELETE function: {}", e))
    })?;

    let after_del_trigger = format!("pgt_ivm_after_del_{pgt_id}");
    let create_after_del_trigger = format!(
        "CREATE TRIGGER {after_del_trigger}
         AFTER DELETE ON {source_table}
         REFERENCING OLD TABLE AS __pgt_oldtable
         FOR EACH STATEMENT
         EXECUTE FUNCTION {after_del_fn}()"
    );
    Spi::run(&create_after_del_trigger).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create IVM AFTER DELETE trigger on {}: {}",
            source_table, e
        ))
    })?;

    // AFTER TRUNCATE: no transition tables, full refresh
    let after_trunc_fn = format!("pgtrickle.pgt_ivm_after_trunc_fn_{pgt_id}_{oid_u32}");
    let create_after_trunc_fn = format!(
        "CREATE OR REPLACE FUNCTION {after_trunc_fn}()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             -- TRUNCATE: fully refresh the stream table
             PERFORM pgtrickle.pgt_ivm_handle_truncate({pgt_id});
             RETURN NULL;
         END;
         $$"
    );
    Spi::run(&create_after_trunc_fn).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create IVM AFTER TRUNCATE function: {}",
            e
        ))
    })?;

    let after_trunc_trigger = format!("pgt_ivm_after_trunc_{pgt_id}");
    let create_after_trunc_trigger = format!(
        "CREATE TRIGGER {after_trunc_trigger}
         AFTER TRUNCATE ON {source_table}
         FOR EACH STATEMENT
         EXECUTE FUNCTION {after_trunc_fn}()"
    );
    Spi::run(&create_after_trunc_trigger).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create IVM AFTER TRUNCATE trigger on {}: {}",
            source_table, e
        ))
    })?;

    pgrx::log!(
        "[pg_trickle] Installed IVM triggers on {} for ST pgt_id={}",
        source_table,
        pgt_id,
    );

    Ok(())
}

/// Remove IVM triggers from a source table for a specific stream table.
///
/// Drops all BEFORE/AFTER triggers and their PL/pgSQL functions that were
/// created by `setup_ivm_triggers`.
pub fn cleanup_ivm_triggers(source_relid: pg_sys::Oid, pgt_id: i64) -> Result<(), PgTrickleError> {
    let oid_u32 = source_relid.to_u32();

    // Get the source table name for trigger drop.
    let source_table =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_relid.into()])
            .unwrap_or(None);

    if let Some(ref table) = source_table {
        // Drop all triggers for this pgt_id on this source table.
        let trigger_names = [
            format!("pgt_ivm_before_insert_{pgt_id}"),
            format!("pgt_ivm_before_update_{pgt_id}"),
            format!("pgt_ivm_before_delete_{pgt_id}"),
            format!("pgt_ivm_before_trunc_{pgt_id}"),
            format!("pgt_ivm_after_ins_{pgt_id}"),
            format!("pgt_ivm_after_upd_{pgt_id}"),
            format!("pgt_ivm_after_del_{pgt_id}"),
            format!("pgt_ivm_after_trunc_{pgt_id}"),
        ];

        for trigger in &trigger_names {
            let drop_sql = format!("DROP TRIGGER IF EXISTS {trigger} ON {table}");
            let _ = Spi::run(&drop_sql);
        }
    }

    // Drop the trigger functions (CASCADE to be safe).
    let fn_names = [
        format!("pgtrickle.pgt_ivm_before_fn_{pgt_id}_{oid_u32}"),
        format!("pgtrickle.pgt_ivm_after_ins_fn_{pgt_id}_{oid_u32}"),
        format!("pgtrickle.pgt_ivm_after_upd_fn_{pgt_id}_{oid_u32}"),
        format!("pgtrickle.pgt_ivm_after_del_fn_{pgt_id}_{oid_u32}"),
        format!("pgtrickle.pgt_ivm_after_trunc_fn_{pgt_id}_{oid_u32}"),
    ];

    for fn_name in &fn_names {
        let drop_sql = format!("DROP FUNCTION IF EXISTS {fn_name}() CASCADE");
        let _ = Spi::run(&drop_sql);
    }

    pgrx::log!(
        "[pg_trickle] Cleaned up IVM triggers for source OID {} (pgt_id={})",
        oid_u32,
        pgt_id,
    );

    Ok(())
}

/// SQL-callable function: apply IVM delta for a stream table.
///
/// Called from the PL/pgSQL AFTER trigger body after the transition tables
/// have been copied to temp tables (`__pgt_newtable_<oid>` /
/// `__pgt_oldtable_<oid>`).
///
/// This function:
/// 1. Loads the stream table metadata from the catalog.
/// 2. Generates (or retrieves from cache) the delta SQL using the DVM engine.
/// 3. Applies the delta (DELETE + INSERT ON CONFLICT) to the stream table.
///
/// Delta SQL templates are cached per (pgt_id, source_oid, has_new, has_old)
/// to avoid re-parsing the defining query on every trigger invocation.
#[pg_extern(schema = "pgtrickle")]
fn pgt_ivm_apply_delta(
    pgt_id: i64,
    source_oid: i32,
    has_new: bool,
    has_old: bool,
) -> Result<(), PgTrickleError> {
    use crate::catalog::StreamTableMeta;

    // EC-25/EC-26: Set the internal_refresh flag so DML guard triggers
    // allow the IVM delta application to modify the storage table.
    Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    let source_oid_u32 = source_oid as u32;

    // Load stream table metadata.
    let st = StreamTableMeta::get_by_id(pgt_id)?.ok_or_else(|| {
        PgTrickleError::NotFound(format!("Stream table with pgt_id={pgt_id} not found"))
    })?;

    // ── Task 5.2: TopK micro-refresh in IMMEDIATE mode ──────────────
    // Instead of delta computation, recompute the top-K rows and diff
    // against the current stream table contents.
    if st.topk_limit.is_some() {
        return apply_topk_micro_refresh(&st);
    }

    // Try to get cached delta SQL template.
    let (delta_sql, user_columns) =
        get_or_compute_ivm_delta(pgt_id, source_oid_u32, has_new, has_old, &st)?;

    // Build the qualified stream table name.
    let st_qualified = format!(
        "\"{}\".\"{}\"",
        st.pgt_schema.replace('"', "\"\""),
        st.pgt_name.replace('"', "\"\""),
    );

    // ── Apply the delta via explicit DML ─────────────────────────────
    // Materialize the delta into a temp table, then apply D/U/I.
    let delta_table = format!("__pgt_ivm_delta_{pgt_id}");

    let materialize_sql = format!("CREATE TEMP TABLE {delta_table} ON COMMIT DROP AS {delta_sql}");
    Spi::run(&materialize_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to materialize IVM delta: {e}")))?;

    // Count rows to check if there's work to do.
    let delta_count = Spi::get_one::<i64>(&format!("SELECT count(*) FROM {delta_table}"))
        .unwrap_or(Some(0))
        .unwrap_or(0);

    if delta_count > 0 {
        // Build column list for the merge/apply.
        let col_list = user_columns
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");

        // DELETE: remove rows that were deleted or updated (action = 'D').
        //
        // EC-06: For keyless sources, use counted DELETE (ROW_NUMBER
        // matching) to avoid deleting ALL duplicates when only a subset
        // should be removed.
        let delete_sql = if st.has_keyless_source {
            format!(
                "DELETE FROM {st_qualified} \
                 WHERE ctid IN (\
                   SELECT numbered_st.st_ctid \
                   FROM (\
                     SELECT st2.ctid AS st_ctid, \
                            st2.__pgt_row_id, \
                            ROW_NUMBER() OVER (\
                              PARTITION BY st2.__pgt_row_id ORDER BY st2.ctid\
                            ) AS st_rn \
                     FROM {st_qualified} st2 \
                     WHERE st2.__pgt_row_id IN (\
                       SELECT DISTINCT __pgt_row_id \
                       FROM {delta_table} \
                       WHERE __pgt_action = 'D'\
                     )\
                   ) numbered_st \
                   JOIN (\
                     SELECT __pgt_row_id, \
                            COUNT(*)::INT AS del_count \
                     FROM {delta_table} \
                     WHERE __pgt_action = 'D' \
                     GROUP BY __pgt_row_id\
                   ) dc ON numbered_st.__pgt_row_id = dc.__pgt_row_id \
                   WHERE numbered_st.st_rn <= dc.del_count\
                 )"
            )
        } else {
            format!(
                "DELETE FROM {st_qualified} AS t
                 USING {delta_table} AS d
                 WHERE d.__pgt_action = 'D'
                   AND t.__pgt_row_id = d.__pgt_row_id"
            )
        };
        Spi::run(&delete_sql)
            .map_err(|e| PgTrickleError::SpiError(format!("IVM delta DELETE failed: {e}")))?;

        // INSERT: add rows that were inserted or updated (action = 'I').
        //
        // EC-06: For keyless sources, use plain INSERT (no ON CONFLICT)
        // since duplicate __pgt_row_id values are expected.
        let insert_sql = if st.has_keyless_source {
            format!(
                "INSERT INTO {st_qualified} (__pgt_row_id, {col_list})
                 SELECT d.__pgt_row_id, {d_col_list}
                 FROM {delta_table} d
                 WHERE d.__pgt_action = 'I'",
                d_col_list = user_columns
                    .iter()
                    .map(|c| format!("d.\"{}\"", c.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        } else {
            format!(
                "INSERT INTO {st_qualified} (__pgt_row_id, {col_list})
                 SELECT d.__pgt_row_id, {d_col_list}
                 FROM {delta_table} d
                 WHERE d.__pgt_action = 'I'
                 ON CONFLICT (__pgt_row_id) DO UPDATE SET {update_set}",
                d_col_list = user_columns
                    .iter()
                    .map(|c| format!("d.\"{}\"", c.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(", "),
                update_set = user_columns
                    .iter()
                    .map(|c| {
                        let quoted = format!("\"{}\"", c.replace('"', "\"\""));
                        format!("{quoted} = EXCLUDED.{quoted}")
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        };
        Spi::run(&insert_sql)
            .map_err(|e| PgTrickleError::SpiError(format!("IVM delta INSERT failed: {e}")))?;
    }

    // Clean up the delta temp table.
    let _ = Spi::run(&format!("DROP TABLE IF EXISTS {delta_table}"));

    pgrx::debug1!(
        "[pg_trickle] IVM delta applied for pgt_id={}, source_oid={}, delta_rows={}",
        pgt_id,
        source_oid_u32,
        delta_count,
    );

    Ok(())
}

/// Task 5.2: TopK micro-refresh for IMMEDIATE mode.
///
/// Instead of delta-based incremental maintenance, recompute the top-K rows
/// using the full defining query (with ORDER BY + LIMIT) and diff against
/// the current stream table contents. This is a "micro-full-refresh" scoped
/// to K rows — fast enough for inline trigger execution when K is small.
fn apply_topk_micro_refresh(st: &crate::catalog::StreamTableMeta) -> Result<(), PgTrickleError> {
    let st_qualified = format!(
        "\"{}\".\"{}\"",
        st.pgt_schema.replace('"', "\"\""),
        st.pgt_name.replace('"', "\"\""),
    );

    let topk_limit = st.topk_limit.ok_or_else(|| {
        PgTrickleError::InternalError("TopK micro-refresh called on non-TopK stream table".into())
    })?;
    let topk_order = st.topk_order_by.as_deref().ok_or_else(|| {
        PgTrickleError::InternalError("TopK stream table missing order_by metadata".into())
    })?;

    // Build the full TopK query from the stored defining query + metadata.
    let topk_query = if let Some(offset) = st.topk_offset {
        format!(
            "{} ORDER BY {} LIMIT {} OFFSET {}",
            st.defining_query, topk_order, topk_limit, offset
        )
    } else {
        format!(
            "{} ORDER BY {} LIMIT {}",
            st.defining_query, topk_order, topk_limit
        )
    };

    let row_id_expr = crate::dvm::row_id_expr_for_query(&st.defining_query);
    let columns = crate::dvm::get_defining_query_columns(&st.defining_query)?;

    // Materialize the new top-K into a temp table.
    let new_topk = format!("__pgt_ivm_topk_{}", st.pgt_id);
    let materialize = format!(
        "CREATE TEMP TABLE {new_topk} ON COMMIT DROP AS \
         SELECT {row_id_expr} AS __pgt_row_id, sub.* \
         FROM ({topk_query}) sub"
    );
    Spi::run(&materialize).map_err(|e| {
        PgTrickleError::SpiError(format!("TopK micro-refresh materialize failed: {e}"))
    })?;

    // DELETE rows that left the top-K.
    let delete_sql = format!(
        "DELETE FROM {st_qualified} AS t \
         WHERE NOT EXISTS (\
             SELECT 1 FROM {new_topk} AS n WHERE n.__pgt_row_id = t.__pgt_row_id\
         )"
    );
    Spi::run(&delete_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("TopK micro-refresh DELETE failed: {e}")))?;

    // Build column lists for INSERT.
    let col_list = columns
        .iter()
        .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let n_col_list = columns
        .iter()
        .map(|c| format!("n.\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let update_set = columns
        .iter()
        .map(|c| {
            let q = format!("\"{}\"", c.replace('"', "\"\""));
            format!("{q} = EXCLUDED.{q}")
        })
        .collect::<Vec<_>>()
        .join(", ");

    // INSERT new rows that entered the top-K (or update changed rows).
    let insert_sql = format!(
        "INSERT INTO {st_qualified} (__pgt_row_id, {col_list}) \
         SELECT n.__pgt_row_id, {n_col_list} \
         FROM {new_topk} n \
         ON CONFLICT (__pgt_row_id) DO UPDATE SET {update_set}"
    );
    Spi::run(&insert_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("TopK micro-refresh INSERT failed: {e}")))?;

    // Clean up.
    let _ = Spi::run(&format!("DROP TABLE IF EXISTS {new_topk}"));

    pgrx::debug1!(
        "[pg_trickle] TopK micro-refresh applied for pgt_id={}",
        st.pgt_id,
    );

    Ok(())
}

/// Get or compute the IVM delta SQL for a given trigger invocation.
///
/// Uses a thread-local cache to avoid re-parsing and re-differentiating
/// the defining query on every trigger invocation. Cache entries are
/// keyed by (pgt_id, source_oid, has_new, has_old) and invalidated when
/// the defining query changes or the shared cache generation advances.
fn get_or_compute_ivm_delta(
    pgt_id: i64,
    source_oid: u32,
    has_new: bool,
    has_old: bool,
    st: &crate::catalog::StreamTableMeta,
) -> Result<(String, Vec<String>), PgTrickleError> {
    use crate::dvm::diff::{DeltaSource, DiffContext, TransitionTableNames};
    use crate::dvm::parser::parse_defining_query;
    use crate::version::Frontier;

    let defining_query = &st.defining_query;
    let query_hash = hash_str(defining_query);

    let cache_key = IvmCacheKey {
        pgt_id,
        source_oid,
        has_new,
        has_old,
    };

    // Cross-session invalidation: flush if the shared generation counter
    // has advanced past our local snapshot.
    let shared_gen = crate::shmem::current_cache_generation();
    LOCAL_IVM_CACHE_GEN.with(|local| {
        if local.get() < shared_gen {
            IVM_DELTA_CACHE.with(|cache| cache.borrow_mut().clear());
            local.set(shared_gen);
        }
    });

    // Check the thread-local cache.
    let cached = IVM_DELTA_CACHE.with(|cache| {
        let map = cache.borrow();
        map.get(&cache_key)
            .filter(|entry| entry.defining_query_hash == query_hash)
            .cloned()
    });

    if let Some(entry) = cached {
        return Ok((entry.delta_sql, entry.user_columns));
    }

    // Cache miss — parse, differentiate, and cache.
    let op_tree = parse_defining_query(defining_query).map_err(|e| {
        PgTrickleError::InternalError(format!("Failed to parse defining query for IVM: {e}"))
    })?;

    // Build transition table names.
    let mut tables = HashMap::new();
    tables.insert(
        source_oid,
        TransitionTableNames {
            new_name: if has_new {
                Some(format!("__pgt_newtable_{source_oid}"))
            } else {
                None
            },
            old_name: if has_old {
                Some(format!("__pgt_oldtable_{source_oid}"))
            } else {
                None
            },
        },
    );

    // Create a DiffContext with transition table source.
    let dummy_frontier = Frontier::default();
    let mut ctx = DiffContext::new(dummy_frontier.clone(), dummy_frontier)
        .with_delta_source(DeltaSource::TransitionTable { tables })
        .with_pgt_name(&st.pgt_schema, &st.pgt_name);

    // Differentiate the operator tree to get delta SQL.
    let (delta_sql, user_columns, _is_dedup) = ctx.differentiate_with_columns(&op_tree)?;

    // Store in cache.
    IVM_DELTA_CACHE.with(|cache| {
        cache.borrow_mut().insert(
            cache_key,
            CachedIvmDelta {
                defining_query_hash: query_hash,
                delta_sql: delta_sql.clone(),
                user_columns: user_columns.clone(),
            },
        );
    });

    Ok((delta_sql, user_columns))
}

/// SQL-callable function: handle TRUNCATE on a base table for an
/// IMMEDIATE-mode stream table.
///
/// Truncates the stream table (equivalent to a full refresh with empty
/// base table for simple views).
#[pg_extern(schema = "pgtrickle")]
fn pgt_ivm_handle_truncate(pgt_id: i64) -> Result<(), PgTrickleError> {
    use crate::catalog::StreamTableMeta;

    let st = StreamTableMeta::get_by_id(pgt_id)?.ok_or_else(|| {
        PgTrickleError::NotFound(format!("Stream table with pgt_id={pgt_id} not found"))
    })?;

    // EC-25/EC-26: Set the internal_refresh flag so DML guard triggers
    // allow the IVM executor to modify the storage table.
    Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // For TRUNCATE, we do a full refresh: truncate the ST and re-populate.
    let st_qualified = format!(
        "\"{}\".\"{}\"",
        st.pgt_schema.replace('"', "\"\""),
        st.pgt_name.replace('"', "\"\""),
    );

    // Truncate the stream table.
    Spi::run(&format!("TRUNCATE {st_qualified}"))
        .map_err(|e| PgTrickleError::SpiError(format!("IVM TRUNCATE failed: {e}")))?;

    // Re-populate from the defining query (which now reads from the
    // truncated base table — i.e., produces no/fewer rows).
    {
        let defining_query = &st.defining_query;
        // Get the user columns from the stream table.
        let col_info = Spi::connect(|client| {
            let query = format!(
                "SELECT attname::text FROM pg_attribute
                 WHERE attrelid = {}::oid AND attnum > 0
                   AND NOT attisdropped AND attname != '__pgt_row_id'
                 ORDER BY attnum",
                st.pgt_relid.to_u32(),
            );
            let table = client.select(&query, None, &[])?;
            let mut cols = Vec::new();
            for row in table {
                if let Some(name) = row.get::<String>(1)? {
                    cols.push(name);
                }
            }
            Ok::<Vec<String>, pgrx::spi::SpiError>(cols)
        })
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to get ST columns: {e}")))?;

        if !col_info.is_empty() {
            let col_list = col_info
                .iter()
                .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(", ");

            // Re-populate: INSERT with hash-based row_id.
            let hash_cols: Vec<String> = col_info
                .iter()
                .map(|c| format!("(\"{}\")::TEXT", c.replace('"', "\"\"")))
                .collect();

            let row_id_expr = if hash_cols.len() == 1 {
                format!("pgtrickle.pg_trickle_hash({})", hash_cols[0])
            } else {
                format!(
                    "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
                    hash_cols.join(", "),
                )
            };

            let repopulate_sql = format!(
                "INSERT INTO {st_qualified} (__pgt_row_id, {col_list})
                 SELECT {row_id_expr}, {col_list} FROM ({defining_query}) __pgt_base"
            );
            Spi::run(&repopulate_sql).map_err(|e| {
                PgTrickleError::SpiError(format!("IVM repopulate after TRUNCATE failed: {e}"))
            })?;
        }
    }

    pgrx::log!("[pg_trickle] IVM TRUNCATE handled for pgt_id={}", pgt_id,);

    Ok(())
}
