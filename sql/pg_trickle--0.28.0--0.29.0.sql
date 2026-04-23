-- pg_trickle 0.28.0 → 0.29.0 upgrade migration
-- ============================================
--
-- v0.29.0 — Relay CLI (`pgtrickle-relay`)
--
-- RELAY-CAT: Catalog tables and SQL API for relay pipeline management.
--
-- New tables:
--   pgtrickle.relay_outbox_config     — forward pipelines (outbox → sink)
--   pgtrickle.relay_inbox_config      — reverse pipelines (source → inbox)
--   pgtrickle.relay_consumer_offsets  — durable per-pipeline offset tracking
--
-- New functions:
--   pgtrickle.set_relay_outbox(name, outbox, group, sink, retention_hours)
--   pgtrickle.set_relay_inbox(name, inbox, source, max_retries, schedule, ...)
--   pgtrickle.enable_relay(name)
--   pgtrickle.disable_relay(name)
--   pgtrickle.delete_relay(name)
--   pgtrickle.get_relay_config(name)
--   pgtrickle.list_relay_configs()
--
-- New trigger:
--   pgtrickle.relay_config_notify() — NOTIFY on relay_*_config changes
--   relay_outbox_config_notify — trigger on relay_outbox_config
--   relay_inbox_config_notify  — trigger on relay_inbox_config
--
-- New role (if not exists):
--   pgtrickle_relay — used by the relay binary for restricted access

-- ── Relay catalog tables ──────────────────────────────────────────────────

-- RELAY-CAT: Forward pipelines — outbox → external sink.
CREATE TABLE IF NOT EXISTS pgtrickle.relay_outbox_config (
    name     TEXT        NOT NULL PRIMARY KEY,
    enabled  BOOLEAN     NOT NULL DEFAULT true,
    config   JSONB       NOT NULL
);

COMMENT ON TABLE pgtrickle.relay_outbox_config IS
    'RELAY-CAT (v0.29.0): Forward relay pipeline definitions (outbox → external sink). '
    'Managed via pgtrickle.set_relay_outbox() and related API functions. '
    'The relay binary watches this table via LISTEN pgtrickle_relay_config.';

COMMENT ON COLUMN pgtrickle.relay_outbox_config.config IS
    'JSONB pipeline config. Required keys: source_type ("outbox"), source (object with '
    'outbox and optional group), sink_type, sink (backend-specific object with '
    'required "type" key). Additional backend keys are backend-specific.';

-- RELAY-CAT: Reverse pipelines — external source → pg-trickle inbox.
CREATE TABLE IF NOT EXISTS pgtrickle.relay_inbox_config (
    name     TEXT        NOT NULL PRIMARY KEY,
    enabled  BOOLEAN     NOT NULL DEFAULT true,
    config   JSONB       NOT NULL
);

COMMENT ON TABLE pgtrickle.relay_inbox_config IS
    'RELAY-CAT (v0.29.0): Reverse relay pipeline definitions (external source → inbox). '
    'Managed via pgtrickle.set_relay_inbox() and related API functions.';

COMMENT ON COLUMN pgtrickle.relay_inbox_config.config IS
    'JSONB pipeline config. Required keys: source_type, source (backend-specific, '
    'required "type" key), sink_type ("pg-inbox"), sink (object with inbox_table).';

-- RELAY-CAT: Durable per-pipeline offset tracking.
-- Written atomically after each committed batch so any pod can resume from
-- exactly the right position when the coordinator advisory lock moves.
CREATE TABLE IF NOT EXISTS pgtrickle.relay_consumer_offsets (
    relay_group_id  TEXT        NOT NULL,
    pipeline_id     TEXT        NOT NULL,
    last_change_id  BIGINT      NOT NULL DEFAULT 0,
    worker_id       TEXT,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (relay_group_id, pipeline_id)
);

COMMENT ON TABLE pgtrickle.relay_consumer_offsets IS
    'RELAY-CAT (v0.29.0): Durable per-pipeline offset tracking for the relay binary. '
    'relay_group_id namespaces separate relay deployments; pipeline_id is the row '
    'primary key from relay_*_config. Written atomically after each committed batch.';

-- ── Relay config NOTIFY trigger ───────────────────────────────────────────

