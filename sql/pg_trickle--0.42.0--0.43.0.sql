-- pg_trickle 0.42.0 -> 0.43.0 upgrade migration
--
-- v0.43.0 — D+I Change-Buffer Schema, GUC Tuning, WAL Diagnostics
--
-- Changes in this release:
--
--   A44-1:  Deep-join threshold GUCs (part3_max_scan_count,
--             deep_join_l0_scan_threshold).
--   A44-2:  GROUP_RESCAN improvement — SUM(CASE …) non-invertible
--             aggregates now produce correct incremental results.
--   A44-3:  WAL poll GUCs (wal_max_changes_per_poll, wal_max_lag_bytes).
--   A44-4:  Cost-cache GUC (cost_cache_capacity).
--   A44-8:  explain_stream_table() extended with GUC threshold section.
--   A44-9:  wal_source_status() — new pg_extern returning per-source
--             WAL CDC diagnostics.
--   A44-10: D+I change-buffer schema — flat column names, no new_/old_
--             prefix; UPDATEs decomposed at write time into a D-row +
--             I-row pair.  The wide-schema migration guard in
--             sync_change_buffer_columns() blocks unsafe migrations on
--             pre-existing buffers.
--
-- Schema changes:
--   + pgtrickle.wal_source_status() RETURNS TABLE (new pg_extern, A44-9)
--   ~ explain_stream_table() extended output (same signature, A44-8)
--
-- GUC additions (pg_trickle.*):
--   + part3_max_scan_count         (integer, default 10000)
--   + deep_join_l0_scan_threshold  (integer, default 256)
--   + wal_max_changes_per_poll     (integer, default 10000)
--   + wal_max_lag_bytes            (integer, default 104857600)
--   + cost_cache_capacity          (integer, default 4096)

-- A44-9: Register the new wal_source_status() function.
-- pgrx does not automatically add new functions during ALTER EXTENSION UPDATE,
-- so we must register it explicitly here.
CREATE OR REPLACE FUNCTION pgtrickle."wal_source_status"()
RETURNS TABLE(
    source_relid        BIGINT,
    source_name         TEXT,
    cdc_mode            TEXT,
    slot_name           TEXT,
    slot_lag_bytes      BIGINT,
    publication_name    TEXT,
    blocked_reason      TEXT,
    transition_started_at TEXT,
    decoder_confirmed_lsn TEXT
)
LANGUAGE c
AS 'MODULE_PATHNAME', 'wal_source_status_wrapper';
