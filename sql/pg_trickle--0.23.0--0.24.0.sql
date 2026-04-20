-- pg_trickle 0.23.0 → 0.24.0 upgrade migration
-- ============================================
--
-- v0.24.0 — Join Correctness & Durability Hardening
--
-- EC01-1: Join row-id hash convergence (code-only, no schema change)
-- EC01-2: PH-D1 cross-cycle phantom cleanup (code-only)
-- EC01-3: Q15 removed from IMMEDIATE_SKIP_ALLOWLIST (test-only)
-- EC01-4: Proptest join convergence harness (test-only)
-- DUR-1: Two-phase frontier commit (new column: tentative_frontier)
-- DUR-2: change_buffer_durability GUC (config-only)
-- DUR-3: Crash-recovery E2E test (test-only)
-- CDC-1: ChangedColsBitmaskFailed error variant (code-only)
-- CDC-2: Partitioned-source publication rebuild (code-only)
-- CDC-3: TOAST-aware CDC hashing (code-only)
-- CDC-4: TOAST workload E2E tests (test-only)
-- OPS-1: History retention pruning in scheduler (uses existing GUC)
-- OPS-2: Frozen-stream-table detector dog-feeding view (code-only)
-- OPS-3: Missing internal catalog indexes (below)
-- TEST-6/7/8: Unit test campaigns (test-only)

-- DUR-1: Add tentative_frontier column for two-phase frontier commit.
-- Phase 1 writes here before MERGE; Phase 2 promotes to frontier after commit.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS tentative_frontier JSONB;

-- OPS-3: Add missing composite indexes for scheduler performance.
-- These indexes reduce per-tick scan time at 100+ stream tables.

-- Index for scheduler status-based filtering + SCC grouping.
CREATE INDEX IF NOT EXISTS idx_pgt_st_status_scc
    ON pgtrickle.pgt_stream_tables (status, scc_id);

-- Index for refresh history lookups by pgt_id + action + timestamp.
CREATE INDEX IF NOT EXISTS idx_pgt_rh_pgt_action_ts
    ON pgtrickle.pgt_refresh_history (pgt_id, action, data_timestamp);

-- Index for change tracking source lookups.
CREATE INDEX IF NOT EXISTS idx_pgt_ct_source_relid
    ON pgtrickle.pgt_change_tracking (source_relid);

-- Record the schema version for the upgrade chain.
INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('0.24.0', 'Join Correctness & Durability Hardening')
ON CONFLICT (version) DO NOTHING;
