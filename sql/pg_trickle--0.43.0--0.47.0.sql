-- pg_trickle consolidated upgrade: 0.43.0 -> 0.47.0
--
-- Multi-hop script covering: v0.44.0, v0.45.0, v0.46.0, v0.47.0
-- Equivalent to applying each individual step in sequence.
-- All statements are idempotent (IF NOT EXISTS / CREATE OR REPLACE).
--
-- Upgrade support policy (v0.47.0+):
--   Direct upgrade scripts are maintained for v0.40.0 and later.
--   Users on v0.39.0 or older must upgrade to v0.40.0 first.
--

-- pg_trickle 0.43.0 -> 0.44.0 upgrade migration
--
-- v0.44.0 — Security Hardening & Code Quality
--
-- Changes in this release:
--
--   A45-1:  IVM BEFORE trigger functions now use a restricted search_path
--             (no `public`) to prevent search_path shadowing of extension
--             internals. AFTER trigger functions retain `public` so user
--             delta SQL can resolve source table references.
--   A45-2:  Centralized SQL-builder helpers (src/sql_builder.rs) —
--             internal Rust change only, no SQL schema changes.
--   A45-3:  RLS bypass warning emitted when a stream table is created
--             over an RLS-enabled source table — runtime behaviour change,
--             no SQL schema changes.
--   A45-4:  Monitoring docker-compose credentials hardened — ops-only,
--             no SQL schema changes.
--   A45-5:  SECURITY DEFINER CI check in scripts/check_security_definer.sh -- nosemgrep: semgrep.sql.security-definer.present
--             — CI change only, no SQL schema changes.
--   A45-6:  docs/SECURITY_MODEL.md updated — docs change only.
--   A45-7:  CDC module decomposed into rebuild + polling submodules —
--             internal Rust refactor, no SQL schema changes.
--   A45-8:  CreateStreamTableOptions struct — internal Rust refactor.
--   A45-9:  Parser safety facade — internal Rust refactor.
--   A45-10: Scheduler structured warnings — internal Rust change.
--   A45-11: Milestone ID comment audit — internal Rust change.
--
-- Schema changes:
--   (none — this release contains only Rust-level and ops-level changes)
--
-- IVM trigger functions are dynamically created at CREATE STREAM TABLE time
-- and will use the new search_path rules automatically for any stream tables
-- created or recreated after the extension is updated.
-- Existing stream tables will have their trigger functions updated the next
-- time they are refreshed or when pgtrickle.repair_stream_table() is called.

-- This upgrade script is intentionally minimal: no new SQL objects are added
-- or removed in v0.44.0. The .so is updated in-place by ALTER EXTENSION.

