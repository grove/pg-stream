-- pg_trickle 0.94.0 -> 0.95.0
-- Durable typed output-delta consumers.

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_output_delta_logs (
    pgt_id BIGINT PRIMARY KEY REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    database_instance_id TEXT NOT NULL,
    delta_relid OID NOT NULL,
    log_head BIGINT NOT NULL DEFAULT 0 CHECK (log_head >= 0),
    output_contract_digest BYTEA NOT NULL,
    row_identity_version SMALLINT NOT NULL CHECK (row_identity_version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_output_delta_batches (
    pgt_id BIGINT NOT NULL REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    database_instance_id TEXT NOT NULL,
    batch_token BIGINT NOT NULL CHECK (batch_token > 0),
    producing_refresh_id BIGINT NOT NULL,
    graph_refresh_id BIGINT,
    mode TEXT NOT NULL CHECK (mode IN ('EXACT', 'FULL_INVALIDATION')),
    row_count BIGINT NOT NULL CHECK (row_count >= 0),
    rows_inserted BIGINT NOT NULL CHECK (rows_inserted >= 0),
    rows_deleted BIGINT NOT NULL CHECK (rows_deleted >= 0),
    output_contract_digest BYTEA NOT NULL,
    row_identity_version SMALLINT NOT NULL CHECK (row_identity_version > 0),
    source_boundary_digest BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (pgt_id, batch_token)
);

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_output_delta_consumers (
    consumer_id UUID PRIMARY KEY,
    pgt_id BIGINT NOT NULL REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    database_instance_id TEXT NOT NULL,
    owner_oid OID NOT NULL,
    consumer_name TEXT NOT NULL CHECK (consumer_name <> ''),
    expected_contract_digest BYTEA NOT NULL,
    output_contract_digest BYTEA NOT NULL,
    row_identity_version SMALLINT NOT NULL CHECK (row_identity_version > 0),
    state TEXT NOT NULL CHECK (state IN ('ACTIVE', 'PAUSED', 'RESNAPSHOT_REQUIRED', 'INVALIDATED', 'DROPPED')),
    state_reason TEXT,
    acknowledged_batch_token BIGINT NOT NULL DEFAULT 0 CHECK (acknowledged_batch_token >= 0),
    acknowledged_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pgt_id, owner_oid, consumer_name)
);

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_output_delta_resnapshots (
    resnapshot_token UUID PRIMARY KEY,
    consumer_id UUID NOT NULL REFERENCES pgtrickle.pgt_output_delta_consumers(consumer_id) ON DELETE CASCADE,
    pgt_id BIGINT NOT NULL REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    log_head BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS pgt_output_delta_batches_pgt_token_idx
    ON pgtrickle.pgt_output_delta_batches (pgt_id, batch_token);
CREATE INDEX IF NOT EXISTS pgt_output_delta_consumers_pgt_state_idx
    ON pgtrickle.pgt_output_delta_consumers (pgt_id, state, acknowledged_batch_token);

CREATE OR REPLACE FUNCTION pgtrickle._reject_output_delta_contract_change()
RETURNS trigger LANGUAGE plpgsql
SET search_path = pgtrickle, pg_catalog, pg_temp
AS $$
BEGIN
    IF (OLD.pgt_relid, OLD.defining_query, OLD.refresh_mode, OLD.orchestration_mode,
        OLD.row_identity_version) IS DISTINCT FROM
       (NEW.pgt_relid, NEW.defining_query, NEW.refresh_mode, NEW.orchestration_mode,
        NEW.row_identity_version)
       AND EXISTS (SELECT 1 FROM pgtrickle.pgt_output_delta_consumers
                   WHERE pgt_id = OLD.pgt_id AND state <> 'DROPPED') THEN
        RAISE EXCEPTION 'PGT_EXT_CONSUMER_BLOCKED: active output-delta consumers require an explicit resnapshot'
            USING ERRCODE = '55006';
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS pgt_stream_tables_output_delta_contract_guard
    ON pgtrickle.pgt_stream_tables;
CREATE TRIGGER pgt_stream_tables_output_delta_contract_guard
    BEFORE UPDATE OF pgt_relid, defining_query, refresh_mode, orchestration_mode,
        row_identity_version ON pgtrickle.pgt_stream_tables
    FOR EACH ROW EXECUTE FUNCTION pgtrickle._reject_output_delta_contract_change();

CREATE OR REPLACE FUNCTION pgtrickle.register_output_delta_consumer(
    stream_table oid, consumer_name text, expected_contract_digest bytea,
    start_position text DEFAULT 'RESNAPSHOT_REQUIRED'
)
RETURNS TABLE (
    consumer_id uuid, stream_table text, consumer_name text, delta_relation text,
    acknowledged_batch_token bigint, output_contract_digest bytea,
    row_identity_version smallint, state text, state_reason text
)
STRICT SECURITY DEFINER
SET search_path TO pgtrickle, pg_catalog, pg_temp
LANGUAGE c AS 'MODULE_PATHNAME', 'register_output_delta_consumer_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle.output_delta_batches(
    consumer_id uuid, through_token bigint DEFAULT NULL
)
RETURNS TABLE (
    batch_token bigint, producing_refresh_id bigint, graph_refresh_id bigint,
    mode text, row_count bigint, rows_inserted bigint, rows_deleted bigint,
    output_contract_digest bytea, row_identity_version smallint,
    source_boundary_digest bytea, created_at timestamptz
)
STRICT SECURITY DEFINER
SET search_path TO pgtrickle, pg_catalog, pg_temp
LANGUAGE c AS 'MODULE_PATHNAME', 'output_delta_batches_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle.ack_output_delta(
    consumer_id uuid, through_token bigint, disposition text
)
RETURNS text STRICT SECURITY DEFINER
SET search_path TO pgtrickle, pg_catalog, pg_temp
LANGUAGE c AS 'MODULE_PATHNAME', 'ack_output_delta_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle.begin_output_delta_resnapshot(consumer_id uuid)
RETURNS TABLE (
    stream_table text, log_head bigint, output_contract_digest bytea,
    row_identity_version smallint, resnapshot_token uuid
)
STRICT SECURITY DEFINER
SET search_path TO pgtrickle, pg_catalog, pg_temp
LANGUAGE c AS 'MODULE_PATHNAME', 'begin_output_delta_resnapshot_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle.ack_output_delta_resnapshot(
    consumer_id uuid, resnapshot_token uuid
)
RETURNS text STRICT SECURITY DEFINER
SET search_path TO pgtrickle, pg_catalog, pg_temp
LANGUAGE c AS 'MODULE_PATHNAME', 'ack_output_delta_resnapshot_wrapper';

