-- pg_trickle 0.6.0 -> 0.7.0 upgrade script
--
-- v0.7.0 adds:
--   CYC-5: last_fixpoint_iterations column for SCC convergence tracking
--   Watermark gating: pgt_watermarks, pgt_watermark_groups catalog tables
--     and SQL functions for cross-source temporal alignment

-- CYC-5: Track the number of fixpoint iterations in the last SCC convergence.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS last_fixpoint_iterations INT;

-- Per-source watermark state: tracks how far each external source has been loaded.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_watermarks (
    source_relid       OID PRIMARY KEY,
    watermark          TIMESTAMPTZ NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    advanced_by        TEXT,
    wal_lsn_at_advance TEXT
);

-- Watermark groups: declare that a set of sources must be temporally aligned.
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_watermark_groups (
    group_id           SERIAL PRIMARY KEY,
    group_name         TEXT UNIQUE NOT NULL,
    source_relids      OID[] NOT NULL,
    tolerance_secs     DOUBLE PRECISION NOT NULL DEFAULT 0,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
