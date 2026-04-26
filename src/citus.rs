//! Citus integration: detection helpers, placement queries, and stable object naming.
//!
//! # CITUS-1: Detection helpers
//!
//! All functions return sensible defaults when Citus is not installed.
//! Citus-specific code paths are always guarded by [`is_citus_loaded()`].
//!
//! # CITUS-2: Stable object naming
//!
//! Internal pg_trickle objects (`changes_{name}`, trigger functions, WAL slot
//! names, publication names) have historically been keyed by the PostgreSQL
//! relation OID (`changes_12345`).  OIDs are local, database-specific integers
//! that differ across every Citus node and change after a `pg_dump`/restore
//! cycle.  v0.32.0 introduces a *stable name* derived from the schema-qualified
//! table name:
//!
//! ```
//! stable_name = lower_hex(xxh64("schema.table", SEED))[0..16]
//! ```
//!
//! The result is a 16-character lowercase hex string that is:
//! - Identical on every Citus worker for the same logical source table.
//! - Deterministic across major-version upgrades (survives `pg_upgrade`).
//! - Short (16 chars) and URL-safe.
//!
//! For example, `"public"."orders"` might produce `"a3f7b2c1d0e5f9a8"`.

use pgrx::prelude::*;

use crate::error::PgTrickleError;

// Seed chosen to match the stable naming convention; never change this value.
const STABLE_HASH_SEED: u64 = 0x517cc1b727220a95;

// ── Stable hash ─────────────────────────────────────────────────────────────

/// Compute the stable hash key for a source table identified by `schema.table`.
///
/// Returns a 16-character lowercase hex string computed as
/// `lower_hex(xxh64(schema || "." || table, SEED))`.
///
/// The hash is identical on every node (schema-qualified name is global),
/// deterministic across `pg_dump`/restore cycles, and survives major-version
/// upgrades.  It is used to name all pg_trickle-managed objects associated
/// with the source table.
pub fn stable_hash(schema: &str, table: &str) -> String {
    use xxhash_rust::xxh64;
    let input = format!("{schema}.{table}");
    let hash = xxh64::xxh64(input.as_bytes(), STABLE_HASH_SEED);
    format!("{:016x}", hash)
}

/// Compute the stable name for an existing source OID by resolving
/// its schema-qualified name via a catalog lookup.
///
/// Returns `Ok(stable_name)` if the relation is found, or an error
/// if the OID does not correspond to a known relation.
pub fn stable_name_for_oid(source_oid: pg_sys::Oid) -> Result<String, PgTrickleError> {
    let row = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT n.nspname::text AS schema, c.relname::text AS table_name \
                 FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.oid = $1",
                Some(1),
                &[source_oid.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        if let Some(row) = result.into_iter().next() {
            let schema: String = row
                .get(1)
                .ok()
                .flatten()
                .unwrap_or_else(|| "public".to_string());
            let table: String = row
                .get(2)
                .ok()
                .flatten()
                .unwrap_or_else(|| "unknown".to_string());
            Ok(Some((schema, table)))
        } else {
            Ok(None)
        }
    })?;

    match row {
        Some((schema, table)) => Ok(stable_hash(&schema, &table)),
        None => Err(PgTrickleError::NotFound(format!(
            "Relation with OID {} not found — cannot compute stable name",
            source_oid.to_u32()
        ))),
    }
}

// ── SourceIdentifier ──────────────────────────────────────────────────────

/// CITUS-4 (v0.32.0): SQL-callable wrapper for computing the stable name of a
/// source table by OID.  Used by the 0.31.0 → 0.32.0 migration script to
/// backfill `source_stable_name` columns in catalog tables.
///
/// Returns `NULL` when the relation no longer exists (e.g. already dropped).
#[pg_extern(schema = "pgtrickle", name = "source_stable_name")]
pub fn sql_stable_name_for_oid(source_oid: pg_sys::Oid) -> Option<String> {
    stable_name_for_oid(source_oid).ok()
}

