-- pg_trickle 0.17.0 -> 0.18.0 upgrade script
--
-- CORR-3: pg_trickle_hash now accepts NULL input (returns a deterministic
-- sentinel hash instead of NULL). The function is no longer STRICT so that
-- rows with NULL group keys receive a non-NULL __pgt_row_id during both
-- FULL and DIFFERENTIAL refresh.
CREATE OR REPLACE FUNCTION pgtrickle."pg_trickle_hash"(
    "input" TEXT
) RETURNS bigint
IMMUTABLE PARALLEL SAFE
LANGUAGE c
AS 'MODULE_PATHNAME', 'pg_trickle_hash_wrapper';

-- UX-1: Template cache observability
CREATE FUNCTION pgtrickle."cache_stats"() RETURNS TABLE (
    "l1_hits" bigint,
    "l2_hits" bigint,
    "misses" bigint,
    "evictions" bigint,
    "l1_size" INT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'cache_stats_wrapper';

-- UX-4: Single-endpoint health summary
CREATE FUNCTION pgtrickle."health_summary"() RETURNS TABLE (
    "total_stream_tables" INT,
    "active_count" INT,
    "error_count" INT,
    "suspended_count" INT,
    "stale_count" INT,
    "reinit_pending" INT,
    "max_staleness_seconds" double precision,
    "scheduler_status" TEXT,
    "cache_hit_rate" double precision
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'health_summary_wrapper';
