//! Change Data Capture via triggers.
//!
//! Tracks DML changes to base tables referenced by stream tables using
//! AFTER INSERT/UPDATE/DELETE triggers that write directly into change
//! buffer tables.
//!
//! # Trigger modes
//!
//! Two trigger granularities are supported, controlled by the
//! `pg_trickle.cdc_trigger_mode` GUC:
//!
//! - **`statement`** (default, v0.4.0+): One `FOR EACH STATEMENT` trigger
//!   invocation per DML statement.  All affected rows are captured in one
//!   bulk `INSERT … SELECT FROM __pgt_new / __pgt_old` using PostgreSQL 10+
//!   transition tables.  Gives **50–80% less write-side overhead** for bulk
//!   DML (e.g. `UPDATE … WHERE region = 'north'` hitting 20K rows).
//!
//! - **`row`**: Legacy `FOR EACH ROW` triggers — one trigger invocation and
//!   one change-buffer INSERT per affected row (behaviour before v0.4.0).
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
//! Statement-level triggers with transition tables are documented in the
//! PostgreSQL manual §43.9 "Trigger Procedures" (available since PG 10).
//!
//! The `pgtrickle_changes` schema and buffer-table pattern is a standard
//! change-capture approach documented in PostgreSQL community literature.
//!
//! # Architecture
//!
//! - One PL/pgSQL trigger function + trigger per tracked base table
//! - Changes are written into `pgtrickle_changes.changes_<oid>` buffer tables
//! - Buffer tables are append-only; consumed changes are deleted after refresh

use pgrx::prelude::*;
use std::collections::HashMap;

use crate::config;
use crate::error::PgTrickleError;

/// Create a CDC trigger on a source table.
///
/// Dispatches to statement-level (`FOR EACH STATEMENT … REFERENCING NEW TABLE
/// AS __pgt_new OLD TABLE AS __pgt_old`) or row-level (`FOR EACH ROW`) based
/// on the `pg_trickle.cdc_trigger_mode` GUC (default: `'statement'`).
///
/// The trigger function name is always `pg_trickle_cdc_fn_{oid}` regardless
/// of mode, so `CREATE OR REPLACE FUNCTION` can switch bodies without touching
/// the trigger DDL.
///
/// `pk_columns` drives the `pk_hash` computation and (for statement mode) the
/// UPDATE JOIN key.  `columns` contains `(name, sql_type)` pairs for the typed
/// change-buffer columns.
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

    // Create trigger function(s) and DML trigger(s) for the current mode.
    //
    // IMPORTANT: all builders use pg_current_wal_insert_lsn() — NOT
    // pg_current_wal_lsn().  The INSERT position advances immediately within
    // the generating transaction, guaranteeing the captured LSN is always
    // ahead of any prior frontier.  pg_current_wal_lsn() (the write position)
    // can lag behind within an uncommitted transaction and produce stale values
    // that cause silent no-op refreshes.
    //
    // PostgreSQL does NOT allow combining INSERT OR UPDATE OR DELETE in a single
    // FOR EACH STATEMENT trigger that also declares REFERENCING transition tables.
    // Statement mode therefore creates 3 per-event triggers.
    let mode = config::pg_trickle_cdc_trigger_mode();
    match mode {
        config::CdcTriggerMode::Statement => {
            let (ins_fn, upd_fn, del_fn) =
                build_stmt_trigger_fn_sql(change_schema, oid_u32, pk_columns, columns);
            Spi::run(&ins_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC INSERT trigger function: {}",
                    e
                ))
            })?;
            Spi::run(&upd_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC UPDATE trigger function: {}",
                    e
                ))
            })?;
            Spi::run(&del_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC DELETE trigger function: {}",
                    e
                ))
            })?;
            Spi::run(&format!(
                "CREATE TRIGGER pg_trickle_cdc_ins_{oid} \
                 AFTER INSERT ON {table} \
                 REFERENCING NEW TABLE AS __pgt_new \
                 FOR EACH STATEMENT EXECUTE FUNCTION {cs}.pg_trickle_cdc_ins_fn_{oid}()",
                oid = oid_u32,
                table = source_table,
                cs = change_schema,
            ))
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC INSERT trigger on {}: {}",
                    source_table, e
                ))
            })?;
            Spi::run(&format!(
                "CREATE TRIGGER pg_trickle_cdc_upd_{oid} \
                 AFTER UPDATE ON {table} \
                 REFERENCING NEW TABLE AS __pgt_new OLD TABLE AS __pgt_old \
                 FOR EACH STATEMENT EXECUTE FUNCTION {cs}.pg_trickle_cdc_upd_fn_{oid}()",
                oid = oid_u32,
                table = source_table,
                cs = change_schema,
            ))
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC UPDATE trigger on {}: {}",
                    source_table, e
                ))
            })?;
            Spi::run(&format!(
                "CREATE TRIGGER pg_trickle_cdc_del_{oid} \
                 AFTER DELETE ON {table} \
                 REFERENCING OLD TABLE AS __pgt_old \
                 FOR EACH STATEMENT EXECUTE FUNCTION {cs}.pg_trickle_cdc_del_fn_{oid}()",
                oid = oid_u32,
                table = source_table,
                cs = change_schema,
            ))
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC DELETE trigger on {}: {}",
                    source_table, e
                ))
            })?;
        }
        config::CdcTriggerMode::Row => {
            let fn_sql = build_row_trigger_fn_sql(change_schema, oid_u32, pk_columns, columns);
            Spi::run(&fn_sql).map_err(|e| {
                PgTrickleError::SpiError(format!("Failed to create CDC trigger function: {}", e))
            })?;
            Spi::run(&format!(
                "CREATE TRIGGER {trigger} \
                 AFTER INSERT OR UPDATE OR DELETE ON {table} \
                 FOR EACH ROW EXECUTE FUNCTION {cs}.pg_trickle_cdc_fn_{oid}()",
                trigger = trigger_name,
                table = source_table,
                cs = change_schema,
                oid = oid_u32,
            ))
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC trigger on {}: {}",
                    source_table, e
                ))
            })?;
        }
    }

    // ── TRUNCATE capture (statement-level trigger) ──────────────────
    // TRUNCATE bypasses row-level triggers entirely. A separate
    // statement-level AFTER TRUNCATE trigger writes a single marker row
    // with action='T' into the change buffer. The refresh engine
    // detects this marker and falls back to a full refresh.
    // WAKE-1: PERFORM pg_notify wakes the scheduler immediately.
    let truncate_fn_sql = format!(
        "CREATE OR REPLACE FUNCTION {change_schema}.pg_trickle_cdc_truncate_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
             INSERT INTO {change_schema}.changes_{oid}
                 (lsn, action)
             VALUES (pg_current_wal_lsn(), 'T');
             PERFORM pg_notify('pgtrickle_wake', '');
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

    // Return the representative trigger name (used for logging only).
    let primary_trig = match mode {
        config::CdcTriggerMode::Statement => format!("pg_trickle_cdc_ins_{}", oid_u32),
        config::CdcTriggerMode::Row => trigger_name,
    };
    Ok(primary_trig)
}

/// Drop a CDC trigger and its function for a source table.
pub fn drop_change_trigger(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();

    // Get the source table name for the trigger drop.
    let source_table =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_oid.into()])
            .unwrap_or(None);

    // Drop all trigger variants using IF EXISTS — handles both row-level
    // (combined) and statement-level (per-event) triggers safely.
    if let Some(ref table) = source_table {
        for trig in &[
            format!("pg_trickle_cdc_{}", oid_u32),     // row-level combined
            format!("pg_trickle_cdc_ins_{}", oid_u32), // statement INSERT
            format!("pg_trickle_cdc_upd_{}", oid_u32), // statement UPDATE
            format!("pg_trickle_cdc_del_{}", oid_u32), // statement DELETE
            format!("pg_trickle_cdc_truncate_{}", oid_u32), // TRUNCATE (both modes)
        ] {
            // nosemgrep: semgrep.rust.spi.run.dynamic-format — DDL cannot be parameterized;
            // trig is built from oid_u32 (a plain integer) and table is a regclass-quoted identifier.
            let _ = Spi::run(&format!("DROP TRIGGER IF EXISTS {trig} ON {table}"));
        }
    }

    // Drop all function variants.
    for fn_suffix in &[
        format!("pg_trickle_cdc_fn_{}", oid_u32), // row-level combined
        format!("pg_trickle_cdc_ins_fn_{}", oid_u32), // statement INSERT
        format!("pg_trickle_cdc_upd_fn_{}", oid_u32), // statement UPDATE
        format!("pg_trickle_cdc_del_fn_{}", oid_u32), // statement DELETE
        format!("pg_trickle_cdc_truncate_fn_{}", oid_u32), // TRUNCATE (both modes)
    ] {
        let _ = Spi::run(&format!(
            "DROP FUNCTION IF EXISTS {cs}.{fn_s}() CASCADE",
            cs = change_schema,
            fn_s = fn_suffix,
        ));
    }

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
/// WB-1: Build the PL/pgSQL expression for the `changed_cols` VARBIT bitmask.
///
/// Bit at position `i` (leftmost = 0) is B'1' when
/// `NEW.col_i IS DISTINCT FROM OLD.col_i`, allowing the scan delta to
/// determine which columns were actually modified by an UPDATE.
/// VARBIT supports tables with arbitrarily many columns — one bit per column.
///
/// Returns `None` for keyless tables (`pk_columns` empty): all columns
/// contribute to the content hash and must always be present.
///
/// For INSERT and DELETE rows `changed_cols` is stored as `NULL`, indicating
/// that all new_*/old_* column values are populated (backward-compatible).
pub fn build_changed_cols_bitmask_expr(
    pk_columns: &[String],
    columns: &[(String, String)],
) -> Option<String> {
    if pk_columns.is_empty() {
        return None; // keyless: must always write all columns
    }
    let parts: Vec<String> = columns
        .iter()
        .map(|(col_name, _)| {
            let qcol = col_name.replace('"', "\"\"");
            format!(
                "(CASE WHEN NEW.\"{qcol}\" IS DISTINCT FROM OLD.\"{qcol}\" \
                 THEN B'1' ELSE B'0' END)::varbit"
            )
        })
        .collect();
    Some(parts.join(" ||\n        "))
}

/// Build typed column definitions for a change buffer table.
///
/// Produces SQL fragments like `,\"new_col\" TYPE,\"old_col\" TYPE` for each
/// column in the input.
fn build_typed_col_defs(columns: &[(String, String)]) -> String {
    columns
        .iter()
        .map(|(name, type_name)| {
            let qname = name.replace('"', "\"\"");
            format!(",\"new_{qname}\" {type_name},\"old_{qname}\" {type_name}")
        })
        .collect::<Vec<_>>()
        .join("")
}

pub fn create_change_buffer_table(
    source_oid: pg_sys::Oid,
    change_schema: &str,
    columns: &[(String, String)],
) -> Result<(), PgTrickleError> {
    // pk_hash is always present (PK hash or all-column content hash).
    // changed_cols is a VARBIT bitmask for UPDATE rows — bit i (leftmost=0)
    // is B'1' when column i changed. NULL for INSERT/DELETE rows.
    let pk_col = ",pk_hash BIGINT,changed_cols VARBIT";

    // Build typed column definitions: "new_col" TYPE, "old_col" TYPE
    let typed_col_defs = build_typed_col_defs(columns);

    // Task 3.3: Determine whether to use partitioned buffer tables.
    let partitioning_mode = crate::config::pg_trickle_buffer_partitioning();
    let use_partitioning = partitioning_mode == "on"
        || (partitioning_mode == "auto" && should_auto_partition(source_oid));

    let partition_clause = if use_partitioning {
        " PARTITION BY RANGE (lsn)"
    } else {
        ""
    };

    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {schema}.changes_{oid} (\
            change_id   BIGSERIAL,\
            lsn         PG_LSN NOT NULL,\
            action      CHAR(1) NOT NULL\
            {pk_col}\
            {typed_col_defs}\
        ){partition_clause}",
        schema = change_schema,
        oid = source_oid.to_u32(),
    );

    Spi::run(&sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create change buffer table: {}", e))
    })?;

    // R2: Explicitly disable RLS on change buffer tables so CDC trigger
    // inserts always succeed, regardless of any schema-level RLS settings.
    let disable_rls_sql = format!(
        "ALTER TABLE {schema}.changes_{oid} DISABLE ROW LEVEL SECURITY",
        schema = change_schema,
        oid = source_oid.to_u32(),
    );
    Spi::run(&disable_rls_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to disable RLS on change buffer: {}", e))
    })?;

    // For partitioned tables, create a default partition to accept any LSN
    // values until the first refresh cycle creates a range partition.
    if use_partitioning {
        let default_part_sql = format!(
            "CREATE TABLE IF NOT EXISTS {schema}.changes_{oid}_default \
             PARTITION OF {schema}.changes_{oid} DEFAULT",
            schema = change_schema,
            oid = source_oid.to_u32(),
        );
        Spi::run(&default_part_sql).map_err(|e| {
            PgTrickleError::SpiError(format!("Failed to create default partition: {}", e))
        })?;
    }

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