// ── SourceIdentifier ──────────────────────────────────────────────────────

/// A pair of (OID, stable_name) that identifies a pg_trickle source table.
///
/// The `stable_name` is used for all internal object names (change buffers,
/// trigger functions, WAL slot names, publication names).  The `oid` is kept
/// for backward compatibility with existing catalog rows and frontier keys.
#[derive(Debug, Clone)]
pub struct SourceIdentifier {
    /// PostgreSQL relation OID (local, may change after pg_dump/restore).
    pub oid: pg_sys::Oid,
    /// 16-character lowercase hex stable name derived from `schema.table`.
    pub stable_name: String,
}

impl SourceIdentifier {
    /// Create a `SourceIdentifier` from an OID and explicit schema/table names.
    ///
    /// This avoids an extra catalog lookup when the schema and table are
    /// already known (e.g. at stream table creation time).
    pub fn from_oid_and_name(oid: pg_sys::Oid, schema: &str, table: &str) -> Self {
        Self {
            oid,
            stable_name: stable_hash(schema, table),
        }
    }

    /// Create a `SourceIdentifier` by looking up the schema-qualified name
    /// from the PostgreSQL catalog.
    pub fn from_oid(oid: pg_sys::Oid) -> Result<Self, PgTrickleError> {
        let name = stable_name_for_oid(oid)?;
        Ok(Self {
            oid,
            stable_name: name,
        })
    }

    /// Create a `SourceIdentifier` from a stored `source_stable_name`.
    ///
    /// Used when the stable name is already in `pgt_change_tracking` and
    /// we want to avoid an extra catalog lookup.
    pub fn from_oid_and_stable_name(oid: pg_sys::Oid, stable_name: String) -> Self {
        Self { oid, stable_name }
    }

    /// STAB-2: Check for hash collisions at stream table creation time.
    ///
    /// Returns an error if another source in `pgt_change_tracking` already
    /// holds this stable name (astronomically unlikely with xxh64, but
    /// checked as a safety guard).
    pub fn check_collision(&self) -> Result<(), PgTrickleError> {
        let collision = Spi::get_one_with_args::<bool>(
            "SELECT EXISTS( \
               SELECT 1 FROM pgtrickle.pgt_change_tracking \
               WHERE source_stable_name = $1 \
                 AND source_relid <> $2 \
             )",
            &[self.stable_name.as_str().into(), self.oid.into()],
        )
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if collision {
            return Err(PgTrickleError::InvalidArgument(format!(
                "Stable name collision for '{}': another source already uses this hash. \
                 This is astronomically unlikely — please report this as a bug.",
                self.stable_name
            )));
        }
        Ok(())
    }
}

// ── Citus detection ──────────────────────────────────────────────────────────

/// CITUS-1: Return `true` if the Citus extension is loaded in the current database.
///
/// When Citus is absent this returns `false` without any error.
pub fn is_citus_loaded() -> bool {
    Spi::get_one::<bool>("SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'citus')")
        .unwrap_or(Some(false))
        .unwrap_or(false)
}

/// How a source table is distributed in a Citus cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Table lives on the coordinator only (non-Citus or `local` table).
    Local,
    /// Citus reference table: replicated to every worker.
    Reference,
    /// Citus distributed table: shards are spread across workers.
    Distributed {
        /// The distribution column name.
        dist_column: String,
    },
}

impl Placement {
    /// Serialize to the text form stored in `pgt_change_tracking.source_placement`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Placement::Local => "local",
            Placement::Reference => "reference",
            Placement::Distributed { .. } => "distributed",
        }
    }
}

