-- pg_trickle 0.5.0 -> 0.6.0 upgrade script
--
-- v0.6.0 adds:
--   CYC-3: Circular dependency foundation catalog columns.
--     - scc_id on pgt_stream_tables (SCC group identifier)
--     - fixpoint_iteration on pgt_refresh_history

-- CYC-3: SCC identifier for circular dependency tracking.
-- NULL means the stream table is not part of a cyclic SCC.
-- All members of the same cycle share the same scc_id value.
ALTER TABLE pgtrickle.pgt_stream_tables
  ADD COLUMN IF NOT EXISTS scc_id INT;

-- CYC-3: Fixpoint iteration counter for refresh history.
-- Records which iteration of the fixed-point loop produced this refresh.
-- NULL for non-cyclic refreshes.
ALTER TABLE pgtrickle.pgt_refresh_history
  ADD COLUMN IF NOT EXISTS fixpoint_iteration INT;
