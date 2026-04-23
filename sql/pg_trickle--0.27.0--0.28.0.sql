-- pg_trickle 0.27.0 → 0.28.0 upgrade migration
-- ============================================
--
-- v0.28.0 — Transactional Inbox & Outbox Patterns
--
-- OUTBOX-1:   enable_outbox(name, retention_hours) — enable outbox pattern for a stream table
-- OUTBOX-2:   disable_outbox(name, if_exists) — disable outbox pattern
-- OUTBOX-3:   outbox_status(name) — JSONB summary of outbox state
-- OUTBOX-6:   outbox_rows_consumed(stream_table, outbox_id) — mark rows consumed
-- OUTBOX-B1:  create_consumer_group(name, outbox, auto_offset_reset) — new consumer group
-- OUTBOX-B2:  drop_consumer_group(name, if_exists) — drop a consumer group
-- OUTBOX-B3:  poll_outbox(group, consumer, batch_size, visibility_seconds) — poll messages
-- OUTBOX-B4:  commit_offset(group, consumer, last_offset) — commit consumed offset
-- OUTBOX-B4:  extend_lease(group, consumer, extension_seconds) — extend visibility lease
-- OUTBOX-B4:  seek_offset(group, consumer, new_offset) — seek to arbitrary offset
-- OUTBOX-B5:  consumer_heartbeat(group, consumer) — signal consumer liveness
-- OUTBOX-B6:  consumer_lag(group) — per-consumer lag metrics
--
-- INBOX-1:    create_inbox(name, ...) — create a named inbox with stream tables
-- INBOX-2:    drop_inbox(name, if_exists, cascade) — drop a named inbox
-- INBOX-3:    enable_inbox_tracking(name, table_ref, ...) — BYOT inbox tracking
-- INBOX-4:    inbox_health(name) — JSONB health summary
-- INBOX-5:    inbox_status(name) — table summary of one or all inboxes
-- INBOX-6:    replay_inbox_messages(name, event_ids) — reset messages for replay
-- INBOX-B1:   enable_inbox_ordering(inbox, aggregate_id_col, sequence_num_col)
-- INBOX-B1:   disable_inbox_ordering(inbox, if_exists)
-- INBOX-B2:   enable_inbox_priority(inbox, priority_col, tiers)
-- INBOX-B2:   disable_inbox_priority(inbox, if_exists)
-- INBOX-B3:   inbox_ordering_gaps(inbox_name) — detect sequence gaps
-- INBOX-B4:   inbox_is_my_partition(aggregate_id, worker_id, total_workers) — partition check
--
-- New GUCs (registered in config.rs; no SQL DDL needed):
--   pg_trickle.outbox_enabled (default true)
--   pg_trickle.outbox_retention_hours (default 24)
--   pg_trickle.outbox_drain_batch_size (default 1000)
--   pg_trickle.outbox_inline_threshold_rows (default 10000)
--   pg_trickle.outbox_claim_check_batch_size (default 1000)
--   pg_trickle.outbox_drain_interval_seconds (default 60)
--   pg_trickle.outbox_storage_critical_mb (default 1024)
--   pg_trickle.outbox_skip_empty_delta (default true)
--   pg_trickle.consumer_dead_threshold_hours (default 24)
--   pg_trickle.consumer_stale_offset_threshold_days (default 7)
--   pg_trickle.consumer_cleanup_enabled (default true)
--   pg_trickle.outbox_force_retention (default false)
--   pg_trickle.inbox_enabled (default true)
--   pg_trickle.inbox_processed_retention_hours (default 72)
--   pg_trickle.inbox_dlq_retention_hours (default 0)
--   pg_trickle.inbox_drain_batch_size (default 1000)
--   pg_trickle.inbox_drain_interval_seconds (default 60)
--   pg_trickle.inbox_dlq_alert_max_per_refresh (default 10)

-- ── Outbox catalog tables ─────────────────────────────────────────────────

-- OUTBOX-1 (v0.28.0): Per-stream-table outbox configuration.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_outbox_config (
    stream_table_oid   OID         NOT NULL PRIMARY KEY,
    stream_table_name  TEXT        NOT NULL,
    outbox_table_name  TEXT        NOT NULL,
    retention_hours    INT         NOT NULL DEFAULT 24,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_drained_at    TIMESTAMPTZ,
    last_drained_count BIGINT      NOT NULL DEFAULT 0,
    CONSTRAINT uq_outbox_table_name UNIQUE (outbox_table_name)
);