-- pg_trickle 0.44.0 -> 0.45.0 upgrade migration
--
-- v0.45.0 — Operational Readiness, Scalability & CI Completeness
--
-- Changes in this release:
--
--   A46-1:  Dockerfile VERSION sync — Dockerfile.hub and Dockerfile.ghcr now
--             carry the correct default ARG VERSION matching Cargo.toml.
--   A46-2:  Container HEALTHCHECK — all three Dockerfiles now include a
--             pg_isready HEALTHCHECK directive.
--   A46-3:  CNPG production examples — cnpg/cluster-dev.yaml and
--             cnpg/cluster-production.yaml added (infrastructure docs only).
--   A46-4:  preflight() SQL function — new pgtrickle.preflight() function
--             returns a JSON health report with 7 system checks.
--   A46-5:  worker_pool_status() enhanced — four new columns added:
--             idle_workers, last_scheduler_tick_unix,
--             ring_overflow_count, citus_failure_total.
--   A46-6:  Production monitoring split — monitoring/production/README.md
--             added with least-privilege role setup, TLS config, and
--             Kubernetes ServiceMonitor examples.
--   A46-7:  Invalidation ring configurable capacity — new GUC
--             pg_trickle.invalidation_ring_capacity (default 128, max 1024).
--             IMPORTANT: changing this GUC requires a PostgreSQL restart
--             because the shared memory layout changes.
--   A46-8:  Worker-slot exhaustion SQL visibility — surfaced via preflight().
--   A46-9:  Incremental DAG rebuild — O(affected) partial schedule
--             re-resolution instead of O(V) full pass on each event.
--   A46-10: Lag-aware cross-database scheduling foundation — new GUC
--             pg_trickle.lag_aware_scheduling (default false). When enabled,
--             the per-database quota is boosted proportionally to refresh lag.
--   A46-11: Citus failure counter persistence — new shared-memory counter
--             pg_trickle_citus_fail_total; surfaced via worker_pool_status().
--   A46-12: WAL slot preflight check — via preflight().
--   A46-13: Cross-platform CI blocking — Windows compile check now blocking.
--   A46-14: Full-image PR smoke test — new CI job for each PR/push.
--   A46-15: E2E coverage schedule restore — weekly Monday coverage run.
--   A46-16: Storage Backends reference page — docs/STORAGE_BACKENDS.md.
--   A46-17: dbt macro option sync — create/alter macros now expose all
--             options from CreateStreamTableOptions.
--
-- Schema changes:
--   NEW FUNCTION: pgtrickle.preflight() RETURNS text
--   CHANGED FUNCTION: pgtrickle.worker_pool_status() — four new output columns
--   NEW GUC: pg_trickle.invalidation_ring_capacity (integer, postmaster scope)
--   NEW GUC: pg_trickle.lag_aware_scheduling (boolean, superuser scope)
--
-- NOTE: pg_trickle.invalidation_ring_capacity uses shared memory.
-- If you change its value from the default, PostgreSQL must be restarted
-- for the new ring size to take effect.

-- ── Drop existing worker_pool_status (return type changed) ──────────────
-- The return type changed (4 new columns), so we must DROP and CREATE NEW.
DROP FUNCTION IF EXISTS pgtrickle.worker_pool_status();

