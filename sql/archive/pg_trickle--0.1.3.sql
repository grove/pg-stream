-- pg_trickle v0.1.3 — archived SQL function baseline for upgrade validation.
--
-- This file is NOT the actual pgrx-generated install script (which was not
-- archived at release time). It is a synthetic baseline listing all SQL
-- functions that existed in v0.1.3, reconstructed from the source code at
-- tag v0.1.3. It is used by scripts/check_upgrade_completeness.sh to
-- determine which functions are NEW in a subsequent version and must appear
-- in the corresponding upgrade script.
--
-- Generated: 2026-03-04 from git tag v0.1.3

-- src/api.rs
CREATE FUNCTION pgtrickle."create_stream_table"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."alter_stream_table"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."drop_stream_table"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."resume_stream_table"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."refresh_stream_table"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."parse_duration_seconds"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."pgt_status"() RETURNS void LANGUAGE c AS '';

-- src/monitor.rs
CREATE FUNCTION pgtrickle."st_refresh_stats"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."get_refresh_history"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."st_auto_threshold"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."get_staleness"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."slot_health"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."explain_st"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."check_cdc_health"() RETURNS void LANGUAGE c AS '';

-- src/hooks.rs (sql = false — not directly in the SQL, but registered manually)
CREATE FUNCTION pgtrickle."_on_ddl_end"() RETURNS event_trigger LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."_on_sql_drop"() RETURNS event_trigger LANGUAGE c AS '';

-- src/hash.rs
CREATE FUNCTION pgtrickle."pg_trickle_hash"() RETURNS void LANGUAGE c AS '';
CREATE FUNCTION pgtrickle."pg_trickle_hash_multi"() RETURNS void LANGUAGE c AS '';

-- Event triggers (existed in v0.1.3)
CREATE EVENT TRIGGER pg_trickle_ddl_tracker ON ddl_command_end EXECUTE FUNCTION pgtrickle._on_ddl_end();
CREATE EVENT TRIGGER pg_trickle_drop_tracker ON sql_drop EXECUTE FUNCTION pgtrickle._on_sql_drop();

-- Views (existed in v0.1.3)
CREATE VIEW pgtrickle.stream_tables_info AS SELECT 1;
CREATE VIEW pgtrickle.pg_stat_stream_tables AS SELECT 1;
