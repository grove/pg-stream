-- pg_trickle 0.30.0 → 0.31.0 upgrade migration
-- ============================================
--
-- v0.31.0 — Performance & Scheduler Intelligence
--
-- Changes in this version:
--   - PERF-4: ENR-based IVM transition tables (new pgt_ivm_apply_delta_enr function)
--   - PERF-3: IVM lock-mode observability counter added to metrics_summary()
--   - PERF-2: Plan-aware delta routing logging (new adaptive_merge_strategy GUC)
--   - PERF-1: Adaptive batch coalescing (new adaptive_batch_coalescing GUC)
--   - SCAL-1: Change buffer back-pressure alerting (new backpressure_consecutive_limit GUC)
--   - Default pg_trickle.use_sqlstate_classification changed from false to true
--
-- New GUCs (set in postgresql.conf or ALTER SYSTEM):
--   pg_trickle.ivm_use_enr                    — ENR-based IVM triggers (default true, PG18+)
--   pg_trickle.adaptive_batch_coalescing      — coalesce source scans (default true)
--   pg_trickle.adaptive_merge_strategy        — plan-aware strategy selection (default false)
--   pg_trickle.backpressure_consecutive_limit — cycles before backpressure alert (default 3)
--
-- Note: pg_trickle.use_sqlstate_classification now defaults to true.
--   Set to false in postgresql.conf to revert to message-text retry classification.
--
-- This migration has no schema changes to catalog tables.
-- All changes are in the Rust extension code and GUC registration.
-- Function signatures are updated automatically by pgrx during ALTER EXTENSION UPDATE.

-- No DDL changes required for this version upgrade.

-- PERF-4: New ENR-based IVM delta application function.
-- Called by trigger bodies generated when pg_trickle.ivm_use_enr = true.
CREATE OR REPLACE FUNCTION pgtrickle."pgt_ivm_apply_delta_enr"(
    "pgt_id"     bigint,
    "source_oid" INT,
    "has_new"    bool,
    "has_old"    bool
) RETURNS VOID
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pgt_ivm_apply_delta_enr_wrapper';
