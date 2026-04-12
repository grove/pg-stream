-- Verify that each stream table has been refreshed at least once.
-- After dbt run + initialization, every table should have at least
-- one successful refresh recorded in pgt_refresh_history.
-- An empty result set means the test passes.
SELECT st.pgt_name
FROM pgtrickle.pgt_stream_tables st
LEFT JOIN pgtrickle.pgt_refresh_history h
  ON h.pgt_id = st.pgt_id AND h.status = 'COMPLETED'
WHERE h.pgt_id IS NULL
  AND st.is_populated = true
GROUP BY st.pgt_name
