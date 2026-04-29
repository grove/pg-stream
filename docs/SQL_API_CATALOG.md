<!-- AUTO-GENERATED — do not edit by hand.
     Run `python3 scripts/gen_catalogs.py` to regenerate.
     CI fails if this file is out of date with source code. -->

# SQL API Reference — pg_trickle

**24 SQL-callable functions** discovered via `#[pg_extern]` in `src/`.

See [docs/SQL_REFERENCE.md](SQL_REFERENCE.md) for full signatures and examples.


| Function | Schema | Returns | Description |
|----------|--------|---------|-------------|
| `pgtrickle.cluster_worker_summary()` | `pgtrickle` | `TableIterator<` | Reads from `pg_stat_activity` (shared catalog) so the calling role needs `pg_monitor` or superuser privilege. |
| `pgtrickle.commit_offset()` | `pgtrickle` | `` | OUTBOX-B4 (v0.28.0): Commit the consumer's offset after successful processing. |
| `pgtrickle.consumer_heartbeat()` | `pgtrickle` | `` | OUTBOX-B5 (v0.28.0): Send a heartbeat from a consumer to signal liveness. |
| `pgtrickle.disable_inbox_ordering()` | `pgtrickle` | `` | INBOX-B1 (v0.28.0): Disable per-aggregate ordering for an inbox. |
| `pgtrickle.disable_inbox_priority()` | `pgtrickle` | `` | INBOX-B2 (v0.28.0): Disable priority-tier processing for an inbox. |
| `pgtrickle.disable_outbox()` | `pgtrickle` | `` | Drops the outbox table, delta-rows table, and latest view, and removes the catalog entry. |
| `pgtrickle.drop_consumer_group()` | `pgtrickle` | `` | OUTBOX-B2 (v0.28.0): Drop a consumer group and all associated offsets/leases. |
| `pgtrickle.drop_snapshot()` | `pgtrickle` | `` | Removes the snapshot table and its catalog row from `pgtrickle.pgt_snapshots`. |
| `pgtrickle.enable_inbox_ordering()` | `pgtrickle` | `` | Creates a `next_<inbox>` stream table using DISTINCT ON to surface only the next unprocessed message per aggregate, ordered by the sequence column. |
| `pgtrickle.enable_outbox()` | `pgtrickle` | `` | # Errors - `OutboxAlreadyEnabled` if outbox is already active for this ST. |
| `pgtrickle.inbox_health()` | `pgtrickle` | `pgrx::JsonB` | INBOX-4 (v0.28.0): Return a JSONB health summary for an inbox. |
| `pgtrickle.inbox_is_my_partition()` | `pgtrickle` | `bool` | Returns true when `aggregate_id` belongs to `worker_id`'s partition out of `total_workers`. |
| `pgtrickle.metrics_summary()` | `pgtrickle` | `TableIterator<` | v0.31.0 (PERF-3): Added `ivm_lock_parse_error_count` — cumulative count of IMMEDIATE-mode lock-mode downgrades due to query parse failures. |
| `pgtrickle.outbox_rows_consumed()` | `pgtrickle` | `` | This updates `last_drained_at` and `last_drained_count` in the catalog and deletes old claim-check delta rows to free storage. |
| `pgtrickle.outbox_status()` | `pgtrickle` | `pgrx::JsonB` | Includes: `enabled`, `outbox_table`, `row_count`, `oldest_row`, `newest_row`, `retention_hours`, `last_drained_at`, `last_drained_count`. |
| `pgtrickle.recommend_schedule()` | `pgtrickle` | `pgrx::JsonB` | PLAN-1 (v0.27.0): Return a schedule recommendation for the given stream table as a JSONB object with keys: `recommended_interval_seconds`, `peak_window_cron`, `confidence` (0–1), `reasoning`. |
| `pgtrickle.replay_inbox_messages()` | `pgtrickle` | `i64` | Returns the number of messages reset. |
| `pgtrickle.restore_from_snapshot()` | `pgtrickle` | `` | The stream table must already be registered. |
| `pgtrickle.restore_stream_tables()` | `pgtrickle` | `Result<(), crate::error::PgTrickleError>` | During a `pg_restore`, `pg_dump` will restore the base storage tables and the `pgtrickle.pgt_stream_tables` catalog, but the necessary CDC triggers and internal wiring will be missing. |
| `pgtrickle.schedule_recommendations()` | `pgtrickle` | `TableIterator<` | PLAN-2 (v0.27.0): Return one schedule recommendation row per registered stream table, sortable by `delta_pct DESC`. |
| `pgtrickle.seek_offset()` | `pgtrickle` | `` | OUTBOX-B4 (v0.28.0): Seek a consumer to a specific offset. |
| `pgtrickle.snapshot_stream_table()` | `pgtrickle` | `` | The snapshot table is created in the `pgtrickle` schema with the naming convention `snapshot_<name>_<epoch_ms>` unless `p_target` is given. |
| `pgtrickle.sql_handle_vp_promoted()` | `pgtrickle` | `bool` | Returns `true` if the payload was valid and a matching source was found; `false` if the payload was invalid or no source matched. |
| `pgtrickle.sql_stable_name_for_oid()` | `pgtrickle` | `Option<String>` | Returns `NULL` when the relation no longer exists (e.g. |
