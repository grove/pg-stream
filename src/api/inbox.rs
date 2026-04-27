//! INBOX-1..9 + INBOX-B1..B4 (v0.28.0): Transactional Inbox Pattern API.
//!
//! Provides `create_inbox()`, `drop_inbox()`, `inbox_status()`,
//! `inbox_health()`, `replay_inbox_messages()`, `enable_inbox_tracking()`,
//! `enable_inbox_ordering()`, `disable_inbox_ordering()`,
//! `enable_inbox_priority()`, `disable_inbox_priority()`,
//! `inbox_ordering_gaps()`, and `inbox_is_my_partition()` SQL functions.
//!
//! # Design
//!
//! An inbox is a PostgreSQL table (typically in the `pgtrickle` schema) that
//! acts as an idempotent message receiver.  The extension automatically
//! creates three stream tables on top of the inbox table:
//!
//! - **`<inbox>_pending`** — messages awaiting processing
//!   (`WHERE processed_at IS NULL AND retry_count < max_retries`)
//! - **`<inbox>_dlq`** — messages in the dead-letter queue
//!   (`WHERE processed_at IS NULL AND retry_count >= max_retries`)
//! - **`<inbox>_stats`** — per-event-type message counts (GROUP BY event_type)
//!
//! Ordering support (INBOX-B1) adds a `next_<inbox>` stream table that
//! presents only the next unprocessed message per aggregate using DISTINCT ON.

use pgrx::prelude::*;

use crate::error::PgTrickleError;

// ── Internal helpers ──────────────────────────────────────────────────────

/// Check whether a relation exists in the given schema.
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

/// Check whether a column exists in the given table.
fn column_exists(schema: &str, table: &str, column: &str) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = $2 AND column_name = $3)",
        &[schema.into(), table.into(), column.into()],
    )
    .unwrap_or(None)
    .unwrap_or(false)
}

/// Check if an inbox config entry exists.
fn inbox_exists(name: &str) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_config WHERE inbox_name = $1)",
        &[name.into()],
    )
    .unwrap_or(None)
    .unwrap_or(false)
}

/// Get inbox config as a struct-like tuple.
fn get_inbox_config(name: &str) -> Result<InboxConfig, PgTrickleError> {
    let row = Spi::connect(|client| {
        let result = client.select(
            "SELECT inbox_name, inbox_schema, max_retries, schedule, with_dead_letter, \
                    with_stats, retention_hours, id_column, processed_at_column, \
                    retry_count_column, error_column, received_at_column, event_type_column \
             FROM pgtrickle.pgt_inbox_config WHERE inbox_name = $1",
            None,
            &[name.into()],
        )?;
        if let Some(row) = result.into_iter().next() {
            return Ok(Some(InboxConfig {
                inbox_name: row.get::<String>(1)?.unwrap_or_default(),
                inbox_schema: row.get::<String>(2)?.unwrap_or("pgtrickle".to_string()),
                max_retries: row.get::<i32>(3)?.unwrap_or(3),
                schedule: row.get::<String>(4)?.unwrap_or("1s".to_string()),
                with_dead_letter: row.get::<bool>(5)?.unwrap_or(true),
                with_stats: row.get::<bool>(6)?.unwrap_or(true),
                retention_hours: row.get::<i32>(7)?.unwrap_or(72),
                id_column: row.get::<String>(8)?.unwrap_or("event_id".to_string()),
                processed_at_column: row.get::<String>(9)?.unwrap_or("processed_at".to_string()),
                retry_count_column: row.get::<String>(10)?.unwrap_or("retry_count".to_string()),
                error_column: row.get::<String>(11)?.unwrap_or("error".to_string()),
                received_at_column: row.get::<String>(12)?.unwrap_or("received_at".to_string()),
                event_type_column: row.get::<String>(13)?.unwrap_or("event_type".to_string()),
            }));
        }
        Ok(None)
    })
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    row.ok_or_else(|| PgTrickleError::InboxNotFound(name.to_string()))
}

struct InboxConfig {
    inbox_name: String,
    inbox_schema: String,
    max_retries: i32,
    schedule: String,
    with_dead_letter: bool,
    with_stats: bool,
    retention_hours: i32,
    id_column: String,
    processed_at_column: String,
    retry_count_column: String,
    error_column: String,
    received_at_column: String,
    event_type_column: String,
}

// ── INBOX-1: create_inbox ─────────────────────────────────────────────────