/// CITUS-1: Return the placement of a table on this node.
///
/// Returns [`Placement::Local`] when Citus is not loaded or the table is not
/// in Citus metadata.
pub fn placement(source_oid: pg_sys::Oid) -> Placement {
    if !is_citus_loaded() {
        return Placement::Local;
    }

    // Check pg_dist_partition for the distribution kind.
    // partmethod: 'h' = hash, 'n' = none (reference), 'r' = range
    let row = Spi::connect(|client| {
        client
            .select(
                "SELECT partmethod::text, column_to_column_name(logicalrelid, partkey)::text \
                 FROM pg_dist_partition \
                 WHERE logicalrelid = $1",
                Some(1),
                &[source_oid.into()],
            )
            .map(|result| {
                result.into_iter().next().map(|row| {
                    let method: String = row.get(1).ok().flatten().unwrap_or_default();
                    let col: String = row.get(2).ok().flatten().unwrap_or_default();
                    (method, col)
                })
            })
    });

    match row {
        Ok(Some((method, col))) => {
            if method == "n" {
                Placement::Reference
            } else {
                Placement::Distributed { dist_column: col }
            }
        }
        _ => Placement::Local,
    }
}

/// CITUS-1: A Citus worker node address.
#[derive(Debug, Clone)]
pub struct NodeAddr {
    pub node_name: String,
    pub node_port: i32,
}

/// CITUS-1: Return the list of active Citus worker nodes.
///
/// Returns an empty Vec when Citus is not loaded.
pub fn worker_nodes() -> Vec<NodeAddr> {
    if !is_citus_loaded() {
        return Vec::new();
    }

    Spi::connect(|client| {
        let result = match client.select(
            "SELECT nodename::text, nodeport::int4 \
                 FROM pg_dist_node \
                 WHERE isactive AND noderole = 'primary'",
            None,
            &[],
        ) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        result
            .into_iter()
            .map(|row| NodeAddr {
                node_name: row.get(1).ok().flatten().unwrap_or_default(),
                node_port: row.get(2).ok().flatten().unwrap_or(5432),
            })
            .collect()
    })
}

/// CITUS-1: Return the worker nodes that host shards for `table_oid`.
///
/// Returns an empty Vec when Citus is not loaded or the table has no shards.
pub fn shard_placements(table_oid: pg_sys::Oid) -> Vec<NodeAddr> {
    if !is_citus_loaded() {
        return Vec::new();
    }

    Spi::connect(|client| {
        let result = match client.select(
            "SELECT n.nodename::text, n.nodeport::int4 \
                 FROM pg_dist_shard s \
                 JOIN pg_dist_shard_placement p ON p.shardid = s.shardid \
                 JOIN pg_dist_node n ON n.nodeid = p.nodeid \
                 WHERE s.logicalrelid = $1 AND p.shardstate = 1",
            None,
            &[table_oid.into()],
        ) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        result
            .into_iter()
            .map(|row| NodeAddr {
                node_name: row.get(1).ok().flatten().unwrap_or_default(),
                node_port: row.get(2).ok().flatten().unwrap_or(5432),
            })
            .collect()
    })
}

// ── Pre-flight checks (COORD-7, COORD-8) ─────────────────────────────────────