// ── ST-to-ST Change Buffer Infrastructure (Phase 8) ────────────────────

/// Create a change buffer table for a stream table source.
///
/// ST change buffers use `changes_pgt_{pgt_id}` naming to avoid collision
/// with base-table buffers (`changes_{oid}`). The schema mirrors base-table
/// buffers but only stores `new_*` columns (no `old_*`) because ST deltas
/// are expressed as I/D pairs, not UPDATEs.
///
/// `columns` are the output columns of the upstream ST (name, type pairs).
pub fn create_st_change_buffer_table(
    pgt_id: i64,
    change_schema: &str,
    columns: &[(String, String)],
) -> Result<(), PgTrickleError> {
    // Build typed column definitions: "new_col" TYPE only (no old_ columns)
    let typed_col_defs: String = columns
        .iter()
        .map(|(name, typ)| format!(",\"new_{}\" {}", name.replace('"', "\"\""), typ,))
        .collect::<Vec<_>>()
        .join("");

    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {schema}.changes_pgt_{id} (\
            change_id   BIGSERIAL,\
            lsn         PG_LSN NOT NULL,\
            action      CHAR(1) NOT NULL,\
            pk_hash     BIGINT\
            {typed_col_defs}\
        )",
        schema = change_schema,
        id = pgt_id,
    );

    Spi::run(&sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create ST change buffer table: {}", e))
    })?;

    // Disable RLS on ST change buffer
    let disable_rls_sql = format!(
        "ALTER TABLE {schema}.changes_pgt_{id} DISABLE ROW LEVEL SECURITY",
        schema = change_schema,
        id = pgt_id,
    );
    Spi::run(&disable_rls_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to disable RLS on ST change buffer: {}", e))
    })?;

    // Covering index matching base-table buffer pattern
    let idx_sql = format!(
        "CREATE INDEX IF NOT EXISTS idx_changes_pgt_{id}_lsn_pk_cid \
         ON {schema}.changes_pgt_{id} (lsn, pk_hash, change_id) INCLUDE (action)",
        schema = change_schema,
        id = pgt_id,
    );
    Spi::run(&idx_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create ST change buffer index: {}", e))
    })?;

    Ok(())
}

/// Drop the change buffer table for a stream table source.
pub fn drop_st_change_buffer_table(pgt_id: i64, change_schema: &str) -> Result<(), PgTrickleError> {
    let sql = format!(
        "DROP TABLE IF EXISTS {schema}.changes_pgt_{id} CASCADE",
        schema = change_schema,
        id = pgt_id,
    );
    Spi::run(&sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to drop ST change buffer table: {}", e))
    })?;
    Ok(())
}

/// Check whether a ST change buffer table exists.
pub fn has_st_change_buffer(pgt_id: i64, change_schema: &str) -> bool {
    Spi::get_one::<bool>(&format!(
        "SELECT EXISTS(\
           SELECT 1 FROM pg_class c \
           JOIN pg_namespace n ON n.oid = c.relnamespace \
           WHERE n.nspname = '{schema}' \
             AND c.relname = 'changes_pgt_{id}'\
         )",
        schema = change_schema,
        id = pgt_id,
    ))
    .unwrap_or(Some(false))
    .unwrap_or(false)
}