/// INBOX-1 (v0.28.0): Create a named transactional inbox.
///
/// Creates the inbox table (if it does not exist), registers it in
/// `pgt_inbox_config`, and auto-creates three stream tables:
/// `<name>_pending`, `<name>_dlq`, `<name>_stats`.
#[pg_extern(schema = "pgtrickle")]
#[allow(clippy::too_many_arguments)]
pub fn create_inbox(
    p_name: &str,
    p_schema: default!(&str, "'pgtrickle'"),
    p_max_retries: default!(i32, 3),
    p_schedule: default!(&str, "'1s'"),
    with_dead_letter: default!(bool, true),
    with_stats: default!(bool, true),
    p_retention_hours: default!(i32, 72),
) {
    create_inbox_impl(
        p_name,
        p_schema,
        p_max_retries,
        p_schedule,
        with_dead_letter,
        with_stats,
        p_retention_hours,
    )
    .unwrap_or_else(|e| pgrx::error!("{}", e))
}

#[allow(clippy::too_many_arguments)]
fn create_inbox_impl(
    name: &str,
    schema: &str,
    max_retries: i32,
    schedule: &str,
    with_dead_letter: bool,
    with_stats: bool,
    retention_hours: i32,
) -> Result<(), PgTrickleError> {
    if inbox_exists(name) {
        return Err(PgTrickleError::InboxAlreadyExists(name.to_string()));
    }

    // Create the inbox table if it doesn't exist yet
    let create_table_sql = format!(
        r#"CREATE TABLE IF NOT EXISTS "{}"."{}" (
    event_id        TEXT        NOT NULL PRIMARY KEY,
    event_type      TEXT,
    source          TEXT,
    aggregate_id    TEXT,
    payload         JSONB,
    received_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at    TIMESTAMPTZ,
    error           TEXT,
    retry_count     INT         NOT NULL DEFAULT 0,
    trace_id        TEXT
);"#,
        schema.replace('"', "\\\""),
        name.replace('"', "\\\"")
    );

    Spi::run(&create_table_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("create inbox table failed: {e}")))?;

    // Create pending stream table
    let pending_st = format!("{}_pending", name);
    let pending_query = format!(
        r#"SELECT * FROM "{}"."{}" WHERE processed_at IS NULL AND retry_count < {}"#,
        schema.replace('"', "\\\""),
        name.replace('"', "\\\""),
        max_retries
    );
    create_inbox_stream_table(schema, &pending_st, &pending_query, schedule, None)?;

    // Create DLQ stream table (if enabled)
    if with_dead_letter {
        let dlq_st = format!("{}_dlq", name);
        let dlq_query = format!(
            r#"SELECT * FROM "{}"."{}" WHERE processed_at IS NULL AND retry_count >= {}"#,
            schema.replace('"', "\\\""),
            name.replace('"', "\\\""),
            max_retries
        );
        create_inbox_stream_table(schema, &dlq_st, &dlq_query, schedule, None)?;
    }

    // Create stats stream table (if enabled)
    if with_stats {
        let stats_st = format!("{}_stats", name);
        let stats_query = format!(
            r#"SELECT event_type, COUNT(*) AS message_count,
               SUM(CASE WHEN processed_at IS NOT NULL THEN 1 ELSE 0 END) AS processed_count,
               SUM(CASE WHEN processed_at IS NULL AND retry_count >= {} THEN 1 ELSE 0 END) AS dlq_count
               FROM "{}"."{}" GROUP BY event_type"#,
            max_retries,
            schema.replace('"', "\\\""),
            name.replace('"', "\\\"")
        );
        create_inbox_stream_table(schema, &stats_st, &stats_query, schedule, None)?;
    }

    // Register in catalog
    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_inbox_config \
         (inbox_name, inbox_schema, max_retries, schedule, with_dead_letter, \
          with_stats, retention_hours) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        &[
            name.into(),
            schema.into(),
            max_retries.into(),
            schedule.into(),
            with_dead_letter.into(),
            with_stats.into(),
            retention_hours.into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("register inbox config failed: {e}")))?;

    pgrx::log!(
        "[pg_trickle] INBOX-1: inbox '{}' created in schema '{}'",
        name,
        schema
    );

    Ok(())
}

/// Helper: create a managed stream table for an inbox.
fn create_inbox_stream_table(
    schema: &str,
    st_name: &str,
    query: &str,
    schedule: &str,
    refresh_mode: Option<&str>,
) -> Result<(), PgTrickleError> {
    let fqn = format!("{}.{}", schema, st_name);
    match refresh_mode {
        Some(mode) => Spi::run_with_args(
            "SELECT pgtrickle.create_stream_table($1, $2, schedule => $3, refresh_mode => $4)",
            &[
                fqn.as_str().into(),
                query.into(),
                schedule.into(),
                mode.into(),
            ],
        ),
        None => Spi::run_with_args(
            "SELECT pgtrickle.create_stream_table($1, $2, schedule => $3)",
            &[fqn.as_str().into(), query.into(), schedule.into()],
        ),
    }
    .map_err(|e| {
        PgTrickleError::SpiError(format!(
            "create inbox stream table '{}' failed: {e}",
            st_name
        ))
    })
}