CREATE OR REPLACE FUNCTION pgtrickle.output_delta_consumer_status()
RETURNS TABLE (
    consumer_id uuid, stream_table text, consumer_name text, delta_relation text,
    state text, state_reason text, acknowledged_batch_token bigint,
    log_head bigint, batch_lag bigint, output_contract_digest bytea,
    row_identity_version smallint
)
STRICT SECURITY DEFINER
SET search_path TO pgtrickle, pg_catalog, pg_temp
LANGUAGE c AS 'MODULE_PATHNAME', 'output_delta_consumer_status_wrapper';

GRANT EXECUTE ON FUNCTION pgtrickle.register_output_delta_consumer(oid, text, bytea, text) TO PUBLIC;
GRANT EXECUTE ON FUNCTION pgtrickle.output_delta_batches(uuid, bigint) TO PUBLIC;
GRANT EXECUTE ON FUNCTION pgtrickle.ack_output_delta(uuid, bigint, text) TO PUBLIC;
GRANT EXECUTE ON FUNCTION pgtrickle.begin_output_delta_resnapshot(uuid) TO PUBLIC;
GRANT EXECUTE ON FUNCTION pgtrickle.ack_output_delta_resnapshot(uuid, uuid) TO PUBLIC;
GRANT EXECUTE ON FUNCTION pgtrickle.output_delta_consumer_status() TO PUBLIC;

INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('0.95.0', 'Durable typed output-delta consumers')
ON CONFLICT (version) DO NOTHING;
