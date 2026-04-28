//! OUTBOX-1..6 (v0.28.0): Transactional Outbox Pattern API.
//!
//! Provides `enable_outbox()`, `disable_outbox()`, `outbox_status()`, and
//! `outbox_rows_consumed()` SQL functions.
//!
//! # Design
//!
//! When a stream table has the outbox pattern enabled, every refresh that
//! produces a non-empty delta writes a summary row to
//! `pgtrickle.outbox_<st_name>` in the same transaction as the MERGE.
//! Consumers are notified via `pg_notify('pgtrickle_outbox_new', outbox_table)`.
//!
//! Two routing strategies exist:
//! - **Inline** (delta_rows ≤ `outbox_inline_threshold_rows`): the delta is
//!   serialized as JSONB in the `payload` column.
//! - **Claim-check** (delta_rows > threshold): the delta rows are written to a
//!   separate `pgtrickle.outbox_delta_rows_<st>` table and the outbox row
//!   carries `is_claim_check = true` with a NULL payload.
//!
//! A view `pgtrickle.pgt_outbox_latest_<st>` always points to the most recent
//! outbox row, making consumption easy.

use pgrx::prelude::*;

use crate::catalog::StreamTableMeta;

use crate::error::PgTrickleError;

// ── Internal helpers ──────────────────────────────────────────────────────

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

/// Build an outbox table name from a stream table name.
/// Convention: `outbox_<st_name>` truncated to 63 bytes.
/// Collision guard: if a relation with that name already exists for a
/// *different* OID, append `_<left(md5(full_name),7)>`.
pub(crate) fn outbox_table_name_for(st_name: &str) -> String {
    let raw = format!("outbox_{}", st_name);
    // PostgreSQL identifier limit is 63 bytes; truncate safely.
    let truncated: String = raw.chars().take(63).collect();
    truncated
}

/// Check if the outbox is already enabled for a given stream table OID.
pub(crate) fn is_outbox_enabled(pgt_id: i64) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_outbox_config WHERE stream_table_oid = $1::oid)",
        &[pgrx::pg_sys::Oid::from(pgt_id as u32).into()],
    )
    .unwrap_or(None)
    .unwrap_or(false)
}

/// Get the outbox table name for a given stream table OID (if enabled).
pub(crate) fn get_outbox_table_name(pgt_id: i64) -> Option<String> {
    Spi::get_one_with_args::<String>(
        "SELECT outbox_table_name FROM pgtrickle.pgt_outbox_config \
         WHERE stream_table_oid = $1::oid",
        &[pgrx::pg_sys::Oid::from(pgt_id as u32).into()],
    )
    .unwrap_or(None)
}

// ── OUTBOX-1: enable_outbox ───────────────────────────────────────────────