// ── INBOX-2: drop_inbox ───────────────────────────────────────────────────

/// INBOX-2 (v0.28.0): Drop a named inbox and its associated stream tables.
///
/// If `p_cascade` is true, also drops the underlying inbox table.
/// If `p_if_exists` is true, silently succeeds when the inbox is not found.
#[pg_extern(schema = "pgtrickle")]
pub fn drop_inbox(
    p_name: &str,
    p_if_exists: default!(bool, false),
    p_cascade: default!(bool, false),
) {
    drop_inbox_impl(p_name, p_if_exists, p_cascade).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn drop_inbox_impl(name: &str, if_exists: bool, cascade: bool) -> Result<(), PgTrickleError> {
    if !inbox_exists(name) {
        if if_exists {
            return Ok(());
        }
        return Err(PgTrickleError::InboxNotFound(name.to_string()));
    }

    let cfg = get_inbox_config(name)?;

    // Drop ordering/priority configs first (FK cascade handles this, but be explicit)
    let _ = Spi::run_with_args(
        "DELETE FROM pgtrickle.pgt_inbox_ordering_config WHERE inbox_name = $1",
        &[name.into()],
    );
    let _ = Spi::run_with_args(
        "DELETE FROM pgtrickle.pgt_inbox_priority_config WHERE inbox_name = $1",
        &[name.into()],
    );

    // Drop stream tables (only those that actually exist in the catalog)
    let st_names = vec![
        format!("{}_pending", name),
        format!("{}_dlq", name),
        format!("{}_stats", name),
        format!("next_{}", name),
    ];
    for st in &st_names {
        let st_exists = Spi::get_one_with_args::<bool>(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables WHERE pgt_name = $1)",
            &[st.as_str().into()],
        )
        .unwrap_or(None)
        .unwrap_or(false);
        if st_exists {
            let fqn = format!("{}.{}", cfg.inbox_schema, st);
            let _ = Spi::run_with_args(
                "SELECT pgtrickle.drop_stream_table($1)",
                &[fqn.as_str().into()],
            );
        }
    }

    // Remove catalog entry
    Spi::run_with_args(
        "DELETE FROM pgtrickle.pgt_inbox_config WHERE inbox_name = $1",
        &[name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("remove inbox config failed: {e}")))?;

    // Optionally drop the backing table
    if cascade && relation_exists(&cfg.inbox_schema, name) {
        let _ = Spi::run(&format!(
            r#"DROP TABLE IF EXISTS "{}"."{}" CASCADE"#,
            cfg.inbox_schema.replace('"', "\\\""),
            name.replace('"', "\\\"")
        ));
    }

    pgrx::log!("[pg_trickle] INBOX-2: inbox '{}' dropped", name);

    Ok(())
}

// ── INBOX-3: enable_inbox_tracking ────────────────────────────────────────

/// INBOX-3 (v0.28.0): Attach an existing table as an inbox (bring-your-own-table).
///
/// Validates that the required columns exist, registers the table in
/// `pgt_inbox_config`, and creates the standard stream tables.
#[pg_extern(schema = "pgtrickle")]
#[allow(clippy::too_many_arguments)]
pub fn enable_inbox_tracking(
    p_name: &str,
    p_table_ref: &str,
    p_id_column: default!(&str, "'event_id'"),
    p_processed_at_column: default!(&str, "'processed_at'"),
    p_retry_count_column: default!(&str, "'retry_count'"),
    p_error_column: default!(&str, "'error'"),
    p_received_at_column: default!(&str, "'received_at'"),
    p_event_type_column: default!(&str, "'event_type'"),
    p_max_retries: default!(i32, 3),
    p_schedule: default!(&str, "'1s'"),
) {
    enable_inbox_tracking_impl(
        p_name,
        p_table_ref,
        p_id_column,
        p_processed_at_column,
        p_retry_count_column,
        p_error_column,
        p_received_at_column,
        p_event_type_column,
        p_max_retries,
        p_schedule,
    )
    .unwrap_or_else(|e| pgrx::error!("{}", e))
}

