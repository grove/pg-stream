-- pg_trickle 0.44.0 -> 0.45.0 upgrade migration
--
-- v0.45.0 — Operational Readiness, Scalability & CI Completeness
--
-- Changes in this release:
--
--   A46-1:  Dockerfile VERSION sync — Dockerfile.hub and Dockerfile.ghcr now
--             carry the correct default ARG VERSION matching Cargo.toml.
--   A46-2:  Container HEALTHCHECK — all three Dockerfiles now include a
--             pg_isready HEALTHCHECK directive.
--   A46-3:  CNPG production examples — cnpg/cluster-dev.yaml and
--             cnpg/cluster-production.yaml added (infrastructure docs only).
--   A46-4:  preflight() SQL function — new pgtrickle.preflight() function
--             returns a JSON health report with 7 system checks.
--   A46-5:  worker_pool_status() enhanced — four new columns added:
--             idle_workers, last_scheduler_tick_unix,
--             ring_overflow_count, citus_failure_total.
--   A46-6:  Production monitoring split — monitoring/production/README.md
--             added with least-privilege role setup, TLS config, and
--             Kubernetes ServiceMonitor examples.
--   A46-7:  Invalidation ring configurable capacity — new GUC
--             pg_trickle.invalidation_ring_capacity (default 128, max 1024).
--             IMPORTANT: changing this GUC requires a PostgreSQL restart
--             because the shared memory layout changes.
--   A46-8:  Worker-slot exhaustion SQL visibility — surfaced via preflight().
--   A46-9:  Incremental DAG rebuild — O(affected) partial schedule
--             re-resolution instead of O(V) full pass on each event.
--   A46-10: Lag-aware cross-database scheduling foundation — new GUC
--             pg_trickle.lag_aware_scheduling (default false). When enabled,
--             the per-database quota is boosted proportionally to refresh lag.
--   A46-11: Citus failure counter persistence — new shared-memory counter
--             pg_trickle_citus_fail_total; surfaced via worker_pool_status().
--   A46-12: WAL slot preflight check — via preflight().
--   A46-13: Cross-platform CI blocking — Windows compile check now blocking.
--   A46-14: Full-image PR smoke test — new CI job for each PR/push.
--   A46-15: E2E coverage schedule restore — weekly Monday coverage run.
--   A46-16: Storage Backends reference page — docs/STORAGE_BACKENDS.md.
--   A46-17: dbt macro option sync — create/alter macros now expose all
--             options from CreateStreamTableOptions.
--
-- Schema changes:
--   NEW FUNCTION: pgtrickle.preflight() RETURNS text
--   CHANGED FUNCTION: pgtrickle.worker_pool_status() — four new output columns
--   NEW GUC: pg_trickle.invalidation_ring_capacity (integer, postmaster scope)
--   NEW GUC: pg_trickle.lag_aware_scheduling (boolean, superuser scope)
--
-- NOTE: pg_trickle.invalidation_ring_capacity uses shared memory.
-- If you change its value from the default, PostgreSQL must be restarted
-- for the new ring size to take effect.

-- ── Drop existing worker_pool_status (return type changed) ──────────────
-- The return type changed (4 new columns), so we must DROP and CREATE NEW.
DROP FUNCTION IF EXISTS pgtrickle.worker_pool_status();

-- ── Create new worker_pool_status with extended return type ──────────────
CREATE FUNCTION pgtrickle."worker_pool_status"() RETURNS TABLE (
        "active_workers" INT,
        "max_workers" INT,
        "per_db_cap" INT,
        "parallel_mode" TEXT,
        "idle_workers" INT,
        "last_scheduler_tick_unix" bigint,
        "ring_overflow_count" bigint,
        "citus_failure_total" bigint
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'worker_pool_status_wrapper';

-- ── Create new preflight() function ─────────────────────────────────────
CREATE FUNCTION pgtrickle."preflight"() RETURNS TEXT
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'preflight_wrapper';