/// OUTBOX-1 (v0.28.0): Enable the transactional outbox pattern for a stream table.
///
/// Creates the `pgtrickle.outbox_<st>` table, a claim-check delta table, and
/// a `pgtrickle.pgt_outbox_latest_<st>` view. Registers the stream table in
/// `pgtrickle.pgt_outbox_config`.
///
/// # Errors
/// - `OutboxAlreadyEnabled` if outbox is already active for this ST.
#[pg_extern(schema = "pgtrickle")]
pub fn enable_outbox(p_name: &str, p_retention_hours: default!(i32, 24)) {
    enable_outbox_impl(p_name, p_retention_hours).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn enable_outbox_impl(name: &str, retention_hours: i32) -> Result<(), PgTrickleError> {
    let (schema, st_name) = resolve_st_name(name)?;
    let meta = StreamTableMeta::get_by_name(&schema, &st_name)?;

    // Check not already enabled
    if is_outbox_enabled(meta.pgt_id) {
        return Err(PgTrickleError::OutboxAlreadyEnabled(format!(
            "{}.{}",
            schema, st_name
        )));
    }

    let base_name = outbox_table_name_for(&st_name);

    // Check for naming collision with different stream tables
    let collision = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_outbox_config \
         WHERE outbox_table_name = $1 AND stream_table_oid <> $2::oid)",
        &[
            base_name.as_str().into(),
            pgrx::pg_sys::Oid::from(meta.pgt_id as u32).into(),
        ],
    )
    .unwrap_or(None)
    .unwrap_or(false);

    let outbox_name = if collision {
        // Append 7-char hex suffix from md5 of the full qualified name
        let full_name = format!("{}.{}", schema, st_name);
        let suffix = md5_hex_prefix(&full_name, 7);
        let raw = format!("outbox_{}_{}", st_name, suffix);
        raw.chars().take(63).collect::<String>()
    } else {
        base_name
    };

    // Create the outbox table
    let create_outbox_sql = format!(
        r#"CREATE TABLE IF NOT EXISTS pgtrickle."{outbox}" (
    id              BIGSERIAL    PRIMARY KEY,
    pgt_id          UUID         NOT NULL DEFAULT gen_random_uuid(),
    refresh_id      UUID,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    inserted_count  BIGINT       NOT NULL DEFAULT 0,
    deleted_count   BIGINT       NOT NULL DEFAULT 0,
    is_claim_check  BOOL         NOT NULL DEFAULT false,
    payload         JSONB
);"#,
        outbox = outbox_name.replace('"', "\\\"")
    );

    Spi::run(&create_outbox_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("create outbox table failed: {e}")))?;

    // Create claim-check delta rows table
    let delta_table = format!(
        "outbox_delta_rows_{}",
        st_name.chars().take(48).collect::<String>()
    );
    let create_delta_sql = format!(
        r#"CREATE TABLE IF NOT EXISTS pgtrickle."{delta}" (
    outbox_id   BIGINT       NOT NULL,
    row_op      TEXT         NOT NULL CHECK (row_op IN ('INSERT','DELETE')),
    row_data    JSONB        NOT NULL,
    row_num     BIGINT       NOT NULL,
    PRIMARY KEY (outbox_id, row_num)
);"#,
        delta = delta_table.replace('"', "\\\"")
    );

    Spi::run(&create_delta_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("create delta table failed: {e}")))?;

    // Create the latest-row view
    let view_name = format!(
        "pgt_outbox_latest_{}",
        st_name.chars().take(48).collect::<String>()
    );
    let create_view_sql = format!(
        r#"CREATE OR REPLACE VIEW pgtrickle."{view}" AS
    SELECT * FROM pgtrickle."{outbox}" ORDER BY id DESC LIMIT 1;"#,
        view = view_name.replace('"', "\\\""),
        outbox = outbox_name.replace('"', "\\\"")
    );

    Spi::run(&create_view_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("create outbox view failed: {e}")))?;

    // Register in catalog
    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_outbox_config \
         (stream_table_oid, stream_table_name, outbox_table_name, retention_hours) \
         VALUES ($1::oid, $2, $3, $4) \
         ON CONFLICT (stream_table_oid) DO UPDATE \
           SET outbox_table_name = EXCLUDED.outbox_table_name, \
               retention_hours   = EXCLUDED.retention_hours",
        &[
            pgrx::pg_sys::Oid::from(meta.pgt_id as u32).into(),
            st_name.as_str().into(),
            outbox_name.as_str().into(),
            retention_hours.into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("register outbox config failed: {e}")))?;

    pgrx::log!(
        "[pg_trickle] OUTBOX-1: outbox enabled for '{}.{}' → pgtrickle.{}",
        schema,
        st_name,
        outbox_name
    );

    Ok(())
}

/// Compute a short hex prefix from the MD5 of the input string.
fn md5_hex_prefix(input: &str, len: usize) -> String {
    // Simple hash using std — avoid crypto dependency
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    input.hash(&mut h);
    let val = h.finish();
    let hex = format!("{:016x}", val);
    hex[..len.min(hex.len())].to_string()
}

// ── OUTBOX-2: disable_outbox ──────────────────────────────────────────────