#[allow(clippy::too_many_arguments)]
fn enable_inbox_tracking_impl(
    name: &str,
    table_ref: &str,
    id_col: &str,
    processed_at_col: &str,
    retry_count_col: &str,
    error_col: &str,
    received_at_col: &str,
    event_type_col: &str,
    max_retries: i32,
    schedule: &str,
) -> Result<(), PgTrickleError> {
    if inbox_exists(name) {
        return Err(PgTrickleError::InboxAlreadyExists(name.to_string()));
    }

    // Parse table_ref into (schema, table)
    let (tbl_schema, tbl_name) = if let Some((s, t)) = table_ref.split_once('.') {
        (
            s.trim_matches('"').to_string(),
            t.trim_matches('"').to_string(),
        )
    } else {
        (
            "public".to_string(),
            table_ref.trim_matches('"').to_string(),
        )
    };

    if !relation_exists(&tbl_schema, &tbl_name) {
        return Err(PgTrickleError::InboxTableNotFound(table_ref.to_string()));
    }

    // Validate required columns
    for (col_name, col_val) in [
        ("id_column", id_col),
        ("processed_at_column", processed_at_col),
        ("retry_count_column", retry_count_col),
    ] {
        if !column_exists(&tbl_schema, &tbl_name, col_val) {
            return Err(PgTrickleError::InboxColumnMissing(
                table_ref.to_string(),
                format!("{} ({})", col_val, col_name),
            ));
        }
    }

    // Register in catalog
    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_inbox_config \
         (inbox_name, inbox_schema, max_retries, schedule, with_dead_letter, \
          with_stats, retention_hours, id_column, processed_at_column, \
          retry_count_column, error_column, received_at_column, event_type_column, \
          is_managed) \
         VALUES ($1, $2, $3, $4, true, true, 72, $5, $6, $7, $8, $9, $10, false)",
        &[
            name.into(),
            tbl_schema.as_str().into(),
            max_retries.into(),
            schedule.into(),
            id_col.into(),
            processed_at_col.into(),
            retry_count_col.into(),
            error_col.into(),
            received_at_col.into(),
            event_type_col.into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("register inbox tracking failed: {e}")))?;

    // Create the standard stream tables
    let pending_query = format!(
        r#"SELECT * FROM "{}"."{}" WHERE {} IS NULL AND {} < {}"#,
        tbl_schema, tbl_name, processed_at_col, retry_count_col, max_retries
    );
    create_inbox_stream_table(
        &tbl_schema,
        &format!("{}_pending", name),
        &pending_query,
        schedule,
        None,
    )?;

    pgrx::log!(
        "[pg_trickle] INBOX-3: inbox tracking enabled for '{}' on table '{}'",
        name,
        table_ref
    );

    Ok(())
}

// ── INBOX-4: inbox_health ─────────────────────────────────────────────────