/// Resolve the output columns of a stream table for ST change buffer creation.
///
/// Returns `(column_name, column_type)` pairs from the ST's storage table,
/// excluding internal columns (`__pgt_row_id`, `__pgt_count`, etc.).
pub fn resolve_st_output_columns(
    pgt_relid: pg_sys::Oid,
) -> Result<Vec<(String, String)>, PgTrickleError> {
    let columns = Spi::connect(|client| {
        let table = client
            .select(
                "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod) AS type_name \
                 FROM pg_attribute a \
                 WHERE a.attrelid = $1 \
                   AND a.attnum > 0 \
                   AND NOT a.attisdropped \
                   AND a.attname::text NOT LIKE '__pgt_%' \
                 ORDER BY a.attnum",
                None,
                &[pgt_relid.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        let mut cols = Vec::new();
        for row in table {
            let name = row
                .get::<String>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let typ = row
                .get::<String>(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_else(|| "text".to_string());
            cols.push((name, typ));
        }
        Ok(cols)
    })?;
    Ok(columns)
}

/// Ensure a ST change buffer exists for an upstream ST that has downstream consumers.
///
/// Called when a new dependency is created (create_stream_table with ST source)
/// or during upgrade migration. Idempotent — skips if buffer already exists.
pub fn ensure_st_change_buffer(
    upstream_pgt_id: i64,
    upstream_pgt_relid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    if has_st_change_buffer(upstream_pgt_id, change_schema) {
        return Ok(());
    }

    let columns = resolve_st_output_columns(upstream_pgt_relid)?;
    create_st_change_buffer_table(upstream_pgt_id, change_schema, &columns)
}

/// Count how many downstream STs depend on a given upstream ST.
pub fn count_downstream_st_consumers(pgt_id: i64) -> i64 {
    // The upstream ST's pgt_relid is stored as source_relid in pgt_dependencies
    // nosemgrep: semgrep.rust.spi.query.dynamic-format — pgt_id is a plain i64, not user-supplied input.
    Spi::get_one::<i64>(&format!(
        "SELECT COUNT(*)::bigint FROM pgtrickle.pgt_dependencies \
         WHERE source_relid = (\
           SELECT pgt_relid FROM pgtrickle.pgt_stream_tables WHERE pgt_id = {pgt_id}\
         ) AND source_type = 'STREAM_TABLE'"
    ))
    .unwrap_or(Some(0))
    .unwrap_or(0)
}

/// C-4: Compact a change buffer by eliminating net-zero pk_hash groups
/// (INSERT followed by DELETE that cancel out) and collapsing multi-change
/// groups to retain only the first and last entries per pk_hash.
///
/// Returns the number of rows deleted, or 0 if compaction was skipped
/// (buffer below threshold or advisory lock unavailable).
///
/// Uses `pg_try_advisory_xact_lock` to serialise with concurrent refresh
/// operations — if the lock cannot be acquired, compaction is skipped
/// rather than blocking.
///
/// **Safety:** Uses `change_id` (the BIGSERIAL primary key) for deletion,
/// never `ctid` which is unstable under concurrent VACUUM.
pub fn compact_change_buffer(
    change_schema: &str,
    source_oid: u32,
    prev_lsn: &str,
    new_lsn: &str,
) -> Result<i64, PgTrickleError> {
    let threshold = crate::config::pg_trickle_compact_threshold();
    if threshold <= 0 {
        return Ok(0);
    }

    // Quick count check — skip if below threshold.
    let pending_count: i64 = Spi::get_one::<i64>(&format!(
        "SELECT count(*)::bigint FROM (\
           SELECT 1 FROM \"{schema}\".changes_{oid} \
           WHERE lsn > '{prev_lsn}'::pg_lsn AND lsn <= '{new_lsn}'::pg_lsn \
           LIMIT {limit}\
         ) __pgt_cnt",
        schema = change_schema,
        oid = source_oid,
        limit = threshold + 1,
    ))
    .unwrap_or(Some(0))
    .unwrap_or(0);

    if pending_count <= threshold {
        return Ok(0);
    }

    // Advisory lock keyed on source OID to serialise with refresh.
    // Use a fixed namespace offset to avoid collisions with other locks.
    let lock_key = 0x5047_5400_i64 | (source_oid as i64);
    let got_lock = Spi::get_one_with_args::<bool>(
        "SELECT pg_try_advisory_xact_lock($1::bigint)",
        &[lock_key.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !got_lock {
        pgrx::debug1!(
            "[pg_trickle] C-4: skipping compaction for changes_{} (advisory lock busy)",
            source_oid,
        );
        return Ok(0);
    }

    // Compact: remove net-zero groups (INSERT→DELETE) and intermediate rows.
    //
    // For each pk_hash in the pending LSN range:
    // - If first_action='I' and last_action='D' → all rows are net no-op
    // - For remaining groups, keep only first (rn_asc=1) and last (rn_desc=1)
    //   rows, removing intermediates.
    //
    // The delta query pipeline handles 2-row groups (first + last) correctly
    // via FIRST_VALUE/LAST_VALUE window functions.
    let compact_sql = format!(
        "DELETE FROM \"{schema}\".changes_{oid} \
         WHERE change_id IN (\
           SELECT change_id FROM (\
             SELECT change_id, \
                    ROW_NUMBER() OVER (PARTITION BY pk_hash ORDER BY change_id) AS rn_asc, \
                    ROW_NUMBER() OVER (PARTITION BY pk_hash ORDER BY change_id DESC) AS rn_desc, \
                    FIRST_VALUE(action) OVER (\
                      PARTITION BY pk_hash ORDER BY change_id\
                    ) AS first_act, \
                    LAST_VALUE(action) OVER (\
                      PARTITION BY pk_hash ORDER BY change_id \
                      ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING\
                    ) AS last_act \
             FROM \"{schema}\".changes_{oid} \
             WHERE lsn > '{prev_lsn}'::pg_lsn AND lsn <= '{new_lsn}'::pg_lsn\
           ) __pgt_ranked \
           WHERE (first_act = 'I' AND last_act = 'D') \
              OR (rn_asc > 1 AND rn_desc > 1)\
         )",
        schema = change_schema,
        oid = source_oid,
    );

    let deleted = Spi::connect_mut(|client| {
        let result = client
            .update(&compact_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(format!("C-4 compaction failed: {}", e)))?;
        Ok::<i64, PgTrickleError>(result.len() as i64)
    })?;

    if deleted > 0 {
        pgrx::info!(
            "[pg_trickle] C-4: compacted changes_{} — removed {} rows ({} pending → {})",
            source_oid,
            deleted,
            pending_count,
            pending_count - deleted,
        );
    }

    Ok(deleted)
}

/// DAG-5: Compact an ST change buffer (`changes_pgt_{pgt_id}`).
///
/// Applies the same net-effect computation as [`compact_change_buffer`] to
/// ST-to-ST change buffers. During rapid-fire upstream refreshes, multiple
/// rounds of I/D pairs for the same `pk_hash` accumulate between downstream
/// reads. This function:
///
/// 1. Removes net-zero groups (INSERT followed by DELETE for the same `pk_hash`).
/// 2. Removes intermediate rows in multi-change groups, keeping only the first
///    and last rows per `pk_hash`.
///
/// Called from `execute_refresh()` before the downstream reads its ST sources'
/// change buffers. Uses the same compaction threshold as base-table compaction.
pub fn compact_st_change_buffer(
    change_schema: &str,
    pgt_id: i64,
    prev_lsn: &str,
    new_lsn: &str,
) -> Result<i64, PgTrickleError> {
    let threshold = crate::config::pg_trickle_compact_threshold();
    if threshold <= 0 {
        return Ok(0);
    }

    // Quick count check — skip if below threshold.
    let pending_count: i64 = Spi::get_one::<i64>(&format!(
        "SELECT count(*)::bigint FROM (\
           SELECT 1 FROM \"{schema}\".changes_pgt_{id} \
           WHERE lsn > '{prev_lsn}'::pg_lsn AND lsn <= '{new_lsn}'::pg_lsn \
           LIMIT {limit}\
         ) __pgt_cnt",
        schema = change_schema,
        id = pgt_id,
        limit = threshold + 1,
    ))
    .unwrap_or(Some(0))
    .unwrap_or(0);

    if pending_count <= threshold {
        return Ok(0);
    }

    // Advisory lock keyed on pgt_id to serialise with refresh.
    // Use a different namespace offset from base-table compaction to avoid collisions.
    let lock_key = compact_st_advisory_lock_key(pgt_id);
    let got_lock = Spi::get_one_with_args::<bool>(
        "SELECT pg_try_advisory_xact_lock($1::bigint)",
        &[lock_key.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !got_lock {
        pgrx::debug1!(
            "[pg_trickle] DAG-5: skipping ST buffer compaction for changes_pgt_{} (advisory lock busy)",
            pgt_id,
        );
        return Ok(0);
    }

    let compact_sql = build_st_compact_sql(change_schema, pgt_id, prev_lsn, new_lsn);

    let deleted = Spi::connect_mut(|client| {
        let result = client.update(&compact_sql, None, &[]).map_err(|e| {
            PgTrickleError::SpiError(format!("DAG-5 ST buffer compaction failed: {}", e))
        })?;
        Ok::<i64, PgTrickleError>(result.len() as i64)
    })?;

    if deleted > 0 {
        pgrx::info!(
            "[pg_trickle] DAG-5: compacted changes_pgt_{} — removed {} rows ({} pending → {})",
            pgt_id,
            deleted,
            pending_count,
            pending_count - deleted,
        );
    }

    Ok(deleted)
}

/// DAG-5: Build the compaction SQL for an ST change buffer.
///
/// Pure function for unit-testability. Generates a DELETE that removes:
/// - Net-zero groups: first_action='I' and last_action='D' for the same pk_hash.
/// - Intermediate rows: all rows except the first and last per pk_hash group.
fn build_st_compact_sql(change_schema: &str, pgt_id: i64, prev_lsn: &str, new_lsn: &str) -> String {
    format!(
        "DELETE FROM \"{schema}\".changes_pgt_{id} \
         WHERE change_id IN (\
           SELECT change_id FROM (\
             SELECT change_id, \
                    ROW_NUMBER() OVER (PARTITION BY pk_hash ORDER BY change_id) AS rn_asc, \
                    ROW_NUMBER() OVER (PARTITION BY pk_hash ORDER BY change_id DESC) AS rn_desc, \
                    FIRST_VALUE(action) OVER (\
                      PARTITION BY pk_hash ORDER BY change_id\
                    ) AS first_act, \
                    LAST_VALUE(action) OVER (\
                      PARTITION BY pk_hash ORDER BY change_id \
                      ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING\
                    ) AS last_act \
             FROM \"{schema}\".changes_pgt_{id} \
             WHERE lsn > '{prev_lsn}'::pg_lsn AND lsn <= '{new_lsn}'::pg_lsn\
           ) __pgt_ranked \
           WHERE (first_act = 'I' AND last_act = 'D') \
              OR (rn_asc > 1 AND rn_desc > 1)\
         )",
        schema = change_schema,
        id = pgt_id,
    )
}

/// DAG-5: Compute the advisory lock key for ST buffer compaction.
///
/// Uses a different namespace offset (0x5047_5500) from base-table compaction
/// (0x5047_5400) to avoid collisions.
fn compact_st_advisory_lock_key(pgt_id: i64) -> i64 {
    0x5047_5500_i64 | pgt_id
}

/// Task 3.3: Check whether the given source table's effective refresh schedule
/// warrants auto-partitioning (>= 30 s).
fn should_auto_partition(source_oid: pg_sys::Oid) -> bool {
    // Look up schedules for all stream tables that depend on this source.
    // If ALL consumers refresh at >= 30 s intervals, the DDL overhead per
    // cycle is worthwhile.
    let default_secs = crate::config::pg_trickle_default_schedule_seconds() as i64;

    let schedules: Vec<Option<String>> = Spi::connect(|client| {
        let table = client
            .select(
                &format!(
                    "SELECT s.schedule::text AS sched \
                     FROM pgtrickle.pgt_stream_tables s \
                     JOIN pgtrickle.pgt_dependencies d ON d.pgt_id = s.pgt_id \
                     WHERE d.source_relid = {oid} AND d.source_type = 'TABLE'",
                    oid = source_oid.to_u32(),
                ),
                None,
                &[],
            )
            .ok()?;
        let mut v = Vec::new();
        for row in table {
            v.push(row.get_by_name::<String, _>("sched").ok().flatten());
        }
        Some(v)
    })
    .unwrap_or_default();

    if schedules.is_empty() {
        return false;
    }

    // Parse each schedule text into seconds; treat unparseable schedules
    // (cron, 'calculated', NULL) as the global default.
    let min_secs = schedules
        .iter()
        .map(|s| {
            s.as_deref()
                .and_then(|txt| crate::api::parse_duration(txt).ok())
                .unwrap_or(default_secs)
        })
        .min()
        .unwrap_or(0);

    min_secs >= 30
}

/// Task 3.3: Check if a change buffer table is partitioned.
pub fn is_buffer_partitioned(change_schema: &str, source_oid: u32) -> bool {
    Spi::get_one::<bool>(&format!(
        "SELECT c.relkind = 'p' \
         FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{schema}' AND c.relname = 'changes_{oid}'",
        schema = change_schema,
        oid = source_oid,
    ))
    .unwrap_or(Some(false))
    .unwrap_or(false)
}

/// Task 3.3: Detach and drop consumed partitions from a partitioned buffer table.
///
/// After a refresh cycle consumes all changes up to `safe_lsn`, any
/// partitions whose upper bound is <= `safe_lsn` are fully consumed and
/// can be detached + dropped.  This is O(1) — no VACUUM needed.
///
/// The default partition is never detached.
pub fn detach_consumed_partitions(
    change_schema: &str,
    source_oid: u32,
    safe_lsn: &str,
) -> Result<u32, PgTrickleError> {
    // Find child partitions with range upper bound <= safe_lsn.
    // pg_catalog.pg_partition_upper_bound() is PG 14+ but we need to
    // parse pg_get_expr(relpartbound) for portability.
    //
    // Partition naming convention: changes_{oid}_p{seq}
    // Each has a bound like: FOR VALUES FROM ('X/Y') TO ('A/B')
    // We find partitions whose upper bound <= safe_lsn.
    let partitions: Vec<(String, String)> = Spi::connect(|client| {
        let sql = format!(
            "SELECT c.relname::text, \
                    pg_get_expr(c.relpartbound, c.oid)::text AS bound_expr \
             FROM pg_inherits i \
             JOIN pg_class c ON c.oid = i.inhrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE i.inhparent = ('{schema}.changes_{oid}')::regclass \
               AND n.nspname = '{schema}' \
               AND c.relname != 'changes_{oid}_default'",
            schema = change_schema,
            oid = source_oid,
        );
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut parts = Vec::new();
        for row in result {
            let name: String = row.get(1).unwrap_or(None).unwrap_or_default();
            let bound: String = row.get(2).unwrap_or(None).unwrap_or_default();
            parts.push((name, bound));
        }
        Ok::<_, PgTrickleError>(parts)
    })?;

    let mut detached = 0u32;
    for (part_name, bound_expr) in &partitions {
        // Parse upper bound from "FOR VALUES FROM ('X/Y') TO ('A/B')"
        let upper = match parse_partition_upper_bound(bound_expr) {
            Some(u) => u,
            None => continue,
        };

        // Compare: upper_bound <= safe_lsn
        let is_consumed =
            Spi::get_one::<bool>(&format!("SELECT '{upper}'::pg_lsn <= '{safe_lsn}'::pg_lsn",))
                .unwrap_or(Some(false))
                .unwrap_or(false);

        if is_consumed {
            // CONCURRENTLY is not available inside a transaction, so use
            // plain DETACH + DROP.
            let detach_sql = format!(
                "ALTER TABLE \"{schema}\".changes_{oid} DETACH PARTITION \"{schema}\".\"{part}\"",
                schema = change_schema,
                oid = source_oid,
                part = part_name,
            );
            if let Err(e) = Spi::run(&detach_sql) {
                pgrx::warning!(
                    "[pg_trickle] Failed to detach partition {}: {}",
                    part_name,
                    e
                );
                continue;
            }
            let drop_sql = format!(
                "DROP TABLE IF EXISTS \"{schema}\".\"{part}\"",
                schema = change_schema,
                part = part_name,
            );
            if let Err(e) = Spi::run(&drop_sql) {
                pgrx::warning!(
                    "[pg_trickle] Failed to drop detached partition {}: {}",
                    part_name,
                    e
                );
            }
            detached += 1;
        }
    }

    Ok(detached)
}

/// Task 3.3: Create a new range partition for the upcoming refresh cycle.
///
/// The partition covers `(prev_lsn, new_lsn]`.  The partition name
/// includes a monotonic sequence number derived from the count of existing
/// child partitions.
pub fn create_cycle_partition(
    change_schema: &str,
    source_oid: u32,
    prev_lsn: &str,
    new_lsn: &str,
) -> Result<String, PgTrickleError> {
    // Sequence number: count existing range partitions (excluding default).
    let seq: i64 = Spi::get_one::<i64>(&format!(
        "SELECT COUNT(*)::BIGINT \
         FROM pg_inherits i \
         JOIN pg_class c ON c.oid = i.inhrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE i.inhparent = ('{schema}.changes_{oid}')::regclass \
           AND n.nspname = '{schema}' \
           AND c.relname != 'changes_{oid}_default'",
        schema = change_schema,
        oid = source_oid,
    ))
    .unwrap_or(Some(0))
    .unwrap_or(0);

    let part_name = format!("changes_{}_p{}", source_oid, seq);
    let sql = format!(
        "CREATE TABLE \"{schema}\".\"{part}\" \
         PARTITION OF \"{schema}\".changes_{oid} \
         FOR VALUES FROM ('{prev_lsn}'::pg_lsn) TO ('{new_lsn}'::pg_lsn)",
        schema = change_schema,
        oid = source_oid,
        part = part_name,
        prev_lsn = prev_lsn,
        new_lsn = new_lsn,
    );
    Spi::run(&sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "Failed to create cycle partition {}: {}",
            part_name, e,
        ))
    })?;

    Ok(part_name)
}

/// Parse the upper bound LSN from a partition bound expression.
///
/// Input format: `FOR VALUES FROM ('0/1234') TO ('0/5678')`
/// Returns: `Some("0/5678")`
fn parse_partition_upper_bound(bound_expr: &str) -> Option<String> {
    // Look for "TO ('" and extract the LSN between the quotes.
    let to_idx = bound_expr.find("TO ('")?;
    let start = to_idx + 5; // skip "TO ('"
    let rest = &bound_expr[start..];
    let end = rest.find("')")?;
    Some(rest[..end].to_string())
}

/// Task 3.5: Extend an existing change buffer table after ADD COLUMN DDL on the
/// source table, and rebuild the CDC trigger function in-place — without a full
/// ST reinitialize.
///
/// For each source column that does **not** yet have a corresponding `new_<col>`
/// column in the change buffer:
/// - `ALTER TABLE changes_{oid} ADD COLUMN IF NOT EXISTS "new_<col>" <type>`
/// - `ALTER TABLE changes_{oid} ADD COLUMN IF NOT EXISTS "old_<col>" <type>`
///
/// After the buffer is extended the CDC trigger function is recreated via
/// `rebuild_cdc_trigger_function`, and the stored column snapshot is refreshed
/// so that the next DDL event picks up the new baseline correctly.
pub fn alter_change_buffer_add_columns(
    source_oid: pg_sys::Oid,
    change_schema: &str,
    pgt_id: i64,
) -> Result<(), PgTrickleError> {
    // Resolve current source columns.
    let source_cols = resolve_source_column_defs(source_oid)?;

    // Query existing columns in the change buffer.
    let buffer_table = format!("{}.changes_{}", change_schema, source_oid.to_u32());
    let existing_sql = format!(
        "SELECT attname::text FROM pg_attribute \
         WHERE attrelid = '{}'::regclass AND attnum > 0 AND NOT attisdropped",
        buffer_table.replace('\'', "''"),
    );
    let existing_set: std::collections::HashSet<String> = Spi::connect(|client| {
        let rows = client
            .select(&existing_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut s = std::collections::HashSet::new();
        for row in rows {
            let name: String = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            s.insert(name);
        }
        Ok(s)
    })?;

    // For each source column missing from the buffer, add both new_ and old_ variants.
    // Ensure changed_cols VARBIT column exists (WB-1: migrated from BIGINT).
    if !existing_set.contains("changed_cols") {
        let add_sql = format!(
            "ALTER TABLE {schema}.changes_{oid} ADD COLUMN IF NOT EXISTS changed_cols VARBIT",
            schema = change_schema,
            oid = source_oid.to_u32(),
        );
        if let Err(e) = Spi::run(&add_sql) {
            pgrx::debug1!(
                "[pg_trickle] alter_change_buffer_add_columns: failed to add changed_cols: {e}"
            );
        }
    } else {
        // WB-1 migration: convert BIGINT bitmask column to VARBIT.
        // atttypid 20 = int8 (BIGINT). NULL-out existing rows — changed_cols
        // is a performance hint only; losing old bitmasks is safe.
        let is_bigint: bool = Spi::get_one::<bool>(&format!(
            "SELECT a.atttypid = 20 \
             FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = '{schema}' AND c.relname = 'changes_{oid}' \
             AND a.attname = 'changed_cols' AND a.attnum > 0 AND NOT a.attisdropped",
            schema = change_schema,
            oid = source_oid.to_u32(),
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);
        if is_bigint {
            let migrate_sql = format!(
                "ALTER TABLE {schema}.changes_{oid} \
                 ALTER COLUMN changed_cols TYPE VARBIT USING NULL",
                schema = change_schema,
                oid = source_oid.to_u32(),
            );
            if let Err(e) = Spi::run(&migrate_sql) {
                pgrx::debug1!(
                    "[pg_trickle] alter_change_buffer_add_columns: \
                     failed to migrate changed_cols to VARBIT: {e}"
                );
            }
        }
    }

    for (col_name, col_type) in &source_cols {
        let new_col = format!("new_{}", col_name);
        if !existing_set.contains(&new_col) {
            let qcol = col_name.replace('"', "\"\"");
            let qtype = col_type.as_str();
            let add_new = format!(
                "ALTER TABLE {schema}.changes_{oid} \
                 ADD COLUMN IF NOT EXISTS \"new_{qcol}\" {qtype}",
                schema = change_schema,
                oid = source_oid.to_u32(),
            );
            let add_old = format!(
                "ALTER TABLE {schema}.changes_{oid} \
                 ADD COLUMN IF NOT EXISTS \"old_{qcol}\" {qtype}",
                schema = change_schema,
                oid = source_oid.to_u32(),
            );
            Spi::run(&add_new).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to add new_{} column to change buffer: {}",
                    col_name, e
                ))
            })?;
            Spi::run(&add_old).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to add old_{} column to change buffer: {}",
                    col_name, e
                ))
            })?;
        }
    }

    // Rebuild CDC trigger function to capture the new columns.
    rebuild_cdc_trigger_function(source_oid, change_schema)?;

    // Refresh stored column snapshot so the next DDL event uses the updated baseline.
    if let Err(e) = crate::catalog::store_column_snapshot_for_pgt_id(pgt_id, source_oid) {
        pgrx::debug1!(
            "[pg_trickle] alter_change_buffer_add_columns: failed to refresh snapshot for ST {}: {}",
            pgt_id,
            e
        );
    }

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

