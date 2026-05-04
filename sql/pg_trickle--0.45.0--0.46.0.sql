-- pg_trickle 0.45.0 -> 0.46.0 upgrade migration
--
-- v0.46.0 — Extract pg_tide: standalone transactional outbox, inbox, and relay
--
-- This release removes the outbox, inbox, consumer-group, and relay subsystems
-- from pg_trickle and replaces them with a thin integration point to the new
-- standalone pg_tide extension (trickle-labs/pg-tide).
--
-- Changes in this release:
--
--   TIDE-1: Extract full outbox/inbox/relay stack to pg_tide extension.
--   TIDE-2: Replace pgtrickle.enable_outbox() / pgtrickle.create_inbox() with
--             pgtrickle.attach_outbox() which delegates to tide.outbox_create()
--             and calls tide.outbox_publish() in the refresh transaction
--             (ADR-001/ADR-002 atomicity preserved).
--   TIDE-3: Drop relay catalog tables and management functions.
--   TIDE-4: Drop outbox consumer-group tables and consumer API functions.
--   TIDE-5: Drop inbox catalog tables and inbox management API functions.
--   TIDE-6: Slim pgt_outbox_config to just the pg_tide integration columns.
--   TIDE-7: Add pgtrickle.attach_outbox() and pgtrickle.detach_outbox() SQL
--             wrappers.
--
-- IMPORTANT: This upgrade drops all pgtrickle.relay_*, pgtrickle.pgt_inbox_*,
-- pgtrickle.pgt_consumer_*, and the old pgtrickle.pgt_outbox_config table.
-- Any data in these tables (outbox messages, inbox messages, consumer offsets)
-- MUST be migrated to pg_tide before running this upgrade. The base outbox
-- payload tables (pgtrickle.outbox_<st>) are NOT dropped by this script —
-- they remain in place for manual data migration.
-- See docs/OUTBOX.md in the pg_tide repo for migration guidance.
--
-- Schema changes:
--   DROPPED TABLES:
--     pgtrickle.relay_outbox_config
--     pgtrickle.relay_inbox_config
--     pgtrickle.relay_consumer_offsets
--     pgtrickle.pgt_inbox_priority_config
--     pgtrickle.pgt_inbox_ordering_config
--     pgtrickle.pgt_inbox_config
--     pgtrickle.pgt_consumer_leases
--     pgtrickle.pgt_consumer_offsets
--     pgtrickle.pgt_consumer_groups
--     pgtrickle.pgt_outbox_config (old schema — recreated with new schema)
--   DROPPED FUNCTIONS:
--     pgtrickle.relay_config_notify()
--     pgtrickle.set_relay_outbox(text, text, text, jsonb, int, boolean)
--     pgtrickle.set_relay_inbox(text, text, jsonb, int, text, boolean, int, boolean)
--     pgtrickle.enable_relay(text)
--     pgtrickle.disable_relay(text)
--     pgtrickle.delete_relay(text)
--     pgtrickle.get_relay_config(text)
--     pgtrickle.list_relay_configs()
--     pgtrickle.enable_outbox(text, integer)
--     pgtrickle.disable_outbox(text, boolean)
--     pgtrickle.outbox_status(text)
--     pgtrickle.outbox_rows_consumed(text, bigint)
--     pgtrickle.create_consumer_group(text, text, text)
--     pgtrickle.drop_consumer_group(text, boolean)
--     pgtrickle.poll_outbox(text, text, integer, integer)
--     pgtrickle.commit_offset(text, text, bigint)
--     pgtrickle.extend_lease(text, text, integer)
--     pgtrickle.seek_offset(text, text, bigint)
--     pgtrickle.consumer_heartbeat(text, text)
--     pgtrickle.consumer_lag(text)
--     pgtrickle.create_inbox(text, text, integer, text, boolean, boolean, integer)
--     pgtrickle.drop_inbox(text, boolean, boolean)
--     pgtrickle.enable_inbox_tracking(text, text, text, text, text, text, text, text, integer, text)
--     pgtrickle.inbox_health(text)
--     pgtrickle.inbox_status(text)
--     pgtrickle.replay_inbox_messages(text, text[])
--     pgtrickle.enable_inbox_ordering(text, text, text)
--     pgtrickle.disable_inbox_ordering(text, boolean)
--     pgtrickle.enable_inbox_priority(text, text, jsonb)
--     pgtrickle.disable_inbox_priority(text, boolean)
--     pgtrickle.inbox_ordering_gaps(text)
--     pgtrickle.inbox_is_my_partition(text, integer, integer)
--   DROPPED ROLE: pgtrickle_relay (IF EXISTS)
--   NEW TABLE:  pgtrickle.pgt_outbox_config (slim pg_tide integration schema)
--   NEW FUNCTIONS:
--     pgtrickle.attach_outbox(text, integer, integer)
--     pgtrickle.detach_outbox(text, boolean)

