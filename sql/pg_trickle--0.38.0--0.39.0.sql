-- pg_trickle 0.38.0 -> 0.39.0 upgrade migration
--
-- v0.39.0 — Operational Truthfulness & Distributed Hardening
--
-- Changes in this release:
--
--   O39-1/O39-8: cdc_capture_mode GUC (discard|hold) — controls what happens
--     to captured changes while CDC is paused.  Loaded at PostgreSQL startup;
--     no catalog object required.
--
--   O39-9: explain_stream_table() — enhanced output includes CDC status,
--     capture mode, backpressure state, and refresh mode reasoning.
--     Pure Rust function, no SQL-level signature change.
--
--   O39-6: SQLSTATE-first SPI retry classification — controlled by GUC
--     pg_trickle.use_sqlstate_classification (default off). No SQL change.
--
--   O39-2: Wake truthfulness — scheduler no longer attempts LISTEN/NOTIFY
--     in background worker contexts; falls back to polling as documented.
--     No SQL change.
--
-- cdc_pause_status() — new SQL function returning per-inbox CDC pause state.

-- ── cdc_pause_status() ─────────────────────────────────────────────────────
-- Returns a summary of CDC pause status and capture mode for all stream tables.
-- Added in v0.39.0 (O39-1/O39-8).

CREATE OR REPLACE FUNCTION pgtrickle."cdc_pause_status"() RETURNS TABLE (
    "paused" bool,
    "capture_mode" TEXT,
    "note" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'cdc_pause_status_wrapper';