/// Resolves the subset of columns required by all downstream stream tables for
/// CDC capture of `source_oid` (F15: Selective CDC Column Capture).
///
/// ## Algorithm
/// 1. Query `pgt_dependencies` for the **union** of `columns_used` across every
///    ST that lists `source_oid` as a base-table dependency.
/// 2. If any ST has `columns_used = NULL` (meaning "all columns", e.g. `SELECT *`),
///    fall back to the full column list to avoid silently dropping needed columns.
/// 3. Always include PK columns (required for `pk_hash` computation and row
///    identity in the change buffer — dropping them would break CDC correctness).
/// 4. Filter `resolve_source_column_defs` to the union ∪ PK set and return only
///    those (column_name, type) pairs in original table ordinal order.
///
/// When the catalog contains no dependencies yet (first-time setup before the
/// ST row has been written), `union_referenced_columns_for_source` returns `None`
/// and this function also falls back to full capture.
pub fn resolve_referenced_column_defs(
    source_oid: pg_sys::Oid,
) -> Result<Vec<(String, String)>, PgTrickleError> {
    // Step 1: ask the catalog for the minimal column set across all downstream STs.
    let maybe_referenced =
        crate::catalog::StDependency::union_referenced_columns_for_source(source_oid)?;

    let referenced = match maybe_referenced.as_deref() {
        // NULL in any row, no rows, or empty list → we cannot safely omit anything.
        None | Some([]) => return resolve_source_column_defs(source_oid),
        Some(cols) => cols.to_vec(),
    };

    // Step 2: always include PK columns.
    let pk_cols = resolve_pk_columns(source_oid)?;

    // Build a lookup set: referenced names (lower-case for case-insensitive match).
    let mut keep: std::collections::HashSet<String> =
        referenced.iter().map(|c| c.to_lowercase()).collect();
    for pk in &pk_cols {
        keep.insert(pk.to_lowercase());
    }

    // Step 3: filter the full column list to the keep set, preserving ordinal order.
    let all_cols = resolve_source_column_defs(source_oid)?;
    let filtered: Vec<(String, String)> = all_cols
        .into_iter()
        .filter(|(name, _)| keep.contains(&name.to_lowercase()))
        .collect();

    // Safety: if the filter would drop all columns (should not happen), fall back.
    if filtered.is_empty() {
        return resolve_source_column_defs(source_oid);
    }

    Ok(filtered)
}