CREATE INDEX IF NOT EXISTS idx_pgt_outbox_config_name
    ON pgtrickle.pgt_outbox_config (stream_table_name);

COMMENT ON TABLE pgtrickle.pgt_outbox_config IS
    'OUTBOX-1 (v0.28.0): Catalog of stream tables with the transactional outbox pattern enabled.';

-- OUTBOX-B1 (v0.28.0): Consumer groups for the outbox pattern.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_consumer_groups (
    group_name         TEXT        NOT NULL PRIMARY KEY,
    outbox_name        TEXT        NOT NULL,
    auto_offset_reset  TEXT        NOT NULL DEFAULT 'latest',
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_consumer_group_auto_offset_reset
        CHECK (auto_offset_reset IN ('earliest', 'latest'))
);

CREATE INDEX IF NOT EXISTS idx_pgt_consumer_groups_outbox
    ON pgtrickle.pgt_consumer_groups (outbox_name);

COMMENT ON TABLE pgtrickle.pgt_consumer_groups IS
    'OUTBOX-B1 (v0.28.0): Named consumer groups that track consumption progress on an outbox.';

-- OUTBOX-B2 (v0.28.0): Per-consumer committed offsets within a group.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_consumer_offsets (
    group_name        TEXT        NOT NULL
                      REFERENCES pgtrickle.pgt_consumer_groups(group_name) ON DELETE CASCADE,
    consumer_id       TEXT        NOT NULL,
    committed_offset  BIGINT      NOT NULL DEFAULT 0,
    last_committed_at TIMESTAMPTZ,
    last_heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_name, consumer_id)
);

COMMENT ON TABLE pgtrickle.pgt_consumer_offsets IS
    'OUTBOX-B2 (v0.28.0): Per-consumer committed offsets and heartbeat tracking.';

-- OUTBOX-B3 (v0.28.0): Visibility leases granted by poll_outbox().
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_consumer_leases (
    group_name     TEXT        NOT NULL,
    consumer_id    TEXT        NOT NULL,
    batch_start    BIGINT      NOT NULL,
    batch_end      BIGINT      NOT NULL,
    lease_expires  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (group_name, consumer_id),
    FOREIGN KEY (group_name, consumer_id)
        REFERENCES pgtrickle.pgt_consumer_offsets(group_name, consumer_id) ON DELETE CASCADE
);

COMMENT ON TABLE pgtrickle.pgt_consumer_leases IS
    'OUTBOX-B3 (v0.28.0): Visibility leases for in-flight outbox message batches.';

-- ── Inbox catalog tables ──────────────────────────────────────────────────

