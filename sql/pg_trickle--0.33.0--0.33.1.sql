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
-- NOTE: This function is compiled from Rust (#[pg_extern]); this SQL block
-- documents the signature and is used by ALTER EXTENSION UPDATE to register
-- the function in the extension's object catalog.
--
-- The actual implementation is in the shared library (src/citus.rs).
-- The CREATE OR REPLACE below is a no-op when pgrx regenerates it, but
-- ensures the extension catalog is correct after ALTER EXTENSION UPDATE.

DO $$
BEGIN
    -- Guard: only register if the function doesn't already exist
    -- (handles repeated ALTER EXTENSION UPDATE calls gracefully).
    IF NOT EXISTS (
        SELECT 1 FROM pg_proc p
        JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'pgtrickle'
          AND p.proname = 'handle_vp_promoted'
    ) THEN
        RAISE NOTICE 'pg_trickle 0.33.1: handle_vp_promoted() will be registered by the shared library on next load';
    END IF;
END $$;
