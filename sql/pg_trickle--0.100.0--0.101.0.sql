-- pg_trickle 0.100.0 -> 0.101.0 upgrade migration
--
-- v0.101.0 adds the durable registry for private INTERSECT/EXCEPT
-- multiplicity state. The state relations themselves are created lazily by
-- the refresh lifecycle and are extension-owned.

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_set_operation_states (
    pgt_id         BIGINT NOT NULL
                   REFERENCES pgtrickle.pgt_stream_tables(pgt_id)
                   ON DELETE CASCADE,
    node_ordinal   INTEGER NOT NULL,
    operation      TEXT NOT NULL CHECK (operation IN ('INTERSECT', 'EXCEPT')),
    is_all         BOOLEAN NOT NULL,
    state_relid    OID NOT NULL,
    schema_version SMALLINT NOT NULL,
    PRIMARY KEY (pgt_id, node_ordinal)
);

REVOKE ALL ON TABLE pgtrickle.pgt_set_operation_states FROM PUBLIC;
SELECT pg_catalog.pg_extension_config_dump('pgtrickle.pgt_set_operation_states', '');
