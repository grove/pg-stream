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

-- ── v0.48.0 additions ──────────────────────────────────────────────────────
--
-- This release completes the AI/RAG feature set:
--
--   VH-1: halfvec / sparsevec aggregate type fix
--           avg(halfvec_col) output columns are now typed halfvec(N),
--           not vector(N).  Existing stream tables with halfvec/sparsevec
--           aggregates should be recreated for correct index support.
--   VH-2: Reactive distance-predicate subscriptions
--           pgtrickle.pgt_distance_subscriptions catalog table.
--           New functions: subscribe_distance(), unsubscribe_distance(),
--                          list_distance_subscriptions().
--   VA-1: embedding_stream_table() ergonomic one-call RAG corpus setup API.
--   VA-4: attach_embedding_outbox() — embedding-specific outbox events.
--           Adds embedding_vector_column column to pgt_outbox_config.
--
-- Schema changes:
--   NEW TABLE: pgtrickle.pgt_distance_subscriptions
--   ALTERED TABLE: pgtrickle.pgt_outbox_config
--     ADD COLUMN embedding_vector_column TEXT
--   NEW FUNCTIONS:
--     pgtrickle.subscribe_distance()
--     pgtrickle.unsubscribe_distance()
--     pgtrickle.list_distance_subscriptions()
--     pgtrickle.embedding_stream_table()
--     pgtrickle.attach_embedding_outbox()

-- ── Step 1: VH-2 — Create distance subscription catalog table ────────────

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_distance_subscriptions (
    stream_table    TEXT NOT NULL,
    channel         TEXT NOT NULL,
    vector_column   TEXT NOT NULL,
    query_vector    TEXT NOT NULL,
    op              TEXT NOT NULL
        CHECK (op IN ('<->', '<=>', '<#>', '<+>', '<<->>', '<<%>>')),
    threshold       DOUBLE PRECISION NOT NULL CHECK (threshold > 0),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (stream_table, channel)
);

COMMENT ON TABLE pgtrickle.pgt_distance_subscriptions IS
    'VH-2 (v0.48.0): Distance-predicate NOTIFY subscriptions per stream table. '
    'Populated via pgtrickle.subscribe_distance() / pgtrickle.unsubscribe_distance().';

-- ── Step 2: VA-4 — Add embedding_vector_column to pgt_outbox_config ──────

ALTER TABLE pgtrickle.pgt_outbox_config
    ADD COLUMN IF NOT EXISTS embedding_vector_column TEXT;

COMMENT ON COLUMN pgtrickle.pgt_outbox_config.embedding_vector_column IS
    'VA-4 (v0.48.0): When set, outbox events carry event_type=''embedding_change'' '
    'and this column name in the payload headers. Set via attach_embedding_outbox().';

-- ── Step 3: Register VH-2 functions ──────────────────────────────────────
-- The implementations live in the .so; these stubs delegate to the C wrappers.

CREATE FUNCTION pgtrickle."subscribe_distance"(
    "stream_table"   TEXT,
    "channel"        TEXT,
    "vector_column"  TEXT,
    "query_vector"   TEXT,
    "op"             TEXT,
    "threshold"      DOUBLE PRECISION
)
RETURNS VOID
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'subscribe_distance_wrapper';

COMMENT ON FUNCTION pgtrickle."subscribe_distance"(TEXT,TEXT,TEXT,TEXT,TEXT,DOUBLE PRECISION) IS
    'VH-2 (v0.48.0): Subscribe a NOTIFY channel to distance-predicate changes on a '
    'stream table.  After each non-empty refresh the background worker evaluates '
    'vector_column op query_vector < threshold and emits pg_notify(channel) when '
    'at least one row matches.  op must be one of: <->, <=>, <#>, <+>, <<->>, <<%>>.';

CREATE FUNCTION pgtrickle."unsubscribe_distance"(
    "stream_table"  TEXT,
    "channel"       TEXT
)
RETURNS VOID
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'unsubscribe_distance_wrapper';

COMMENT ON FUNCTION pgtrickle."unsubscribe_distance"(TEXT,TEXT) IS
    'VH-2 (v0.48.0): Remove a distance-predicate subscription.';

CREATE FUNCTION pgtrickle."list_distance_subscriptions"()
RETURNS TABLE(
    "stream_table"   TEXT,
    "channel"        TEXT,
    "vector_column"  TEXT,
    "op"             TEXT,
    "threshold"      DOUBLE PRECISION,
    "created_at"     TIMESTAMPTZ
)
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'list_distance_subscriptions_wrapper';

COMMENT ON FUNCTION pgtrickle."list_distance_subscriptions"() IS
    'VH-2 (v0.48.0): List all active distance-predicate subscriptions.';

-- ── Step 4: Register VA-1 embedding_stream_table() ───────────────────────

CREATE FUNCTION pgtrickle."embedding_stream_table"(
    "name"             TEXT,
    "source_table"     TEXT,
    "vector_column"    TEXT,
    "extra_columns"    TEXT    DEFAULT NULL,
    "refresh_interval" TEXT    DEFAULT '1m',
    "index_type"       TEXT    DEFAULT 'hnsw',
    "dry_run"          BOOLEAN DEFAULT FALSE
)
RETURNS TABLE("action" TEXT)
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'embedding_stream_table_wrapper';

COMMENT ON FUNCTION pgtrickle."embedding_stream_table"(TEXT,TEXT,TEXT,TEXT,TEXT,TEXT,BOOLEAN) IS
    'VA-1 (v0.48.0): One-call RAG corpus setup. Creates a stream table over '
    'source_table with an HNSW/IVFFlat index on the vector column and '
    'post_refresh_action=reindex_if_drift. Pass dry_run=>true to preview the '
    'generated SQL without executing it.';

-- ── Step 5: Register VA-4 attach_embedding_outbox() ──────────────────────

CREATE FUNCTION pgtrickle."attach_embedding_outbox"(
    "p_name"                     TEXT,
    "p_vector_column"            TEXT,
    "p_retention_hours"          INTEGER DEFAULT 24,
    "p_inline_threshold_rows"    INTEGER DEFAULT 10000
)
RETURNS VOID
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'attach_embedding_outbox_wrapper';

COMMENT ON FUNCTION pgtrickle."attach_embedding_outbox"(TEXT,TEXT,INTEGER,INTEGER) IS
    'VA-4 (v0.48.0): Attach a pg_tide outbox configured for embedding events. '
    'Each refresh delta is published with event_type=''embedding_change'' and the '
    'vector_column name in the outbox headers.';
