-- pg_trickle 0.8.0 -> 0.9.0 upgrade script
--
-- v0.9.0 adds incremental aggregate maintenance via algebraic decomposition.
--
-- New auxiliary columns (__pgt_aux_sum_*, __pgt_aux_count_*, __pgt_aux_sum2_*)
-- are managed dynamically per stream table, so no global ALTER TABLE is needed.
-- However, existing stream tables using AVG/STDDEV/VAR aggregates must be
-- reinitialized after upgrade so that auxiliary columns are added and the
-- algebraic differential path activates.
--
-- The extension's next refresh of affected stream tables will automatically
-- detect missing auxiliary columns and perform a full reinitialize.

-- New API functions
CREATE OR REPLACE FUNCTION pgtrickle."restore_stream_tables"() RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'restore_stream_tables_wrapper';