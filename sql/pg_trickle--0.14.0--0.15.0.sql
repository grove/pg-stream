-- pg_trickle 0.14.0 -> 0.15.0 upgrade script
--
-- v0.15.0: External Test Suites & Integration
--
-- VOL-1: volatile_function_policy GUC
--   Registered via pgrx GUC infrastructure (no DDL needed — GUC is
--   automatically available after extension upgrade).
--
-- TRUNC-1: TRUNCATE capture for trigger-mode CDC
--   Already implemented in v0.14.0 via CDC TRUNCATE triggers. No schema
--   changes needed for this release.
--
-- G15-BC: bulk_create(definitions JSONB)
--   New SQL-callable function — CREATE FUNCTION added below.
--
-- G8.1: Cross-session MERGE cache invalidation
--   Already implemented via shared-memory CACHE_GENERATION counter.
--   No catalog schema changes needed.
--
-- EXPL-ENH: explain_st() enhancements
--   New output properties (refresh_timing_stats, source_partitions,
--   dependency_graph_dot) added to existing explain_st() function.
--   No DDL changes needed — function signature unchanged.
--
-- PH-D2: merge_join_strategy GUC
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--
-- PH-E1: max_delta_estimate_rows GUC
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   Pre-flight delta output cardinality estimation before MERGE execution.
--
-- EC-01: JOIN key change + DELETE correctness fix
--   Already implemented in v0.14.0 via R₀ pre-change snapshot strategy.
--   Documentation updated to replace the known limitation section.
--
-- WM-7: watermark_holdback_timeout GUC
--   Registered via pgrx GUC infrastructure (auto-available after upgrade).
--   Detects stuck watermarks and pauses downstream stream tables.
--
-- No catalog schema changes in this upgrade step.

-- drop_stream_table: add cascade parameter (defaults to true)
-- The old 1-arg overload is replaced by a 2-arg overload with a default.
DROP FUNCTION IF EXISTS pgtrickle."drop_stream_table"(TEXT);
CREATE FUNCTION pgtrickle."drop_stream_table"(
    "name" TEXT /* &str */,
    "cascade" BOOL DEFAULT true /* bool */
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'drop_stream_table_wrapper';

-- G15-BC: bulk_create(definitions JSONB)
-- New function added in 0.15.0 for batch stream table creation.
CREATE FUNCTION pgtrickle."bulk_create"(
    "definitions" jsonb /* pgrx::datum::json::JsonB */
) RETURNS jsonb /* pgrx::datum::json::JsonB */
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'bulk_create_wrapper';
