-- Verify that the order_totals_fused stream table has fuse_mode = 'auto'.
-- Returns rows if the fuse mode is NOT 'auto' (test fails).
SELECT pgt_name, fuse_mode, fuse_ceiling, fuse_sensitivity
FROM pgtrickle.pgt_stream_tables
WHERE pgt_name = 'order_totals_fused'
  AND (
    COALESCE(fuse_mode, 'off') != 'auto'
    OR fuse_ceiling IS DISTINCT FROM 100000
    OR fuse_sensitivity IS DISTINCT FROM 3
  )