/// OUTBOX-2 (v0.28.0): Disable the transactional outbox pattern for a stream table.
///
/// Drops the outbox table, delta-rows table, and latest view, and removes the
/// catalog entry. If `p_if_exists` is true, silently succeeds when the outbox
/// is not enabled.
#[pg_extern(schema = "pgtrickle")]
pub fn disable_outbox(p_name: &str, p_if_exists: default!(bool, false)) {
    disable_outbox_impl(p_name, p_if_exists).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn disable_outbox_impl(name: &str, if_exists: bool) -> Result<(), PgTrickleError> {
    let (schema, st_name) = resolve_st_name(name)?;
    let meta = StreamTableMeta::get_by_name(&schema, &st_name)?;

    let outbox_table = match get_outbox_table_name(meta.pgt_id) {
        Some(t) => t,
        None => {
            if if_exists {
                return Ok(());
            }
            return Err(PgTrickleError::OutboxNotEnabled(format!(
                "{}.{}",
                schema, st_name
            )));
        }
    };

    // Drop the latest view
    let view_name = format!(
        "pgt_outbox_latest_{}",
        st_name.chars().take(48).collect::<String>()
    );
    let _ = Spi::run(&format!(
        r#"DROP VIEW IF EXISTS pgtrickle."{}""#,
        view_name.replace('"', "\\\"")
    ));

    // Drop the delta-rows table
    let delta_name = format!(
        "outbox_delta_rows_{}",
        st_name.chars().take(48).collect::<String>()
    );
    let _ = Spi::run(&format!(
        r#"DROP TABLE IF EXISTS pgtrickle."{}" CASCADE"#,
        delta_name.replace('"', "\\\"")
    ));

    // Drop the outbox table
    let _ = Spi::run(&format!(
        r#"DROP TABLE IF EXISTS pgtrickle."{}" CASCADE"#,
        outbox_table.replace('"', "\\\"")
    ));

    // Remove from catalog
    Spi::run_with_args(
        "DELETE FROM pgtrickle.pgt_outbox_config WHERE stream_table_oid = $1::oid",
        &[pgrx::pg_sys::Oid::from(meta.pgt_id as u32).into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("unregister outbox config failed: {e}")))?;

    pgrx::log!(
        "[pg_trickle] OUTBOX-2: outbox disabled for '{}.{}'",
        schema,
        st_name
    );

    Ok(())
}

// ── OUTBOX-5: outbox_status ───────────────────────────────────────────────

/// OUTBOX-5 (v0.28.0): Return a JSONB summary of the outbox for a stream table.
///
/// Includes: `enabled`, `outbox_table`, `row_count`, `oldest_row`, `newest_row`,
/// `retention_hours`, `last_drained_at`, `last_drained_count`.
#[pg_extern(schema = "pgtrickle")]
pub fn outbox_status(p_name: &str) -> pgrx::JsonB {
    outbox_status_impl(p_name).unwrap_or_else(|e| pgrx::error!("{}", e))
}

#[allow(clippy::collapsible_if)]
fn outbox_status_impl(name: &str) -> Result<pgrx::JsonB, PgTrickleError> {
    let (schema, st_name) = resolve_st_name(name)?;
    let meta = StreamTableMeta::get_by_name(&schema, &st_name)?;

    let outbox_table = match get_outbox_table_name(meta.pgt_id) {
        Some(t) => t,
        None => {
            let v = serde_json::json!({
                "enabled": false,
                "stream_table": format!("{}.{}", schema, st_name),
            });
            return Ok(pgrx::JsonB(v));
        }
    };

    // Query stats from outbox table
    let stats_sql = format!(
        "SELECT COUNT(*), MIN(created_at), MAX(created_at) \
         FROM pgtrickle.\"{}\"",
        outbox_table.replace('"', "\\\"")
    );

    let (row_count, oldest_row, newest_row) = Spi::connect(|client| {
        let result = client.select(&stats_sql, None, &[])?;
        if let Some(row) = result.into_iter().next() {
            let count = row.get::<i64>(1)?.unwrap_or(0);
            let oldest = row.get::<TimestampWithTimeZone>(2)?.map(|t| t.to_string());
            let newest = row.get::<TimestampWithTimeZone>(3)?.map(|t| t.to_string());
            return Ok::<_, pgrx::spi::SpiError>((count, oldest, newest));
        }
        Ok((0_i64, None::<String>, None::<String>))
    })
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    // Get catalog data
    let catalog_row = Spi::get_one_with_args::<pgrx::JsonB>(
        "SELECT row_to_json(c)::jsonb \
         FROM pgtrickle.pgt_outbox_config c \
         WHERE stream_table_oid = $1::oid",
        &[pgrx::pg_sys::Oid::from(meta.pgt_id as u32).into()],
    )
    .unwrap_or(None);

    let mut v = serde_json::json!({
        "enabled": true,
        "stream_table": format!("{}.{}", schema, st_name),
        "outbox_table": format!("pgtrickle.{}", outbox_table),
        "row_count": row_count,
        "oldest_row": oldest_row,
        "newest_row": newest_row,
    });

    // Merge catalog fields if available
    if let (Some(pgrx::JsonB(cat)), serde_json::Value::Object(out)) = (catalog_row, &mut v) {
        if let Some(map) = cat.as_object() {
            for (k, val) in map {
                if !out.contains_key(k) {
                    out.insert(k.clone(), val.clone());
                }
            }
        }
    }

    Ok(pgrx::JsonB(v))
}

// ── OUTBOX-6: outbox_rows_consumed ────────────────────────────────────────

/// OUTBOX-6 (v0.28.0): Mark outbox rows up to `p_outbox_id` as consumed.
///
/// This updates `last_drained_at` and `last_drained_count` in the catalog and
/// deletes old claim-check delta rows to free storage. It does **not** delete
/// outbox header rows — retention-based cleanup does that.
#[pg_extern(schema = "pgtrickle")]
pub fn outbox_rows_consumed(p_stream_table: &str, p_outbox_id: i64) {
    outbox_rows_consumed_impl(p_stream_table, p_outbox_id).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn outbox_rows_consumed_impl(stream_table: &str, outbox_id: i64) -> Result<(), PgTrickleError> {
    let (schema, st_name) = resolve_st_name(stream_table)?;
    let meta = StreamTableMeta::get_by_name(&schema, &st_name)?;

    let outbox_table = match get_outbox_table_name(meta.pgt_id) {
        Some(t) => t,
        None => {
            return Err(PgTrickleError::OutboxNotEnabled(format!(
                "{}.{}",
                schema, st_name
            )));
        }
    };

    // Delete claim-check delta rows for consumed outbox entries
    let delta_name = format!(
        "outbox_delta_rows_{}",
        st_name.chars().take(48).collect::<String>()
    );
    let delete_delta_sql = format!(
        r#"DELETE FROM pgtrickle."{}" WHERE outbox_id <= $1"#,
        delta_name.replace('"', "\\\"")
    );
    let _ = Spi::run_with_args(&delete_delta_sql, &[outbox_id.into()]);

    // Update catalog stats
    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_outbox_config \
         SET last_drained_at    = now(), \
             last_drained_count = (SELECT COUNT(*) FROM pgtrickle.pgt_outbox_config \
                                   WHERE stream_table_oid = $2::oid) \
         WHERE stream_table_oid = $2::oid",
        &[
            outbox_id.into(),
            pgrx::pg_sys::Oid::from(meta.pgt_id as u32).into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("update outbox catalog failed: {e}")))?;

    pgrx::log!(
        "[pg_trickle] OUTBOX-6: acknowledged outbox rows up to {} for '{}.{}' (outbox: {})",
        outbox_id,
        schema,
        st_name,
        outbox_table
    );

    Ok(())
}

// ── OUTBOX-B1..B7: Consumer Groups ───────────────────────────────────────

/// OUTBOX-B1 (v0.28.0): Create a consumer group for an outbox stream table.
///
/// A consumer group tracks multiple concurrent consumers' positions in the
/// outbox, providing at-least-once delivery semantics with visibility leases.
#[pg_extern(schema = "pgtrickle")]
pub fn create_consumer_group(
    p_name: &str,
    p_outbox: &str,
    p_auto_offset_reset: default!(&str, "'latest'"),
) {
    create_consumer_group_impl(p_name, p_outbox, p_auto_offset_reset)
        .unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn create_consumer_group_impl(
    name: &str,
    outbox: &str,
    auto_offset_reset: &str,
) -> Result<(), PgTrickleError> {
    if auto_offset_reset != "earliest" && auto_offset_reset != "latest" {
        return Err(PgTrickleError::InvalidArgument(format!(
            "auto_offset_reset must be 'earliest' or 'latest', got: {auto_offset_reset}"
        )));
    }

    // Check group doesn't exist yet
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_consumer_groups WHERE group_name = $1)",
        &[name.into()],
    )
    .unwrap_or(None)
    .unwrap_or(false);

    if exists {
        return Err(PgTrickleError::ConsumerGroupAlreadyExists(name.to_string()));
    }

    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_consumer_groups \
         (group_name, outbox_name, auto_offset_reset) VALUES ($1, $2, $3)",
        &[name.into(), outbox.into(), auto_offset_reset.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("create consumer group failed: {e}")))?;

    pgrx::log!(
        "[pg_trickle] OUTBOX-B1: consumer group '{}' created for outbox '{}'",
        name,
        outbox
    );

    Ok(())
}

