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
--   - pgtrickle.repair_stream_table(text) → text (added via pg_extern)
--     No SQL-level DDL needed; pgrx registers this on CREATE/ALTER EXTENSION.

SELECT 1;