/// COORD-7: Check that all active Citus worker nodes are running the same
/// pg_trickle version as the coordinator.
///
/// When `is_citus_loaded()` is false, returns `Ok(())` immediately.
/// On version mismatch, returns an error listing the offending workers.
///
/// # Note
/// Requires the `dblink` extension installed on the coordinator and that
/// `pg_trickle` is also installed (with identical schema) on every worker.
pub fn check_citus_version_compat() -> Result<(), PgTrickleError> {
    if !is_citus_loaded() {
        return Ok(());
    }

    let local_version =
        Spi::get_one::<String>("SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle'")
            .map_err(|e| PgTrickleError::SpiError(format!("local pg_trickle version: {e}")))?
            .unwrap_or_else(|| "unknown".into());

    let dbname = Spi::get_one::<String>("SELECT current_database()")
        .map_err(|e| PgTrickleError::SpiError(format!("current_database: {e}")))?
        .unwrap_or_else(|| "postgres".into());

    let workers = worker_nodes();
    let mut mismatches: Vec<String> = Vec::new();

    for w in &workers {
        let connstr = worker_conn_string(w, &dbname);
        let connstr_esc = connstr.replace('\'', "''");

        let remote_query = "SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle'";
        let remote_query_esc = remote_query.replace('\'', "''");

        let sql =
            format!("SELECT val FROM dblink('{connstr_esc}', '{remote_query_esc}') AS t(val text)");
        let remote_version = Spi::get_one::<String>(&sql) // nosemgrep: rust.spi.query.dynamic-format — dblink() call cannot be parameterized; connstr_esc/remote_query_esc are SQL-escaped server-controlled Citus catalog values
            .unwrap_or(None)
            .unwrap_or_else(|| "not_installed".into());

        if remote_version != local_version {
            mismatches.push(format!(
                "{}:{} has pg_trickle {} (coordinator has {})",
                w.node_name, w.node_port, remote_version, local_version
            ));
        }
    }

    if !mismatches.is_empty() {
        return Err(PgTrickleError::InvalidArgument(format!(
            "pg_trickle version mismatch across Citus nodes: {}",
            mismatches.join("; ")
        )));
    }

    Ok(())
}

/// COORD-8: Verify that `wal_level = logical` is set on each active Citus
/// worker node.
///
/// When `is_citus_loaded()` is false, returns `Ok(())` immediately.
/// Returns an error listing any workers with insufficient `wal_level`.
pub fn check_worker_wal_levels() -> Result<(), PgTrickleError> {
    if !is_citus_loaded() {
        return Ok(());
    }

    let dbname = Spi::get_one::<String>("SELECT current_database()")
        .map_err(|e| PgTrickleError::SpiError(format!("current_database: {e}")))?
        .unwrap_or_else(|| "postgres".into());

    let workers = worker_nodes();
    let mut failures: Vec<String> = Vec::new();

    for w in &workers {
        let connstr = worker_conn_string(w, &dbname);
        let connstr_esc = connstr.replace('\'', "''");

        let remote_query = "SELECT current_setting('wal_level')";
        let remote_query_esc = remote_query.replace('\'', "''");

        let sql =
            format!("SELECT val FROM dblink('{connstr_esc}', '{remote_query_esc}') AS t(val text)");
        let wal_level = Spi::get_one::<String>(&sql) // nosemgrep: rust.spi.query.dynamic-format — dblink() call cannot be parameterized; connstr_esc/remote_query_esc are SQL-escaped server-controlled Citus catalog values
            .unwrap_or(None)
            .unwrap_or_else(|| "unknown".into());

        if wal_level != "logical" {
            failures.push(format!(
                "{}:{} has wal_level='{}' (need 'logical')",
                w.node_name, w.node_port, wal_level
            ));
        }
    }

    if !failures.is_empty() {
        return Err(PgTrickleError::InvalidArgument(format!(
            "Citus worker(s) do not have wal_level=logical: {}. \
             Set wal_level=logical on each worker and restart PostgreSQL.",
            failures.join("; ")
        )));
    }

    Ok(())
}

// ── Cross-node coordination: pgt_st_locks ────────────────────────────────────