-- RELAY-CAT: Shared trigger function — TG_TABLE_NAME identifies direction.
CREATE OR REPLACE FUNCTION pgtrickle.relay_config_notify()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
BEGIN
    PERFORM pg_notify(
        'pgtrickle_relay_config',
        json_build_object(
            'direction', TG_TABLE_NAME,
            'event',     TG_OP,
            'name',      COALESCE(NEW.name, OLD.name),
            'enabled',   COALESCE(NEW.enabled, OLD.enabled)
        )::text
    );
    RETURN NULL;
END;
$$;

COMMENT ON FUNCTION pgtrickle.relay_config_notify() IS
    'RELAY-CAT (v0.29.0): Sends NOTIFY pgtrickle_relay_config with pipeline change '
    'info whenever relay_outbox_config or relay_inbox_config is modified. '
    'The running relay binary uses this for hot-reload without restart.';

DROP TRIGGER IF EXISTS relay_outbox_config_notify ON pgtrickle.relay_outbox_config;
CREATE TRIGGER relay_outbox_config_notify
    AFTER INSERT OR UPDATE OR DELETE ON pgtrickle.relay_outbox_config
    FOR EACH ROW EXECUTE FUNCTION pgtrickle.relay_config_notify();

DROP TRIGGER IF EXISTS relay_inbox_config_notify ON pgtrickle.relay_inbox_config;
CREATE TRIGGER relay_inbox_config_notify
    AFTER INSERT OR UPDATE OR DELETE ON pgtrickle.relay_inbox_config
    FOR EACH ROW EXECUTE FUNCTION pgtrickle.relay_config_notify();

-- ── Relay SQL API functions ───────────────────────────────────────────────

-- RELAY-CAT: Upsert a forward pipeline (outbox → external sink).
CREATE OR REPLACE FUNCTION pgtrickle.set_relay_outbox(
    p_name            TEXT,
    p_outbox          TEXT,
    p_group           TEXT,
    p_sink            JSONB,
    p_retention_hours INT  DEFAULT 24,
    p_enabled         BOOLEAN DEFAULT true
) RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    v_sink_type TEXT;
    v_config    JSONB;
BEGIN
    -- Validate required sink key
    v_sink_type := p_sink ->> 'type';
    IF v_sink_type IS NULL OR v_sink_type = '' THEN
        RAISE EXCEPTION 'relay.invalid_config: sink JSONB must contain a "type" key '
              '(e.g. "nats", "kafka", "http", "redis", "sqs", "rabbitmq", "stdout", "pg-inbox"). '
              'Got: %', p_sink
            USING ERRCODE = 'raise_exception';
    END IF;

    -- Validate source type
    IF p_outbox IS NULL OR p_outbox = '' THEN
        RAISE EXCEPTION 'relay.invalid_config: outbox name must not be empty'
            USING ERRCODE = 'raise_exception';
    END IF;

    -- Build config JSONB
    v_config := jsonb_build_object(
        'source_type', 'outbox',
        'source',      jsonb_build_object(
                           'outbox',          p_outbox,
                           'group',           p_group,
                           'retention_hours', p_retention_hours
                       ),
        'sink_type',   v_sink_type,
        'sink',        p_sink
    );

    INSERT INTO pgtrickle.relay_outbox_config (name, enabled, config)
    VALUES (p_name, p_enabled, v_config)
    ON CONFLICT (name) DO UPDATE
        SET enabled = EXCLUDED.enabled,
            config  = EXCLUDED.config;
END;
$$;

COMMENT ON FUNCTION pgtrickle.set_relay_outbox(TEXT, TEXT, TEXT, JSONB, INT, BOOLEAN) IS
    'RELAY-CAT (v0.29.0): Upsert a forward relay pipeline (stream table outbox → external sink). '
    'Validates required sink.type key and outbox name. The outbox is enabled automatically '
    'by the relay binary if not already active. Triggers hot-reload via NOTIFY.';

-- RELAY-CAT: Upsert a reverse pipeline (external source → pg-trickle inbox).
CREATE OR REPLACE FUNCTION pgtrickle.set_relay_inbox(
    p_name              TEXT,
    p_inbox             TEXT,
    p_source            JSONB,
    p_max_retries       INT       DEFAULT 3,
    p_schedule          TEXT      DEFAULT '1s',
    p_with_dead_letter  BOOLEAN   DEFAULT true,
    p_retention_hours   INT       DEFAULT 24,
    p_enabled           BOOLEAN   DEFAULT true
) RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    v_source_type TEXT;
    v_config      JSONB;
