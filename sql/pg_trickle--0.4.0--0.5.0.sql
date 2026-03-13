-- pg_trickle 0.4.0 -> 0.5.0 upgrade script
--
-- v0.5.0 adds:
--   Phase 1: Row-Level Security (RLS) passthrough for stream tables.
--   Phase 2: RLS-aware refresh executor.
--   Phase 3 (Bootstrap Source Gating): gate_source() / ungate_source() /
--            source_gates() + scheduler skip logic.
--
-- New catalog table: pgtrickle.pgt_source_gates
-- Tracks which source tables are currently "gated" (bootstrapping in progress).
-- When a source is gated the scheduler skips all stream tables that depend on
-- it, logging SKIP+SKIPPED in pgt_refresh_history, until ungate_source() is
-- called.

-- Bootstrap source gates (Phase 3)
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_source_gates (
    source_relid    OID PRIMARY KEY,
    gated           BOOLEAN NOT NULL DEFAULT true,
    gated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    ungated_at      TIMESTAMPTZ,
    gated_by        TEXT
);
