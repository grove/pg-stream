-- pg_trickle 0.15.0 -> 0.16.0 upgrade script
--
-- v0.16.0: Performance & Refresh Optimization

-- BUF-LIMIT: max_buffer_rows GUC
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   Hard limit on change buffer rows per source table (default: 1,000,000).
--   Forces FULL refresh + truncate when exceeded.
