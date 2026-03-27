-- pg_trickle 0.11.0 -> 0.12.0 upgrade script
--
-- Changes will be documented here as phases are implemented.
--
-- Phase 1 (quick wins):
--   PERF-3: tiered_scheduling GUC default changed to true. No DDL required.
--
-- Phase 2 (developer tooling):
--   DT-1: pgtrickle.explain_query_rewrite(query TEXT) SQL function.
--   DT-2: pgtrickle.diagnose_errors(name TEXT) SQL function.
--   DT-3: pgtrickle.list_auxiliary_columns(name TEXT) SQL function.
--   DT-4: pgtrickle.validate_query(query TEXT) SQL function.
--
-- Phase 5 (scalability foundations):
--   A-2: Columnar change tracking -- per-column bitmask superset support.
--        Changes may require ALTER TABLE on existing change buffer tables;
--        handled at runtime by alter_change_buffer_add_columns().
--   D-4: Shared change buffers -- new pgt_shared_change_buffers catalog
--        table; multi-frontier cleanup coordination.
--
-- ── Phase 2: Developer Diagnostic Functions ───────────────────────────────

-- DT-1: explain_query_rewrite — walk a query through every DVM rewrite pass.
CREATE OR REPLACE FUNCTION pgtrickle."explain_query_rewrite"(
        "query" TEXT
) RETURNS TABLE (
        "pass_name" TEXT,
        "changed" bool,
        "sql_after" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'explain_query_rewrite_wrapper';

-- DT-2: diagnose_errors — last 5 FAILED refresh events with classification.
CREATE OR REPLACE FUNCTION pgtrickle."diagnose_errors"(
        "name" TEXT
) RETURNS TABLE (
        "event_time" timestamp with time zone,
        "error_type" TEXT,
        "error_message" TEXT,
        "remediation" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'diagnose_errors_wrapper';

-- DT-3: list_auxiliary_columns — __pgt_* columns on a stream table's storage.
CREATE OR REPLACE FUNCTION pgtrickle."list_auxiliary_columns"(
        "name" TEXT
) RETURNS TABLE (
        "column_name" TEXT,
        "data_type" TEXT,
        "purpose" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'list_auxiliary_columns_wrapper';

-- DT-4: validate_query — parse and validate without creating a stream table.
CREATE OR REPLACE FUNCTION pgtrickle."validate_query"(
        "query" TEXT
) RETURNS TABLE (
        "check_name" TEXT,
        "result" TEXT,
        "severity" TEXT
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'validate_query_wrapper';

