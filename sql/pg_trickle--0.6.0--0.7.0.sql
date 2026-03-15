-- pg_trickle 0.6.0 -> 0.7.0 upgrade script
--
-- v0.7.0 adds:
--   CYC-5/6/7: last_fixpoint_iterations column, pgt_scc_status(), and
--     updated pgt_status() (adds scc_id column to return type)
--   Watermark gating: pgt_watermarks, pgt_watermark_groups catalog tables
--     and SQL functions for cross-source temporal alignment

-- ── CYC-5: Track the number of fixpoint iterations ─────────────────────────
-- New column on pgt_stream_tables for SCC convergence tracking.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS last_fixpoint_iterations INT;

-- ── CYC-7: pgt_status() — updated return type includes scc_id ─────────────
-- Replace the 0.6.0 definition (which lacked scc_id in its return set).
DROP FUNCTION IF EXISTS pgtrickle."pgt_status"();
CREATE FUNCTION pgtrickle."pgt_status"()
RETURNS TABLE (
    "name"               TEXT,
    "status"             TEXT,
    "refresh_mode"       TEXT,
    "is_populated"       bool,
    "consecutive_errors" INT,
    "schedule"           TEXT,
    "data_timestamp"     TIMESTAMPTZ,
    "staleness"          INTERVAL,
    "scc_id"             INT
)
LANGUAGE c
AS 'MODULE_PATHNAME', 'pgt_status_wrapper';

-- ── CYC-7: pgt_scc_status() — new function ────────────────────────────────
-- Shows status of all cyclic strongly connected components.
CREATE FUNCTION pgtrickle."pgt_scc_status"()
RETURNS TABLE (
    "scc_id"            INT,
    "member_count"      INT,
    "members"           TEXT[],
    "last_iterations"   INT,
    "last_converged_at" TIMESTAMPTZ
)
LANGUAGE c
AS 'MODULE_PATHNAME', 'pgt_scc_status_wrapper';

-- ── Watermark gating: catalog tables ──────────────────────────────────────
-- Per-source watermark state: tracks how far each external source has been loaded.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_watermarks (
    source_relid       OID PRIMARY KEY,
    watermark          TIMESTAMPTZ NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    advanced_by        TEXT,
    wal_lsn_at_advance TEXT
);

-- Watermark groups: declare that a set of sources must be temporally aligned.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_watermark_groups (
    group_id           SERIAL PRIMARY KEY,
    group_name         TEXT UNIQUE NOT NULL,
    source_relids      OID[] NOT NULL,
    tolerance_secs     DOUBLE PRECISION NOT NULL DEFAULT 0,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Watermark gating: SQL functions ───────────────────────────────────────

-- advance_watermark(source, watermark): signal that a source's data is
-- complete through the given timestamp. Monotonic + idempotent.
CREATE FUNCTION pgtrickle."advance_watermark"(
    "source"    TEXT,
    "watermark" TIMESTAMPTZ
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'advance_watermark_wrapper';

-- create_watermark_group(name, sources, tolerance_secs): declare that a set
-- of sources must be aligned within tolerance_secs before downstream STs refresh.
CREATE FUNCTION pgtrickle."create_watermark_group"(
    "group_name"     TEXT,
    "sources"        TEXT[],
    "tolerance_secs" float8 DEFAULT 0
) RETURNS INT
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_watermark_group_wrapper';

-- drop_watermark_group(name): remove a watermark group.
CREATE FUNCTION pgtrickle."drop_watermark_group"(
    "group_name" TEXT
) RETURNS void
LANGUAGE c
AS 'MODULE_PATHNAME', 'drop_watermark_group_wrapper';

-- watermarks(): current watermark state for all registered sources.
CREATE FUNCTION pgtrickle."watermarks"()
RETURNS TABLE (
    "source_table" TEXT,
    "schema_name"  TEXT,
    "watermark"    TIMESTAMPTZ,
    "updated_at"   TIMESTAMPTZ,
    "advanced_by"  TEXT,
    "wal_lsn"      TEXT
)
LANGUAGE c
AS 'MODULE_PATHNAME', 'watermarks_wrapper';

-- watermark_groups(): all watermark group definitions.
CREATE FUNCTION pgtrickle."watermark_groups"()
RETURNS TABLE (
    "group_name"     TEXT,
    "source_count"   INT,
    "tolerance_secs" float8,
    "created_at"     TIMESTAMPTZ
)
LANGUAGE c
AS 'MODULE_PATHNAME', 'watermark_groups_wrapper';

-- watermark_status(): live alignment status per group.
CREATE FUNCTION pgtrickle."watermark_status"()
RETURNS TABLE (
    "group_name"            TEXT,
    "min_watermark"         TIMESTAMPTZ,
    "max_watermark"         TIMESTAMPTZ,
    "lag_secs"              float8,
    "aligned"               bool,
    "sources_with_watermark" INT,
    "sources_total"          INT
)
LANGUAGE c
AS 'MODULE_PATHNAME', 'watermark_status_wrapper';