/// INBOX-4 (v0.28.0): Return a JSONB health summary for an inbox.
#[pg_extern(schema = "pgtrickle")]
pub fn inbox_health(p_name: &str) -> pgrx::JsonB {
    inbox_health_impl(p_name).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn inbox_health_impl(name: &str) -> Result<pgrx::JsonB, PgTrickleError> {
    let cfg = get_inbox_config(name)?;

    let total_q = format!(
        r#"SELECT COUNT(*) FROM "{}"."{}" "#,
        cfg.inbox_schema, cfg.inbox_name
    );
    let total: i64 = Spi::get_one::<i64>(&total_q).unwrap_or(None).unwrap_or(0);

    let pending_q = format!(
        r#"SELECT COUNT(*) FROM "{}"."{}" WHERE {} IS NULL AND {} < {}"#,
        cfg.inbox_schema,
        cfg.inbox_name,
        cfg.processed_at_column,
        cfg.retry_count_column,
        cfg.max_retries
    );
    let pending: i64 = Spi::get_one::<i64>(&pending_q).unwrap_or(None).unwrap_or(0);

    let dlq_q = format!(
        r#"SELECT COUNT(*) FROM "{}"."{}" WHERE {} IS NULL AND {} >= {}"#,
        cfg.inbox_schema,
        cfg.inbox_name,
        cfg.processed_at_column,
        cfg.retry_count_column,
        cfg.max_retries
    );
    let dlq: i64 = Spi::get_one::<i64>(&dlq_q).unwrap_or(None).unwrap_or(0);

    let processed_q = format!(
        r#"SELECT COUNT(*) FROM "{}"."{}" WHERE {} IS NOT NULL"#,
        cfg.inbox_schema, cfg.inbox_name, cfg.processed_at_column
    );
    let processed: i64 = Spi::get_one::<i64>(&processed_q)
        .unwrap_or(None)
        .unwrap_or(0);

    let dlq_alert = crate::config::PGS_INBOX_DLQ_ALERT_MAX_PER_REFRESH.get();
    let health_status = if dlq_alert > 0 && dlq >= dlq_alert as i64 {
        "WARNING"
    } else {
        "OK"
    };

    let v = serde_json::json!({
        "inbox_name": cfg.inbox_name,
        "inbox_schema": cfg.inbox_schema,
        "status": health_status,
        "total_messages": total,
        "pending_messages": pending,
        "dlq_messages": dlq,
        "processed_messages": processed,
        "max_retries": cfg.max_retries,
        "retention_hours": cfg.retention_hours,
    });

    Ok(pgrx::JsonB(v))
}

// ── INBOX-5: inbox_status ─────────────────────────────────────────────────

/// INBOX-5 (v0.28.0): Return a table row summary for one or all inboxes.
///
/// If `p_name` is NULL, returns all inboxes.
#[allow(clippy::type_complexity)]
#[pg_extern(schema = "pgtrickle")]
pub fn inbox_status(
    p_name: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(inbox_name, String),
        name!(inbox_schema, String),
        name!(max_retries, i32),
        name!(schedule, String),
        name!(with_dead_letter, bool),
        name!(with_stats, bool),
        name!(created_at, Option<TimestampWithTimeZone>),
    ),
> {
    match inbox_status_impl(p_name) {
        Ok(rows) => TableIterator::new(rows),
        Err(e) => pgrx::error!("{}", e),
    }
}

#[allow(clippy::type_complexity)]
fn inbox_status_impl(
    name: Option<&str>,
) -> Result<
    Vec<(
        String,
        String,
        i32,
        String,
        bool,
        bool,
        Option<TimestampWithTimeZone>,
    )>,
    PgTrickleError,
> {
    let rows = Spi::connect(|client| {
        let result = match name {
            Some(n) => client.select(
                "SELECT inbox_name, inbox_schema, max_retries, schedule, \
                         with_dead_letter, with_stats, created_at \
                  FROM pgtrickle.pgt_inbox_config WHERE inbox_name = $1 ORDER BY inbox_name",
                None,
                &[n.into()],
            )?,
            None => client.select(
                "SELECT inbox_name, inbox_schema, max_retries, schedule, \
                         with_dead_letter, with_stats, created_at \
                  FROM pgtrickle.pgt_inbox_config ORDER BY inbox_name",
                None,
                &[],
            )?,
        };
        let mut out = Vec::new();
        for row in result {
            let iname = row.get::<String>(1)?.unwrap_or_default();
            let ischema = row.get::<String>(2)?.unwrap_or_default();
            let retries = row.get::<i32>(3)?.unwrap_or(3);
            let sched = row.get::<String>(4)?.unwrap_or_default();
            let dlq = row.get::<bool>(5)?.unwrap_or(true);
            let stats = row.get::<bool>(6)?.unwrap_or(true);
            let created = row.get::<TimestampWithTimeZone>(7)?;
            out.push((iname, ischema, retries, sched, dlq, stats, created));
        }
        Ok(out)
    })
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    Ok(rows)
}

// ── INBOX-6: replay_inbox_messages ────────────────────────────────────────

/// INBOX-6 (v0.28.0): Reset the retry count and processed_at for the given
/// message IDs, allowing them to be reprocessed.
///
/// Returns the number of messages reset.
#[pg_extern(schema = "pgtrickle")]
pub fn replay_inbox_messages(p_name: &str, p_event_ids: Vec<String>) -> i64 {
    replay_inbox_messages_impl(p_name, p_event_ids).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn replay_inbox_messages_impl(name: &str, event_ids: Vec<String>) -> Result<i64, PgTrickleError> {
    let cfg = get_inbox_config(name)?;

    if event_ids.is_empty() {
        return Ok(0);
    }

    // Build a literal ARRAY[...] for safe inline substitution.
    // We control the quoting to prevent SQL injection via explicit single-quote escaping.
    let quoted: Vec<String> = event_ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect();
    let arr_literal = format!("ARRAY[{}]::text[]", quoted.join(", "));

    let count_sql = format!(
        r#"WITH upd AS (
           UPDATE "{}"."{}"
           SET {} = NULL, {} = 0, {} = NULL
           WHERE {} = ANY({})
           RETURNING 1
        ) SELECT COUNT(*) FROM upd"#,
        cfg.inbox_schema,
        cfg.inbox_name,
        cfg.processed_at_column,
        cfg.retry_count_column,
        cfg.error_column,
        cfg.id_column,
        arr_literal
    );

    let count: i64 = Spi::get_one::<i64>(&count_sql).unwrap_or(None).unwrap_or(0);

    pgrx::log!(
        "[pg_trickle] INBOX-6: {} message(s) queued for replay in inbox '{}'",
        count,
        name
    );

    Ok(count)
}

// ── INBOX-B1: enable_inbox_ordering ──────────────────────────────────────

/// INBOX-B1 (v0.28.0): Enable per-aggregate ordering for an inbox.
///
/// Creates a `next_<inbox>` stream table using DISTINCT ON to surface only
/// the next unprocessed message per aggregate, ordered by the sequence column.
#[pg_extern(schema = "pgtrickle")]
pub fn enable_inbox_ordering(p_inbox: &str, p_aggregate_id_col: &str, p_sequence_num_col: &str) {
    enable_inbox_ordering_impl(p_inbox, p_aggregate_id_col, p_sequence_num_col)
        .unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn enable_inbox_ordering_impl(
    inbox: &str,
    aggregate_id_col: &str,
    sequence_num_col: &str,
) -> Result<(), PgTrickleError> {
    let cfg = get_inbox_config(inbox)?;

    // Check no priority config exists (conflict)
    let has_priority = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_priority_config WHERE inbox_name = $1)",
        &[inbox.into()],
    )
    .unwrap_or(None)
    .unwrap_or(false);

    if has_priority {
        return Err(PgTrickleError::InboxOrderingPriorityConflict(
            inbox.to_string(),
        ));
    }

    // Validate columns exist in the inbox table
    for col in [aggregate_id_col, sequence_num_col] {
        if !column_exists(&cfg.inbox_schema, &cfg.inbox_name, col) {
            return Err(PgTrickleError::InboxColumnMissing(
                cfg.inbox_name.clone(),
                col.to_string(),
            ));
        }
    }

    // Expand * to explicit column names to avoid the DVM rewriter producing
    // `__pgt_do.*` (an invalid column reference) from DISTINCT ON queries.
    let select_list = Spi::get_one_with_args::<String>(
        "SELECT string_agg(quote_ident(attname), ', ' ORDER BY attnum) \
         FROM pg_attribute \
         WHERE attrelid = ($1 || '.' || $2)::regclass \
           AND attnum > 0 \
           AND NOT attisdropped",
        &[
            cfg.inbox_schema.as_str().into(),
            cfg.inbox_name.as_str().into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("get inbox columns failed: {e}")))?
    .unwrap_or_else(|| "*".to_string());

    // Create the `next_<inbox>` stream table
    let next_st = format!("next_{}", inbox);
    let next_query = format!(
        r#"SELECT DISTINCT ON ({agg}) {select_list} FROM "{}"."{}"
           WHERE {} IS NULL AND {} < {}
           ORDER BY {agg}, {} ASC"#,
        cfg.inbox_schema,
        cfg.inbox_name,
        cfg.processed_at_column,
        cfg.retry_count_column,
        cfg.max_retries,
        sequence_num_col,
        agg = aggregate_id_col,
        select_list = select_list
    );
    create_inbox_stream_table(
        &cfg.inbox_schema,
        &next_st,
        &next_query,
        &cfg.schedule,
        Some("FULL"),
    )?;

    // Register ordering config
    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_inbox_ordering_config \
         (inbox_name, aggregate_id_col, sequence_num_col) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (inbox_name) DO UPDATE \
           SET aggregate_id_col = EXCLUDED.aggregate_id_col, \
               sequence_num_col = EXCLUDED.sequence_num_col",
        &[
            inbox.into(),
            aggregate_id_col.into(),
            sequence_num_col.into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("register ordering config failed: {e}")))?;

    pgrx::log!(
        "[pg_trickle] INBOX-B1: ordering enabled for inbox '{}' on aggregate_id='{}', seq='{}'",
        inbox,
        aggregate_id_col,
        sequence_num_col
    );

    Ok(())
}

