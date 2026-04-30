-- pg_trickle 0.41.0 -> 0.42.0 upgrade migration
--
-- v0.42.0 — Repair API, Docs Overhaul & Test Infrastructure
--
-- Changes in this release:
--
--   A42-1: repair_stream_table() SQL function (new pg_extern).
--   A42-11: SUM(CASE) AST-level non-invertibility detection.
--   A42-13: WAL decoder SQL parameterization (security hardening).
--   A42-14: Stale EC-06 comment cleanup.
--
-- Schema changes:
--   - pgtrickle.repair_stream_table(text) → text (new pg_extern, A42-1)

-- A42-1: Register the new repair_stream_table function.
-- pgrx does not automatically add new functions during ALTER EXTENSION UPDATE,
-- so we must register it explicitly here.
CREATE OR REPLACE FUNCTION pgtrickle."repair_stream_table"(
        "name" TEXT
) RETURNS TEXT
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'repair_stream_table_wrapper';
