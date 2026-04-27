-- pg_trickle 0.34.0 → 0.35.0 upgrade migration
-- All DDL is idempotent (IF NOT EXISTS / IF EXISTS / ADD COLUMN IF NOT EXISTS).

-- ── A11: History table start_time index ─────────────────────────────────────
-- Speeds up SLA summary queries and history retention pruning which filter on
-- start_time.
CREATE INDEX IF NOT EXISTS pgt_refresh_history_start_time_idx
    ON pgtrickle.pgt_refresh_history (start_time DESC);

-- ── UX-SUB: Reactive subscriptions catalog table ─────────────────────────────
-- Stores (stream_table, channel) pairs that the background worker will notify
-- after every non-empty refresh cycle.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_subscriptions (
    stream_table TEXT NOT NULL,
    channel      TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (stream_table, channel)
);

COMMENT ON TABLE pgtrickle.pgt_subscriptions IS
    'v0.35.0 UX-SUB: NOTIFY channel subscriptions per stream table. '
    'Populated via pgtrickle.subscribe() / pgtrickle.unsubscribe().';

-- ── UX-SHADOW: Shadow-ST zero-downtime evolution columns ─────────────────────
-- Added to pgt_stream_tables so the scheduler can track in-progress shadow
-- builds without extra catalog tables.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS in_shadow_build  BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS shadow_table_name TEXT;

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.in_shadow_build IS
    'v0.35.0 UX-SHADOW: TRUE while a zero-downtime shadow build is in progress.';
COMMENT ON COLUMN pgtrickle.pgt_stream_tables.shadow_table_name IS
    'v0.35.0 UX-SHADOW: Name of the shadow table currently being built '
    '(NULL when in_shadow_build is FALSE).';
