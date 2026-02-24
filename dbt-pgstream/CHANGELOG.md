# Changelog

All notable changes to the dbt-pgstream package will be documented in this file.

## [Unreleased]

## [0.1.0] - 2026-XX-XX

### Added
- Custom `stream_table` materialization
- SQL API wrapper macros (create, alter, drop, refresh)
- Utility macros (stream_table_exists, get_stream_table_info)
- Freshness monitoring via `pgstream_check_freshness` run-operation (raises error on breach)
- CDC health check via `pgstream_check_cdc_health` run-operation
- `pgstream_refresh` and `drop_all_stream_tables` run-operations
- `drop_all_stream_tables_force` for dropping all stream tables (including non-dbt)
- Integration test suite with seed data, polling helper, and query-change test
- CI pipeline (dbt 1.6-1.9 version matrix in main repo workflow)
