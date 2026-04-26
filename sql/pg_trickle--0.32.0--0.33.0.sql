-- pg_trickle 0.32.0 → 0.33.0 upgrade migration
-- ============================================
--
-- v0.33.0 — CITUS distributed stream table support
--
-- Changes in this version:
--   - CITUS-7: Add `output_distribution_column TEXT DEFAULT NULL` parameter to
--              pgtrickle.create_stream_table(), create_stream_table_if_not_exists(),
--              and create_or_replace_stream_table().  When the parameter is non-NULL
--              and Citus is loaded, the output storage table is converted to a
--              Citus distributed table on that column immediately after creation.
--              Pass 's' to co-locate stream table output with pg_ripple VP shards.
--   - CIT-1:  Distributed stream tables now use DELETE+INSERT instead of MERGE
--             during differential refresh (Citus blocks cross-shard MERGE).
--   - CIT-2:  New pgtrickle.pgt_st_locks table for cross-node refresh coordination.
--   - CIT-3:  New pgtrickle.pgt_worker_slots table for per-worker WAL slot tracking.
--   - CIT-4:  New pgtrickle.citus_status view for per-worker CDC observability.
--
-- Migration is safe to run on a live system.

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 1: Cross-node advisory lock table
-- ─────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_st_locks (
    lock_key    TEXT        NOT NULL,
    holder      TEXT        NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    CONSTRAINT pgt_st_locks_pkey PRIMARY KEY (lock_key)
);
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_st_locks', '');

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 2: Per-worker WAL slot tracking table
-- ─────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_worker_slots (
    pgt_id       BIGINT NOT NULL
                 REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    source_relid OID    NOT NULL,
    worker_name  TEXT   NOT NULL,
    worker_port  INT    NOT NULL DEFAULT 5432,
    slot_name    TEXT   NOT NULL,
    last_frontier TEXT,
    CONSTRAINT pgt_worker_slots_pkey
        PRIMARY KEY (pgt_id, source_relid, worker_name, worker_port)
);
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_worker_slots', '');

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 3: Citus status observability view
-- ─────────────────────────────────────────────────────────────────────────

CREATE OR REPLACE VIEW pgtrickle.citus_status AS
SELECT
    st.pgt_id,
    st.pgt_schema,
    st.pgt_name,
    ct.source_relid,
    ct.source_stable_name,
    ct.slot_name           AS coordinator_slot,
    ct.source_placement,
    ct.frontier_per_node,
    ws.worker_name,
    ws.worker_port,
    ws.slot_name           AS worker_slot,
    ws.last_frontier       AS worker_frontier
FROM pgtrickle.pgt_change_tracking ct
JOIN pgtrickle.pgt_stream_tables   st ON st.pgt_id = ANY(ct.tracked_by_pgt_ids)
LEFT JOIN pgtrickle.pgt_worker_slots ws
       ON ws.pgt_id       = st.pgt_id
      AND ws.source_relid = ct.source_relid
WHERE ct.source_placement = 'distributed';

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 4: Replace create_stream_table with new signature (output_distribution_column)
-- Drop the old signature first — PostgreSQL cannot replace a function whose
-- argument list has changed, so CREATE OR REPLACE would silently create a
-- second overload rather than replacing the original.
-- ─────────────────────────────────────────────────────────────────────────

DROP FUNCTION IF EXISTS pgtrickle."create_stream_table"(
        TEXT, TEXT,
        TEXT, TEXT,
        bool, TEXT, TEXT, TEXT, bool, bool, TEXT, INT, double precision
);

CREATE OR REPLACE FUNCTION pgtrickle."create_stream_table"(
        "name" TEXT,
        "query" TEXT,
        "schedule" TEXT DEFAULT 'calculated',
        "refresh_mode" TEXT DEFAULT 'AUTO',
        "initialize" bool DEFAULT true,
        "diamond_consistency" TEXT DEFAULT NULL,
        "diamond_schedule_policy" TEXT DEFAULT NULL,
        "cdc_mode" TEXT DEFAULT NULL,
        "append_only" bool DEFAULT false,
        "pooler_compatibility_mode" bool DEFAULT false,
        "partition_by" TEXT DEFAULT NULL,
        "max_differential_joins" INT DEFAULT NULL,
        "max_delta_fraction" double precision DEFAULT NULL,
        "output_distribution_column" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_wrapper';

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 5: Replace create_stream_table_if_not_exists with new signature
-- Drop old signature first (see STEP 4 comment).
-- ─────────────────────────────────────────────────────────────────────────

DROP FUNCTION IF EXISTS pgtrickle."create_stream_table_if_not_exists"(
        TEXT, TEXT,
        TEXT, TEXT,
        bool, TEXT, TEXT, TEXT, bool, bool, TEXT, INT, double precision
);

CREATE OR REPLACE FUNCTION pgtrickle."create_stream_table_if_not_exists"(
        "name" TEXT,
        "query" TEXT,
        "schedule" TEXT DEFAULT 'calculated',
        "refresh_mode" TEXT DEFAULT 'AUTO',
        "initialize" bool DEFAULT true,
        "diamond_consistency" TEXT DEFAULT NULL,
        "diamond_schedule_policy" TEXT DEFAULT NULL,
        "cdc_mode" TEXT DEFAULT NULL,
        "append_only" bool DEFAULT false,
        "pooler_compatibility_mode" bool DEFAULT false,
        "partition_by" TEXT DEFAULT NULL,
        "max_differential_joins" INT DEFAULT NULL,
        "max_delta_fraction" double precision DEFAULT NULL,
        "output_distribution_column" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_stream_table_if_not_exists_wrapper';

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 6: Replace create_or_replace_stream_table with new signature
-- Drop old signature first (see STEP 4 comment).
-- ─────────────────────────────────────────────────────────────────────────

DROP FUNCTION IF EXISTS pgtrickle."create_or_replace_stream_table"(
        TEXT, TEXT,
        TEXT, TEXT,
        bool, TEXT, TEXT, TEXT, bool, bool, TEXT, INT, double precision
);

CREATE OR REPLACE FUNCTION pgtrickle."create_or_replace_stream_table"(
        "name" TEXT,
        "query" TEXT,
        "schedule" TEXT DEFAULT 'calculated',
        "refresh_mode" TEXT DEFAULT 'AUTO',
        "initialize" bool DEFAULT true,
        "diamond_consistency" TEXT DEFAULT NULL,
        "diamond_schedule_policy" TEXT DEFAULT NULL,
        "cdc_mode" TEXT DEFAULT NULL,
        "append_only" bool DEFAULT false,
        "pooler_compatibility_mode" bool DEFAULT false,
        "partition_by" TEXT DEFAULT NULL,
        "max_differential_joins" INT DEFAULT NULL,
        "max_delta_fraction" double precision DEFAULT NULL,
        "output_distribution_column" TEXT DEFAULT NULL
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_or_replace_stream_table_wrapper';

-- ─────────────────────────────────────────────────────────────────────────
-- New function: pgtrickle.handle_vp_promoted
-- ─────────────────────────────────────────────────────────────────────────
-- src/citus.rs:752
-- pg_trickle::citus::handle_vp_promoted
CREATE OR REPLACE FUNCTION pgtrickle."handle_vp_promoted"(
    "payload" TEXT /* &str */
) RETURNS bool /* bool */
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'sql_handle_vp_promoted_wrapper';
