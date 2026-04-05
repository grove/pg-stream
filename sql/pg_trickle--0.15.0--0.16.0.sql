-- pg_trickle 0.15.0 -> 0.16.0 upgrade script
--
-- v0.16.0: Performance & Refresh Optimization

-- BUF-LIMIT: max_buffer_rows GUC
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   Hard limit on change buffer rows per source table (default: 1,000,000).
--   Forces FULL refresh + truncate when exceeded.

-- AUTO-IDX: auto_index GUC
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   Controls automatic index creation on GROUP BY/DISTINCT columns and
--   covering __pgt_row_id index (default: true).

-- PH-D1: merge_strategy + merge_strategy_threshold GUCs
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   merge_strategy: 'auto' (default), 'merge', 'delete_insert'
--   merge_strategy_threshold: 0.01 (default) — delta ratio threshold for auto mode

-- A-3-AO: Append-only heuristic auto-promotion
--   No schema changes required. The heuristic automatically promotes
--   stream tables to append-only mode when change buffers contain only
--   INSERT actions. Uses the existing is_append_only catalog column.

-- G14-SHC: Shared template cache — catalog-backed cross-backend cache
--   Eliminates cold-start latency when new backends connect (e.g., PgBouncer
--   transaction mode). Templates are persisted in an UNLOGGED table and
--   loaded on first cache miss (~1ms vs ~45ms for full re-parse).
CREATE UNLOGGED TABLE IF NOT EXISTS pgtrickle.pgt_template_cache (
    pgt_id       BIGINT PRIMARY KEY
                 REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    query_hash   BIGINT NOT NULL,
    delta_sql    TEXT NOT NULL,
    columns      TEXT[] NOT NULL,
    source_oids  INTEGER[] NOT NULL,
    is_dedup     BOOLEAN NOT NULL DEFAULT FALSE,
    key_changed  BOOLEAN NOT NULL DEFAULT FALSE,
    all_algebraic BOOLEAN NOT NULL DEFAULT FALSE,
    cached_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
