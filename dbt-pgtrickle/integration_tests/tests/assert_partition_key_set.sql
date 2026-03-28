-- Verify that the order_totals_partitioned stream table has partition_by set.
-- Returns rows if the partition key is NOT set (test fails).
SELECT pgt_name, st_partition_key
FROM pgtrickle.pgt_stream_tables
WHERE pgt_name = 'order_totals_partitioned'
  AND st_partition_key IS NULL