-- INBOX-1 (v0.28.0): Named inbox configurations.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_inbox_config (
    inbox_name             TEXT        NOT NULL PRIMARY KEY,
    inbox_schema           TEXT        NOT NULL DEFAULT 'pgtrickle',
    max_retries            INT         NOT NULL DEFAULT 3,
    schedule               TEXT        NOT NULL DEFAULT '1s',
    with_dead_letter       BOOL        NOT NULL DEFAULT true,
    with_stats             BOOL        NOT NULL DEFAULT true,
    retention_hours        INT         NOT NULL DEFAULT 72,
    id_column              TEXT        NOT NULL DEFAULT 'event_id',
    processed_at_column    TEXT        NOT NULL DEFAULT 'processed_at',
    retry_count_column     TEXT        NOT NULL DEFAULT 'retry_count',
    error_column           TEXT        NOT NULL DEFAULT 'error',
    received_at_column     TEXT        NOT NULL DEFAULT 'received_at',
    event_type_column      TEXT        NOT NULL DEFAULT 'event_type',
    is_managed             BOOL        NOT NULL DEFAULT true,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE pgtrickle.pgt_inbox_config IS
    'INBOX-1 (v0.28.0): Catalog of named transactional inbox configurations.';

-- INBOX-B1 (v0.28.0): Per-inbox ordering configuration.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_inbox_ordering_config (
    inbox_name        TEXT NOT NULL PRIMARY KEY
                      REFERENCES pgtrickle.pgt_inbox_config(inbox_name) ON DELETE CASCADE,
    aggregate_id_col  TEXT NOT NULL,
    sequence_num_col  TEXT NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE pgtrickle.pgt_inbox_ordering_config IS
    'INBOX-B1 (v0.28.0): Ordering configuration for per-aggregate sequenced inbox processing.';

-- INBOX-B2 (v0.28.0): Per-inbox priority tiers configuration.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_inbox_priority_config (
    inbox_name    TEXT NOT NULL PRIMARY KEY
                  REFERENCES pgtrickle.pgt_inbox_config(inbox_name) ON DELETE CASCADE,
    priority_col  TEXT NOT NULL,
    tiers         JSONB,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE pgtrickle.pgt_inbox_priority_config IS
    'INBOX-B2 (v0.28.0): Priority tier configuration for inbox message processing.';

-- ── OUTBOX-1: enable_outbox ───────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."enable_outbox"(
    p_name            text,
    p_retention_hours integer DEFAULT 24
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'enable_outbox_wrapper';

COMMENT ON FUNCTION pgtrickle.enable_outbox(text, integer) IS
'OUTBOX-1 (v0.28.0): Enable the transactional outbox pattern for a stream table.
Creates the pgtrickle.outbox_<st> table, a claim-check delta table, and a
pgtrickle.pgt_outbox_latest_<st> view. Registers the stream table in
pgtrickle.pgt_outbox_config.';

-- ── OUTBOX-2: disable_outbox ──────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."disable_outbox"(
    p_name       text,
    p_if_exists  boolean DEFAULT false
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'disable_outbox_wrapper';

COMMENT ON FUNCTION pgtrickle.disable_outbox(text, boolean) IS
'OUTBOX-2 (v0.28.0): Disable the transactional outbox pattern for a stream table.
Drops the outbox table, delta table, and latest view. Removes the catalog entry.';

-- ── OUTBOX-3: outbox_status ───────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."outbox_status"(
    p_name  text
) RETURNS jsonb
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'outbox_status_wrapper';

COMMENT ON FUNCTION pgtrickle.outbox_status(text) IS
'OUTBOX-3 (v0.28.0): Return a JSONB summary of the outbox state for a stream table.
Includes: outbox_table_name, row_count, oldest_row, newest_row, retention_hours,
last_drained_at, last_drained_count.';

-- ── OUTBOX-6: outbox_rows_consumed ────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."outbox_rows_consumed"(
    p_stream_table  text,
    p_outbox_id     bigint
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'outbox_rows_consumed_wrapper';

COMMENT ON FUNCTION pgtrickle.outbox_rows_consumed(text, bigint) IS
'OUTBOX-6 (v0.28.0): Mark outbox rows up to p_outbox_id as consumed.
Updates last_drained_at and last_drained_count in the catalog and deletes old
claim-check delta rows to free storage.';

-- ── OUTBOX-B1: create_consumer_group ─────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."create_consumer_group"(
    p_name              text,
    p_outbox            text,
    p_auto_offset_reset text DEFAULT 'latest'
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'create_consumer_group_wrapper';

COMMENT ON FUNCTION pgtrickle.create_consumer_group(text, text, text) IS
'OUTBOX-B1 (v0.28.0): Create a named consumer group for an outbox.
auto_offset_reset must be ''earliest'' or ''latest''.';

-- ── OUTBOX-B2: drop_consumer_group ───────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."drop_consumer_group"(
    p_name       text,
    p_if_exists  boolean DEFAULT false
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'drop_consumer_group_wrapper';

COMMENT ON FUNCTION pgtrickle.drop_consumer_group(text, boolean) IS
'OUTBOX-B2 (v0.28.0): Drop a consumer group and all associated offsets/leases.';

-- ── OUTBOX-B3: poll_outbox ────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."poll_outbox"(
    p_group              text,
    p_consumer           text,
    p_batch_size         integer DEFAULT 100,
    p_visibility_seconds integer DEFAULT 30
) RETURNS TABLE (
    outbox_id       bigint,
    pgt_id          uuid,
    created_at      timestamptz,
    inserted_count  bigint,
    deleted_count   bigint,
    is_claim_check  boolean,
    payload         jsonb
)
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'poll_outbox_wrapper';

COMMENT ON FUNCTION pgtrickle.poll_outbox(text, text, integer, integer) IS
'OUTBOX-B3 (v0.28.0): Poll the outbox for new messages as a consumer.
Returns up to p_batch_size outbox rows. Grants a visibility lease for
p_visibility_seconds seconds.';

-- ── OUTBOX-B4: commit_offset ──────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."commit_offset"(
    p_group        text,
    p_consumer     text,
    p_last_offset  bigint
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'commit_offset_wrapper';

COMMENT ON FUNCTION pgtrickle.commit_offset(text, text, bigint) IS
'OUTBOX-B4 (v0.28.0): Commit the consumed offset for a consumer in a group.
Clears the visibility lease.';

-- ── OUTBOX-B4: extend_lease ───────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."extend_lease"(
    p_group              text,
    p_consumer           text,
    p_extension_seconds  integer DEFAULT 30
) RETURNS timestamptz
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'extend_lease_wrapper';

COMMENT ON FUNCTION pgtrickle.extend_lease(text, text, integer) IS
'OUTBOX-B4 (v0.28.0): Extend the visibility lease for an in-flight batch.
Returns the new lease expiry timestamp, or NULL if no active lease.';

-- ── OUTBOX-B4: seek_offset ────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."seek_offset"(
    p_group       text,
    p_consumer    text,
    p_new_offset  bigint
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'seek_offset_wrapper';

COMMENT ON FUNCTION pgtrickle.seek_offset(text, text, bigint) IS
'OUTBOX-B4 (v0.28.0): Seek a consumer to a specific offset (for replay/reset).';

-- ── OUTBOX-B5: consumer_heartbeat ────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."consumer_heartbeat"(
    p_group     text,
    p_consumer  text
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'consumer_heartbeat_wrapper';

COMMENT ON FUNCTION pgtrickle.consumer_heartbeat(text, text) IS
'OUTBOX-B5 (v0.28.0): Send a heartbeat from a consumer to signal liveness.';

-- ── OUTBOX-B6: consumer_lag ───────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."consumer_lag"(
    p_group  text
) RETURNS TABLE (
    consumer_id       text,
    committed_offset  bigint,
    max_outbox_id     bigint,
    lag               bigint,
    last_heartbeat_at timestamptz,
    is_alive          boolean
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'consumer_lag_wrapper';

COMMENT ON FUNCTION pgtrickle.consumer_lag(text) IS
'OUTBOX-B6 (v0.28.0): Return per-consumer lag metrics for a consumer group.';

-- ── INBOX-1: create_inbox ─────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."create_inbox"(
    p_name             text,
    p_schema           text    DEFAULT 'pgtrickle',
    p_max_retries      integer DEFAULT 3,
    p_schedule         text    DEFAULT '1s',
    with_dead_letter   boolean DEFAULT true,
    with_stats         boolean DEFAULT true,
    p_retention_hours  integer DEFAULT 72
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'create_inbox_wrapper';

COMMENT ON FUNCTION pgtrickle.create_inbox(text, text, integer, text, boolean, boolean, integer) IS
'INBOX-1 (v0.28.0): Create a named transactional inbox with managed stream tables.
Creates <name>_pending, optionally <name>_dlq and <name>_stats stream tables.';

-- ── INBOX-2: drop_inbox ───────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."drop_inbox"(
    p_name       text,
    p_if_exists  boolean DEFAULT false,
    p_cascade    boolean DEFAULT false
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'drop_inbox_wrapper';

COMMENT ON FUNCTION pgtrickle.drop_inbox(text, boolean, boolean) IS
'INBOX-2 (v0.28.0): Drop a named inbox and its associated stream tables.
If p_cascade is true, also drops the underlying inbox table.';

-- ── INBOX-3: enable_inbox_tracking ───────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."enable_inbox_tracking"(
    p_name                 text,
    p_table_ref            text,
    p_id_column            text    DEFAULT 'event_id',
    p_processed_at_column  text    DEFAULT 'processed_at',
    p_retry_count_column   text    DEFAULT 'retry_count',
    p_error_column         text    DEFAULT 'error',
    p_received_at_column   text    DEFAULT 'received_at',
    p_event_type_column    text    DEFAULT 'event_type',
    p_max_retries          integer DEFAULT 3,
    p_schedule             text    DEFAULT '1s'
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'enable_inbox_tracking_wrapper';

COMMENT ON FUNCTION pgtrickle.enable_inbox_tracking(text, text, text, text, text, text, text, text, integer, text) IS
'INBOX-3 (v0.28.0): Bring-your-own-table inbox tracking mode.
Attaches pg_trickle stream tables to an existing table as an inbox processor.';

-- ── INBOX-4: inbox_health ─────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."inbox_health"(
    p_name  text
) RETURNS jsonb
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'inbox_health_wrapper';

COMMENT ON FUNCTION pgtrickle.inbox_health(text) IS
'INBOX-4 (v0.28.0): Return a JSONB health summary for an inbox.
Includes total, pending, dlq, and processed message counts.';

-- ── INBOX-5: inbox_status ─────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."inbox_status"(
    p_name  text DEFAULT NULL
) RETURNS TABLE (
    inbox_name     text,
    inbox_schema   text,
    max_retries    integer,
    schedule       text,
    with_dead_letter boolean,
    with_stats     boolean,
    created_at     timestamptz
)
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'inbox_status_wrapper';

COMMENT ON FUNCTION pgtrickle.inbox_status(text) IS
'INBOX-5 (v0.28.0): Return a table row summary for one or all inboxes.
If p_name is NULL, returns all inboxes.';

-- ── INBOX-6: replay_inbox_messages ────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."replay_inbox_messages"(
    p_name       text,
    p_event_ids  text[]
) RETURNS bigint
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'replay_inbox_messages_wrapper';

COMMENT ON FUNCTION pgtrickle.replay_inbox_messages(text, text[]) IS
'INBOX-6 (v0.28.0): Reset message state to re-queue them for processing.
Clears processed_at, retry_count, and error for the specified event IDs.';

-- ── INBOX-B1: enable_inbox_ordering ──────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."enable_inbox_ordering"(
    p_inbox               text,
    p_aggregate_id_col    text,
    p_sequence_num_col    text
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'enable_inbox_ordering_wrapper';

COMMENT ON FUNCTION pgtrickle.enable_inbox_ordering(text, text, text) IS
'INBOX-B1 (v0.28.0): Enable per-aggregate ordering for an inbox.
Creates a next_<inbox> stream table that surfaces only the next unprocessed
message per aggregate, ordered by the sequence column.';

-- ── INBOX-B1: disable_inbox_ordering ─────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."disable_inbox_ordering"(
    p_inbox      text,
    p_if_exists  boolean DEFAULT false
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'disable_inbox_ordering_wrapper';

COMMENT ON FUNCTION pgtrickle.disable_inbox_ordering(text, boolean) IS
'INBOX-B1 (v0.28.0): Disable per-aggregate ordering for an inbox.
Drops the next_<inbox> stream table and removes the ordering config.';

-- ── INBOX-B2: enable_inbox_priority ──────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."enable_inbox_priority"(
    p_inbox        text,
    p_priority_col text,
    p_tiers        jsonb DEFAULT NULL
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'enable_inbox_priority_wrapper';

COMMENT ON FUNCTION pgtrickle.enable_inbox_priority(text, text, jsonb) IS
'INBOX-B2 (v0.28.0): Enable priority-tier processing for an inbox.
Registers a priority column and optional tier configuration.';

-- ── INBOX-B2: disable_inbox_priority ─────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."disable_inbox_priority"(
    p_inbox      text,
    p_if_exists  boolean DEFAULT false
) RETURNS void
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'disable_inbox_priority_wrapper';

COMMENT ON FUNCTION pgtrickle.disable_inbox_priority(text, boolean) IS
'INBOX-B2 (v0.28.0): Disable priority-tier processing for an inbox.';

-- ── INBOX-B3: inbox_ordering_gaps ────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."inbox_ordering_gaps"(
    p_inbox_name  text
) RETURNS TABLE (
    aggregate_id  text,
    expected_seq  bigint,
    found_seq     bigint
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'inbox_ordering_gaps_wrapper';

COMMENT ON FUNCTION pgtrickle.inbox_ordering_gaps(text) IS
'INBOX-B3 (v0.28.0): Return sequence gaps for each aggregate in an ordered inbox.
Returns (aggregate_id, expected_seq, found_seq) for non-contiguous sequence numbers.';

-- ── INBOX-B4: inbox_is_my_partition ──────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."inbox_is_my_partition"(
    p_aggregate_id   text,
    p_worker_id      integer,
    p_total_workers  integer
) RETURNS boolean
IMMUTABLE STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'inbox_is_my_partition_wrapper';

COMMENT ON FUNCTION pgtrickle.inbox_is_my_partition(text, integer, integer) IS
'INBOX-B4 (v0.28.0): Consistent-hash partition check for horizontal inbox scaling.
Returns true when aggregate_id belongs to worker_id''s partition out of total_workers.
Uses FNV-1a hash for stable, coordination-free distribution.';
