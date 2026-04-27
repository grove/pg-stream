-- pg_trickle 0.36.0 → 0.37.0 upgrade migration
-- All DDL is idempotent (IF NOT EXISTS / IF EXISTS / ADD COLUMN IF NOT EXISTS).

-- ── F10 (v0.37.0): W3C Trace Context propagation ────────────────────────────
-- The __pgt_trace_context column stores the W3C traceparent string that was
-- active in the session when a DML statement was executed.
-- New change buffers created by create_stream_table() already have this column;
-- existing buffers need an ALTER TABLE for the upgrade path.
--
-- Because change buffer table names are dynamic (changes_<source_table>), we
-- generate and execute ALTER TABLE statements for all existing change buffers.
DO $$
DECLARE
    rec RECORD;
BEGIN
    FOR rec IN
        SELECT table_name
        FROM information_schema.tables
        WHERE table_schema = 'pgtrickle_changes'
          AND table_type = 'BASE TABLE'
    LOOP
        EXECUTE format(
            'ALTER TABLE pgtrickle_changes.%I ADD COLUMN IF NOT EXISTS __pgt_trace_context TEXT',
            rec.table_name
        );
    END LOOP;
END;
$$;

-- ── F10 (v0.37.0): GUC documentation comments ────────────────────────────────
-- The GUCs are registered automatically by the C extension when loaded.
-- Document expected GUC names here for operational clarity:
--
--   pg_trickle.enable_trace_propagation  (BOOL, default: false)
--     When true, trace context is read from pg_trickle.trace_id and emitted
--     via OTLP/HTTP to pg_trickle.otel_endpoint after each differential refresh.
--
--   pg_trickle.otel_endpoint  (STRING, default: '')
--     HTTP endpoint for OTLP JSON export, e.g. 'http://localhost:4318'.
--     When empty, trace spans are logged at INFO level instead.
--
--   pg_trickle.trace_id  (STRING, default: '')
--     Set in application sessions before DML: SET pg_trickle.trace_id = 'traceparent'.
--     Captured by CDC triggers into __pgt_trace_context change buffer column.
--
-- ── F4 (v0.37.0): pgvector incremental aggregates ────────────────────────────
-- No schema changes needed for F4. The vector aggregate reclassification
-- (AggFunc::Avg → AggFunc::VectorAvg when the argument column has type vector,
-- halfvec, or sparsevec) is handled entirely in the DVM planner at runtime.
--
--   pg_trickle.enable_vector_agg  (BOOL, default: false)
--     When true, the DVM planner reclassifies avg(vector) and sum(vector)
--     aggregates to use pgvector-native incremental operators.

-- ── Upgrade complete ──────────────────────────────────────────────────────────
-- No pgrx-generated C function signatures changed in v0.37.0.
-- GUC variables are registered at extension load time and require no SQL stubs.
