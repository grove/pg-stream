-- pg_trickle 0.47.0 -> 0.48.0 upgrade migration
--
-- v0.48.0 — Complete Embedding Programme
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