/// INBOX-B1 (v0.28.0): Disable per-aggregate ordering for an inbox.
#[pg_extern(schema = "pgtrickle")]
pub fn disable_inbox_ordering(p_inbox: &str, p_if_exists: default!(bool, false)) {
    disable_inbox_ordering_impl(p_inbox, p_if_exists).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn disable_inbox_ordering_impl(inbox: &str, if_exists: bool) -> Result<(), PgTrickleError> {
    let cfg = get_inbox_config(inbox)?;

    let deleted = Spi::get_one_with_args::<i64>(
        "WITH d AS (DELETE FROM pgtrickle.pgt_inbox_ordering_config \
         WHERE inbox_name = $1 RETURNING 1) SELECT COUNT(*) FROM d",
        &[inbox.into()],
    )
    .unwrap_or(None)
    .unwrap_or(0);

    if deleted == 0 && !if_exists {
        return Err(PgTrickleError::InboxNotFound(format!(
            "ordering not enabled for inbox '{inbox}'"
        )));
    }

    // Drop the next_<inbox> stream table if it exists
    let next_st = format!("next_{}", inbox);
    let next_st_exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables WHERE pgt_name = $1)",
        &[next_st.as_str().into()],
    )
    .unwrap_or(None)
    .unwrap_or(false);
    if next_st_exists {
        let next_fqn = format!("{}.next_{}", cfg.inbox_schema, inbox);
        let _ = Spi::run_with_args(
            "SELECT pgtrickle.drop_stream_table($1)",
            &[next_fqn.as_str().into()],
        );
    }

    Ok(())
}

