-- pg_trickle 0.10.0 -> 0.11.0 upgrade script
--
-- Changes:
--   G12-ERM-1: Add effective_refresh_mode column to pgt_stream_tables.
--              Populated by the scheduler after each completed refresh to
--              record the mode that was actually used (FULL, DIFFERENTIAL,
--              APPEND_ONLY, TOP_K, NO_DATA).

-- ── Schema Changes ─────────────────────────────────────────────────────────

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS effective_refresh_mode TEXT;