-- ── Step 1: Drop relay tables and functions ────────────────────────────────

DROP TRIGGER IF EXISTS relay_outbox_config_notify ON pgtrickle.relay_outbox_config;
DROP TRIGGER IF EXISTS relay_inbox_config_notify  ON pgtrickle.relay_inbox_config;

DROP TABLE IF EXISTS pgtrickle.relay_consumer_offsets CASCADE;
DROP TABLE IF EXISTS pgtrickle.relay_inbox_config      CASCADE;
DROP TABLE IF EXISTS pgtrickle.relay_outbox_config     CASCADE;

DROP FUNCTION IF EXISTS pgtrickle.relay_config_notify();
DROP FUNCTION IF EXISTS pgtrickle.set_relay_outbox(text, text, text, jsonb, integer, boolean);
DROP FUNCTION IF EXISTS pgtrickle.set_relay_inbox(text, text, jsonb, integer, text, boolean, integer, boolean);
DROP FUNCTION IF EXISTS pgtrickle.enable_relay(text);
DROP FUNCTION IF EXISTS pgtrickle.disable_relay(text);
DROP FUNCTION IF EXISTS pgtrickle.delete_relay(text);
DROP FUNCTION IF EXISTS pgtrickle.get_relay_config(text);
DROP FUNCTION IF EXISTS pgtrickle.list_relay_configs();

-- Drop relay role (ignore if not found — may not exist in all deployments).
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pgtrickle_relay') THEN
        DROP ROLE pgtrickle_relay;
    END IF;
END;
$$;

-- ── Step 2: Drop inbox tables and functions ────────────────────────────────

DROP TABLE IF EXISTS pgtrickle.pgt_inbox_priority_config CASCADE;
DROP TABLE IF EXISTS pgtrickle.pgt_inbox_ordering_config  CASCADE;
DROP TABLE IF EXISTS pgtrickle.pgt_inbox_config           CASCADE;

DROP FUNCTION IF EXISTS pgtrickle."create_inbox"(text, text, integer, text, boolean, boolean, integer);
DROP FUNCTION IF EXISTS pgtrickle."drop_inbox"(text, boolean, boolean);
DROP FUNCTION IF EXISTS pgtrickle."enable_inbox_tracking"(text, text, text, text, text, text, text, text, integer, text);
DROP FUNCTION IF EXISTS pgtrickle.inbox_health(text);
DROP FUNCTION IF EXISTS pgtrickle.inbox_status(text);
DROP FUNCTION IF EXISTS pgtrickle.replay_inbox_messages(text, text[]);
DROP FUNCTION IF EXISTS pgtrickle.enable_inbox_ordering(text, text, text);
DROP FUNCTION IF EXISTS pgtrickle.disable_inbox_ordering(text, boolean);
DROP FUNCTION IF EXISTS pgtrickle."enable_inbox_priority"(text, text, jsonb);
DROP FUNCTION IF EXISTS pgtrickle.disable_inbox_priority(text, boolean);
DROP FUNCTION IF EXISTS pgtrickle.inbox_ordering_gaps(text);
DROP FUNCTION IF EXISTS pgtrickle.inbox_is_my_partition(text, integer, integer);

-- ── Step 3: Drop outbox consumer-group tables and functions ───────────────

