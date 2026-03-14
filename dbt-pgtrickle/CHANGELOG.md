# Changelog

All notable changes to the dbt-pgtrickle package will be documented in this file.

## [Unreleased]

### Added
- `pgtrickle_create_or_replace_stream_table()` adapter macro — wraps pg_trickle 0.6.0's idempotent DDL function
- `pgtrickle_has_create_or_replace()` utility macro — detects pg_trickle ≥ 0.6.0

### Changed
- `stream_table` materialization now uses `create_or_replace_stream_table()` when pg_trickle ≥ 0.6.0 is detected, with automatic fallback to the legacy check-then-decide pattern for older versions

## [0.1.0] - 2026-XX-XX

### Added
- Custom `stream_table` materialization
- SQL API wrapper macros (create, alter, drop, refresh)
- Utility macros (stream_table_exists, get_stream_table_info)
- Freshness monitoring via `pgtrickle_check_freshness` run-operation (raises error on breach)
- CDC health check via `pgtrickle_check_cdc_health` run-operation
- `pgtrickle_refresh` and `drop_all_stream_tables` run-operations
- `drop_all_stream_tables_force` for dropping all stream tables (including non-dbt)
- Integration test suite with seed data, polling helper, and query-change test
- CI pipeline (dbt 1.9–1.11 version matrix in main repo workflow)