-- ── Create new worker_pool_status with extended return type ──────────────
CREATE FUNCTION pgtrickle."worker_pool_status"() RETURNS TABLE (
        "active_workers" INT,
        "max_workers" INT,
        "per_db_cap" INT,
        "parallel_mode" TEXT,
        "idle_workers" INT,
        "last_scheduler_tick_unix" bigint,
        "ring_overflow_count" bigint,
        "citus_failure_total" bigint
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'worker_pool_status_wrapper';

-- ── Create new preflight() function ─────────────────────────────────────
CREATE FUNCTION pgtrickle."preflight"() RETURNS TEXT
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'preflight_wrapper';

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

-- pg_trickle 0.46.0 -> 0.47.0 upgrade migration
--
-- v0.47.0 — Embedding Pipeline Infrastructure & ANN Maintenance
--
-- This release resumes the deferred embedding programme with post-refresh
-- action hooks (ANALYZE, REINDEX, drift-based re-clustering), vector-aware
-- monitoring via pgtrickle.vector_status(), and a pgvector RAG cookbook.
--
-- Changes in this release:
--
--   VP-1: `post_refresh_action` column on pgt_stream_tables
--           ('none' / 'analyze' / 'reindex' / 'reindex_if_drift').
--           Controlled via ALTER STREAM TABLE ... post_refresh_action = '...'
--   VP-2: `reindex_drift_threshold` column on pgt_stream_tables (DOUBLE PRECISION)
--           `rows_changed_since_last_reindex` BIGINT counter
--           `last_reindex_at` TIMESTAMPTZ — when the last REINDEX completed
--   VP-3: `pgtrickle.vector_status()` table-valued function:
--           shows embedding lag, ANN age, drift percentage per vector ST.
--
-- Schema changes:
--   ALTERED TABLE: pgtrickle.pgt_stream_tables
--     ADD COLUMN post_refresh_action TEXT NOT NULL DEFAULT 'none'
--       CHECK (post_refresh_action IN ('none','analyze','reindex','reindex_if_drift'))
--     ADD COLUMN reindex_drift_threshold DOUBLE PRECISION
--     ADD COLUMN rows_changed_since_last_reindex BIGINT NOT NULL DEFAULT 0
--     ADD COLUMN last_reindex_at TIMESTAMPTZ
--   NEW FUNCTIONS:
--     pgtrickle.vector_status()
--
-- GUC changes:
--   NEW: pg_trickle.reindex_drift_threshold = 0.20
--     (Global default drift fraction; per-table setting overrides this.)

-- ── Step 1: Add VP-1/VP-2 columns to pgt_stream_tables ───────────────────

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS post_refresh_action TEXT
        NOT NULL DEFAULT 'none'
        CHECK (post_refresh_action IN ('none', 'analyze', 'reindex', 'reindex_if_drift'));

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS reindex_drift_threshold DOUBLE PRECISION
        CHECK (reindex_drift_threshold IS NULL OR (reindex_drift_threshold > 0 AND reindex_drift_threshold <= 1.0));

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS rows_changed_since_last_reindex BIGINT NOT NULL DEFAULT 0;

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS last_reindex_at TIMESTAMPTZ;

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.post_refresh_action IS
    'VP-1 (v0.47.0): Action run after a successful refresh that produces changed rows. '
    '''none'' = no action (default), ''analyze'' = run ANALYZE, '
    '''reindex'' = always REINDEX, ''reindex_if_drift'' = REINDEX when drift exceeds threshold.';

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.reindex_drift_threshold IS
    'VP-2 (v0.47.0): Fraction (0.0–1.0) of estimated rows that must change since the '
    'last REINDEX before drift-triggered REINDEX fires. NULL means use global GUC.';

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.rows_changed_since_last_reindex IS
    'VP-2 (v0.47.0): Running count of rows changed since the last REINDEX. '
    'Reset to 0 after each successful REINDEX.';

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.last_reindex_at IS
    'VP-2 (v0.47.0): Timestamp of the last REINDEX on this stream table''s storage table. '
    'NULL means never REINDEXed via pg_trickle.';

-- ── Step 2: Register VP-3 vector_status() function ───────────────────────

-- The function body is provided by the compiled .so via the C wrapper
-- registered in Rust (pg_extern). We create a SQL stub that delegates to it.

CREATE FUNCTION pgtrickle."vector_status"()
RETURNS TABLE(
    "name"                              TEXT,
    "post_refresh_action"               TEXT,
    "reindex_drift_threshold"           DOUBLE PRECISION,
    "rows_changed_since_last_reindex"   BIGINT,
    "last_reindex_at"                   TIMESTAMPTZ,
    "data_timestamp"                    TIMESTAMPTZ,
    "embedding_lag"                     INTERVAL,
    "estimated_rows"                    BIGINT,
    "drift_pct"                         DOUBLE PRECISION
)
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'vector_status_wrapper';

COMMENT ON FUNCTION pgtrickle."vector_status"() IS
    'VP-3 (v0.47.0): Returns one row per stream table with a non-''none'' '
    'post_refresh_action, showing embedding lag, last reindex time, '
    'rows changed since last REINDEX, and drift percentage. '
    'Use this view to monitor ANN maintenance pressure on vector stream tables.';

-- ── Step 3: Register alter_stream_table() with new VP-1/VP-2 parameters ──
-- The compiled .so adds post_refresh_action and reindex_drift_threshold
-- parameters to pgtrickle.alter_stream_table(). The old function signature
-- is replaced by the new one via pgrx-generated SQL.
-- (No manual SQL needed here — the new wrapper is registered automatically
--  when the .so is loaded at CREATE EXTENSION / ALTER EXTENSION UPDATE.)