/// OUTBOX-B2 (v0.28.0): Drop a consumer group and all associated offsets/leases.
#[pg_extern(schema = "pgtrickle")]
pub fn drop_consumer_group(p_name: &str, p_if_exists: default!(bool, false)) {
    drop_consumer_group_impl(p_name, p_if_exists).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn drop_consumer_group_impl(name: &str, if_exists: bool) -> Result<(), PgTrickleError> {
    let deleted = Spi::get_one_with_args::<i64>(
        "WITH deleted AS (DELETE FROM pgtrickle.pgt_consumer_groups \
         WHERE group_name = $1 RETURNING 1) SELECT COUNT(*) FROM deleted",
        &[name.into()],
    )
    .unwrap_or(None)
    .unwrap_or(0);

    if deleted == 0 && !if_exists {
        return Err(PgTrickleError::ConsumerGroupNotFound(name.to_string()));
    }

    Ok(())
}

/// OUTBOX-B3 (v0.28.0): Poll the outbox for new messages as a consumer.
///
/// Returns up to `p_batch_size` outbox rows starting after the consumer's
/// committed offset. Grants a visibility lease for `p_visibility_seconds`
/// seconds — the consumer must call `commit_offset()` or `extend_lease()`
/// before the lease expires.
///
/// Returns: `(outbox_id, pgt_id, created_at, inserted_count, deleted_count,
///            is_claim_check, payload)`
#[allow(clippy::type_complexity)]
#[pg_extern(schema = "pgtrickle")]
pub fn poll_outbox(
    p_group: &str,
    p_consumer: &str,
    p_batch_size: default!(i32, 100),
    p_visibility_seconds: default!(i32, 30),
) -> TableIterator<
    'static,
    (
        name!(outbox_id, i64),
        name!(pgt_id, pgrx::Uuid),
        name!(created_at, TimestampWithTimeZone),
        name!(inserted_count, i64),
        name!(deleted_count, i64),
        name!(is_claim_check, bool),
        name!(payload, Option<pgrx::JsonB>),
    ),
> {
    match poll_outbox_impl(p_group, p_consumer, p_batch_size, p_visibility_seconds) {
        Ok(rows) => TableIterator::new(rows),
        Err(e) => pgrx::error!("{}", e),
    }
}

#[allow(clippy::type_complexity)]
fn poll_outbox_impl(
    group: &str,
    consumer: &str,
    batch_size: i32,
    visibility_seconds: i32,
) -> Result<
    Vec<(
        i64,
        pgrx::Uuid,
        TimestampWithTimeZone,
        i64,
        i64,
        bool,
        Option<pgrx::JsonB>,
    )>,
    PgTrickleError,
> {
    // Look up the group
    let (outbox_name, _) = get_consumer_group(group)?;

    // Get or initialize consumer offset
    let committed_offset = Spi::get_one_with_args::<i64>(
        "SELECT committed_offset FROM pgtrickle.pgt_consumer_offsets \
         WHERE group_name = $1 AND consumer_id = $2",
        &[group.into(), consumer.into()],
    )
    .unwrap_or(None);

    let start_offset = match committed_offset {
        Some(off) => off,
        None => {
            // First-time consumer — initialize based on auto_offset_reset
            let reset = Spi::get_one_with_args::<String>(
                "SELECT auto_offset_reset FROM pgtrickle.pgt_consumer_groups \
                 WHERE group_name = $1",
                &[group.into()],
            )
            .unwrap_or(None)
            .unwrap_or_else(|| "latest".to_string());

            let initial_offset = if reset == "earliest" {
                0_i64
            } else {
                // latest: start from the current max id
                get_outbox_max_id(&outbox_name).unwrap_or(0)
            };

            // Register consumer
            Spi::run_with_args(
                "INSERT INTO pgtrickle.pgt_consumer_offsets \
                 (group_name, consumer_id, committed_offset, last_heartbeat_at) \
                 VALUES ($1, $2, $3, now()) \
                 ON CONFLICT (group_name, consumer_id) DO NOTHING",
                &[group.into(), consumer.into(), initial_offset.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

            initial_offset
        }
    };

    // Fetch rows from the outbox
    let query = format!(
        r#"SELECT id, pgt_id, created_at, inserted_count, deleted_count,
                  is_claim_check, payload
           FROM pgtrickle."{}"
           WHERE id > $1
           ORDER BY id ASC
           LIMIT $2"#,
        outbox_name.replace('"', "\\\"")
    );

    let rows = Spi::connect(|client| {
        let result = client.select(
            &query,
            None,
            &[start_offset.into(), (batch_size as i64).into()],
        )?;
        let mut out = Vec::new();
        for row in result {
            let id = row.get::<i64>(1)?.unwrap_or(0);
            let pgt_id = row
                .get::<pgrx::Uuid>(2)?
                .unwrap_or(pgrx::Uuid::from_bytes([0u8; 16]));
            let created_at = row.get::<TimestampWithTimeZone>(3)?;
            let inserted = row.get::<i64>(4)?.unwrap_or(0);
            let deleted = row.get::<i64>(5)?.unwrap_or(0);
            let claim_check = row.get::<bool>(6)?.unwrap_or(false);
            let payload = row.get::<pgrx::JsonB>(7)?;
            if let Some(ts) = created_at {
                out.push((id, pgt_id, ts, inserted, deleted, claim_check, payload));
            }
        }
        Ok(out)
    })
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    if !rows.is_empty() {
        let last_id = rows.last().map(|(id, ..)| *id).unwrap_or(start_offset);
        let lease_expires_expr = format!("now() + interval '{} seconds'", visibility_seconds);

        // Update heartbeat
        let _ = Spi::run_with_args(
            "UPDATE pgtrickle.pgt_consumer_offsets \
             SET last_heartbeat_at = now() \
             WHERE group_name = $1 AND consumer_id = $2",
            &[group.into(), consumer.into()],
        );

        // Grant or extend lease
        let lease_sql = format!(
            "INSERT INTO pgtrickle.pgt_consumer_leases \
             (group_name, consumer_id, batch_start, batch_end, lease_expires) \
             VALUES ($1, $2, $3, $4, {lease_expires_expr}) \
             ON CONFLICT (group_name, consumer_id) DO UPDATE \
               SET batch_start   = EXCLUDED.batch_start, \
                   batch_end     = EXCLUDED.batch_end, \
                   lease_expires = EXCLUDED.lease_expires"
        );
        let _ = Spi::run_with_args(
            &lease_sql,
            &[
                group.into(),
                consumer.into(),
                start_offset.into(),
                last_id.into(),
            ],
        );
    }

    Ok(rows)
}

/// Get the max id from an outbox table (for `auto_offset_reset = 'latest'`).
fn get_outbox_max_id(outbox_name: &str) -> Option<i64> {
    let sql = format!(
        r#"SELECT COALESCE(MAX(id), 0) FROM pgtrickle."{}""#,
        outbox_name.replace('"', "\\\"")
    );
    Spi::get_one::<i64>(&sql).unwrap_or(None)
}

/// Look up a consumer group, returning `(outbox_name, auto_offset_reset)`.
fn get_consumer_group(group: &str) -> Result<(String, String), PgTrickleError> {
    let row = Spi::connect(|client| {
        let result = client.select(
            "SELECT outbox_name, auto_offset_reset \
             FROM pgtrickle.pgt_consumer_groups WHERE group_name = $1",
            None,
            &[group.into()],
        )?;
        if let Some(row) = result.into_iter().next() {
            let outbox = row.get::<String>(1)?.unwrap_or_default();
            let reset = row.get::<String>(2)?.unwrap_or_default();
            return Ok(Some((outbox, reset)));
        }
        Ok(None)
    })
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    row.ok_or_else(|| PgTrickleError::ConsumerGroupNotFound(group.to_string()))
}

/// OUTBOX-B4 (v0.28.0): Commit the consumer's offset after successful processing.
#[pg_extern(schema = "pgtrickle")]
pub fn commit_offset(p_group: &str, p_consumer: &str, p_last_offset: i64) {
    commit_offset_impl(p_group, p_consumer, p_last_offset).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn commit_offset_impl(group: &str, consumer: &str, last_offset: i64) -> Result<(), PgTrickleError> {
    // Verify group exists
    let _ = get_consumer_group(group)?;

    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_consumer_offsets \
         SET committed_offset  = $3, \
             last_committed_at = now(), \
             last_heartbeat_at = now() \
         WHERE group_name = $1 AND consumer_id = $2",
        &[group.into(), consumer.into(), last_offset.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("commit offset failed: {e}")))?;

    // Clear the lease
    let _ = Spi::run_with_args(
        "DELETE FROM pgtrickle.pgt_consumer_leases \
         WHERE group_name = $1 AND consumer_id = $2",
        &[group.into(), consumer.into()],
    );

    Ok(())
}

/// OUTBOX-B4 (v0.28.0): Extend the visibility lease for a consumer.
#[pg_extern(schema = "pgtrickle")]
pub fn extend_lease(
    p_group: &str,
    p_consumer: &str,
    p_extension_seconds: default!(i32, 30),
) -> Option<TimestampWithTimeZone> {
    extend_lease_impl(p_group, p_consumer, p_extension_seconds)
        .unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn extend_lease_impl(
    group: &str,
    consumer: &str,
    extension_seconds: i32,
) -> Result<Option<TimestampWithTimeZone>, PgTrickleError> {
    let _ = get_consumer_group(group)?;

    let new_expiry = Spi::get_one_with_args::<TimestampWithTimeZone>(
        &format!(
            "UPDATE pgtrickle.pgt_consumer_leases \
             SET lease_expires = lease_expires + interval '{} seconds' \
             WHERE group_name = $1 AND consumer_id = $2 \
             RETURNING lease_expires",
            extension_seconds
        ),
        &[group.into(), consumer.into()],
    )
    .unwrap_or(None);

    Ok(new_expiry)
}

/// OUTBOX-B4 (v0.28.0): Seek a consumer to a specific offset.
#[pg_extern(schema = "pgtrickle")]
pub fn seek_offset(p_group: &str, p_consumer: &str, p_new_offset: i64) {
    seek_offset_impl(p_group, p_consumer, p_new_offset).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn seek_offset_impl(group: &str, consumer: &str, new_offset: i64) -> Result<(), PgTrickleError> {
    let _ = get_consumer_group(group)?;

    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_consumer_offsets \
         (group_name, consumer_id, committed_offset, last_heartbeat_at) \
         VALUES ($1, $2, $3, now()) \
         ON CONFLICT (group_name, consumer_id) DO UPDATE \
           SET committed_offset = EXCLUDED.committed_offset, \
               last_heartbeat_at = now()",
        &[group.into(), consumer.into(), new_offset.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("seek offset failed: {e}")))?;

    Ok(())
}

/// OUTBOX-B5 (v0.28.0): Send a heartbeat from a consumer to signal liveness.
#[pg_extern(schema = "pgtrickle")]
pub fn consumer_heartbeat(p_group: &str, p_consumer: &str) {
    consumer_heartbeat_impl(p_group, p_consumer).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn consumer_heartbeat_impl(group: &str, consumer: &str) -> Result<(), PgTrickleError> {
    let _ = get_consumer_group(group)?;

    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_consumer_offsets \
         SET last_heartbeat_at = now() \
         WHERE group_name = $1 AND consumer_id = $2",
        &[group.into(), consumer.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("heartbeat failed: {e}")))?;

    Ok(())
}

/// OUTBOX-B6 (v0.28.0): Return the current consumer lag for each consumer in a group.
///
/// Returns: `(consumer_id, committed_offset, max_outbox_id, lag, last_heartbeat_at, is_alive)`
#[allow(clippy::type_complexity)]
#[pg_extern(schema = "pgtrickle")]
pub fn consumer_lag(
    p_group: &str,
) -> TableIterator<
    'static,
    (
        name!(consumer_id, String),
        name!(committed_offset, i64),
        name!(max_outbox_id, i64),
        name!(lag, i64),
        name!(last_heartbeat_at, Option<TimestampWithTimeZone>),
        name!(is_alive, bool),
    ),
> {
    match consumer_lag_impl(p_group) {
        Ok(rows) => TableIterator::new(rows),
        Err(e) => pgrx::error!("{}", e),
    }
}

#[allow(clippy::type_complexity)]
fn consumer_lag_impl(
    group: &str,
) -> Result<Vec<(String, i64, i64, i64, Option<TimestampWithTimeZone>, bool)>, PgTrickleError> {
    let (outbox_name, _) = get_consumer_group(group)?;

    let max_id = get_outbox_max_id(&outbox_name).unwrap_or(0);

    let dead_hours = crate::config::PGS_CONSUMER_DEAD_THRESHOLD_HOURS.get();

    let rows = Spi::connect(|client| {
        let result = client.select(
            &format!(
                "SELECT consumer_id, committed_offset, last_heartbeat_at, \
                  (now() - last_heartbeat_at) < interval '{dead_hours} hours' AS is_alive \
                 FROM pgtrickle.pgt_consumer_offsets \
                 WHERE group_name = $1 \
                 ORDER BY consumer_id"
            ),
            None,
            &[group.into()],
        )?;
        let mut out = Vec::new();
        for row in result {
            let consumer = row.get::<String>(1)?.unwrap_or_default();
            let offset = row.get::<i64>(2)?.unwrap_or(0);
            let heartbeat = row.get::<TimestampWithTimeZone>(3)?;
            let alive = row.get::<bool>(4)?.unwrap_or(false);
            let lag = (max_id - offset).max(0);
            out.push((consumer, offset, max_id, lag, heartbeat, alive));
        }
        Ok(out)
    })
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    Ok(rows)
}

// ── Public helper: write an outbox row after a refresh MERGE ─────────────

/// OUTBOX-3 (v0.28.0): Write an outbox notification row after a successful
/// refresh MERGE. Called from the refresh path when a stream table has the
/// outbox pattern enabled.
///
/// This function is designed for the hot path — it should be called only when
/// `is_outbox_enabled()` returns true, avoiding the SPI lookup overhead for
/// the common (outbox disabled) case.
///
/// For the **inline** path (total delta ≤ `inline_threshold_rows`) the current
/// rows of the stream table are serialised as a JSONB array in `payload`.
/// For the **claim-check** path (total delta > threshold) the rows are written
/// to `pgtrickle.outbox_delta_rows_<st>` with `row_op = 'INSERT'`, reflecting
/// the post-refresh snapshot. A true row-level delta (distinguishing inserts
/// from deletes during the MERGE) would require capturing rows inside the MERGE
/// execution and is deferred to a future enhancement.
pub(crate) fn write_outbox_row(
    pgt_id: i64,
    refresh_id: Option<&str>,
    inserted_count: i64,
    deleted_count: i64,
    inline_threshold_rows: i32,
    st_schema: &str,
    st_table: &str,
) -> Result<(), PgTrickleError> {
    use crate::config::PGS_OUTBOX_SKIP_EMPTY_DELTA;

    // Skip empty delta if configured
    if PGS_OUTBOX_SKIP_EMPTY_DELTA.get() && inserted_count == 0 && deleted_count == 0 {
        return Ok(());
    }

    let outbox_name = match get_outbox_table_name(pgt_id) {
        Some(n) => n,
        None => return Ok(()), // outbox was disabled between the hot-path check and here
    };

    let total_delta = inserted_count + deleted_count;
    let is_claim_check = total_delta > inline_threshold_rows as i64;

    // For the inline path: build a JSONB array of the stream table's current
    // rows (up to the threshold). Pass NULL for the claim-check path — the
    // rows will be written to the delta table below.
    let quoted_schema = crate::api::helpers::quote_identifier(st_schema);
    let quoted_table = crate::api::helpers::quote_identifier(st_table);
    let payload: Option<pgrx::JsonB> = if !is_claim_check && total_delta > 0 {
        let sql = format!(
            "SELECT COALESCE(json_agg(row_to_json(t))::jsonb, '[]'::jsonb) \
             FROM {quoted_schema}.{quoted_table} AS t \
             LIMIT $1"
        );
        Spi::get_one_with_args::<pgrx::JsonB>(&sql, &[(inline_threshold_rows as i64).into()])
            .unwrap_or(None)
    } else {
        None
    };

    // Insert the outbox header row. The refresh_id column is UUID; pass NULL
    // when no UUID is provided rather than an empty string (which would fail
    // the ::uuid cast).
    let insert_sql = format!(
        r#"INSERT INTO pgtrickle."{outbox}"
           (refresh_id, inserted_count, deleted_count, is_claim_check, payload)
           VALUES ($1::uuid, $2, $3, $4, $5)
           RETURNING id"#,
        outbox = outbox_name.replace('"', "\\\"")
    );

    let outbox_row_id: i64 = Spi::get_one_with_args::<i64>(
        &insert_sql,
        &[
            // Pass NULL when refresh_id is None so that ::uuid receives a typed
            // NULL rather than an empty-string coercion (which would error).
            match refresh_id {
                Some(s) => s.into(),
                None => Option::<&str>::None.into(),
            },
            inserted_count.into(),
            deleted_count.into(),
            is_claim_check.into(),
            payload.into(),
        ],
    )
    .unwrap_or(None)
    .unwrap_or(0);

    // For the claim-check path: populate the delta rows table with the
    // stream table's current rows. Each row is recorded with row_op='INSERT'
    // representing the post-refresh state. Bug #660 (true delta capture during
    // MERGE) is tracked as a future enhancement.
    if is_claim_check && outbox_row_id > 0 {
        let st_name_part: String = st_table.chars().take(48).collect();
        let delta_table = format!("outbox_delta_rows_{}", st_name_part);
        let insert_delta_sql = format!(
            r#"INSERT INTO pgtrickle."{delta}"
               (outbox_id, row_op, row_data, row_num)
               SELECT $1, 'INSERT', row_to_json(t)::jsonb, row_number() OVER ()
               FROM {quoted_schema}.{quoted_table} AS t"#,
            delta = delta_table.replace('"', "\\\""),
        );
        if let Err(e) = Spi::run_with_args(&insert_delta_sql, &[outbox_row_id.into()]) {
            pgrx::warning!(
                "[pg_trickle] OUTBOX-3: failed to write claim-check delta rows \
                 for outbox '{}': {}",
                outbox_name,
                e
            );
        }
    }

    // Notify any listeners
    let _ = Spi::run_with_args(
        "SELECT pg_notify('pgtrickle_outbox_new', $1)",
        &[outbox_name.as_str().into()],
    );

    Ok(())
}

// ── A06 (v0.35.0): Unit tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── outbox_table_name_for ─────────────────────────────────────────────

    #[test]
    fn test_outbox_table_name_for_simple() {
        assert_eq!(outbox_table_name_for("orders"), "outbox_orders");
    }

    #[test]
    fn test_outbox_table_name_for_truncated_at_63_chars() {
        // Input that would produce a name longer than 63 characters.
        let long_name = "a".repeat(60);
        let result = outbox_table_name_for(&long_name);
        assert!(
            result.len() <= 63,
            "outbox table name must be <= 63 chars, got {}",
            result.len()
        );
    }

    #[test]
    fn test_outbox_table_name_for_empty() {
        // Empty stream table name produces "outbox_" (7 chars, well under limit).
        let result = outbox_table_name_for("");
        assert_eq!(result, "outbox_");
    }

    #[test]
    fn test_outbox_table_name_for_exactly_56_char_input() {
        // 7 prefix chars + 56 input chars = 63 total (at the PostgreSQL limit).
        let name = "b".repeat(56);
        let result = outbox_table_name_for(&name);
        assert_eq!(result.len(), 63);
        assert!(result.starts_with("outbox_b"));
    }

    #[test]
    fn test_outbox_table_name_for_unicode_chars() {
        // Multibyte UTF-8: truncation uses char count, not byte count.
        let name = "ÄÖÜ_table";
        let result = outbox_table_name_for(name);
        assert!(
            result.chars().count() <= 63,
            "char count must be <= 63, got {}",
            result.chars().count()
        );
    }
}
