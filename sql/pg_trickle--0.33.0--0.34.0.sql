-- pg_trickle 0.33.0 → 0.34.0 upgrade migration
-- ============================================
--
-- v0.34.0 — Citus: Automated Distributed CDC Scheduler Wiring and Shard Rebalance Auto-Recovery
--
-- Changes in this version:
--   - COORD-10: Scheduler now calls poll_worker_slot_changes() for distributed sources.
--   - COORD-11: Scheduler now calls ensure_worker_slot() on first tick and after
--               topology changes.
--   - COORD-12: Scheduler acquires, extends, and releases pgt_st_locks lease
--               automatically during distributed refresh cycles.
--   - COORD-13: Scheduler detects pg_dist_node topology changes; auto-recovers
--               worker slots after shard rebalances.
--   - COORD-14: Worker unreachability handled gracefully: skip + retry + alert.
--   - COORD-15: New GUC pg_trickle.citus_worker_retry_ticks (default 5).
--   - COORD-16: citus_status view extended with lease_health and last_polled_at columns.
--
-- Migration is safe to run on a live system.
-- The new scheduler behaviour activates automatically for stream tables with
-- source_placement = 'distributed'.  No manual reconfiguration required.

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 1: Add last_polled_at column to pgt_worker_slots
--         (COORD-16: needed for last-poll timestamp in citus_status)
-- ─────────────────────────────────────────────────────────────────────────

ALTER TABLE pgtrickle.pgt_worker_slots
    ADD COLUMN IF NOT EXISTS last_polled_at TIMESTAMPTZ DEFAULT NULL;

-- ─────────────────────────────────────────────────────────────────────────
-- STEP 2: Replace citus_status view with extended version
--         (COORD-16: adds lease_health and last_polled_at columns)
-- ─────────────────────────────────────────────────────────────────────────

CREATE OR REPLACE VIEW pgtrickle.citus_status AS
SELECT
    st.pgt_id,
    st.pgt_schema,
    st.pgt_name,
    ct.source_relid,
    ct.source_stable_name,
    ct.slot_name                AS coordinator_slot,
    ct.source_placement,
    ct.frontier_per_node,
    ws.worker_name,
    ws.worker_port,
    ws.slot_name                AS worker_slot,
    ws.last_frontier            AS worker_frontier,
    ws.last_polled_at,
    -- COORD-16: Lease health — shows whether the coordination lock is currently held
    -- and by whom. NULL when no lock is active for this stream table.
    lk.holder                   AS lease_holder,
    lk.acquired_at              AS lease_acquired_at,
    lk.expires_at               AS lease_expires_at,
    CASE
        WHEN lk.lock_key IS NULL         THEN 'unlocked'
        WHEN lk.expires_at < now()       THEN 'expired'
        ELSE 'locked'
    END                         AS lease_health
FROM pgtrickle.pgt_change_tracking ct
JOIN pgtrickle.pgt_stream_tables   st ON st.pgt_id = ANY(ct.tracked_by_pgt_ids)
LEFT JOIN pgtrickle.pgt_worker_slots ws
       ON ws.pgt_id       = st.pgt_id
      AND ws.source_relid = ct.source_relid
LEFT JOIN pgtrickle.pgt_st_locks lk
       ON lk.lock_key = 'pgt_' || st.pgt_id::text
WHERE ct.source_placement = 'distributed';