/// Returns `true` when F15 selective capture would restrict the tracked columns
/// for `source_oid` (i.e. not every column on the source is needed).
///
/// Used in monitoring and explain output to indicate that column-level pruning
/// is active for a given source table.
pub fn is_selective_capture_active(source_oid: pg_sys::Oid) -> bool {
    // Selective capture is active when the union of referenced columns is
    // a strict subset of the full column list.
    let Ok(Some(referenced)) =
        crate::catalog::StDependency::union_referenced_columns_for_source(source_oid)
    else {
        return false;
    };
    let Ok(pk_cols) = resolve_pk_columns(source_oid) else {
        return false;
    };
    let Ok(all_cols) = resolve_source_column_defs(source_oid) else {
        return false;
    };
    let mut keep: std::collections::HashSet<String> =
        referenced.iter().map(|c| c.to_lowercase()).collect();
    for pk in &pk_cols {
        keep.insert(pk.to_lowercase());
    }
    keep.len() < all_cols.len()
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

// ── Trigger SQL builders ──────────────────────────────────────────────────

/// Generate the PL/pgSQL function body for a **row-level** CDC trigger.
///
/// Uses `NEW` / `OLD` record variables available in `FOR EACH ROW` triggers.
/// Produces one change-buffer INSERT per affected row.
fn build_row_trigger_fn_sql(
    change_schema: &str,
    oid_u32: u32,
    pk_columns: &[String],
    columns: &[(String, String)],
) -> String {
    let (pk_hash_new, pk_hash_old) = build_pk_hash_trigger_exprs(pk_columns, columns);
    let ins_pk = format!(", {pk_hash_new}");
    let upd_pk = format!(", {pk_hash_new}");
    let del_pk = format!(", {pk_hash_old}");

    let bitmask_opt = build_changed_cols_bitmask_expr(pk_columns, columns);
    let upd_cc_decl = if bitmask_opt.is_some() {
        ", changed_cols"
    } else {
        ""
    };
    let upd_cc_val = bitmask_opt
        .as_deref()
        .map(|e| format!(",\n        ({e})"))
        .unwrap_or_default();

    let ncn: String = columns
        .iter()
        .map(|(n, _)| format!(", \"new_{}\"", n.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let ocn: String = columns
        .iter()
        .map(|(n, _)| format!(", \"old_{}\"", n.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let nv: String = columns
        .iter()
        .map(|(n, _)| format!(", NEW.\"{}\"", n.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let ov: String = columns
        .iter()
        .map(|(n, _)| format!(", OLD.\"{}\"", n.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");

    // WAKE-1: PERFORM pg_notify wakes the scheduler via LISTEN/NOTIFY
    // when event-driven mode is active. The NOTIFY is coalesced by
    // PostgreSQL — only one notification per transaction regardless of
    // how many rows are affected. Cost is negligible (~0.5 µs).
    format!(
        "CREATE OR REPLACE FUNCTION {cs}.pg_trickle_cdc_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
             IF TG_OP = 'INSERT' THEN
                 INSERT INTO {cs}.changes_{oid}
                     (lsn, action, pk_hash{ncn})
                 VALUES (pg_current_wal_insert_lsn(), 'I'
                         {ip}{nv});
                 PERFORM pg_notify('pgtrickle_wake', '');
                 RETURN NEW;
             ELSIF TG_OP = 'UPDATE' THEN
                 -- changed_cols IS NULL for INSERT/DELETE (all columns populated).
                 INSERT INTO {cs}.changes_{oid}
                     (lsn, action, pk_hash{uccd}{ncn}{ocn})
                 VALUES (pg_current_wal_insert_lsn(), 'U'
                         {up}{ucv}{nv}{ov});
                 PERFORM pg_notify('pgtrickle_wake', '');
                 RETURN NEW;
             ELSIF TG_OP = 'DELETE' THEN
                 INSERT INTO {cs}.changes_{oid}
                     (lsn, action, pk_hash{ocn})
                 VALUES (pg_current_wal_insert_lsn(), 'D'
                         {dp}{ov});
                 PERFORM pg_notify('pgtrickle_wake', '');
                 RETURN OLD;
             END IF;
             RETURN NULL;
         END;
         $$",
        cs = change_schema,
        oid = oid_u32,
        ip = ins_pk,
        up = upd_pk,
        uccd = upd_cc_decl,
        ucv = upd_cc_val,
        dp = del_pk,
    )
}

/// Generate the PL/pgSQL function body for a **statement-level** CDC trigger.
///
/// Uses `__pgt_new` / `__pgt_old` transition table aliases (declared in the
/// trigger's `REFERENCING` clause).  All affected rows are captured in a
/// single bulk `INSERT … SELECT FROM __pgt_new/old`, giving **50–80% less
/// write-side overhead** for bulk DML versus per-row triggers.
///
/// **UPDATE handling:**
/// - *Keyed tables*: JOIN `__pgt_new n` with `__pgt_old o` on the PK and emit
///   one `'U'` row per updated row, including the `changed_cols` bitmask.
/// - *Keyless tables*: no stable row identity for a JOIN.  UPDATE is split
///   into DELETE from `__pgt_old` + INSERT from `__pgt_new`, preserving the
///   DVM semantics the downstream engine expects.
fn build_stmt_trigger_fn_sql(
    change_schema: &str,
    oid_u32: u32,
    pk_columns: &[String],
    columns: &[(String, String)],
) -> (String, String, String) {
    let pkn = build_pk_hash_stmt_expr("n", pk_columns, columns);
    let pko = build_pk_hash_stmt_expr("o", pk_columns, columns);

    let ncn: String = columns
        .iter()
        .map(|(n, _)| format!(", \"new_{}\"", n.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let ocn: String = columns
        .iter()
        .map(|(n, _)| format!(", \"old_{}\"", n.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let ncr: String = columns
        .iter()
        .map(|(n, _)| format!(", n.\"{}\"", n.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");
    let ocr: String = columns
        .iter()
        .map(|(n, _)| format!(", o.\"{}\"", n.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("");

    // INSERT trigger function — only accesses __pgt_new transition table.
    // WAKE-1: PERFORM pg_notify wakes the scheduler immediately.
    let ins_fn = format!(
        "CREATE OR REPLACE FUNCTION {cs}.pg_trickle_cdc_ins_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
             INSERT INTO {cs}.changes_{oid}
                 (lsn, action, pk_hash{ncn})
             SELECT pg_current_wal_insert_lsn(), 'I', {pkn}{ncr}
             FROM __pgt_new n;
             PERFORM pg_notify('pgtrickle_wake', '');
             RETURN NULL;
         END;
         $$",
        cs = change_schema,
        oid = oid_u32,
    );

    // UPDATE trigger function — accesses both __pgt_new and __pgt_old.
    // WAKE-1: PERFORM pg_notify wakes the scheduler immediately.
    let upd_fn = if pk_columns.is_empty() {
        // Keyless table: no PK join possible — model UPDATE as DELETE+INSERT.
        format!(
            "CREATE OR REPLACE FUNCTION {cs}.pg_trickle_cdc_upd_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
             INSERT INTO {cs}.changes_{oid}
                 (lsn, action, pk_hash{ocn})
             SELECT pg_current_wal_insert_lsn(), 'D', {pko}{ocr}
             FROM __pgt_old o;
             INSERT INTO {cs}.changes_{oid}
                 (lsn, action, pk_hash{ncn})
             SELECT pg_current_wal_insert_lsn(), 'I', {pkn}{ncr}
             FROM __pgt_new n;
             PERFORM pg_notify('pgtrickle_wake', '');
             RETURN NULL;
         END;
         $$",
            cs = change_schema,
            oid = oid_u32,
        )
    } else {
        let join = build_pk_join_condition(pk_columns);
        let bitmask_opt = build_changed_cols_bitmask_stmt_expr(pk_columns, columns);
        let uccd = if bitmask_opt.is_some() {
            ", changed_cols"
        } else {
            ""
        };
        let ucv = bitmask_opt
            .as_deref()
            .map(|e| format!(",\n\t\t        ({e})"))
            .unwrap_or_default();
        // PK-changing UPDATE: when a row's PK changes, the JOIN on PK
        // produces zero rows for that update.  Emit separate D + I records
        // for rows whose old PK has no match in __pgt_new (DELETE) and
        // whose new PK has no match in __pgt_old (INSERT).
        let not_exists_join = build_pk_join_condition(pk_columns);
        format!(
            "CREATE OR REPLACE FUNCTION {cs}.pg_trickle_cdc_upd_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
             INSERT INTO {cs}.changes_{oid}
                 (lsn, action, pk_hash{uccd}{ncn}{ocn})
             SELECT pg_current_wal_insert_lsn(), 'U', {pkn}{ucv}{ncr}{ocr}
             FROM __pgt_new n JOIN __pgt_old o ON {join};
             INSERT INTO {cs}.changes_{oid}
                 (lsn, action, pk_hash{ocn})
             SELECT pg_current_wal_insert_lsn(), 'D', {pko}{ocr}
             FROM __pgt_old o
             WHERE NOT EXISTS (SELECT 1 FROM __pgt_new n WHERE {not_exists_join});
             INSERT INTO {cs}.changes_{oid}
                 (lsn, action, pk_hash{ncn})
             SELECT pg_current_wal_insert_lsn(), 'I', {pkn}{ncr}
             FROM __pgt_new n
             WHERE NOT EXISTS (SELECT 1 FROM __pgt_old o WHERE {not_exists_join});
             PERFORM pg_notify('pgtrickle_wake', '');
             RETURN NULL;
         END;
         $$",
            cs = change_schema,
            oid = oid_u32,
        )
    };

    // DELETE trigger function — only accesses __pgt_old transition table.
    // WAKE-1: PERFORM pg_notify wakes the scheduler immediately.
    let del_fn = format!(
        "CREATE OR REPLACE FUNCTION {cs}.pg_trickle_cdc_del_fn_{oid}()
         RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
             INSERT INTO {cs}.changes_{oid}
                 (lsn, action, pk_hash{ocn})
             SELECT pg_current_wal_insert_lsn(), 'D', {pko}{ocr}
             FROM __pgt_old o;
             PERFORM pg_notify('pgtrickle_wake', '');
             RETURN NULL;
         END;
         $$",
        cs = change_schema,
        oid = oid_u32,
    );

    (ins_fn, upd_fn, del_fn)
}
/// Build a `pk_hash` expression for statement-level triggers using a table alias.
///
/// Echoes `build_pk_hash_trigger_exprs` but generates `{prefix}."col"` instead
/// of `NEW."col"` / `OLD."col"`.
fn build_pk_hash_stmt_expr(
    prefix: &str,
    pk_columns: &[String],
    all_columns: &[(String, String)],
) -> String {
    let hash_cols: Vec<String> = if pk_columns.is_empty() {
        all_columns.iter().map(|(n, _)| n.clone()).collect()
    } else {
        pk_columns.to_vec()
    };

    if hash_cols.is_empty() {
        return "0".to_string();
    }

    if hash_cols.len() == 1 {
        let col = format!("\"{}\"", hash_cols[0].replace('"', "\"\""));
        format!("pgtrickle.pg_trickle_hash({prefix}.{col}::text)")
    } else {
        let items: Vec<String> = hash_cols
            .iter()
            .map(|c| format!("{prefix}.\"{}\"::text", c.replace('"', "\"\"")))
            .collect();
        format!(
            "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
            items.join(", ")
        )
    }
}

/// WB-1: Build the `changed_cols` VARBIT expression for statement-level UPDATE triggers.
///
/// Uses `n."col" IS DISTINCT FROM o."col"` (transition table aliases) instead
/// of `NEW."col" IS DISTINCT FROM OLD."col"`.
/// Bit at position `i` (leftmost = 0) is B'1' when column `i` changed.
fn build_changed_cols_bitmask_stmt_expr(
    pk_columns: &[String],
    columns: &[(String, String)],
) -> Option<String> {
    if pk_columns.is_empty() {
        return None;
    }
    let parts: Vec<String> = columns
        .iter()
        .map(|(col_name, _)| {
            let qcol = col_name.replace('"', "\"\"");
            format!(
                "(CASE WHEN n.\"{qcol}\" IS DISTINCT FROM o.\"{qcol}\" \
                 THEN B'1' ELSE B'0' END)::varbit"
            )
        })
        .collect();
    Some(parts.join(" ||\n\t\t        "))
}

/// Build the JOIN condition for the UPDATE path in statement-level triggers.
///
/// Returns `n."pk1" = o."pk1" AND n."pk2" = o."pk2"` (etc.).
/// Returns `"TRUE"` when called with an empty PK — should not normally happen
/// because keyless tables take the DELETE+INSERT path instead.
fn build_pk_join_condition(pk_columns: &[String]) -> String {
    if pk_columns.is_empty() {
        return "TRUE".to_string();
    }
    pk_columns
        .iter()
        .map(|col| {
            let qcol = col.replace('"', "\"\"");
            format!("n.\"{qcol}\" IS NOT DISTINCT FROM o.\"{qcol}\"")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

// ── Frontier / Position Queries ─────────────────────────────────────────

/// Get the current WAL insert LSN (the latest insert position).
///
/// This represents the "now" position in the WAL and is used as the
/// upper bound of the new frontier.
///
/// Uses `pg_current_wal_insert_lsn()` rather than `pg_current_wal_lsn()`
/// to match the trigger function.  The insert position is always >=
/// the write position and reflects WAL records generated by the current
/// (not-yet-committed) transaction.  See the trigger function comment
/// in `install_trigger_for_source` for the full rationale.
/// Get the schema-qualified table name (schema.table) for a source OID.
///
/// Always includes the schema, even for tables in the search path
/// (e.g. returns `public.orders` not just `orders`), because the
/// `test_decoding` logical decoding plugin always emits fully qualified
/// names in its output.
pub fn get_qualified_table_name(source_oid: pg_sys::Oid) -> Result<String, PgTrickleError> {
    Spi::get_one_with_args::<String>(
        "SELECT format('%I.%I', n.nspname::text, c.relname::text) \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.oid = $1",
        &[source_oid.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .ok_or_else(|| {
        PgTrickleError::NotFound(format!("Table with OID {} not found", source_oid.to_u32()))
    })
}

pub fn get_current_wal_lsn() -> Result<String, PgTrickleError> {
    let lsn = Spi::get_one::<String>("SELECT pg_current_wal_insert_lsn()::text")
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
    // F15: use the minimal column set (union of columns_used across all downstream STs,
    // always including PK columns). Falls back to full capture when any ST uses SELECT *.
    let columns = resolve_referenced_column_defs(source_oid)?;

    // Nothing to rebuild if the table has no user columns.
    if columns.is_empty() {
        return Ok(());
    }

    let oid_u32 = source_oid.to_u32();

    // Rebuild the function body(ies) for the current CDC trigger mode GUC.
    // The trigger DDL itself (FOR EACH ROW vs FOR EACH STATEMENT) is NOT
    // changed here — only the function bodies. To also migrate the trigger type
    // use `rebuild_cdc_trigger()` instead.
    let mode = config::pg_trickle_cdc_trigger_mode();
    match mode {
        config::CdcTriggerMode::Statement => {
            let (ins_fn, upd_fn, del_fn) =
                build_stmt_trigger_fn_sql(change_schema, oid_u32, &pk_columns, &columns);
            Spi::run(&ins_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to rebuild CDC INSERT trigger function: {}",
                    e
                ))
            })?;
            Spi::run(&upd_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to rebuild CDC UPDATE trigger function: {}",
                    e
                ))
            })?;
            Spi::run(&del_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to rebuild CDC DELETE trigger function: {}",
                    e
                ))
            })?;
        }
        config::CdcTriggerMode::Row => {
            let fn_sql = build_row_trigger_fn_sql(change_schema, oid_u32, &pk_columns, &columns);
            Spi::run(&fn_sql).map_err(|e| {
                PgTrickleError::SpiError(format!("Failed to rebuild CDC trigger function: {}", e))
            })?;
        }
    }

    // Sync change buffer table schema: add any columns that are present in
    // the current source but missing from the buffer (e.g. after ADD COLUMN).
    sync_change_buffer_columns(source_oid, change_schema, &columns)?;

    Ok(())
}

/// Rebuild the CDC trigger function body **and** replace the trigger DDL for a
/// source table.
///
/// Unlike `rebuild_cdc_trigger_function` (which only replaces the function body
/// via `CREATE OR REPLACE`), this function also:
/// 1. Drops the existing DML trigger on the source table.
/// 2. Creates a new trigger whose type (`FOR EACH STATEMENT` or `FOR EACH ROW`)
///    matches the current `pg_trickle.cdc_trigger_mode` GUC value.
///
/// Use this to migrate existing stream tables after changing the GUC, or call
/// it from the upgrade script via `pgtrickle.rebuild_cdc_triggers()`.
pub fn rebuild_cdc_trigger(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<String, PgTrickleError> {
    let pk_columns = resolve_pk_columns(source_oid)?;
    // F15: use the minimal column set (union of columns_used across all downstream STs,
    // always including PK columns). Falls back to full capture when any ST uses SELECT *.
    let columns = resolve_referenced_column_defs(source_oid)?;

    if columns.is_empty() {
        return Ok(String::new());
    }

    let oid_u32 = source_oid.to_u32();

    // Resolve source table name; skip gracefully if the table no longer exists.
    let source_table = match Spi::get_one_with_args::<String>(
        "SELECT $1::oid::regclass::text",
        &[source_oid.into()],
    ) {
        Ok(Some(t)) => t,
        _ => return Ok(String::new()),
    };

    let mode = config::pg_trickle_cdc_trigger_mode();

    // 1. Rebuild trigger function(s) for the current mode.
    match mode {
        config::CdcTriggerMode::Statement => {
            let (ins_fn, upd_fn, del_fn) =
                build_stmt_trigger_fn_sql(change_schema, oid_u32, &pk_columns, &columns);
            Spi::run(&ins_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to rebuild CDC INSERT trigger function: {}",
                    e
                ))
            })?;
            Spi::run(&upd_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to rebuild CDC UPDATE trigger function: {}",
                    e
                ))
            })?;
            Spi::run(&del_fn).map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to rebuild CDC DELETE trigger function: {}",
                    e
                ))
            })?;
        }
        config::CdcTriggerMode::Row => {
            let fn_sql = build_row_trigger_fn_sql(change_schema, oid_u32, &pk_columns, &columns);
            Spi::run(&fn_sql).map_err(|e| {
                PgTrickleError::SpiError(format!("Failed to rebuild CDC trigger function: {}", e))
            })?;
        }
    }

    // 2. Drop ALL existing trigger variants (handles both row-level and
    //    statement-level triggers — whichever mode was active before).
    for trig in &[
        format!("pg_trickle_cdc_{}", oid_u32),
        format!("pg_trickle_cdc_ins_{}", oid_u32),
        format!("pg_trickle_cdc_upd_{}", oid_u32),
        format!("pg_trickle_cdc_del_{}", oid_u32),
    ] {
        // nosemgrep: semgrep.rust.spi.run.dynamic-format — DDL cannot be parameterized;
        // trig is built from oid_u32 (a plain integer) and source_table is a regclass-quoted identifier.
        let _ = Spi::run(&format!("DROP TRIGGER IF EXISTS {trig} ON {source_table}"));
    }

    // 3. Create new trigger(s) matching the current mode.
    match mode {
        config::CdcTriggerMode::Statement => {
            Spi::run(&format!(
                "CREATE TRIGGER pg_trickle_cdc_ins_{oid} \
                 AFTER INSERT ON {table} \
                 REFERENCING NEW TABLE AS __pgt_new \
                 FOR EACH STATEMENT EXECUTE FUNCTION {cs}.pg_trickle_cdc_ins_fn_{oid}()",
                oid = oid_u32,
                table = source_table,
                cs = change_schema,
            ))
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC INSERT trigger on {}: {}",
                    source_table, e
                ))
            })?;
            Spi::run(&format!(
                "CREATE TRIGGER pg_trickle_cdc_upd_{oid} \
                 AFTER UPDATE ON {table} \
                 REFERENCING NEW TABLE AS __pgt_new OLD TABLE AS __pgt_old \
                 FOR EACH STATEMENT EXECUTE FUNCTION {cs}.pg_trickle_cdc_upd_fn_{oid}()",
                oid = oid_u32,
                table = source_table,
                cs = change_schema,
            ))
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC UPDATE trigger on {}: {}",
                    source_table, e
                ))
            })?;
            Spi::run(&format!(
                "CREATE TRIGGER pg_trickle_cdc_del_{oid} \
                 AFTER DELETE ON {table} \
                 REFERENCING OLD TABLE AS __pgt_old \
                 FOR EACH STATEMENT EXECUTE FUNCTION {cs}.pg_trickle_cdc_del_fn_{oid}()",
                oid = oid_u32,
                table = source_table,
                cs = change_schema,
            ))
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC DELETE trigger on {}: {}",
                    source_table, e
                ))
            })?;
        }
        config::CdcTriggerMode::Row => {
            Spi::run(&format!(
                "CREATE TRIGGER pg_trickle_cdc_{oid} \
                 AFTER INSERT OR UPDATE OR DELETE ON {table} \
                 FOR EACH ROW EXECUTE FUNCTION {cs}.pg_trickle_cdc_fn_{oid}()",
                oid = oid_u32,
                table = source_table,
                cs = change_schema,
            ))
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "Failed to create CDC trigger on {}: {}",
                    source_table, e
                ))
            })?;
        }
    }

    // 4. Sync the change buffer column schema.
    sync_change_buffer_columns(source_oid, change_schema, &columns)?;

    let primary_trig = match mode {
        config::CdcTriggerMode::Statement => format!("pg_trickle_cdc_ins_{}", oid_u32),
        config::CdcTriggerMode::Row => format!("pg_trickle_cdc_{}", oid_u32),
    };
    Ok(primary_trig)
}

