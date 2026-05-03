<!-- AUTO-GENERATED — do not edit by hand.
     Run `python3 scripts/gen_catalogs.py` to regenerate.
     CI fails if this file is out of date with source code. -->

# SQL API Reference — pg_trickle

**100 SQL-callable functions** discovered via `#[pg_extern]` in `src/`.

See [docs/SQL_REFERENCE.md](SQL_REFERENCE.md) for full signatures and examples.


| Function | Schema | Returns | Description |
|----------|--------|---------|-------------|
| `pgtrickle._signal_launcher_rescan()` | `pgtrickle` | `` | Also safe to call manually if the launcher needs a nudge. |
| `pgtrickle.bootstrap_gate_status_fn()` | `pgtrickle` | `TableIterator<` | BOOT-F3: Designed for debugging "why isn't my stream table refreshing?" situations by showing the full gate lifecycle at a glance. |
| `pgtrickle.bulk_alter_stream_tables()` | `pgtrickle` | `i32` | # Example ```sql SELECT pgtrickle.bulk_alter_stream_tables(     ARRAY['public.orders_summary', 'public.daily_revenue'],     '{"schedule": "5m", "tier": "warm"}'::jsonb ); ```. |
| `pgtrickle.bulk_create()` | `pgtrickle` | `pgrx::JsonB` | On any error, the entire transaction is rolled back (standard PostgreSQL transactional semantics). |
| `pgtrickle.bulk_drop_stream_tables()` | `pgtrickle` | `i32` | # Example ```sql SELECT pgtrickle.bulk_drop_stream_tables(     ARRAY['public.orders_summary', 'public.stale_view'] ); ```. |
| `pgtrickle.cache_stats()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.cache_stats()`. |
| `pgtrickle.cdc_pause_status()` | `pgtrickle` | `TableIterator<` | Returns a table with one row containing: - `paused` — `true` when `cdc_paused = on` - `capture_mode` — `'discard'` or `'hold'` - `note` — human-readable explanation of the current state. |
| `pgtrickle.change_buffer_sizes()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.change_buffer_sizes()`. |
| `pgtrickle.check_cdc_health()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.check_cdc_health()`. |
| `pgtrickle.clear_caches()` | `pgtrickle` | `i64` | Use during debugging, emergency migration rollback, or after a query definition change that was not captured by the normal DDL invalidation path. |
| `pgtrickle.cluster_worker_summary()` | `pgtrickle` | `TableIterator<` | Reads from `pg_stat_activity` (shared catalog) so the calling role needs `pg_monitor` or superuser privilege. |
| `pgtrickle.commit_offset()` | `pgtrickle` | `` | OUTBOX-B4 (v0.28.0): Commit the consumer's offset after successful processing. |
| `pgtrickle.consumer_heartbeat()` | `pgtrickle` | `` | OUTBOX-B5 (v0.28.0): Send a heartbeat from a consumer to signal liveness. |
| `pgtrickle.convert_buffers_to_unlogged()` | `pgtrickle` | `Result<i64, PgTrickleError>` | **Warning:** After conversion, buffer contents will be lost on crash recovery. |
| `pgtrickle.dedup_stats_fn()` | `pgtrickle` | `TableIterator<` | Example: ```sql SELECT * FROM pgtrickle.dedup_stats(); ```. |
| `pgtrickle.dependency_tree()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.dependency_tree()`. |
| `pgtrickle.diamond_groups()` | `pgtrickle` | `TableIterator<` | Returns one row per group member, indicating which group it belongs to, whether it is a convergence (fan-in) node, the group's current epoch, and the effective schedule policy. |
| `pgtrickle.disable_inbox_ordering()` | `pgtrickle` | `` | INBOX-B1 (v0.28.0): Disable per-aggregate ordering for an inbox. |
| `pgtrickle.disable_inbox_priority()` | `pgtrickle` | `` | INBOX-B2 (v0.28.0): Disable priority-tier processing for an inbox. |
| `pgtrickle.disable_outbox()` | `pgtrickle` | `` | Drops the outbox table, delta-rows table, and latest view, and removes the catalog entry. |
| `pgtrickle.drain()` | `pgtrickle` | `` | # Example ```sql -- Quiesce before pg_upgrade or rolling restart: SELECT pgtrickle.drain(); -- Confirm drained: SELECT pgtrickle.is_drained(); -- Resume normal operation after maintenance: UPDATE pgtrickle.pgt_stream_tables SET status = status; -- noop, scheduler picks up ```. |
| `pgtrickle.drop_consumer_group()` | `pgtrickle` | `` | OUTBOX-B2 (v0.28.0): Drop a consumer group and all associated offsets/leases. |
| `pgtrickle.drop_inbox_impl()` | `pgtrickle` | `Result<(), PgTrickleError>` | If `p_cascade` is true, also drops the underlying inbox table. |
| `pgtrickle.drop_refresh_group()` | `pgtrickle` | `Result<(), PgTrickleError>` | Drop a refresh group by name. |
| `pgtrickle.drop_snapshot()` | `pgtrickle` | `` | Removes the snapshot table and its catalog row from `pgtrickle.pgt_snapshots`. |
| `pgtrickle.drop_stream_table()` | `pgtrickle` | `` | Changed in v0.19.0 (UX-6): default flipped from `true` to `false` to prevent accidental cascading drops. |
| `pgtrickle.drop_stream_table_publication()` | `pgtrickle` | `` | CDC-PUB-2: Drop the logical replication publication for a stream table. |
| `pgtrickle.drop_watermark_group()` | `pgtrickle` | `Result<(), PgTrickleError>` | Drop a watermark group by name. |
| `pgtrickle.enable_inbox_ordering()` | `pgtrickle` | `` | Creates a `next_<inbox>` stream table using DISTINCT ON to surface only the next unprocessed message per aggregate, ordered by the sequence column. |
| `pgtrickle.enable_outbox()` | `pgtrickle` | `` | # Errors - `OutboxAlreadyEnabled` if outbox is already active for this ST. |
| `pgtrickle.exec_stream_ddl()` | `pgtrickle` | `bool` | # Example ```sql SELECT pgtrickle.exec_stream_ddl(   'CREATE STREAM TABLE revenue AS SELECT SUM(amount) FROM orders' ); ```. |
| `pgtrickle.explain_dag()` | `pgtrickle` | `` | Node colours: user STs = blue, self-monitoring STs = green, suspended = red, fused = orange. |
| `pgtrickle.explain_diff_sql()` | `pgtrickle` | `Option<String>` | Exposed as `pgtrickle.explain_diff_sql(name)`. |
| `pgtrickle.explain_stream_table()` | `pgtrickle` | `Result<String, PgTrickleError>` | v0.39.0 extends the output to include: - Explicit DIFF/FULL fallback reason from the stream table catalog - Whether `force_full_refresh` GUC is overriding the mode - The effective refresh mode from the last completed refresh cycle - Whether the backpressure or CDC-pause state is active. |
| `pgtrickle.export_definition()` | `pgtrickle` | `Result<String, PgTrickleError>` | Returns a `DROP STREAM TABLE IF EXISTS` + `CREATE STREAM TABLE . |
| `pgtrickle.fuse_status()` | `pgtrickle` | `TableIterator<` | Returns one row per stream table with fuse configuration and state. |
| `pgtrickle.gate_source()` | `pgtrickle` | `Result<(), PgTrickleError>` | `source` is the source table name, optionally schema-qualified. |
| `pgtrickle.get_staleness()` | `pgtrickle` | `Option<f64>` |  |
| `pgtrickle.health_check()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.health_check()`. |
| `pgtrickle.health_summary()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.health_summary()`. |
| `pgtrickle.inbox_health()` | `pgtrickle` | `pgrx::JsonB` | INBOX-4 (v0.28.0): Return a JSONB health summary for an inbox. |
| `pgtrickle.inbox_is_my_partition()` | `pgtrickle` | `bool` | Returns true when `aggregate_id` belongs to `worker_id`'s partition out of `total_workers`. |
| `pgtrickle.is_drained()` | `pgtrickle` | `bool` | A scheduler is considered drained when `DRAIN_COMPLETED >= DRAIN_REQUESTED` in shared memory. |
| `pgtrickle.list_subscriptions()` | `pgtrickle` | `TableIterator<` | Returns a table with columns (stream_table TEXT, channel TEXT, created_at TIMESTAMPTZ). |
| `pgtrickle.metrics_summary()` | `pgtrickle` | `TableIterator<` | v0.31.0 (PERF-3): Added `ivm_lock_parse_error_count` — cumulative count of IMMEDIATE-mode lock-mode downgrades due to query parse failures. |
| `pgtrickle.migrate()` | `pgtrickle` | `String` | This is a convenience function for users who upgrade the extension without using `ALTER EXTENSION pg_trickle UPDATE` — it ensures the catalog schema matches the library expectations. |
| `pgtrickle.outbox_rows_consumed()` | `pgtrickle` | `` | This updates `last_drained_at` and `last_drained_count` in the catalog and deletes old claim-check delta rows to free storage. |
| `pgtrickle.outbox_status()` | `pgtrickle` | `pgrx::JsonB` | Includes: `enabled`, `outbox_table`, `row_count`, `oldest_row`, `newest_row`, `retention_hours`, `last_drained_at`, `last_drained_count`. |
| `pgtrickle.parse_duration_seconds()` | `pgtrickle` | `Option<i64>` | Used by SQL views to compare schedule. |
| `pgtrickle.pg_trickle_hash()` | `pgtrickle` | `i64` | NULL input is mapped to a deterministic sentinel (`\x00NULL\x00`) — the same encoding used by [`pg_trickle_hash_multi`] — so that rows with NULL-valued group keys receive a non-NULL `__pgt_row_id`. |
| `pgtrickle.pg_trickle_hash_multi()` | `pgtrickle` | `i64` | The hash output is identical to the previous xxh64-based implementation **except** that it now uses xxh3 which produces different numeric values. |
| `pgtrickle.pg_trickle_on_ddl_end()` | `pgtrickle` | `` | Registered via `extension_sql!()` in lib.rs as: ```sql CREATE FUNCTION pgtrickle._on_ddl_end() RETURNS event_trigger . |
| `pgtrickle.pg_trickle_on_sql_drop()` | `pgtrickle` | `` | Detects when upstream source tables or ST storage tables themselves are dropped and reacts accordingly. |
| `pgtrickle.pgt_ivm_handle_truncate()` | `pgtrickle` | `Result<(), PgTrickleError>` | Truncates the stream table (equivalent to a full refresh with empty base table for simple views). |
| `pgtrickle.pgt_scc_status()` | `pgtrickle` | `TableIterator<` | Returns one row per SCC, summarising its members, most recent fixpoint iteration count, and last convergence time. |
| `pgtrickle.pgt_status()` | `pgtrickle` | `TableIterator<` | Returns a summary row per stream table including schedule configuration, data timestamp, and computed staleness interval. |
| `pgtrickle.pgtrickle_refresh_stats()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.pgtrickle_refresh_stats()`. |
| `pgtrickle.preflight()` | `pgtrickle` | `String` | Returns a JSON string with one entry per check: `pass` (bool), `check` (name), `detail` (human-readable message). |
| `pgtrickle.rebuild_cdc_triggers()` | `pgtrickle` | `&'static str` | Returns `'done'` on success. |
| `pgtrickle.recommend_schedule()` | `pgtrickle` | `pgrx::JsonB` | PLAN-1 (v0.27.0): Return a schedule recommendation for the given stream table as a JSONB object with keys: `recommended_interval_seconds`, `peak_window_cron`, `confidence` (0–1), `reasoning`. |
| `pgtrickle.refresh_efficiency()` | `pgtrickle` | `Result<` | Returns operational metrics for each stream table: FULL vs DIFFERENTIAL timing, change ratios, speedup factor, and refresh counts. |
| `pgtrickle.refresh_groups_fn()` | `pgtrickle` | `TableIterator<` | Return all user-declared refresh groups with member details. |
| `pgtrickle.refresh_stream_table()` | `pgtrickle` | `` | Manually trigger a synchronous refresh of a stream table. |
| `pgtrickle.repair_stream_table()` | `pgtrickle` | `String` | Steps performed (actions taken are summarized in the return text): 1. |
| `pgtrickle.replay_inbox_messages()` | `pgtrickle` | `i64` | Returns the number of messages reset. |
| `pgtrickle.reset_fuse()` | `pgtrickle` | `` | Returns nothing on success; raises an ERROR if the stream table does not exist or the fuse is not blown. |
| `pgtrickle.restore_from_snapshot()` | `pgtrickle` | `` | The stream table must already be registered. |
| `pgtrickle.restore_stream_tables()` | `pgtrickle` | `Result<(), crate::error::PgTrickleError>` | During a `pg_restore`, `pg_dump` will restore the base storage tables and the `pgtrickle.pgt_stream_tables` catalog, but the necessary CDC triggers and internal wiring will be missing. |
| `pgtrickle.resume_stream_table()` | `pgtrickle` | `` | Resume a suspended stream table, clearing its consecutive error count and re-enabling automated and manual refreshes. |
| `pgtrickle.schedule_recommendations()` | `pgtrickle` | `TableIterator<` | PLAN-2 (v0.27.0): Return one schedule recommendation row per registered stream table, sortable by `delta_pct DESC`. |
| `pgtrickle.scheduler_overhead()` | `pgtrickle` | `TableIterator<` | Computes busy-time ratio, queue depth, avg dispatch latency, and the fraction of CPU spent on self-monitoring STs vs user STs from refresh history. |
| `pgtrickle.seek_offset()` | `pgtrickle` | `` | OUTBOX-B4 (v0.28.0): Seek a consumer to a specific offset. |
| `pgtrickle.self_monitoring_status()` | `pgtrickle` | `TableIterator<` | For each of the five expected DF stream tables, reports whether it exists, its current status, refresh mode, and last refresh time. |
| `pgtrickle.set_stream_table_sla()` | `pgtrickle` | `` | Accepts an interval and stores it as `freshness_deadline_ms`. |
| `pgtrickle.setup_self_monitoring()` | `pgtrickle` | `` | UX-2: Emits a warm-up hint if `pgt_refresh_history` has fewer than 50 rows. |
| `pgtrickle.shared_buffer_stats_fn()` | `pgtrickle` | `TableIterator<` | Example: ```sql SELECT * FROM pgtrickle.shared_buffer_stats(); ```. |
| `pgtrickle.sla_summary()` | `pgtrickle` | `TableIterator<` | Returns per-stream-table statistics: p50/p99 refresh latency, freshness lag, error rate, and remaining error budget. |
| `pgtrickle.slot_health()` | `pgtrickle` | `TableIterator<` | Returns trigger/slot name, source table, active status, retained WAL bytes, and the CDC mode (`trigger`, `wal`, or `transitioning`). |
| `pgtrickle.snapshot_stream_table()` | `pgtrickle` | `` | The snapshot table is created in the `pgtrickle` schema with the naming convention `snapshot_<name>_<epoch_ms>` unless `p_target` is given. |
| `pgtrickle.source_gates_fn()` | `pgtrickle` | `TableIterator<` | Only rows that have ever been gated appear in this view (one row per source_relid in `pgt_source_gates`). |
| `pgtrickle.sql_handle_vp_promoted()` | `pgtrickle` | `bool` | Returns `true` if the payload was valid and a matching source was found; `false` if the payload was invalid or no source matched. |
| `pgtrickle.sql_stable_name_for_oid()` | `pgtrickle` | `Option<String>` | Returns `NULL` when the relation no longer exists (e.g. |
| `pgtrickle.st_auto_threshold()` | `pgtrickle` | `Option<f64>` | Returns the per-ST `auto_threshold` if set, otherwise the global `pg_trickle.differential_max_change_ratio` GUC. |
| `pgtrickle.st_refresh_stats()` | `pgtrickle` | `TableIterator<` | This is the primary monitoring function, exposed as `pgtrickle.st_refresh_stats()`. |
| `pgtrickle.stream_table_to_publication()` | `pgtrickle` | `` | Creates a PostgreSQL publication exposing the named stream table so that Kafka Connect, Debezium, and other logical replication subscribers can receive change events without a separate replication slot. |
| `pgtrickle.subscribe()` | `pgtrickle` | `Result<(), PgTrickleError>` | The subscription is stored in `pgtrickle.pgt_subscriptions` and survives restarts. |
| `pgtrickle.teardown_self_monitoring()` | `pgtrickle` | `` | Safe with partial setups: each table is dropped individually, and missing tables are silently skipped (STAB-5). |
| `pgtrickle.trigger_inventory()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.trigger_inventory()`. |
| `pgtrickle.ungate_source()` | `pgtrickle` | `Result<(), PgTrickleError>` | `source` is the source table name, optionally schema-qualified. |
| `pgtrickle.unsubscribe()` | `pgtrickle` | `Result<(), PgTrickleError>` | UX-SUB: Remove a NOTIFY subscription for a stream table / channel pair. |
| `pgtrickle.version()` | `pgtrickle` | `&'static str` |  |
| `pgtrickle.version_check()` | `pgtrickle` | `String` | Returns a JSON string with library_version, extension_version, pg_version, and a boolean `version_match`. |
| `pgtrickle.view_evolution_status()` | `pgtrickle` | `TableIterator<` | During a zero-downtime schema evolution (ALTER STREAM TABLE), pg_trickle builds the new definition in a shadow table. |
| `pgtrickle.wal_source_status()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.wal_source_status()`. |
| `pgtrickle.watermark_groups_fn()` | `pgtrickle` | `TableIterator<` | Return all watermark group definitions. |
| `pgtrickle.watermark_status_fn()` | `pgtrickle` | `TableIterator<` | Shows per-group lag, whether the group is currently aligned, and the effective minimum watermark. |
| `pgtrickle.watermarks_fn()` | `pgtrickle` | `TableIterator<` | Return the current watermark state for all registered sources. |
| `pgtrickle.worker_allocation_status_fn()` | `pgtrickle` | `TableIterator<` | Columns: - `db_name`: The current database name. |
| `pgtrickle.worker_pool_status()` | `pgtrickle` | `TableIterator<` | Exposed as `pgtrickle.worker_pool_status()`. |
| `pgtrickle.write_and_refresh()` | `pgtrickle` | `` | Calling `pgtrickle.write_and_refresh(sql, name)` guarantees the refresh sees the writes from `sql` because both run in the same transaction. |
