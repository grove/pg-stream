-- pg_trickle 0.26.0 → 0.27.0 upgrade migration
-- ============================================
--
-- v0.27.0 — Operability, Observability & DR
--
-- DEP-1/DEP-2: pgrx 0.17.0 → 0.18.0 upgrade (code-only)
--
-- SNAP-1: snapshot_stream_table(name, target) — new SQL function
-- SNAP-2: restore_from_snapshot(name, source) — new SQL function
-- SNAP-3: list_snapshots(name) + drop_snapshot(snapshot_table) — new SQL functions
-- PLAN-1: recommend_schedule(name) — new SQL function
-- PLAN-2: schedule_recommendations() — new SQL function
-- CLUS-1: cluster_worker_summary() — new SQL function
-- METR-3: metrics_summary() — new SQL function
--
-- New GUCs (registered in config.rs; no SQL DDL needed):
--   pg_trickle.schedule_recommendation_min_samples (default 20)
--   pg_trickle.schedule_alert_cooldown_seconds (default 300)
--   pg_trickle.metrics_request_timeout_ms (default 5000)

-- ── SNAP catalog table ────────────────────────────────────────────────────
-- Metadata catalog for stream-table snapshots (SNAP-1/2/3)
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_snapshots (
    snapshot_id      BIGSERIAL PRIMARY KEY,
    pgt_id           BIGINT NOT NULL
                     REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    snapshot_schema  TEXT NOT NULL,
    snapshot_table   TEXT NOT NULL,
    snapshot_version TEXT NOT NULL,
    frontier         JSONB,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_snapshot_table UNIQUE (snapshot_schema, snapshot_table)
);

CREATE INDEX IF NOT EXISTS idx_pgt_snapshots_pgt_id
    ON pgtrickle.pgt_snapshots (pgt_id);

-- ── SNAP-1: snapshot_stream_table ────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."snapshot_stream_table"(
    p_name        text,
    p_target      text DEFAULT NULL
) RETURNS text
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'snapshot_stream_table_wrapper';

COMMENT ON FUNCTION pgtrickle.snapshot_stream_table(text, text) IS
'SNAP-1 (v0.27.0): Export the current content of a stream table into an
archival snapshot table. Returns the fully-qualified name of the created
snapshot table. If p_target is NULL, the target name is auto-generated as
pgtrickle.snapshot_<name>_<epoch_ms>.';

-- ── SNAP-2: restore_from_snapshot ─────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."restore_from_snapshot"(
    p_name        text,
    p_source      text
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'restore_from_snapshot_wrapper';

COMMENT ON FUNCTION pgtrickle.restore_from_snapshot(text, text) IS
'SNAP-2 (v0.27.0): Rehydrate a stream table from an archival snapshot created
by snapshot_stream_table(). The first refresh after restore uses DIFFERENTIAL
mode — the frontier is aligned to the snapshot frontier, skipping the initial
FULL refresh cycle.';

-- ── SNAP-3a: list_snapshots ────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."list_snapshots"(
    p_name        text
) RETURNS TABLE (
    snapshot_table  text,
    created_at      timestamptz,
    row_count       bigint,
    frontier        jsonb,
    size_bytes      bigint
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'list_snapshots_wrapper';

COMMENT ON FUNCTION pgtrickle.list_snapshots(text) IS
'SNAP-3 (v0.27.0): List all archival snapshot tables for the given stream table.
Returns one row per snapshot, ordered by creation time descending.';

-- ── SNAP-3b: drop_snapshot ────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."drop_snapshot"(
    p_snapshot_table  text
) RETURNS void
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'drop_snapshot_wrapper';

COMMENT ON FUNCTION pgtrickle.drop_snapshot(text) IS
'SNAP-3 (v0.27.0): Drop an archival snapshot table created by
snapshot_stream_table() and remove its pgtrickle.pgt_snapshots catalog row.';

-- ── PLAN-1: recommend_schedule ────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."recommend_schedule"(
    p_name  text
) RETURNS jsonb
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'recommend_schedule_wrapper';

COMMENT ON FUNCTION pgtrickle.recommend_schedule(text) IS
'PLAN-1 (v0.27.0): Analyse the per-stream-table cost-model history and return
a recommended refresh schedule as a JSONB object with keys:
recommended_interval_seconds, peak_window_cron, confidence (0-1), reasoning.
Returns confidence=0.0 when fewer than
pg_trickle.schedule_recommendation_min_samples observations are available.';

-- ── PLAN-2: schedule_recommendations ─────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."schedule_recommendations"()
RETURNS TABLE (
    name                          text,
    current_interval_seconds      double precision,
    recommended_interval_seconds  double precision,
    delta_pct                     double precision,
    confidence                    double precision,
    reasoning                     text
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'schedule_recommendations_wrapper';

COMMENT ON FUNCTION pgtrickle.schedule_recommendations() IS
'PLAN-2 (v0.27.0): Return one schedule recommendation row per registered
stream table. Sort by delta_pct DESC to find the most mis-tuned stream tables.';

-- ── CLUS-1: cluster_worker_summary ───────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."cluster_worker_summary"()
RETURNS TABLE (
    db_oid                 bigint,
    db_name                text,
    active_workers         integer,
    scheduler_pid          integer,
    scheduler_running      boolean,
    total_active_workers   integer
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'cluster_worker_summary_wrapper';

COMMENT ON FUNCTION pgtrickle.cluster_worker_summary() IS
'CLUS-1 (v0.27.0): Return per-database worker allocation from the
shared-memory worker-pool block and pg_stat_activity.
Accessible from any database in the cluster.';

-- ── METR-3: metrics_summary ───────────────────────────────────────────────
CREATE OR REPLACE FUNCTION pgtrickle."metrics_summary"()
RETURNS TABLE (
    db_name                  text,
    total_stream_tables      bigint,
    active_stream_tables     bigint,
    suspended_stream_tables  bigint,
    total_refreshes          bigint,
    successful_refreshes     bigint,
    failed_refreshes         bigint,
    total_rows_processed     bigint,
    active_workers           integer
)
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'metrics_summary_wrapper';

COMMENT ON FUNCTION pgtrickle.metrics_summary() IS
'METR-3 (v0.27.0): Cluster-wide metrics summary aggregating refresh counts,
error counts, and worker utilisation across all stream tables in this database.
Designed as the data source for the Grafana cluster-overview dashboard.';