// ── INBOX-B2: enable_inbox_priority ──────────────────────────────────────

/// INBOX-B2 (v0.28.0): Enable priority-tier processing for an inbox.
///
/// Registers a priority column and optional tier configuration. A separate
/// stream table is not created for priority — the `<inbox>_pending` ST is
/// refreshed with an ORDER BY on the priority column.
#[pg_extern(schema = "pgtrickle")]
pub fn enable_inbox_priority(
    p_inbox: &str,
    p_priority_col: &str,
    p_tiers: default!(Option<pgrx::JsonB>, "NULL"),
) {
    enable_inbox_priority_impl(p_inbox, p_priority_col, p_tiers)
        .unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn enable_inbox_priority_impl(
    inbox: &str,
    priority_col: &str,
    tiers: Option<pgrx::JsonB>,
) -> Result<(), PgTrickleError> {
    let cfg = get_inbox_config(inbox)?;

    // Check no ordering config exists (conflict)
    let has_ordering = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_inbox_ordering_config WHERE inbox_name = $1)",
        &[inbox.into()],
    )
    .unwrap_or(None)
    .unwrap_or(false);

    if has_ordering {
        return Err(PgTrickleError::InboxOrderingPriorityConflict(
            inbox.to_string(),
        ));
    }

    if !column_exists(&cfg.inbox_schema, &cfg.inbox_name, priority_col) {
        return Err(PgTrickleError::InboxColumnMissing(
            cfg.inbox_name.clone(),
            priority_col.to_string(),
        ));
    }

    let tiers_json = tiers.map(|t| t.0.to_string());

    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_inbox_priority_config \
         (inbox_name, priority_col, tiers) \
         VALUES ($1, $2, $3::jsonb) \
         ON CONFLICT (inbox_name) DO UPDATE \
           SET priority_col = EXCLUDED.priority_col, \
               tiers = EXCLUDED.tiers",
        &[
            inbox.into(),
            priority_col.into(),
            tiers_json.as_deref().unwrap_or("null").into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(format!("register priority config failed: {e}")))?;

    pgrx::log!(
        "[pg_trickle] INBOX-B2: priority enabled for inbox '{}' on column '{}'",
        inbox,
        priority_col
    );

    Ok(())
}

