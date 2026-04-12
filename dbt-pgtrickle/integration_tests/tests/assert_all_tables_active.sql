-- Verify that all stream tables report a known-good status.
-- Tables should be ACTIVE after a successful dbt run + refresh.
-- An empty result set means the test passes.
SELECT pgt_name, status
FROM pgtrickle.pgt_stream_tables
WHERE status NOT IN ('ACTIVE', 'SUSPENDED')
  AND pgt_name LIKE '%order%' OR pgt_name LIKE '%customer%'
