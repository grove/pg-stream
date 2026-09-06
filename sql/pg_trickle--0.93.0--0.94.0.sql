-- pg_trickle 0.93.0 -> 0.94.0
-- Strict transactional graph refresh for externally coordinated graphs.

CREATE SEQUENCE IF NOT EXISTS pgtrickle.pgt_graph_refresh_id_seq;

CREATE OR REPLACE FUNCTION pgtrickle."refresh_graph_strict"(
    "roots" regclass[],
    "expected_graph_digest" bytea,
    "full_policy" text DEFAULT 'ALLOW'
)
RETURNS TABLE (
    "contract_version" smallint,
    "graph_refresh_id" bigint,
    "graph_digest" bytea,
    "source_boundary" jsonb,
    "source_boundary_digest" bytea,
    "node_results" jsonb
)
STRICT SECURITY DEFINER
SET search_path TO pgtrickle, pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'refresh_graph_strict_wrapper';

GRANT EXECUTE ON FUNCTION pgtrickle.refresh_graph_strict(regclass[], bytea, text) TO PUBLIC;

INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('0.94.0', 'Strict transactional graph refresh and source-boundary proofs')
ON CONFLICT (version) DO NOTHING;
