-- pg_trickle 0.2.0 → 0.2.1 upgrade script
--
-- EC-06: Add has_keyless_source flag to pgt_stream_tables.
-- When TRUE, the stream table uses a non-unique index on __pgt_row_id
-- and the apply logic uses counted DELETE instead of MERGE, because
-- identical duplicate rows produce the same content hash → same row_id.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS has_keyless_source BOOLEAN NOT NULL DEFAULT FALSE;

-- EC-16: Add function_hashes column to pgt_stream_tables.
-- Stores the last-seen md5(prosrc) hash for each function referenced in
-- the defining query (functions_used array).  On each differential refresh
-- cycle the scheduler recomputes these hashes and, when any hash changes,
-- forces a full refresh and logs a NOTICE — detecting silent ALTER FUNCTION
-- body changes that would otherwise go undetected until a manual refresh.
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS function_hashes TEXT;
