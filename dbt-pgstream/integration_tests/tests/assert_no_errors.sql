-- Verify no stream tables have consecutive errors.
-- An empty result set means the test passes.
SELECT pgs_name, consecutive_errors
FROM pgstream.pgs_stream_tables
WHERE consecutive_errors > 0
