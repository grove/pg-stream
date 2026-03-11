-- pg_trickle 0.3.0 -> 0.4.0 upgrade script
-- CSS1: LSN tick watermark column for cross-source snapshot consistency.
ALTER TABLE pgtrickle.pgt_refresh_history
    ADD COLUMN IF NOT EXISTS tick_watermark_lsn PG_LSN;