BEGIN
    -- Validate required source key
    v_source_type := p_source ->> 'type';
    IF v_source_type IS NULL OR v_source_type = '' THEN
        RAISE EXCEPTION 'relay.invalid_config: source JSONB must contain a "type" key '
              '(e.g. "kafka", "nats", "http", "redis", "sqs", "rabbitmq", "stdin"). '
              'Got: %', p_source
            USING ERRCODE = 'raise_exception';
    END IF;

    -- Validate sink type
    IF p_inbox IS NULL OR p_inbox = '' THEN
        RAISE EXCEPTION 'relay.invalid_config: inbox name must not be empty'
            USING ERRCODE = 'raise_exception';
    END IF;

    -- Build config JSONB
    v_config := jsonb_build_object(
        'source_type',  v_source_type,
        'source',       p_source,
        'sink_type',    'pg-inbox',
        'sink',         jsonb_build_object(
                            'inbox',              p_inbox,
                            'max_retries',        p_max_retries,
                            'schedule',           p_schedule,
                            'with_dead_letter',   p_with_dead_letter,
                            'retention_hours',    p_retention_hours
                        )
    );

    INSERT INTO pgtrickle.relay_inbox_config (name, enabled, config)
    VALUES (p_name, p_enabled, v_config)
    ON CONFLICT (name) DO UPDATE
        SET enabled = EXCLUDED.enabled,
            config  = EXCLUDED.config;
END;
$$;

COMMENT ON FUNCTION pgtrickle.set_relay_inbox(TEXT, TEXT, JSONB, INT, TEXT, BOOLEAN, INT, BOOLEAN) IS
    'RELAY-CAT (v0.29.0): Upsert a reverse relay pipeline (external source → pg-trickle inbox). '
    'Validates required source.type key and inbox name. The inbox is created automatically '
    'by the relay binary if not already active. Triggers hot-reload via NOTIFY.';

-- RELAY-CAT: Enable a named pipeline (searches both tables).
CREATE OR REPLACE FUNCTION pgtrickle.enable_relay(
    p_name TEXT
) RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    v_found_outbox BOOLEAN;
    v_found_inbox  BOOLEAN;
BEGIN
    UPDATE pgtrickle.relay_outbox_config
       SET enabled = true
     WHERE name = p_name;
    GET DIAGNOSTICS v_found_outbox = ROW_COUNT;

    UPDATE pgtrickle.relay_inbox_config
       SET enabled = true
     WHERE name = p_name;
    GET DIAGNOSTICS v_found_inbox = ROW_COUNT;

    -- v_found_outbox / v_found_inbox hold 1 if updated, 0 if not
    IF v_found_outbox::int + v_found_inbox::int = 0 THEN
        RAISE EXCEPTION 'relay: pipeline "%" not found in relay_outbox_config or relay_inbox_config',
              p_name
            USING ERRCODE = 'no_data_found';
    END IF;
END;
$$;

COMMENT ON FUNCTION pgtrickle.enable_relay(TEXT) IS
    'RELAY-CAT (v0.29.0): Enable a relay pipeline by name. '
    'Searches both relay_outbox_config and relay_inbox_config. '
    'Raises no_data_found if no pipeline with that name exists.';

-- RELAY-CAT: Disable a named pipeline (searches both tables).
CREATE OR REPLACE FUNCTION pgtrickle.disable_relay(
    p_name TEXT
) RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    v_found_outbox BOOLEAN;
    v_found_inbox  BOOLEAN;
BEGIN
    UPDATE pgtrickle.relay_outbox_config
       SET enabled = false
     WHERE name = p_name;
    GET DIAGNOSTICS v_found_outbox = ROW_COUNT;

    UPDATE pgtrickle.relay_inbox_config
       SET enabled = false
     WHERE name = p_name;
    GET DIAGNOSTICS v_found_inbox = ROW_COUNT;

    IF v_found_outbox::int + v_found_inbox::int = 0 THEN
        RAISE EXCEPTION 'relay: pipeline "%" not found in relay_outbox_config or relay_inbox_config',
              p_name
            USING ERRCODE = 'no_data_found';
    END IF;
END;
$$;

COMMENT ON FUNCTION pgtrickle.disable_relay(TEXT) IS
    'RELAY-CAT (v0.29.0): Disable a relay pipeline by name. '
    'Searches both relay_outbox_config and relay_inbox_config. '
    'Raises no_data_found if no pipeline with that name exists.';

-- RELAY-CAT: Delete a named pipeline (searches both tables).
CREATE OR REPLACE FUNCTION pgtrickle.delete_relay(
    p_name TEXT
) RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    v_deleted_outbox INT;
    v_deleted_inbox  INT;