/// INBOX-B2 (v0.28.0): Disable priority-tier processing for an inbox.
#[pg_extern(schema = "pgtrickle")]
pub fn disable_inbox_priority(p_inbox: &str, p_if_exists: default!(bool, false)) {
    disable_inbox_priority_impl(p_inbox, p_if_exists).unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn disable_inbox_priority_impl(inbox: &str, if_exists: bool) -> Result<(), PgTrickleError> {
    if !inbox_exists(inbox) {
        if if_exists {
            return Ok(());
        }
        return Err(PgTrickleError::InboxNotFound(inbox.to_string()));
    }

    let deleted = Spi::get_one_with_args::<i64>(
        "WITH d AS (DELETE FROM pgtrickle.pgt_inbox_priority_config \
         WHERE inbox_name = $1 RETURNING 1) SELECT COUNT(*) FROM d",
        &[inbox.into()],
    )
    .unwrap_or(None)
    .unwrap_or(0);

    if deleted == 0 && !if_exists {
        return Err(PgTrickleError::InboxNotFound(format!(
            "priority not enabled for inbox '{inbox}'"
        )));
    }

    Ok(())
}

// ── INBOX-B3: inbox_ordering_gaps ────────────────────────────────────────

/// INBOX-B3 (v0.28.0): Return sequence gaps for each aggregate in an ordered inbox.
///
/// Returns `(aggregate_id, expected_seq, found_seq)` for rows where the
/// sequence number is not contiguous.
#[pg_extern(schema = "pgtrickle")]
pub fn inbox_ordering_gaps(
    p_inbox_name: &str,
) -> TableIterator<
    'static,
    (
        name!(aggregate_id, String),
        name!(expected_seq, i64),
        name!(found_seq, i64),
    ),
> {
    match inbox_ordering_gaps_impl(p_inbox_name) {
        Ok(rows) => TableIterator::new(rows),
        Err(e) => pgrx::error!("{}", e),
    }
}

fn inbox_ordering_gaps_impl(inbox_name: &str) -> Result<Vec<(String, i64, i64)>, PgTrickleError> {
    let cfg = get_inbox_config(inbox_name)?;

    let ord_cfg = Spi::connect(|client| {
        let result = client.select(
            "SELECT aggregate_id_col, sequence_num_col \
             FROM pgtrickle.pgt_inbox_ordering_config WHERE inbox_name = $1",
            None,
            &[inbox_name.into()],
        )?;
        if let Some(row) = result.into_iter().next() {
            let agg = row.get::<String>(1)?.unwrap_or_default();
            let seq = row.get::<String>(2)?.unwrap_or_default();
            return Ok(Some((agg, seq)));
        }
        Ok(None)
    })
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    let (agg_col, seq_col) = match ord_cfg {
        Some(c) => c,
        None => {
            return Err(PgTrickleError::InboxNotFound(format!(
                "ordering not enabled for inbox '{inbox_name}'"
            )));
        }
    };

    let gaps_sql = format!(
        r#"SELECT {agg}::text, expected_seq, found_seq
           FROM (
               SELECT {agg}::text AS {agg},
                      LAG({seq}) OVER (PARTITION BY {agg} ORDER BY {seq}) + 1 AS expected_seq,
                      {seq} AS found_seq
               FROM "{}"."{}"
               WHERE {processed} IS NULL
           ) _gaps
           WHERE expected_seq < found_seq"#,
        cfg.inbox_schema,
        cfg.inbox_name,
        agg = agg_col,
        seq = seq_col,
        processed = cfg.processed_at_column
    );

    let rows = Spi::connect(|client| {
        let result = client.select(&gaps_sql, None, &[])?;
        let mut out = Vec::new();
        for row in result {
            let agg = row.get::<String>(1)?.unwrap_or_default();
            let expected = row.get::<i64>(2)?.unwrap_or(0);
            let found = row.get::<i64>(3)?.unwrap_or(0);
            out.push((agg, expected, found));
        }
        Ok(out)
    })
    .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

    Ok(rows)
}

// ── INBOX-B4: inbox_is_my_partition ──────────────────────────────────────

/// INBOX-B4 (v0.28.0): Consistent-hash partition check for horizontal scaling.
///
/// Returns true when `aggregate_id` belongs to `worker_id`'s partition out of
/// `total_workers`.  Workers can filter `<inbox>_pending` using this function
/// to distribute load without coordination.
#[pg_extern(schema = "pgtrickle", immutable, strict)]
pub fn inbox_is_my_partition(p_aggregate_id: &str, p_worker_id: i32, p_total_workers: i32) -> bool {
    if p_total_workers <= 0 {
        return true; // degenerate case: all messages belong to worker 0
    }
    // Use a simple but stable hash (FNV-1a)
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in p_aggregate_id.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    (hash % p_total_workers as u64) == p_worker_id as u64
}

// ── A06 (v0.35.0): Unit tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── inbox_is_my_partition ─────────────────────────────────────────────

    #[test]
    fn test_is_my_partition_degenerate_zero_workers() {
        // With total_workers == 0 every aggregate_id belongs to "all" workers.
        assert!(inbox_is_my_partition("any-id", 0, 0));
        assert!(inbox_is_my_partition("other", 99, 0));
    }

    #[test]
    fn test_is_my_partition_single_worker() {
        // With 1 worker, worker 0 owns all messages.
        assert!(inbox_is_my_partition("order-1", 0, 1));
        assert!(inbox_is_my_partition("user-42", 0, 1));
    }

    #[test]
    fn test_is_my_partition_two_workers_exclusive() {
        // Each message belongs to exactly one of the two workers.
        let ids = ["a", "b", "c", "d", "e", "hello", "world"];
        for id in ids {
            let w0 = inbox_is_my_partition(id, 0, 2);
            let w1 = inbox_is_my_partition(id, 1, 2);
            assert!(w0 ^ w1, "id '{}' should belong to exactly one worker", id);
        }
    }

    #[test]
    fn test_is_my_partition_stability() {
        // Same input must always map to the same worker (hash is deterministic).
        for _ in 0..10 {
            assert_eq!(
                inbox_is_my_partition("stable-id-42", 0, 4),
                inbox_is_my_partition("stable-id-42", 0, 4)
            );
        }
    }

    #[test]
    fn test_is_my_partition_consistent_hashing() {
        // With 4 workers, the four calls with worker_id 0..3 sum to exactly 1 true.
        let id = "partition-test-id";
        let owned: Vec<bool> = (0..4).map(|w| inbox_is_my_partition(id, w, 4)).collect();
        assert_eq!(
            owned.iter().filter(|&&v| v).count(),
            1,
            "exactly one worker should own the id, got {:?}",
            owned
        );
    }

    #[test]
    fn test_is_my_partition_empty_id() {
        // Empty aggregate_id should not panic and should assign to a deterministic worker.
        let w0 = inbox_is_my_partition("", 0, 3);
        let w1 = inbox_is_my_partition("", 1, 3);
        let w2 = inbox_is_my_partition("", 2, 3);
        assert_eq!(
            [w0, w1, w2].iter().filter(|&&v| v).count(),
            1,
            "empty id should still map to exactly one worker"
        );
    }
}
