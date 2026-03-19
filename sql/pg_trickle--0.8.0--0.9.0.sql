-- pg_trickle 0.8.0 -> 0.9.0 upgrade script
--
-- v0.9.0 adds incremental aggregate maintenance via algebraic decomposition.
--
-- New auxiliary columns (__pgt_aux_sum_*, __pgt_aux_count_*, __pgt_aux_sum2_*)
-- are managed dynamically per stream table, so no global ALTER TABLE is needed.
-- However, existing stream tables using AVG/STDDEV/VAR aggregates must be
-- reinitialized after upgrade so that auxiliary columns are added and the
-- algebraic differential path activates.
--
-- The extension's next refresh of affected stream tables will automatically
-- detect missing auxiliary columns and perform a full reinitialize.

-- Cross-Source Snapshot Consistency: User-declared groups
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_refresh_groups (
    group_id    SERIAL PRIMARY KEY,
    group_name  TEXT NOT NULL UNIQUE,
    member_oids OID[] NOT NULL,
    isolation   TEXT NOT NULL DEFAULT 'read_committed'
                CHECK (isolation IN ('read_committed', 'repeatable_read')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- New API functions
CREATE OR REPLACE FUNCTION pgtrickle."restore_stream_tables"() RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'restore_stream_tables_wrapper';

-- Refresh group management API (A8)
CREATE OR REPLACE FUNCTION pgtrickle."create_refresh_group"(
    "group_name" TEXT,
    "members" TEXT[],
    "isolation" TEXT DEFAULT 'read_committed'
) RETURNS INT
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'create_refresh_group_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."drop_refresh_group"(
    "group_name" TEXT
) RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'drop_refresh_group_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle."refresh_groups"()
RETURNS TABLE (
    "group_id" INT,
    "group_name" TEXT,
    "member_count" INT,
    "isolation" TEXT,
    "created_at" TIMESTAMPTZ
)
LANGUAGE c
AS 'MODULE_PATHNAME', 'refresh_groups_fn_wrapper';