BEGIN
    DELETE FROM pgtrickle.relay_outbox_config WHERE name = p_name;
    GET DIAGNOSTICS v_deleted_outbox = ROW_COUNT;

    DELETE FROM pgtrickle.relay_inbox_config WHERE name = p_name;
    GET DIAGNOSTICS v_deleted_inbox = ROW_COUNT;

    IF v_deleted_outbox + v_deleted_inbox = 0 THEN
        RAISE EXCEPTION 'relay: pipeline "%" not found in relay_outbox_config or relay_inbox_config',
              p_name
            USING ERRCODE = 'no_data_found';
    END IF;
END;
$$;

COMMENT ON FUNCTION pgtrickle.delete_relay(TEXT) IS
    'RELAY-CAT (v0.29.0): Delete a relay pipeline by name. '
    'Searches both relay_outbox_config and relay_inbox_config. '
    'Raises no_data_found if no pipeline with that name exists.';

-- RELAY-CAT: Fetch config for a single named pipeline.
CREATE OR REPLACE FUNCTION pgtrickle.get_relay_config(
    p_name TEXT
) RETURNS TABLE (
    name       TEXT,
    direction  TEXT,
    enabled    BOOLEAN,
    config     JSONB
)
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
BEGIN
    RETURN QUERY
        SELECT r.name, 'forward'::TEXT, r.enabled, r.config
          FROM pgtrickle.relay_outbox_config r
         WHERE r.name = p_name
         UNION ALL
        SELECT r.name, 'reverse'::TEXT, r.enabled, r.config
          FROM pgtrickle.relay_inbox_config r
         WHERE r.name = p_name;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'relay: pipeline "%" not found in relay_outbox_config or relay_inbox_config',
              p_name
            USING ERRCODE = 'no_data_found';
    END IF;
END;
$$;

COMMENT ON FUNCTION pgtrickle.get_relay_config(TEXT) IS
    'RELAY-CAT (v0.29.0): Return the config for a single named relay pipeline. '
    'Returns: name TEXT, direction TEXT (''forward''/''reverse''), enabled BOOLEAN, config JSONB. '
    'Raises no_data_found if no pipeline with that name exists.';

-- RELAY-CAT: List all pipelines (both directions).
CREATE OR REPLACE FUNCTION pgtrickle.list_relay_configs()
RETURNS TABLE (
    name       TEXT,
    direction  TEXT,
    enabled    BOOLEAN,
    config     JSONB
)
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
BEGIN
    RETURN QUERY
        SELECT r.name, 'forward'::TEXT, r.enabled, r.config
          FROM pgtrickle.relay_outbox_config r
         UNION ALL
        SELECT r.name, 'reverse'::TEXT, r.enabled, r.config
          FROM pgtrickle.relay_inbox_config r
         ORDER BY name;
END;
$$;

COMMENT ON FUNCTION pgtrickle.list_relay_configs() IS
    'RELAY-CAT (v0.29.0): List all relay pipelines (both forward and reverse). '
    'Returns: name TEXT, direction TEXT (''forward''/''reverse''), enabled BOOLEAN, config JSONB. '
    'Ordered by pipeline name.';

-- ── Access control ────────────────────────────────────────────────────────

-- Create the relay role if it does not exist (idempotent).
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pgtrickle_relay') THEN
        CREATE ROLE pgtrickle_relay NOLOGIN;
    END IF;
END;
$$;

-- Revoke direct table access from the relay role; all mutations go through
-- the SECURITY DEFINER API functions.
REVOKE ALL ON pgtrickle.relay_outbox_config    FROM pgtrickle_relay;
REVOKE ALL ON pgtrickle.relay_inbox_config     FROM pgtrickle_relay;
REVOKE ALL ON pgtrickle.relay_consumer_offsets FROM pgtrickle_relay;

-- Grant execute on the API functions only.
GRANT EXECUTE ON FUNCTION pgtrickle.set_relay_outbox(TEXT, TEXT, TEXT, JSONB, INT, BOOLEAN)
    TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.set_relay_inbox(TEXT, TEXT, JSONB, INT, TEXT, BOOLEAN, INT, BOOLEAN)
    TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.enable_relay(TEXT)          TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.disable_relay(TEXT)         TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.delete_relay(TEXT)          TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.get_relay_config(TEXT)      TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.list_relay_configs()        TO pgtrickle_relay;
