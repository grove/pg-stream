-- pg_trickle 0.6.0 -> 0.7.0 upgrade script
--
-- v0.7.0 adds:
--   CYC-5: last_fixpoint_iterations column for SCC convergence tracking

-- CYC-5: Track the number of fixpoint iterations in the last SCC convergence.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS last_fixpoint_iterations INT;