/// Sync the change buffer table schema to match the current source columns.
///
/// When a column is added to the source table (ALTER TABLE … ADD COLUMN),
/// the CDC trigger function is rebuilt to write into `"new_<col>"` and
/// `"old_<col>"` columns — but those columns don't exist in the buffer
/// table yet. This function adds any missing `new_*` / `old_*` columns
/// using `ALTER TABLE … ADD COLUMN IF NOT EXISTS`.
///
/// F39: Columns that were dropped from the source are now cleaned up
/// by dropping the orphaned `new_*` / `old_*` columns from the buffer
/// table, avoiding unbounded column accumulation over time.
fn sync_change_buffer_columns(
    source_oid: pg_sys::Oid,
    change_schema: &str,
    columns: &[(String, String)],
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();
    let buffer_table = format!("{}.changes_{}", change_schema, oid_u32);

    // Fetch existing column names and types from the change buffer table.
    let existing_sql = format!(
        "SELECT attname::text, format_type(atttypid, atttypmod) \
         FROM pg_attribute \
         WHERE attrelid = '{buffer_table}'::regclass \
           AND attnum > 0 AND NOT attisdropped",
    );

    // Map of column name → current type string in the change buffer.
    let existing_cols: std::collections::HashMap<String, String> = Spi::connect(|client| {
        let result = client
            .select(&existing_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut map = std::collections::HashMap::new();
        for row in result {
            let name: Option<String> = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            let type_str: Option<String> = row
                .get(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            if let Some(n) = name {
                map.insert(n, type_str.unwrap_or_default());
            }
        }
        Ok(map)
    })?;

    // Build the set of expected new_* / old_* column names from current source columns.
    let expected_data_cols: std::collections::HashSet<String> = columns
        .iter()
        .flat_map(|(col_name, _)| [format!("new_{}", col_name), format!("old_{}", col_name)])
        .collect();

    // System columns: never dropped, not tracked as data columns.
    // changed_cols is a system column (Task 3.1 bitmask — preserved across schema changes).
    let system_cols: std::collections::HashSet<&str> =
        ["change_id", "lsn", "action", "pk_hash", "changed_cols"]
            .iter()
            .copied()
            .collect();

    // Ensure changed_cols VARBIT column exists (WB-1: migrated from BIGINT).
    if !existing_cols.contains_key("changed_cols") {
        let add_sql =
            format!("ALTER TABLE {buffer_table} ADD COLUMN IF NOT EXISTS changed_cols VARBIT");
        if let Err(e) = Spi::run(&add_sql) {
            pgrx::debug1!("pg_trickle_cdc: failed to add changed_cols to {buffer_table}: {e}");
        }
    } else {
        // WB-1 migration: convert BIGINT bitmask column to VARBIT on existing buffers.
        let is_bigint = existing_cols
            .get("changed_cols")
            .map(|t| t == "bigint")
            .unwrap_or(false);
        if is_bigint {
            let migrate_sql = format!(
                "ALTER TABLE {buffer_table} ALTER COLUMN changed_cols TYPE VARBIT USING NULL"
            );
            if let Err(e) = Spi::run(&migrate_sql) {
                pgrx::debug1!(
                    "pg_trickle_cdc: failed to migrate changed_cols to VARBIT on {buffer_table}: {e}"
                );
            }
        }
    }

    for existing in existing_cols.keys() {
        if system_cols.contains(existing.as_str()) {
            continue;
        }
        if !expected_data_cols.contains(existing) {
            let sql = format!(
                "ALTER TABLE {buffer_table} DROP COLUMN IF EXISTS \"{}\"",
                existing.replace('"', "\"\"")
            );
            if let Err(e) = Spi::run(&sql) {
                pgrx::warning!(
                    "pg_trickle_cdc: failed to drop orphaned column \"{}\" from {}: {}",
                    existing,
                    buffer_table,
                    e
                );
            } else {
                pgrx::debug1!(
                    "pg_trickle_cdc: dropped orphaned column \"{}\" from {}",
                    existing,
                    buffer_table
                );
            }
        }
    }

    // For each source column, add new_<col> and old_<col> if missing.
    for (col_name, col_type) in columns {
        let new_col = format!("new_{}", col_name);
        let old_col = format!("old_{}", col_name);

        match existing_cols.get(&new_col) {
            None => {
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
            Some(existing_type) if existing_type != col_type => {
                // Type widening (e.g. VARCHAR(50) → VARCHAR(200)): update the
                // buffer column type so the CDC trigger can store wider values.
                let sql = format!(
                    "ALTER TABLE {buffer_table} ALTER COLUMN \"{new_col}\" TYPE {col_type}"
                );
                if let Err(e) = Spi::run(&sql) {
                    pgrx::warning!(
                        "pg_trickle_cdc: failed to widen \"{}\" in {}: {}",
                        new_col,
                        buffer_table,
                        e
                    );
                } else {
                    pgrx::debug1!(
                        "pg_trickle_cdc: widened column \"{}\" to {} in {}",
                        new_col,
                        col_type,
                        buffer_table
                    );
                }
            }
            _ => {}
        }

        match existing_cols.get(&old_col) {
            None => {
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
            Some(existing_type) if existing_type != col_type => {
                let sql = format!(
                    "ALTER TABLE {buffer_table} ALTER COLUMN \"{old_col}\" TYPE {col_type}"
                );
                if let Err(e) = Spi::run(&sql) {
                    pgrx::warning!(
                        "pg_trickle_cdc: failed to widen \"{}\" in {}: {}",
                        old_col,
                        buffer_table,
                        e
                    );
                } else {
                    pgrx::debug1!(
                        "pg_trickle_cdc: widened column \"{}\" to {} in {}",
                        old_col,
                        col_type,
                        buffer_table
                    );
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Check if a CDC trigger exists for a source table.
pub fn trigger_exists(source_oid: pg_sys::Oid) -> Result<bool, PgTrickleError> {
    let oid_u32 = source_oid.to_u32();
    // Check for any DML CDC trigger: the legacy combined row-level trigger
    // OR any of the three per-event statement-level triggers.
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(
            SELECT 1 FROM pg_trigger
            WHERE tgname IN ($1, $3, $4, $5)
              AND tgrelid = $2
        )",
        &[
            format!("pg_trickle_cdc_{}", oid_u32).as_str().into(),
            source_oid.into(),
            format!("pg_trickle_cdc_ins_{}", oid_u32).as_str().into(),
            format!("pg_trickle_cdc_upd_{}", oid_u32).as_str().into(),
            format!("pg_trickle_cdc_del_{}", oid_u32).as_str().into(),
        ],
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
    can_use_logical_replication_for_mode(&config::pg_trickle_cdc_mode())
}

/// Check if the server supports logical replication for a requested CDC mode.
pub fn can_use_logical_replication_for_mode(cdc_mode: &str) -> Result<bool, PgTrickleError> {
    if cdc_mode.eq_ignore_ascii_case("trigger") {
        return Ok(false);
    }

    let wal_level = Spi::get_one::<String>("SELECT current_setting('wal_level')")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or_default();

    if wal_level != "logical" {
        if cdc_mode.eq_ignore_ascii_case("wal") {
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
             AND tgname NOT LIKE 'pg_trickle_%' \
         )",
        st_relid.to_u32(),
    ))
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))
    .map(|v| v.unwrap_or(false))
}

// ═══════════════════════════════════════════════════════════════════════════
// EC-05 — Foreign table polling CDC
// ═══════════════════════════════════════════════════════════════════════════

/// Set up polling-based CDC for a foreign table source.
///
/// Creates the change buffer table (same schema as trigger-based CDC) plus
/// a snapshot table that stores the previous contents of the foreign table.
/// No trigger is installed — instead, [`poll_foreign_table_changes`] is
/// called before each differential refresh to compute deltas.
pub fn setup_foreign_table_polling(
    source_oid: pg_sys::Oid,
    pgt_id: i64,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();

    // Check if already tracked
    let already_tracked = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_change_tracking WHERE source_relid = $1)",
        &[source_oid.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !already_tracked {
        let col_defs = resolve_source_column_defs(source_oid)?;

        // Create the change buffer table (same as trigger-based CDC).
        create_change_buffer_table(source_oid, change_schema, &col_defs)?;

        // Create a snapshot table: stores the previous contents of the
        // foreign table so we can compute EXCEPT-based deltas on each poll.
        let snapshot_table = format!("\"{change_schema}\".snapshot_{oid_u32}");
        let source_table = Spi::get_one_with_args::<String>(
            "SELECT $1::oid::regclass::text",
            &[source_oid.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .ok_or_else(|| {
            PgTrickleError::NotFound(format!("Foreign table with OID {oid_u32} not found"))
        })?;

        // Create snapshot as an empty copy of the source table structure.
        Spi::run(&format!(
            "CREATE TABLE IF NOT EXISTS {snapshot_table} (LIKE {source_table} INCLUDING ALL)"
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        // Seed the snapshot with the current foreign table contents.
        Spi::run(&format!(
            "INSERT INTO {snapshot_table} SELECT * FROM {source_table}"
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        // Record tracking with synthetic slot_name indicating polling CDC.
        Spi::run_with_args(
            "INSERT INTO pgtrickle.pgt_change_tracking \
             (source_relid, slot_name, tracked_by_pgt_ids) \
             VALUES ($1, $2, ARRAY[$3])",
            &[
                source_oid.into(),
                format!("foreign_poll_{oid_u32}").as_str().into(),
                pgt_id.into(),
            ],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    } else {
        // Already tracked — add this pgt_id to the tracking array.
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_change_tracking \
             SET tracked_by_pgt_ids = array_append(tracked_by_pgt_ids, $1) \
             WHERE source_relid = $2 AND NOT ($1 = ANY(tracked_by_pgt_ids))",
            &[pgt_id.into(), source_oid.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    Ok(())
}

/// Poll a foreign table source for changes and populate the change buffer.
///
/// Computes the symmetric difference between the current foreign table
/// contents and the snapshot table using EXCEPT ALL, inserts the deltas
/// into the change buffer, and refreshes the snapshot.
///
/// Uses `pg_current_wal_insert_lsn()` as the LSN for delta rows (even
/// though the foreign table itself has no WAL entries, the local write
/// to the change buffer does).
pub fn poll_foreign_table_changes(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();
    let change_table = format!("\"{change_schema}\".changes_{oid_u32}");
    let snapshot_table = format!("\"{change_schema}\".snapshot_{oid_u32}");

    let source_table =
        Spi::get_one_with_args::<String>("SELECT $1::oid::regclass::text", &[source_oid.into()])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            .ok_or_else(|| {
                PgTrickleError::NotFound(format!("Foreign table with OID {oid_u32} not found"))
            })?;

    let col_defs = resolve_source_column_defs(source_oid)?;
    if col_defs.is_empty() {
        return Ok(());
    }
    let pk_columns = resolve_pk_columns(source_oid)?;

    // Build column lists for INSERT into change buffer.
    let col_names: Vec<String> = col_defs
        .iter()
        .map(|(name, _)| format!("\"{}\"", name.replace('"', "\"\"")))
        .collect();
    let new_cols: Vec<String> = col_defs
        .iter()
        .map(|(name, _)| format!("\"new_{}\"", name.replace('"', "\"\"")))
        .collect();
    let old_cols: Vec<String> = col_defs
        .iter()
        .map(|(name, _)| format!("\"old_{}\"", name.replace('"', "\"\"")))
        .collect();

    // Build pk_hash expression for delta rows.
    let hash_cols: Vec<String> = if pk_columns.is_empty() {
        col_defs.iter().map(|(n, _)| n.clone()).collect()
    } else {
        pk_columns.clone()
    };
    let pk_hash_expr = if hash_cols.len() == 1 {
        let c = format!("\"{}\"", hash_cols[0].replace('"', "\"\""));
        format!("pgtrickle.pg_trickle_hash({c}::text)")
    } else {
        let items: Vec<String> = hash_cols
            .iter()
            .map(|c| format!("\"{}\"::text", c.replace('"', "\"\"")))
            .collect();
        format!(
            "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
            items.join(", ")
        )
    };

    let col_list = col_names.join(", ");
    let new_col_list = new_cols.join(", ");
    let old_col_list = old_cols.join(", ");

    // NULL placeholders for the opposite side of each delta row.
    let null_list = col_defs
        .iter()
        .map(|_| "NULL")
        .collect::<Vec<_>>()
        .join(", ");

    // ── Deleted rows: in snapshot but not in current foreign table ──
    // These appear as 'D' (delete) rows in the change buffer.
    let deleted_sql = format!(
        "INSERT INTO {change_table} (lsn, action, pk_hash, {old_col_list}, {new_col_list}) \
         SELECT pg_current_wal_insert_lsn(), 'D', {pk_hash_expr}, {col_list}, {null_list} \
         FROM (\
           SELECT {col_list} FROM {snapshot_table} \
           EXCEPT ALL \
           SELECT {col_list} FROM {source_table}\
         ) __pgt_del"
    );
    Spi::run(&deleted_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // ── Inserted rows: in current foreign table but not in snapshot ──
    // These appear as 'I' (insert) rows in the change buffer.
    let inserted_sql = format!(
        "INSERT INTO {change_table} (lsn, action, pk_hash, {new_col_list}, {old_col_list}) \
         SELECT pg_current_wal_insert_lsn(), 'I', {pk_hash_expr}, {col_list}, {null_list} \
         FROM (\
           SELECT {col_list} FROM {source_table} \
           EXCEPT ALL \
           SELECT {col_list} FROM {snapshot_table}\
         ) __pgt_ins"
    );
    Spi::run(&inserted_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // ── Refresh snapshot — replace contents with current foreign table ──
    Spi::run(&format!("TRUNCATE {snapshot_table}"))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    Spi::run(&format!(
        "INSERT INTO {snapshot_table} SELECT * FROM {source_table}"
    ))
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    Ok(())
}

/// Set up polling-based CDC for a materialized view source (P2-4).
///
/// Identical to [`setup_foreign_table_polling`] — creates a snapshot table
/// and change buffer so that EXCEPT ALL can compute deltas when the matview
/// is refreshed externally.
pub fn setup_matview_polling(
    source_oid: pg_sys::Oid,
    pgt_id: i64,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let oid_u32 = source_oid.to_u32();

    let already_tracked = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_change_tracking WHERE source_relid = $1)",
        &[source_oid.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !already_tracked {
        let col_defs = resolve_source_column_defs(source_oid)?;

        create_change_buffer_table(source_oid, change_schema, &col_defs)?;

        let snapshot_table = format!("\"{change_schema}\".snapshot_{oid_u32}");
        let source_table = Spi::get_one_with_args::<String>(
            "SELECT $1::oid::regclass::text",
            &[source_oid.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .ok_or_else(|| {
            PgTrickleError::NotFound(format!("Materialized view with OID {oid_u32} not found"))
        })?;

        // nosemgrep: rust.spi.run.dynamic-format — DDL (LIKE) cannot use
        // parameterized queries; snapshot_table is built from a PG OID and
        // source_table is oid::regclass::text, both extension-controlled.
        Spi::run(&format!(
            "CREATE TABLE IF NOT EXISTS {snapshot_table} (LIKE {source_table} INCLUDING ALL)"
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        // nosemgrep: rust.spi.run.dynamic-format — same rationale as above.
        Spi::run(&format!(
            "INSERT INTO {snapshot_table} SELECT * FROM {source_table}"
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        Spi::run_with_args(
            "INSERT INTO pgtrickle.pgt_change_tracking \
             (source_relid, slot_name, tracked_by_pgt_ids) \
             VALUES ($1, $2, ARRAY[$3])",
            &[
                source_oid.into(),
                format!("matview_poll_{oid_u32}").as_str().into(),
                pgt_id.into(),
            ],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    } else {
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_change_tracking \
             SET tracked_by_pgt_ids = array_append(tracked_by_pgt_ids, $1) \
             WHERE source_relid = $2 AND NOT ($1 = ANY(tracked_by_pgt_ids))",
            &[pgt_id.into(), source_oid.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    Ok(())
}

/// Poll a materialized view source for changes (P2-4).
///
/// Identical to [`poll_foreign_table_changes`] — uses EXCEPT ALL between
/// the current matview contents and the snapshot to detect inserts/deletes.
pub fn poll_matview_changes(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    // Delegate to the same implementation — the logic is identical regardless
    // of whether the source is a foreign table or a materialized view.
    poll_foreign_table_changes(source_oid, change_schema)
}

// ── PERF-2: Auto Buffer Partitioning ────────────────────────────────────

/// Pure decision logic for auto-promotion — testable without a PostgreSQL
/// backend.  All GUC values are passed as parameters.
///
/// Returns `true` when:
/// 1. `mode` is `"auto"`
/// 2. The buffer is not already partitioned
/// 3. `pending_count > threshold` and `threshold > 0`
fn should_promote_inner(
    pending_count: i64,
    already_partitioned: bool,
    mode: &str,
    threshold: i64,
) -> bool {
    if already_partitioned {
        return false;
    }
    if mode != "auto" {
        return false;
    }
    if threshold <= 0 {
        return false;
    }
    pending_count > threshold
}

/// Should an unpartitioned buffer be promoted to RANGE(lsn) partitioned mode?
///
/// Reads GUC values and delegates to the pure [`should_promote_inner`].
pub fn should_promote_to_partitioned(pending_count: i64, already_partitioned: bool) -> bool {
    let mode = crate::config::pg_trickle_buffer_partitioning();
    let threshold = crate::config::pg_trickle_compact_threshold();
    should_promote_inner(pending_count, already_partitioned, &mode, threshold)
}

/// Convert an existing unpartitioned change buffer to RANGE(lsn) partitioned
/// mode at runtime.
///
/// Strategy:
/// 1. Rename the existing heap table to a temporary name
/// 2. Create a new partitioned table with the same schema
/// 3. Create a default partition to accept incoming rows immediately
/// 4. Migrate existing rows via INSERT … SELECT
/// 5. Recreate the covering index
/// 6. Drop the old table
///
/// This runs inside the current transaction and acquires an ACCESS EXCLUSIVE
/// lock on the buffer table during the rename — which is safe because it
/// runs between refresh cycles when no concurrent readers exist.
pub fn convert_buffer_to_partitioned(
    change_schema: &str,
    source_oid: u32,
) -> Result<i64, PgTrickleError> {
    let table_name = format!("changes_{source_oid}");
    let migrated_name = format!("changes_{source_oid}_pre_part");

    // Step 1: Rename the existing unpartitioned table.
    let rename_sql = format!(
        "ALTER TABLE \"{schema}\".\"{table}\" RENAME TO \"{migrated}\"",
        schema = change_schema,
        table = table_name,
        migrated = migrated_name,
    );
    Spi::run(&rename_sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "PERF-2: Failed to rename buffer for partitioning: {e}"
        ))
    })?;

    // Step 2: Read the column definitions from the renamed table so we can
    // recreate the partitioned table with identical schema.
    let col_defs: Vec<(String, String, i32)> = Spi::connect(|client| {
        let sql = format!(
            "SELECT column_name::text, data_type::text, ordinal_position::int \
             FROM information_schema.columns \
             WHERE table_schema = '{schema}' AND table_name = '{migrated}' \
             ORDER BY ordinal_position",
            schema = change_schema,
            migrated = migrated_name,
        );
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut cols = Vec::new();
        for row in result {
            let name: String = row.get(1).unwrap_or(None).unwrap_or_default();
            let dtype: String = row.get(2).unwrap_or(None).unwrap_or_default();
            let pos: i32 = row.get(3).unwrap_or(None).unwrap_or(0);
            cols.push((name, dtype, pos));
        }
        Ok::<_, PgTrickleError>(cols)
    })?;

    // Build CREATE TABLE statement replicating all columns.
    let col_sql: String = col_defs
        .iter()
        .map(|(name, dtype, _)| {
            let qname = name.replace('"', "\"\"");
            // Map information_schema types back to SQL types.
            let sql_type = match dtype.as_str() {
                "bigint" => "BIGINT",
                "pg_lsn" => "PG_LSN",
                "character" => "CHAR(1)",
                "bit varying" => "VARBIT",
                _ => dtype.as_str(),
            };
            // change_id uses BIGSERIAL for auto-increment.
            if name == "change_id" {
                return format!("\"{qname}\" BIGSERIAL");
            }
            // lsn is NOT NULL.
            if name == "lsn" {
                return format!("\"{qname}\" {sql_type} NOT NULL");
            }
            // action is NOT NULL.
            if name == "action" {
                return format!("\"{qname}\" {sql_type} NOT NULL");
            }
            format!("\"{qname}\" {sql_type}")
        })
        .collect::<Vec<_>>()
        .join(", ");

    let create_sql = format!(
        "CREATE TABLE \"{schema}\".\"{table}\" ({col_sql}) PARTITION BY RANGE (lsn)",
        schema = change_schema,
        table = table_name,
    );
    Spi::run(&create_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("PERF-2: Failed to create partitioned buffer: {e}"))
    })?;

    // Disable RLS on the new partitioned table.
    Spi::run(&format!(
        "ALTER TABLE \"{schema}\".\"{table}\" DISABLE ROW LEVEL SECURITY",
        schema = change_schema,
        table = table_name,
    ))
    .map_err(|e| PgTrickleError::SpiError(format!("PERF-2: Failed to disable RLS: {e}")))?;

    // Step 3: Create default partition.
    let default_sql = format!(
        "CREATE TABLE \"{schema}\".\"changes_{oid}_default\" \
         PARTITION OF \"{schema}\".\"{table}\" DEFAULT",
        schema = change_schema,
        oid = source_oid,
        table = table_name,
    );
    Spi::run(&default_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("PERF-2: Failed to create default partition: {e}"))
    })?;

    // Step 4: Migrate existing rows.
    let migrate_sql = format!(
        "INSERT INTO \"{schema}\".\"{table}\" SELECT * FROM \"{schema}\".\"{migrated}\"",
        schema = change_schema,
        table = table_name,
        migrated = migrated_name,
    );
    Spi::run(&migrate_sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "PERF-2: Failed to migrate rows to partitioned buffer: {e}"
        ))
    })?;

    // Count migrated rows for logging.
    let migrated_count = Spi::get_one::<i64>(&format!(
        "SELECT count(*)::bigint FROM \"{schema}\".\"{table}\"",
        schema = change_schema,
        table = table_name,
    ))
    .unwrap_or(Some(0))
    .unwrap_or(0);

    // Step 5: Recreate the covering index.
    let idx_sql = format!(
        "CREATE INDEX IF NOT EXISTS idx_changes_{oid}_lsn_pk_cid \
         ON \"{schema}\".changes_{oid} (lsn, pk_hash, change_id) INCLUDE (action)",
        schema = change_schema,
        oid = source_oid,
    );
    Spi::run(&idx_sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "PERF-2: Failed to create index on partitioned buffer: {e}"
        ))
    })?;

    // Step 6: Drop the old table.
    let drop_sql = format!(
        "DROP TABLE IF EXISTS \"{schema}\".\"{migrated}\"",
        schema = change_schema,
        migrated = migrated_name,
    );
    Spi::run(&drop_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("PERF-2: Failed to drop old buffer table: {e}"))
    })?;

    pgrx::log!(
        "pg_trickle PERF-2: promoted changes_{} to RANGE(lsn) partitioned mode ({} rows migrated)",
        source_oid,
        migrated_count,
    );

    Ok(migrated_count)
}

/// PERF-2: Check if a buffer should be auto-promoted and do the conversion.
///
/// Called after compaction in `execute_differential_refresh()`. If the
/// buffer accumulated more rows than `compact_threshold` in a single
/// refresh cycle AND `buffer_partitioning = 'auto'`, converts the buffer
/// to RANGE(lsn) partitioned mode for O(1) cleanup.
///
/// Returns `Ok(true)` if the buffer was promoted, `Ok(false)` otherwise.
pub fn maybe_auto_promote_buffer(
    change_schema: &str,
    source_oid: u32,
    pending_count: i64,
) -> Result<bool, PgTrickleError> {
    let already_partitioned = is_buffer_partitioned(change_schema, source_oid);
    if !should_promote_to_partitioned(pending_count, already_partitioned) {
        return Ok(false);
    }

    convert_buffer_to_partitioned(change_schema, source_oid)?;
    Ok(true)
}

/// Count the number of pending change rows in a buffer between two LSN boundaries.
///
/// Used by the auto-promote heuristic to measure buffer fill rate within
/// a single refresh cycle.
pub fn count_pending_changes(
    change_schema: &str,
    source_oid: u32,
    prev_lsn: &str,
    new_lsn: &str,
) -> i64 {
    Spi::get_one::<i64>(&format!(
        "SELECT count(*)::bigint FROM \"{schema}\".changes_{oid} \
         WHERE lsn > '{prev_lsn}'::pg_lsn AND lsn <= '{new_lsn}'::pg_lsn",
        schema = change_schema,
        oid = source_oid,
    ))
    .unwrap_or(Some(0))
    .unwrap_or(0)
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

    // ── build_changed_cols_bitmask_expr tests ────────────────────────

    #[test]
    fn test_bitmask_none_when_empty_pk() {
        let cols = vec![("a".to_string(), "int".to_string())];
        assert!(build_changed_cols_bitmask_expr(&[], &cols).is_none());
    }

    #[test]
    fn test_bitmask_wide_table_64plus_cols_is_ok() {
        // WB-1: 64+ column tables must produce a bitmask (VARBIT, no cap).
        let pk = vec!["id".to_string()];
        let cols: Vec<_> = (0..64)
            .map(|i| (format!("c{i}"), "int".to_string()))
            .collect();
        assert!(
            build_changed_cols_bitmask_expr(&pk, &cols).is_some(),
            "WB-1: should produce VARBIT bitmask for 64 columns"
        );
    }

    #[test]
    fn test_bitmask_63_cols_is_ok() {
        let pk = vec!["id".to_string()];
        let cols: Vec<_> = (0..63)
            .map(|i| (format!("c{i}"), "int".to_string()))
            .collect();
        assert!(build_changed_cols_bitmask_expr(&pk, &cols).is_some());
    }

    #[test]
    fn test_bitmask_single_non_pk_col() {
        let pk = vec!["id".to_string()];
        let cols = vec![
            ("id".to_string(), "integer".to_string()),
            ("val".to_string(), "text".to_string()),
        ];
        let expr = build_changed_cols_bitmask_expr(&pk, &cols).unwrap();
        // Each column gets a CASE expression with its bit value
        assert!(
            expr.contains("IS DISTINCT FROM"),
            "should use IS DISTINCT FROM: {expr}"
        );
        assert!(expr.contains("NEW."), "should reference NEW: {expr}");
        assert!(expr.contains("OLD."), "should reference OLD: {expr}");
    }

    #[test]
    fn test_bitmask_uses_varbit_concatenation() {
        // WB-1: bitmask expression must use VARBIT (not BIGINT) and concat (||).
        let pk = vec!["id".to_string()];
        let cols = vec![
            ("a".to_string(), "int".to_string()),
            ("b".to_string(), "int".to_string()),
            ("c".to_string(), "int".to_string()),
        ];
        let expr = build_changed_cols_bitmask_expr(&pk, &cols).unwrap();
        assert!(expr.contains("::varbit"), "should use VARBIT type: {expr}");
        assert!(expr.contains("B'1'"), "should use B'1' literal: {expr}");
        assert!(expr.contains("B'0'"), "should use B'0' literal: {expr}");
        assert!(expr.contains("||"), "should use || concatenation: {expr}");
        assert!(
            !expr.contains("::BIGINT"),
            "must not use BIGINT (old format): {expr}"
        );
    }

    #[test]
    fn test_bitmask_quotes_column_names() {
        let pk = vec!["id".to_string()];
        let cols = vec![
            ("id".to_string(), "int".to_string()),
            (r#"has"quote"#.to_string(), "text".to_string()),
        ];
        let expr = build_changed_cols_bitmask_expr(&pk, &cols).unwrap();
        assert!(
            expr.contains(r#"has""quote"#),
            "should double-quote: {expr}"
        );
    }

    // ── parse_partition_upper_bound tests ────────────────────────────

    #[test]
    fn test_parse_valid_range() {
        assert_eq!(
            parse_partition_upper_bound("FOR VALUES FROM ('0/0') TO ('1/A3F')"),
            Some("1/A3F".to_string()),
        );
    }

    #[test]
    fn test_parse_no_match() {
        assert_eq!(parse_partition_upper_bound("LIST (1, 2, 3)"), None);
    }

    #[test]
    fn test_parse_empty_string() {
        assert_eq!(parse_partition_upper_bound(""), None);
    }

    #[test]
    fn test_parse_only_from_no_to() {
        assert_eq!(parse_partition_upper_bound("FOR VALUES FROM ('0/0')"), None,);
    }

    #[test]
    fn test_parse_realistic_lsn_bounds() {
        assert_eq!(
            parse_partition_upper_bound("FOR VALUES FROM ('0/15B3D20') TO ('0/2A7C640')"),
            Some("0/2A7C640".to_string()),
        );
    }

    // ── build_typed_col_defs tests ──────────────────────────────────

    #[test]
    fn test_typed_col_defs_basic() {
        let cols = vec![
            ("id".to_string(), "integer".to_string()),
            ("name".to_string(), "text".to_string()),
        ];
        let result = build_typed_col_defs(&cols);
        assert!(result.contains(r#","new_id" integer"#));
        assert!(result.contains(r#","old_id" integer"#));
        assert!(result.contains(r#","new_name" text"#));
        assert!(result.contains(r#","old_name" text"#));
    }

    #[test]
    fn test_typed_col_defs_empty() {
        assert_eq!(build_typed_col_defs(&[]), "");
    }

    #[test]
    fn test_typed_col_defs_quotes_special_chars() {
        let cols = vec![(r#"my"col"#.to_string(), "varchar(100)".to_string())];
        let result = build_typed_col_defs(&cols);
        assert!(
            result.contains(r#""new_my""col""#),
            "should double-quote: {result}"
        );
    }

    #[test]
    fn test_typed_col_defs_preserves_type() {
        let cols = vec![("ts".to_string(), "timestamp with time zone".to_string())];
        let result = build_typed_col_defs(&cols);
        assert!(result.contains("timestamp with time zone"));
    }

    // ── F15: Selective CDC Column Capture ─────────────────────────────────────

    /// Pure helper that mirrors the filter logic inside `resolve_referenced_column_defs`,
    /// extracted so it can be unit-tested without a PostgreSQL backend.
    ///
    /// Given:
    /// - `all_cols`   — the full ordered column list of the source table
    /// - `referenced` — union of `columns_used` across all downstream STs (`None` = keep all)
    /// - `pk_cols`    — primary key column names (always retained)
    ///
    /// Returns the filtered column list, preserving original ordinal order.
    fn filter_cdc_columns<'a>(
        all_cols: &'a [(String, String)],
        referenced: Option<&[String]>,
        pk_cols: &[String],
    ) -> Vec<&'a (String, String)> {
        let referenced = match referenced {
            None | Some([]) => return all_cols.iter().collect(),
            Some(r) => r,
        };

        let mut keep: std::collections::HashSet<String> =
            referenced.iter().map(|c| c.to_lowercase()).collect();
        for pk in pk_cols {
            keep.insert(pk.to_lowercase());
        }

        let filtered: Vec<&(String, String)> = all_cols
            .iter()
            .filter(|(name, _)| keep.contains(&name.to_lowercase()))
            .collect();

        if filtered.is_empty() {
            all_cols.iter().collect()
        } else {
            filtered
        }
    }

    fn s(name: &str, ty: &str) -> (String, String) {
        (name.to_string(), ty.to_string())
    }

    #[test]
    fn test_f15_filter_none_referenced_keeps_all() {
        // When referenced is None (SELECT * or no deps), all columns are kept.
        let all = vec![s("id", "int4"), s("name", "text"), s("secret", "text")];
        let result = filter_cdc_columns(&all, None, &[]);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_f15_filter_empty_referenced_keeps_all() {
        // Empty referenced slice also falls back to full capture.
        let all = vec![s("id", "int4"), s("name", "text")];
        let result = filter_cdc_columns(&all, Some(&[]), &[]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_f15_filter_restricts_to_referenced_plus_pk() {
        let all = vec![
            s("id", "int4"),
            s("name", "text"),
            s("email", "text"),
            s("secret", "text"),
        ];
        let referenced = vec!["name".to_string()];
        let pk = vec!["id".to_string()];
        let result = filter_cdc_columns(&all, Some(&referenced), &pk);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
        // id (pk) and name (referenced) must be present; email and secret must not.
        assert!(names.contains(&"id"), "PK column must be retained");
        assert!(
            names.contains(&"name"),
            "referenced column must be retained"
        );
        assert!(
            !names.contains(&"email"),
            "unreferenced column must be dropped"
        );
        assert!(
            !names.contains(&"secret"),
            "unreferenced column must be dropped"
        );
    }

    #[test]
    fn test_f15_filter_preserves_ordinal_order() {
        let all = vec![
            s("a", "int4"),
            s("b", "text"),
            s("c", "boolean"),
            s("d", "float8"),
        ];
        let referenced = vec!["d".to_string(), "b".to_string()];
        let pk = vec!["a".to_string()];
        let result = filter_cdc_columns(&all, Some(&referenced), &pk);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
        // Must be in original table ordinal order: a, b, d  (c dropped).
        assert_eq!(names, vec!["a", "b", "d"]);
    }

    #[test]
    fn test_f15_filter_case_insensitive() {
        let all = vec![s("UserID", "int4"), s("Email", "text"), s("Notes", "text")];
        let referenced = vec!["email".to_string()];
        let pk = vec!["userid".to_string()];
        let result = filter_cdc_columns(&all, Some(&referenced), &pk);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"UserID"), "PK matched case-insensitively");
        assert!(
            names.contains(&"Email"),
            "referenced matched case-insensitively"
        );
        assert!(!names.contains(&"Notes"), "unreferenced column dropped");
    }

    #[test]
    fn test_f15_filter_fallback_when_filtered_set_empty() {
        // If the filter accidentally produces zero columns, fall back to all.
        let all = vec![s("id", "int4"), s("val", "text")];
        // Referenced and pk contain a column that doesn't exist on the table.
        let referenced = vec!["nonexistent".to_string()];
        let pk: Vec<String> = vec![];
        let result = filter_cdc_columns(&all, Some(&referenced), &pk);
        // Should fall back to full list because filtered set is empty.
        assert_eq!(result.len(), 2);
    }

    // ── build_pk_join_condition tests (SF-9) ────────────────────────

    #[test]
    fn test_pk_join_single_column_uses_is_not_distinct_from() {
        let pk = vec!["id".to_string()];
        let result = build_pk_join_condition(&pk);
        assert!(
            result.contains("IS NOT DISTINCT FROM"),
            "should use IS NOT DISTINCT FROM, got: {result}"
        );
        assert!(result.contains(r#"n."id""#), "got: {result}");
        assert!(result.contains(r#"o."id""#), "got: {result}");
    }

    #[test]
    fn test_pk_join_composite_key() {
        let pk = vec!["a".to_string(), "b".to_string()];
        let result = build_pk_join_condition(&pk);
        assert!(result.contains(" AND "), "got: {result}");
        assert!(
            result.contains(r#"n."a" IS NOT DISTINCT FROM o."a""#),
            "got: {result}"
        );
        assert!(
            result.contains(r#"n."b" IS NOT DISTINCT FROM o."b""#),
            "got: {result}"
        );
    }

    #[test]
    fn test_pk_join_empty_returns_true() {
        assert_eq!(build_pk_join_condition(&[]), "TRUE");
    }

    // ── DAG-5: ST buffer compaction tests ─────────────────────────────

    #[test]
    fn test_build_st_compact_sql_contains_table_name() {
        let sql = build_st_compact_sql("pgtrickle_changes", 42, "0/0", "0/FFFF");
        assert!(
            sql.contains("changes_pgt_42"),
            "SQL should reference the ST change buffer table, got: {sql}"
        );
    }

    #[test]
    fn test_build_st_compact_sql_uses_lsn_range() {
        let sql = build_st_compact_sql("pgtrickle_changes", 1, "0/1000", "0/2000");
        assert!(
            sql.contains("'0/1000'::pg_lsn"),
            "SQL should contain prev_lsn, got: {sql}"
        );
        assert!(
            sql.contains("'0/2000'::pg_lsn"),
            "SQL should contain new_lsn, got: {sql}"
        );
    }

    #[test]
    fn test_build_st_compact_sql_removes_net_zero_groups() {
        let sql = build_st_compact_sql("pgtrickle_changes", 1, "0/0", "0/FFFF");
        assert!(
            sql.contains("first_act = 'I' AND last_act = 'D'"),
            "Should remove net-zero INSERT→DELETE groups, got: {sql}"
        );
    }

    #[test]
    fn test_build_st_compact_sql_removes_intermediates() {
        let sql = build_st_compact_sql("pgtrickle_changes", 1, "0/0", "0/FFFF");
        assert!(
            sql.contains("rn_asc > 1 AND rn_desc > 1"),
            "Should remove intermediate rows, got: {sql}"
        );
    }

    #[test]
    fn test_build_st_compact_sql_uses_schema() {
        let sql = build_st_compact_sql("my_schema", 99, "0/0", "0/FFFF");
        assert!(
            sql.contains("\"my_schema\".changes_pgt_99"),
            "SQL should use the provided schema, got: {sql}"
        );
    }

    #[test]
    fn test_build_st_compact_sql_partitions_by_pk_hash() {
        let sql = build_st_compact_sql("pgtrickle_changes", 1, "0/0", "0/FFFF");
        assert!(
            sql.contains("PARTITION BY pk_hash"),
            "Should partition by pk_hash, got: {sql}"
        );
    }

    #[test]
    fn test_compact_st_advisory_lock_key_different_from_base_table() {
        // ST compaction uses 0x5047_5500, base-table uses 0x5047_5400.
        let st_key = compact_st_advisory_lock_key(42);
        let base_key = 0x5047_5400_i64 | 42;
        assert_ne!(
            st_key, base_key,
            "ST and base-table lock keys must not collide"
        );
    }

    #[test]
    fn test_compact_st_advisory_lock_key_embeds_pgt_id() {
        assert_eq!(compact_st_advisory_lock_key(0), 0x5047_5500);
        assert_eq!(compact_st_advisory_lock_key(1), 0x5047_5501);
        assert_eq!(compact_st_advisory_lock_key(255), 0x5047_55FF);
    }

    #[test]
    fn test_compact_st_advisory_lock_key_different_pgt_ids_differ() {
        let k1 = compact_st_advisory_lock_key(1);
        let k2 = compact_st_advisory_lock_key(2);
        assert_ne!(k1, k2, "Different pgt_ids must produce different keys");
    }

    // ── PERF-2: should_promote_inner tests ─────────────────────────

    #[test]
    fn test_promote_already_partitioned_returns_false() {
        assert!(!should_promote_inner(999_999, true, "auto", 100_000));
    }

    #[test]
    fn test_promote_below_threshold_returns_false() {
        assert!(!should_promote_inner(50_000, false, "auto", 100_000));
    }

    #[test]
    fn test_promote_at_threshold_returns_false() {
        // Exactly at threshold — not above.
        assert!(!should_promote_inner(100_000, false, "auto", 100_000));
    }

    #[test]
    fn test_promote_above_threshold_auto_mode() {
        assert!(should_promote_inner(100_001, false, "auto", 100_000));
    }

    #[test]
    fn test_promote_above_threshold_off_mode() {
        // Mode = "off" — never promote.
        assert!(!should_promote_inner(100_001, false, "off", 100_000));
    }

    #[test]
    fn test_promote_above_threshold_on_mode() {
        // Mode = "on" — buffers created pre-partitioned, never runtime promote.
        assert!(!should_promote_inner(100_001, false, "on", 100_000));
    }

    #[test]
    fn test_promote_zero_pending_returns_false() {
        assert!(!should_promote_inner(0, false, "auto", 100_000));
    }

    #[test]
    fn test_promote_negative_pending_returns_false() {
        assert!(!should_promote_inner(-1, false, "auto", 100_000));
    }

    #[test]
    fn test_promote_zero_threshold_returns_false() {
        // Threshold disabled (0) — never promote.
        assert!(!should_promote_inner(999_999, false, "auto", 0));
    }

    #[test]
    fn test_promote_negative_threshold_returns_false() {
        assert!(!should_promote_inner(999_999, false, "auto", -1));
    }
}
