-- pg_trickle 0.24.0 → 0.25.0 upgrade migration
-- ============================================
--
-- v0.25.0 — Scheduler Scalability & Pooler Performance
--
-- SCAL-1: Shmem catalog snapshot cache (code-only)
-- SCAL-2: Batched change detection (code-only)
-- SCAL-3: Split PGS_STATE lock into 3 per-concern locks (code-only)
-- SCAL-4: Copy-on-write DAG rebuild (code-only)
-- SCAL-5: Persistent worker pool option — pg_trickle.worker_pool_size GUC (code-only)
-- CACHE-1: L0 cross-backend shmem template cache signal (code-only)
-- CACHE-2: L1 LRU eviction — pg_trickle.template_cache_max_entries GUC (code-only)
-- CACHE-3: pgtrickle.clear_caches() SQL function (new function below)
-- PERF-1: xxh3 streaming hash in pg_trickle_hash_multi (code-only)
-- PERF-2: Pre-sized SQL buffer in project operator (code-only)
-- PERF-3: Shmem adaptive cost-model state (code-only)
-- PRED-1: Robustness guards on predictive cost model (code-only)
-- PUB-1: Subscriber-LSN lag tracking (code-only; uses pg_replication_slots)
-- PUB-2: worker_allocation_status() monitoring view (new function below)

-- CACHE-3: Register pgtrickle.clear_caches() function.
-- Flushes L1 thread-local, L2 catalog, and bumps CACHE_GENERATION.
-- Returns the number of L2 entries removed.
CREATE OR REPLACE FUNCTION pgtrickle."clear_caches"() RETURNS bigint
    LANGUAGE c STRICT
AS 'MODULE_PATHNAME', 'clear_caches_wrapper';

-- PUB-2: Register pgtrickle.worker_allocation_status() monitoring function.
-- Returns per-DB worker used/quota/queued and cluster-wide active/max counts.
CREATE OR REPLACE FUNCTION pgtrickle."worker_allocation_status"()
RETURNS TABLE (
    "db_name"        text,
    "workers_used"   bigint,
    "workers_quota"  bigint,
    "workers_queued" bigint,
    "cluster_active" bigint,
    "cluster_max"    bigint
)
    LANGUAGE c STRICT
AS 'MODULE_PATHNAME', 'worker_allocation_status_fn_wrapper';

-- MON-1: Register self-monitoring functions.
-- setup_self_monitoring() — installs the self-monitoring background worker.
CREATE OR REPLACE FUNCTION pgtrickle."setup_self_monitoring"() RETURNS void
    STRICT
    LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'setup_self_monitoring_wrapper';

-- teardown_self_monitoring() — uninstalls the self-monitoring background worker.
CREATE OR REPLACE FUNCTION pgtrickle."teardown_self_monitoring"() RETURNS void
    STRICT
    LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'teardown_self_monitoring_wrapper';

-- self_monitoring_status() — returns per-stream monitoring status.
CREATE OR REPLACE FUNCTION pgtrickle."self_monitoring_status"() RETURNS TABLE (
    "st_name"        TEXT,
    "exists"         bool,
    "status"         TEXT,
    "refresh_mode"   TEXT,
    "last_refresh_at" TEXT,
    "total_refreshes" bigint
)
    STRICT
    LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'self_monitoring_status_wrapper';

-- Record the schema version for the upgrade chain.
INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('0.25.0', 'Scheduler Scalability & Pooler Performance')
ON CONFLICT (version) DO NOTHING;
