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
