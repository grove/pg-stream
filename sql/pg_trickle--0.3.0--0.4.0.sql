-- pg_trickle 0.3.0 -> 0.4.0 upgrade script
--
-- v0.4.0 adds the parallel refresh infrastructure:
--
--   Phase 2: Job table for execution-unit dispatch and worker budget tracking.
--   Phase 3: Dynamic refresh worker entry point.
--
-- New catalog table: pgtrickle.pgt_scheduler_jobs
-- Tracks execution-unit dispatch, worker assignment, and completion status
-- for parallel refresh coordination.

-- Scheduler job table for parallel refresh dispatch
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_scheduler_jobs (
    job_id          BIGSERIAL PRIMARY KEY,
    dag_version     BIGINT NOT NULL,
    unit_key        TEXT NOT NULL,
    unit_kind       TEXT NOT NULL
                     CHECK (unit_kind IN ('singleton', 'atomic_group', 'immediate_closure')),
    member_pgt_ids  BIGINT[] NOT NULL,
    root_pgt_id     BIGINT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'QUEUED'
                     CHECK (status IN ('QUEUED', 'RUNNING', 'SUCCEEDED',
                                       'RETRYABLE_FAILED', 'PERMANENT_FAILED', 'CANCELLED')),
    scheduler_pid   INT NOT NULL,
    worker_pid      INT,
    attempt_no      INT NOT NULL DEFAULT 1,
    enqueued_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at      TIMESTAMPTZ,
    finished_at     TIMESTAMPTZ,
    outcome_detail  TEXT,
    retryable       BOOLEAN
);

-- Polling active/queued jobs by status and enqueue order
CREATE INDEX IF NOT EXISTS idx_sched_jobs_status_enqueued
    ON pgtrickle.pgt_scheduler_jobs (status, enqueued_at);

-- Prevent duplicate in-flight jobs for the same execution unit
CREATE INDEX IF NOT EXISTS idx_sched_jobs_unit_status
    ON pgtrickle.pgt_scheduler_jobs (unit_key, status);

-- Cleanup of old completed/failed jobs
CREATE INDEX IF NOT EXISTS idx_sched_jobs_finished
    ON pgtrickle.pgt_scheduler_jobs (finished_at)
    WHERE finished_at IS NOT NULL;
-- CSS1: LSN tick watermark column for cross-source snapshot consistency.
ALTER TABLE pgtrickle.pgt_refresh_history
    ADD COLUMN IF NOT EXISTS tick_watermark_lsn PG_LSN;

-- B2: Statement-level CDC trigger migration.
-- Declare the new rebuild helper (compiled into the 0.4.0 .so) and run it
-- once to migrate all existing row-level CDC triggers to statement-level.
-- The function is retained as pgtrickle.rebuild_cdc_triggers() for manual use
-- (e.g. after changing pg_trickle.cdc_trigger_mode).
CREATE OR REPLACE FUNCTION pgtrickle.rebuild_cdc_triggers()
    RETURNS text
    STRICT
    LANGUAGE c /* Rust */
    AS 'MODULE_PATHNAME', 'rebuild_cdc_triggers_wrapper';

SELECT pgtrickle.rebuild_cdc_triggers();