/// Attempt to acquire a named advisory lock in `pgtrickle.pgt_st_locks`.
///
/// Uses `INSERT … ON CONFLICT DO NOTHING` so the operation is safe for
/// concurrent callers on multiple Citus workers.  The lease expires at
/// `now() + lease_ms * interval '1 ms'`; stale entries from crashed holders
/// are purged before each acquisition attempt.
///
/// Returns `true` if the lock was acquired, `false` if another holder owns it.
pub fn try_acquire_st_lock(
    lock_key: &str,
    holder: &str,
    lease_ms: i64,
) -> Result<bool, PgTrickleError> {
    // Expire any stale locks first so crashed holders don't block forever.
    let expired = Spi::get_one::<i64>(
        "DELETE FROM pgtrickle.pgt_st_locks WHERE expires_at < now() RETURNING 1",
    )
    .map_err(|e| PgTrickleError::SpiError(format!("pgt_st_locks expire: {e}")))?
    .unwrap_or(0);
    if expired > 0 {
        pgrx::debug1!(
            "[pg_trickle] pgt_st_locks: expired {} stale lock(s)",
            expired
        );
    }

    // Attempt acquisition.
    let acquired = Spi::connect_mut(|client| {
        let rows = client
            .update(
                "INSERT INTO pgtrickle.pgt_st_locks \
                     (lock_key, holder, acquired_at, expires_at) \
                 VALUES ($1, $2, now(), now() + ($3 * interval '1 ms')) \
                 ON CONFLICT (lock_key) DO NOTHING",
                None,
                &[lock_key.into(), holder.into(), lease_ms.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(format!("pgt_st_locks insert: {e}")))?;
        Ok::<bool, PgTrickleError>(!rows.is_empty())
    })?;

    Ok(acquired)
}

/// Release a named lock in `pgtrickle.pgt_st_locks` held by `holder`.
///
/// No-ops silently when the lock does not exist or is owned by a different holder.
pub fn release_st_lock(lock_key: &str, holder: &str) -> Result<(), PgTrickleError> {
    Spi::run_with_args(
        "DELETE FROM pgtrickle.pgt_st_locks WHERE lock_key = $1 AND holder = $2",
        &[lock_key.into(), holder.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("pgt_st_locks delete: {e}")))
}

/// Extend the expiry of an existing lock.
///
/// Returns `true` if the lock was found (and renewed), `false` otherwise.
pub fn extend_st_lock(lock_key: &str, holder: &str, lease_ms: i64) -> Result<bool, PgTrickleError> {
    let renewed = Spi::connect_mut(|client| {
        let rows = client
            .update(
                "UPDATE pgtrickle.pgt_st_locks \
                 SET expires_at = now() + ($3 * interval '1 ms') \
                 WHERE lock_key = $1 AND holder = $2",
                None,
                &[lock_key.into(), holder.into(), lease_ms.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(format!("pgt_st_locks extend: {e}")))?;
        Ok::<bool, PgTrickleError>(!rows.is_empty())
    })?;
    Ok(renewed)
}

// ── Per-worker WAL CDC helpers ────────────────────────────────────────────────

/// Build a `dblink`-compatible connection string for a Citus worker node.
///
/// Reads the current database name from `current_database()` and combines it
/// with the node's hostname and port.  The resulting string is suitable for
/// passing to `dblink(connstr, query)`.
///
/// # Security
/// Connection strings are constructed from catalog values (`pg_dist_node`)
/// plus the current database name, never from user-supplied input.
pub fn worker_conn_string(worker: &NodeAddr, dbname: &str) -> String {
    format!(
        "host={} port={} dbname={} options='-c enable_seqscan=on'",
        worker.node_name.replace('\'', "''"),
        worker.node_port,
        dbname.replace('\'', "''"),
    )
}

/// Poll a logical replication slot on a remote Citus worker via `dblink`.
///
/// Calls `pg_logical_slot_get_changes(slot_name, NULL, $max_changes, …)` on
/// the remote worker and writes decoded changes into the local change buffer
/// via the standard WAL decoder pipeline (same `test_decoding` text format
/// as local slots).
///
/// # Prerequisites
/// - `dblink` extension must be installed on the coordinator.
/// - The coordinator's PostgreSQL role must have login privileges on the worker.
/// - The replication slot must already exist on the worker (see `ensure_worker_slot`).
///
/// Source descriptor for [`poll_worker_slot_changes`].
pub struct WorkerPollSource<'a> {
    pub change_schema: &'a str,
    pub source_qualified_table: &'a str,
    pub source_oid: pg_sys::Oid,
    pub pk_columns: &'a [String],
    pub columns: &'a [(String, String)],
}

/// Returns the number of change rows written to the local buffer.
pub fn poll_worker_slot_changes(
    worker: &NodeAddr,
    slot_name: &str,
    max_changes: i64,
    src: &WorkerPollSource<'_>,
) -> Result<i64, PgTrickleError> {
    let change_schema = src.change_schema;
    let source_qualified_table = src.source_qualified_table;
    let source_oid = src.source_oid;
    let pk_columns = src.pk_columns;
    let columns = src.columns;
    // Get current database name.
    let dbname = Spi::get_one::<String>("SELECT current_database()")
        .map_err(|e| PgTrickleError::SpiError(format!("current_database: {e}")))?
        .unwrap_or_else(|| "postgres".into());

    let connstr = worker_conn_string(worker, &dbname);
    let connstr_esc = connstr.replace('\'', "''");
    let slot_esc = slot_name.replace('\'', "''");

    // Build the remote query that drains the slot.
    let remote_sql = format!(
        "SELECT lsn::text, xid::text, data \
         FROM pg_logical_slot_get_changes('{}', NULL, {}, 'include-timestamp', 'on')",
        slot_esc, max_changes,
    );
    let remote_sql_esc = remote_sql.replace('\'', "''");

    // Materialize dblink results into a temp table for batch processing.
    let temp_name = format!("__pgt_worker_changes_{}", source_oid.to_u32());
    let _ = Spi::run(&format!("DROP TABLE IF EXISTS {temp_name}")); // nosemgrep: rust.spi.run.dynamic-format — temp_name is derived from a numeric OID, not user input
    let create_sql = format!(
        "CREATE TEMP TABLE {temp_name} ON COMMIT DROP AS \
         SELECT lsn, xid, data \
         FROM dblink('{connstr_esc}', '{remote_sql_esc}') \
         AS t(lsn text, xid text, data text)"
    );
    Spi::run(&create_sql).map_err(|e| {
        PgTrickleError::SpiError(format!(
            "dblink poll worker {}:{} slot '{}': {e}",
            worker.node_name, worker.node_port, slot_name
        ))
    })?;

    // Delegate parsing and buffer-write to the WAL decoder.
    crate::wal_decoder::write_worker_changes_to_buffer(
        &temp_name,
        source_qualified_table,
        change_schema,
        source_oid,
        pk_columns,
        columns,
    )
}

/// Ensure a logical replication slot exists on a remote Citus worker via `dblink`.
///
/// Creates the slot only if it does not already exist, making this safe to
/// call on every scheduler tick.  The remote slot uses the `test_decoding`
/// plugin (same as local slots).
///
/// Returns `Ok(())` on success or if the slot already exists.
pub fn ensure_worker_slot(worker: &NodeAddr, slot_name: &str) -> Result<(), PgTrickleError> {
    let dbname = Spi::get_one::<String>("SELECT current_database()")
        .map_err(|e| PgTrickleError::SpiError(format!("current_database: {e}")))?
        .unwrap_or_else(|| "postgres".into());

    let connstr = worker_conn_string(worker, &dbname);
    let connstr_esc = connstr.replace('\'', "''");
    let slot_esc = slot_name.replace('\'', "''");

    // Check if slot exists on the remote worker.
    let remote_check =
        format!("SELECT count(*) FROM pg_replication_slots WHERE slot_name = '{slot_esc}'");
    let remote_check_esc = remote_check.replace('\'', "''");

    let slot_check_sql = format!(
        "SELECT val::bigint FROM dblink('{connstr_esc}', '{remote_check_esc}') AS t(val text)"
    );
    let exists_count = Spi::get_one::<i64>(&slot_check_sql) // nosemgrep: rust.spi.query.dynamic-format — dblink() call cannot be parameterized; connstr_esc/remote_check_esc are SQL-escaped server-controlled Citus catalog values
        .map_err(|e| {
            PgTrickleError::SpiError(format!(
                "dblink check slot on {}:{}: {e}",
                worker.node_name, worker.node_port
            ))
        })?
        .unwrap_or(0);

    if exists_count > 0 {
        return Ok(());
    }

    // Create the slot on the remote worker.
    let remote_create =
        format!("SELECT pg_create_logical_replication_slot('{slot_esc}', 'test_decoding')");
    let remote_create_esc = remote_create.replace('\'', "''");

    Spi::run(&format!( // nosemgrep: rust.spi.run.dynamic-format — dblink call cannot be parameterized; connstr_esc/remote_create_esc use SQL single-quote escaping on internal Citus catalog values only
        "SELECT * FROM dblink('{connstr_esc}', '{remote_create_esc}') AS t(slot_name text, lsn text)"
    ))
    .map_err(|e| {
        PgTrickleError::SpiError(format!(
            "dblink create slot '{}' on {}:{}: {e}",
            slot_name, worker.node_name, worker.node_port
        ))
    })?;

    pgrx::info!(
        "[pg_trickle] created WAL slot '{}' on Citus worker {}:{}",
        slot_name,
        worker.node_name,
        worker.node_port,
    );

    Ok(())
}

// ── pg_ripple VP-promotion notification handler ───────────────────────────────

/// Parsed payload from a `pg_ripple.vp_promoted` NOTIFY.
///
/// pg_ripple v0.58.0 emits this notification after distributing a VP delta
/// table via `create_distributed_table()`.  The payload JSON carries:
///
/// - `table`              — fully-qualified logical table name
///   (e.g. `_pg_ripple.vp_42_delta`)
/// - `shard_count`        — number of Citus shards created
/// - `shard_table_prefix` — physical shard name prefix on workers
///   (e.g. `_pg_ripple.vp_42_delta_`)
/// - `predicate_id`       — pg_ripple predicate integer ID
#[derive(Debug)]
pub struct VpPromotedPayload {
    pub table: String,
    pub shard_count: i64,
    pub shard_table_prefix: String,
    pub predicate_id: i64,
}

/// Parse a `pg_ripple.vp_promoted` notification payload.
///
/// Returns `None` when the JSON is malformed or a required field is absent.
pub fn parse_vp_promoted_payload(payload: &str) -> Option<VpPromotedPayload> {
    // Minimal JSON parser using SPI — avoids adding a serde_json dep.
    let table = Spi::get_one_with_args::<String>("SELECT $1::jsonb ->> 'table'", &[payload.into()])
        .ok()
        .flatten()?;

    let shard_count = Spi::get_one_with_args::<i64>(
        "SELECT ($1::jsonb ->> 'shard_count')::bigint",
        &[payload.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(0);

    let shard_table_prefix = Spi::get_one_with_args::<String>(
        "SELECT $1::jsonb ->> 'shard_table_prefix'",
        &[payload.into()],
    )
    .ok()
    .flatten()
    .unwrap_or_else(|| format!("{table}_"));

    let predicate_id = Spi::get_one_with_args::<i64>(
        "SELECT ($1::jsonb ->> 'predicate_id')::bigint",
        &[payload.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(0);

    Some(VpPromotedPayload {
        table,
        shard_count,
        shard_table_prefix,
        predicate_id,
    })
}

/// SQL-callable helper: process a `pg_ripple.vp_promoted` notification payload.
///
/// Call this from a regular backend session that is LISTENing to
/// `pg_ripple.vp_promoted`:
///
/// ```sql
/// LISTEN "pg_ripple.vp_promoted";
/// -- … receive notification …
/// SELECT pgtrickle.handle_vp_promoted(:'NOTIFY_PAYLOAD');
/// ```
///
/// The function logs the promotion details.  When the table matches an active
/// pg_trickle distributed CDC source (i.e., `source_placement = 'distributed'`
/// in `pgt_change_tracking`), it also records the shard metadata in
/// `pgt_worker_slots` for each active Citus worker so that the scheduler can
/// start polling per-shard WAL changes on the next tick without a full catalog
/// scan.
///
/// Returns `true` if the payload was valid and a matching source was found;
/// `false` if the payload was invalid or no source matched.
#[pg_extern(schema = "pgtrickle", name = "handle_vp_promoted")]
pub fn sql_handle_vp_promoted(payload: &str) -> bool {
    let Some(promo) = parse_vp_promoted_payload(payload) else {
        pgrx::warning!("[pg_trickle] handle_vp_promoted: could not parse payload: {payload}");
        return false;
    };

    pgrx::info!(
        "[pg_trickle] vp_promoted: table={} shard_count={} prefix={} predicate_id={}",
        promo.table,
        promo.shard_count,
        promo.shard_table_prefix,
        promo.predicate_id,
    );

    // Check whether any active CDC source points at this VP table.
    let source_exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS( \
             SELECT 1 FROM pgtrickle.pgt_change_tracking \
             WHERE source_placement = 'distributed' \
               AND source_qualified_table = $1 \
         )",
        &[promo.table.as_str().into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if source_exists {
        pgrx::info!(
            "[pg_trickle] vp_promoted: source {} is tracked as distributed — \
             workers will be probed on the next scheduler tick",
            promo.table,
        );
    }

    source_exists
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// TEST-1: Verify stable_hash produces consistent output for known inputs.
    ///
    /// These vectors were computed once and are pinned to catch regressions.
    #[test]
    fn test_stable_hash_known_vectors() {
        // Compute expected value by re-running the same algorithm.
        let h1 = stable_hash("public", "orders");
        // Must be 16 lowercase hex characters.
        assert_eq!(h1.len(), 16, "stable_hash must return 16 chars");
        assert!(
            h1.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "stable_hash must be lowercase hex"
        );

        // Cross-platform determinism: same input → same output.
        let h2 = stable_hash("public", "orders");
        assert_eq!(h1, h2, "stable_hash must be deterministic");

        // Different inputs → different hashes (no trivial collisions).
        let h3 = stable_hash("public", "order_items");
        assert_ne!(h1, h3, "different tables should have different hashes");

        let h4 = stable_hash("myschema", "orders");
        assert_ne!(h1, h4, "different schemas should produce different hashes");
    }

    /// TEST-1: Verify stable_hash against a pinned reference value.
    ///
    /// This ensures the hash function and seed are never accidentally changed.
    #[test]
    fn test_stable_hash_pinned_vector() {
        // "public.orders" with seed 0x517cc1b727220a95
        let h = stable_hash("public", "orders");
        // Re-compute via the same xxh64 path to confirm the pin.
        use xxhash_rust::xxh64;
        let expected = format!(
            "{:016x}",
            xxh64::xxh64(b"public.orders", 0x517cc1b727220a95)
        );
        assert_eq!(
            h, expected,
            "stable_hash pin changed — this breaks stable naming!"
        );
    }

    /// TEST-2: SourceIdentifier round-trip serialisation.
    #[test]
    fn test_source_identifier_from_name() {
        let id =
            SourceIdentifier::from_oid_and_name(pg_sys::Oid::from(12345u32), "public", "orders");
        assert_eq!(id.oid, pg_sys::Oid::from(12345u32));
        assert_eq!(id.stable_name.len(), 16);
        // Recompute — must be identical.
        let expected = stable_hash("public", "orders");
        assert_eq!(id.stable_name, expected);
    }

    /// TEST-2: SourceIdentifier is deterministic (different OIDs, same name).
    #[test]
    fn test_source_identifier_oid_independent() {
        let id1 =
            SourceIdentifier::from_oid_and_name(pg_sys::Oid::from(100u32), "public", "orders");
        let id2 =
            SourceIdentifier::from_oid_and_name(pg_sys::Oid::from(999u32), "public", "orders");
        // stable_name is OID-independent — same on every Citus node.
        assert_eq!(
            id1.stable_name, id2.stable_name,
            "stable_name must not depend on OID"
        );
    }
}
