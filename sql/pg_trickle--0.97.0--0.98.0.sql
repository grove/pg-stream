-- pg_trickle 0.97.0 -> 0.98.0
-- Rebuild triggers before converting legacy WAL metadata so an upgrade never
-- creates a trigger-free interval.

SELECT pgtrickle.rebuild_cdc_triggers();

DO $$
DECLARE
    v_slot_name text;
BEGIN
    FOR v_slot_name IN
        SELECT DISTINCT d.slot_name
        FROM pgtrickle.pgt_dependencies AS d
        WHERE d.slot_name IS NOT NULL
    LOOP
        IF EXISTS (
            SELECT 1
            FROM pg_replication_slots AS s
            WHERE s.slot_name = v_slot_name
        ) THEN
            PERFORM pg_drop_replication_slot(v_slot_name);
        END IF;
    END LOOP;
END
$$;

UPDATE pgtrickle.pgt_stream_tables AS st
SET needs_reinit = true,
    requested_cdc_mode = 'trigger',
    frontier = NULL,
    updated_at = now()
WHERE EXISTS (
    SELECT 1
    FROM pgtrickle.pgt_dependencies AS d
    WHERE d.pgt_id = st.pgt_id
      AND d.cdc_mode IN ('WAL', 'TRANSITIONING')
      AND d.source_type = 'TABLE'
);

UPDATE pgtrickle.pgt_dependencies
SET cdc_mode = 'TRIGGER',
    slot_name = NULL,
    decoder_confirmed_lsn = NULL,
    transition_started_at = NULL,
    cutover_target = NULL,
    cutover_lsn = NULL
WHERE cdc_mode IN ('WAL', 'TRANSITIONING');

INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('0.98.0', 'Risk containment, trigger-only capture, and fail-closed integration contracts')
ON CONFLICT (version) DO NOTHING;