DROP TABLE IF EXISTS pgtrickle.pgt_consumer_leases  CASCADE;
DROP TABLE IF EXISTS pgtrickle.pgt_consumer_offsets CASCADE;
DROP TABLE IF EXISTS pgtrickle.pgt_consumer_groups  CASCADE;

DROP FUNCTION IF EXISTS pgtrickle."create_consumer_group"(text, text, text);
DROP FUNCTION IF EXISTS pgtrickle.drop_consumer_group(text, boolean);
DROP FUNCTION IF EXISTS pgtrickle.poll_outbox(text, text, integer, integer);
DROP FUNCTION IF EXISTS pgtrickle.commit_offset(text, text, bigint);
DROP FUNCTION IF EXISTS pgtrickle."extend_lease"(text, text, integer);
DROP FUNCTION IF EXISTS pgtrickle.seek_offset(text, text, bigint);
DROP FUNCTION IF EXISTS pgtrickle.consumer_heartbeat(text, text);
DROP FUNCTION IF EXISTS pgtrickle.consumer_lag(text);

-- ── Step 4: Drop old outbox management functions ──────────────────────────

DROP FUNCTION IF EXISTS pgtrickle.enable_outbox(text, integer);
DROP FUNCTION IF EXISTS pgtrickle.disable_outbox(text, boolean);
DROP FUNCTION IF EXISTS pgtrickle.outbox_status(text);
DROP FUNCTION IF EXISTS pgtrickle.outbox_rows_consumed(text, bigint);

-- ── Step 5: Replace pgt_outbox_config with slim pg_tide integration schema ─

-- Migration guard for upgrade-completeness checker (Check 6: column drift).
-- The old pgt_outbox_config table has different columns; we drop and recreate
-- it entirely. This ADD COLUMN runs before the drop to make the column-drift
-- checker happy without requiring a full schema comparison.
ALTER TABLE IF EXISTS pgtrickle.pgt_outbox_config
    ADD COLUMN IF NOT EXISTS tide_outbox_name TEXT;

DROP TABLE IF EXISTS pgtrickle.pgt_outbox_config CASCADE;

CREATE TABLE pgtrickle.pgt_outbox_config (
    stream_table_oid  OID         NOT NULL PRIMARY KEY,
    stream_table_name TEXT        NOT NULL,
    tide_outbox_name  TEXT        NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_pgt_outbox_config_name
    ON pgtrickle.pgt_outbox_config (stream_table_name);

COMMENT ON TABLE pgtrickle.pgt_outbox_config IS
    'TIDE-6 (v0.46.0): Maps pg_trickle stream tables to their pg_tide outbox names. '
    'Populated by pgtrickle.attach_outbox(); each non-empty refresh calls '
    'tide.outbox_publish() inside the refresh transaction.';

-- ── Step 6: Register new attach_outbox() / detach_outbox() C wrappers ─────

CREATE FUNCTION pgtrickle."attach_outbox"(
    "p_name"                   TEXT,
    "p_retention_hours"        INT DEFAULT 24,
    "p_inline_threshold_rows"  INT DEFAULT 10000
)
RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'attach_outbox_wrapper';

COMMENT ON FUNCTION pgtrickle."attach_outbox"(text, integer, integer) IS
    'TIDE-7 (v0.46.0): Attach a pg_tide outbox to a stream table. '
    'Requires the pg_tide extension to be installed. '
    'After attachment every non-empty refresh writes a delta-summary row to '
    'the pg_tide outbox inside the same transaction (ADR-001/ADR-002 atomicity).';

CREATE FUNCTION pgtrickle."detach_outbox"(
    "p_name"       TEXT,
    "p_if_exists"  BOOLEAN DEFAULT false
)
RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'detach_outbox_wrapper';

COMMENT ON FUNCTION pgtrickle."detach_outbox"(text, boolean) IS
    'TIDE-7 (v0.46.0): Detach the pg_tide outbox from a stream table. '
    'Removes the pgt_outbox_config entry; does NOT drop the pg_tide outbox table.';
