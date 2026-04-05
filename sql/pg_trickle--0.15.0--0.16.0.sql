-- pg_trickle 0.15.0 -> 0.16.0 upgrade script
--
-- v0.16.0: Performance & Refresh Optimization

-- BUF-LIMIT: max_buffer_rows GUC
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   Hard limit on change buffer rows per source table (default: 1,000,000).
--   Forces FULL refresh + truncate when exceeded.

-- AUTO-IDX: auto_index GUC
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   Controls automatic index creation on GROUP BY/DISTINCT columns and
--   covering __pgt_row_id index (default: true).

-- PH-D1: merge_strategy + merge_strategy_threshold GUCs
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   merge_strategy: 'auto' (default), 'merge', 'delete_insert'
--   merge_strategy_threshold: 0.01 (default) — delta ratio threshold for auto mode
