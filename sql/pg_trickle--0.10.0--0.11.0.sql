-- pg_trickle 0.10.0 -> 0.11.0 upgrade script
--
-- Changes:
--   G12-ERM-1: Add effective_refresh_mode column to pgt_stream_tables.
--              Populated by the scheduler after each completed refresh to
--              record the mode that was actually used (FULL, DIFFERENTIAL,
--              APPEND_ONLY, TOP_K, NO_DATA).
--
--   WB-1: changed_cols column in CDC change buffer tables migrated from
--         BIGINT to VARBIT. The migration is performed at runtime by the
--         extension when each change buffer is first accessed after upgrade
--         (via alter_change_buffer_add_columns / rebuild_change_trigger_with_columns).
--         No explicit ALTER TABLE is required here because change buffer
--         tables live in the pgtrickle_changes schema and are managed
--         per-source-table by the Rust code.

-- ── Schema Changes ─────────────────────────────────────────────────────────

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS effective_refresh_mode TEXT;
