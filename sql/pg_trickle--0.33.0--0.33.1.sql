-- pg_trickle 0.33.0 → 0.33.1 upgrade migration
-- ============================================
--
-- v0.33.1 — pg_ripple v0.58.0 Citus co-location helper
--
-- Changes in this version:
--   - New SQL function pgtrickle.handle_vp_promoted(payload TEXT) RETURNS BOOLEAN
--     Processes a pg_ripple.vp_promoted NOTIFY payload, logs the promotion, and
--     (when a matching distributed CDC source exists) signals the scheduler to
--     probe per-worker WAL slots on the next tick.
--
-- No schema (table/view) changes in this version.

-- ─────────────────────────────────────────────────────────────────────────
-- New function: pgtrickle.handle_vp_promoted
-- ─────────────────────────────────────────────────────────────────────────
-- src/citus.rs:752
-- pg_trickle::citus::handle_vp_promoted
CREATE OR REPLACE FUNCTION pgtrickle."handle_vp_promoted"(
    "payload" TEXT /* &str */
) RETURNS bool /* bool */
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'sql_handle_vp_promoted_wrapper';
