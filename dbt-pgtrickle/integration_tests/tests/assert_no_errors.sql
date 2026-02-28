-- Verify no stream tables have consecutive errors.
-- An empty result set means the test passes.
SELECT pgt_name, consecutive_errors
FROM pgtrickle.pgt_stream_tables
WHERE consecutive_errors > 0
