-- Self-Monitoring Quick Start
-- ======================
-- Run with: psql -f sql/self_monitoring_setup.sql
--
-- Creates all five self-monitoring stream tables, enables threshold auto-apply,
-- and sets up anomaly alerting. Idempotent — safe to run multiple times.

-- Step 1: Create all self-monitoring stream tables.
SELECT pgtrickle.setup_self_monitoring();

-- Step 2: Enable threshold auto-apply (optional).
-- Values: 'off' (default), 'threshold_only', 'full'
SET pg_trickle.self_monitoring_auto_apply = 'threshold_only';

-- Step 3: Listen for anomaly notifications.
LISTEN pg_trickle_alert;

-- Step 4: Check self-monitoring status.
SELECT * FROM pgtrickle.self_monitoring_status();

-- Step 5: View initial threshold recommendations (after history accumulates).
-- Note: This will be empty until at least 10 refresh cycles have run.
-- SELECT * FROM pgtrickle.df_threshold_advice WHERE confidence IN ('HIGH', 'MEDIUM');

-- Step 6: View the DAG to verify self-monitoring STs are included.
SELECT pgtrickle.explain_dag();
