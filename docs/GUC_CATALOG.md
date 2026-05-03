<!-- AUTO-GENERATED — do not edit by hand.
     Run `python3 scripts/gen_catalogs.py` to regenerate.
     CI fails if this file is out of date with source code. -->

# GUC Reference — pg_trickle

**132 configuration parameters** extracted from `src/config.rs`.

See [docs/CONFIGURATION.md](CONFIGURATION.md) for full descriptions and usage examples.


| GUC name | Type | Default | Description |
|----------|------|---------|-------------|
| `(registration pending — PGS_ADAPTIVE_BATCH_COALESCING)` | `bool` | `true` | Disable if the batched query plan is unexpectedly slow (rare). |
| `(registration pending — PGS_ADAPTIVE_MERGE_STRATEGY)` | `bool` | `false` | Default `false` — the fixed `merge_strategy` GUC governs. |
| `(registration pending — PGS_AGGREGATE_FAST_PATH)` | `bool` | `true` | B-1: Aggregate fast-path — use explicit DML instead of MERGE for GROUP BY queries where all aggregates are algebraically invertible (COUNT, SUM, AVG, etc.). |
| `(registration pending — PGS_AGG_DIFF_CARDINALITY_THRESHOLD)` | `i32` | `1000` | Set to 0 to disable the cardinality warning. |
| `(registration pending — PGS_ALGEBRAIC_DRIFT_RESET_CYCLES)` | `i32` | `0` | Set to 0 to disable periodic drift reset (default). |
| `(registration pending — PGS_ALLOW_CIRCULAR)` | `bool` | `false` | When `false` (default), cycle detection rejects any stream table creation that would introduce a cycle in the dependency graph. |
| `(registration pending — PGS_ANALYZE_BEFORE_DELTA)` | `bool` | `true` | When enabled, `ANALYZE pgtrickle_changes.changes_<oid>` is run before the delta SQL is executed. |
| `(registration pending — PGS_AUTO_BACKOFF)` | `bool` | `true` | This prevents CPU runaway when a stream table's refresh cost exceeds its schedule budget and an operator is not available to respond manually. |
| `(registration pending — PGS_AUTO_INDEX)` | `bool` | `true` | When enabled, `create_stream_table()` automatically creates indexes on GROUP BY keys, DISTINCT columns, and adds INCLUDE clauses to the `__pgt_row_id` index for stream tables with ≤ 8 output columns. |
| `(registration pending — PGS_BACKPRESSURE_CONSECUTIVE_LIMIT)` | `i32` | `3` | Default: 3 cycles. |
| `(registration pending — PGS_BLOCK_SOURCE_DDL)` | `bool` | `true` | Default is `true` (enabled) as of v0.11.0 — set to `false` to restore the previous permissive behavior (DDL triggers reinitialization instead of blocking). |
| `(registration pending — PGS_BUFFER_ALERT_THRESHOLD)` | `i32` | `1000000` | When any source table's change buffer exceeds this number of rows, a `BufferGrowthWarning` alert is emitted. |
| `(registration pending — PGS_BUFFER_PARTITIONING)` | `Option\<std::ffi::CString` | `"off"` | Controls whether change buffer tables use `PARTITION BY RANGE (lsn)`: - `"off"` (default): Unpartitioned heap tables (current behaviour). |
| `(registration pending — PGS_CDC_CAPTURE_MODE)` | `Option\<std::ffi::CString` | `"discard"` | Use `pgtrickle.cdc_capture_mode()` to inspect the active mode at runtime. |
| `(registration pending — PGS_CDC_MODE)` | `Option\<std::ffi::CString` | `"auto"` | - `"auto"` (default): Use triggers for creation, transition to WAL if   `wal_level = logical` is available. |
| `(registration pending — PGS_CDC_PAUSED)` | `bool` | `false` | Default: `false` (CDC writes are enabled). |
| `(registration pending — PGS_CDC_TRIGGER_MODE)` | `Option\<std::ffi::CString` | `"statement"` | Changing this GUC takes effect for newly created stream tables. |
| `(registration pending — PGS_CHANGE_BUFFER_DURABILITY)` | `Option\<std::ffi::CString` | `"unlogged"` | This GUC supersedes `pg_trickle.unlogged_buffers` (which is now a compatibility alias: `true` maps to `"unlogged"`, `false` to `"logged"`). |
| `(registration pending — PGS_CHANGE_BUFFER_SCHEMA)` | `Option\<std::ffi::CString` | `"pgtrickle_changes"` | Schema name for change buffer tables. |
| `(registration pending — PGS_CITUS_ST_LOCK_LEASE_MS)` | `i32` | `60000` | Default: 60 000 ms (60 seconds). |
| `(registration pending — PGS_CITUS_WORKER_RETRY_TICKS)` | `i32` | `5` | Default: 5 ticks. |
| `(registration pending — PGS_CLEANUP_USE_TRUNCATE)` | `bool` | `true` | Set to false if the TRUNCATE AccessExclusiveLock on the change buffer is problematic for concurrent DML on the source table. |
| `(registration pending — PGS_COLUMNAR_BACKEND)` | `Option\<std::ffi::CString` | `"none"` | When set, `create_stream_table()` uses the specified columnar backend and routes differential refresh to the `delete_insert` strategy (columnar backends are append-only). |
| `(registration pending — PGS_COMPACT_THRESHOLD)` | `i32` | `100000` | Set to 0 to disable compaction. |
| `(registration pending — PGS_CONNECTION_POOLER_MODE)` | `Option\<std::ffi::CString` | `"off"` | Overrides the per-ST `pooler_compatibility_mode` for all stream tables. |
| `(registration pending — PGS_CONSUMER_CLEANUP_ENABLED)` | `bool` | `true` | OUTBOX-B5 (v0.28.0): Enable/disable automatic cleanup of dead/stale consumers. |
| `(registration pending — PGS_CONSUMER_DEAD_THRESHOLD_HOURS)` | `i32` | `24` | OUTBOX-B5 (v0.28.0): Hours of no heartbeat after which a consumer is considered dead. |
| `(registration pending — PGS_CONSUMER_STALE_OFFSET_THRESHOLD_DAYS)` | `i32` | `7` | OUTBOX-B5 (v0.28.0): Days of no progress after which a consumer offset is considered stale. |
| `(registration pending — PGS_COST_MODEL_SAFETY_MARGIN)` | `f64` | `0.8` | Default 0.8 — DIFFERENTIAL is chosen unless it's estimated to cost more than 80% of FULL. |
| `(registration pending — PGS_DEEP_JOIN_L0_SCAN_THRESHOLD)` | `i32` | `4` | Default: 4 (matches the previously hardcoded `DEEP_JOIN_L0_SCAN_THRESHOLD`). |
| `(registration pending — PGS_DEFAULT_SCHEDULE_SECONDS)` | `i32` | `1` | Default effective schedule (in seconds) for isolated CALCULATED stream tables that have no downstream dependents. |
| `(registration pending — PGS_DELTA_AMPLIFICATION_THRESHOLD)` | `f64` | `100.0` | Set to 0.0 to disable amplification detection. |
| `(registration pending — PGS_DELTA_ENABLE_NESTLOOP)` | `bool` | `true` | When enabled, `SET LOCAL enable_nestloop = off` is applied inside `execute_delta_sql` before running the generated delta SQL. |
| `(registration pending — PGS_DELTA_WORK_MEM)` | `i32` | `0` | Set to 0 (default) to inherit the session `work_mem`. |
| `(registration pending — PGS_DELTA_WORK_MEM_CAP_MB)` | `i32` | `0` | Set to 0 to disable the cap (default — no limit enforced). |
| `(registration pending — PGS_DIFFERENTIAL_MAX_CHANGE_RATIO)` | `f64` | `0.15` | Set to 0.0 to disable adaptive fallback (always use DIFFERENTIAL). |
| `(registration pending — PGS_DIFF_OUTPUT_FORMAT)` | `Option\<std::ffi::CString` | `"split"` | Controls how the DI-2 aggregate UPDATE-split surfaces changes: - `"split"` (default): Emit DELETE+INSERT pairs for aggregate UPDATEs. |
| `(registration pending — PGS_DRAIN_TIMEOUT)` | `i32` | `60` | Default: 60 seconds. |
| `(registration pending — PGS_ENABLED)` | `bool` | `true` | Master enable/disable switch for the extension. |
| `(registration pending — PGS_ENABLE_TRACE_PROPAGATION)` | `bool` | `false` | F10 (v0.37.0): Enable W3C Trace Context propagation through the refresh pipeline. |
| `(registration pending — PGS_ENABLE_VECTOR_AGG)` | `bool` | `false` | F4 (v0.37.0): Enable pgVectorMV — incremental vector aggregate operators. |
| `(registration pending — PGS_ENFORCE_BACKPRESSURE)` | `bool` | `false` | Default: `false` (alerts only, no throttling). |
| `(registration pending — PGS_FORCE_FULL_REFRESH)` | `bool` | `false` | Useful for SRE diagnosis when a cluster-wide `refresh_strategy = 'full'` still has DIFFERENTIAL STs due to explicit per-ST row values. |
| `(registration pending — PGS_FOREIGN_TABLE_POLLING)` | `bool` | `false` | When enabled, foreign tables used in DIFFERENTIAL / IMMEDIATE mode defining queries will be supported via a snapshot-comparison approach: before each refresh cycle the scheduler materializes a snapshot of the foreign table into a local shadow table, then computes EXCEPT ALL deltas against the previous snapshot. |
| `(registration pending — PGS_FRONTIER_HOLDBACK_MODE)` | `Option\<std::ffi::CString` | `"xmin"` | \| Value \| Meaning \| \|-------\|---------\| \| `"xmin"` (default) \| Probe `pg_stat_activity` + `pg_prepared_xacts` once per tick and cap the frontier to the safe upper bound. |
| `(registration pending — PGS_FRONTIER_HOLDBACK_WARN_SECONDS)` | `i32` | `60` | Set to 0 to disable the warning (not recommended for production). |
| `(registration pending — PGS_FUSE_DEFAULT_CEILING)` | `i32` | `0` | Set to 0 to disable the global default ceiling (per-ST ceiling only). |
| `(registration pending — PGS_HISTORY_PRUNE_INTERVAL_SECONDS)` | `i32` | `60` | Default: 60 seconds. |
| `(registration pending — PGS_HISTORY_RETENTION_DAYS)` | `i32` | `90` | The scheduler runs a daily cleanup that deletes rows from `pgtrickle.pgt_refresh_history` older than this many days. |
| `(registration pending — PGS_INBOX_DLQ_ALERT_MAX_PER_REFRESH)` | `i32` | `10` | INBOX-7 (v0.28.0): Maximum DLQ alerts raised per refresh cycle (0 = disabled). |
| `(registration pending — PGS_INBOX_DLQ_RETENTION_HOURS)` | `i32` | `0` | INBOX-1 (v0.28.0): Default retention (hours) for dead-letter queue rows (0 = keep forever). |
| `(registration pending — PGS_INBOX_DRAIN_BATCH_SIZE)` | `i32` | `500` | INBOX-1 (v0.28.0): Batch size for inbox drain operations. |
| `(registration pending — PGS_INBOX_DRAIN_INTERVAL_SECONDS)` | `i32` | `60` | INBOX-1 (v0.28.0): Seconds between inbox background drain sweeps (0 = disabled). |
| `(registration pending — PGS_INBOX_ENABLED)` | `bool` | `true` | INBOX-1 (v0.28.0): Master enable/disable for the transactional inbox feature. |
| `(registration pending — PGS_INBOX_PROCESSED_RETENTION_HOURS)` | `i32` | `72` | INBOX-1 (v0.28.0): Default retention (hours) for successfully processed inbox messages. |
| `(registration pending — PGS_IVM_RECURSIVE_MAX_DEPTH)` | `i32` | `100` | Set to 0 to disable the depth guard (allow unlimited recursion). |
| `(registration pending — PGS_IVM_TOPK_MAX_LIMIT)` | `i32` | `1000` | TopK queries with `LIMIT > threshold` are rejected in IMMEDIATE mode because inline recomputation of large result sets adds unacceptable latency to the trigger path. |
| `(registration pending — PGS_IVM_USE_ENR)` | `bool` | `false` | When false, the legacy temp-table copy behaviour is used. |
| `(registration pending — PGS_LOG_DELTA_SQL)` | `bool` | `false` | **Do not enable in production** — every refresh will emit potentially large SQL strings to the server log. |
| `(registration pending — PGS_LOG_FORMAT)` | `Option\<std::ffi::CString` | `"text"` | - `"text"` (default): Unstructured human-readable messages via `pgrx::log!()`. |
| `(registration pending — PGS_LOG_MERGE_SQL)` | `bool` | `false` | Intended for debugging MERGE query generation only. |
| `(registration pending — PGS_MATVIEW_POLLING)` | `bool` | `false` | When `true`, materialized views referenced in DIFFERENTIAL/IMMEDIATE defining queries will be supported via a snapshot-comparison approach (same mechanism as foreign table polling). |
| `(registration pending — PGS_MAX_BUFFER_ROWS)` | `i32` | `1000000` | Set to 0 to disable the limit. |
| `(registration pending — PGS_MAX_CHANGE_BUFFER_ALERT_ROWS)` | `i32` | `0` | Set to 0 to disable (default). |
| `(registration pending — PGS_MAX_CONCURRENT_REFRESHES)` | `i32` | `4` | Maximum number of concurrent refresh workers. |
| `(registration pending — PGS_MAX_CONSECUTIVE_ERRORS)` | `i32` | `3` | Maximum consecutive errors before auto-suspending a stream table. |
| `(registration pending — PGS_MAX_DELTA_ESTIMATE_ROWS)` | `i32` | `0` | Set to 0 to disable the estimation check (default). |
| `(registration pending — PGS_MAX_DYNAMIC_REFRESH_WORKERS)` | `i32` | `4` | This is distinct from `pg_trickle.max_concurrent_refreshes`, which is the per-database dispatch cap. |
| `(registration pending — PGS_MAX_FIXPOINT_ITERATIONS)` | `i32` | `100` | When stream tables form a cyclic dependency (circular reference), the scheduler iterates to a fixed point. |
| `(registration pending — PGS_MAX_GROUPING_SET_BRANCHES)` | `i32` | `64` | Maximum allowed grouping set branches for CUBE/ROLLUP expansion (EC-02). |
| `(registration pending — PGS_MAX_PARALLEL_WORKERS)` | `i32` | `0` | Default 0 = serial mode (existing behavior preserved). |
| `(registration pending — PGS_MAX_PARSE_DEPTH)` | `i32` | `64` | Prevents stack-overflow crashes on pathological queries with deeply nested subqueries, CTEs, or set operations. |
| `(registration pending — PGS_MAX_PARSE_NODES)` | `i32` | `0` | Queries that exceed this limit are rejected with `QueryTooComplex` to prevent unbounded memory allocation in the parse advisory warnings cache and CTE registry. |
| `(registration pending — PGS_MERGE_JOIN_STRATEGY)` | `Option\<std::ffi::CString` | `"auto"` | Controls the join strategy hint applied via `SET LOCAL` during MERGE: - `"auto"` (default): delta-size heuristics choose the strategy. |
| `(registration pending — PGS_MERGE_PLANNER_HINTS)` | `bool` | `true` | Deprecated — use `pg_trickle.planner_aggressive` instead. |
| `(registration pending — PGS_MERGE_SEQSCAN_THRESHOLD)` | `f64` | `0.001` | Set to 0.0 to disable this optimization. |
| `(registration pending — PGS_MERGE_STRATEGY)` | `Option\<std::ffi::CString` | `"auto"` | The former `"delete_insert"` value was removed in v0.19.0 (CORR-1). |
| `(registration pending — PGS_MERGE_STRATEGY_THRESHOLD)` | `f64` | `0.01` | Default: 0.01 (1%). |
| `(registration pending — PGS_MERGE_WORK_MEM_MB)` | `i32` | `64` | A higher value lets PostgreSQL use larger hash tables for the MERGE join, avoiding disk-spilling sort/merge strategies on large deltas. |
| `(registration pending — PGS_METRICS_PORT)` | `i32` | `0` | Example: ```sql ALTER SYSTEM SET pg_trickle.metrics_port = 9188; SELECT pg_reload_conf(); ```. |
| `(registration pending — PGS_METRICS_REQUEST_TIMEOUT_MS)` | `i32` | `5000` | Protects the scheduler from a slow client stalling the tick loop. |
| `(registration pending — PGS_MIN_SCHEDULE_SECONDS)` | `i32` | `1` | Minimum allowed schedule in seconds. |
| `(registration pending — PGS_NOTIFY_COALESCE_MS)` | `i32` | `250` | Default: 250 ms. |
| `(registration pending — PGS_ONLINE_SCHEMA_EVOLUTION)` | `bool` | `false` | Default: `false` (standard ALTER QUERY reinit behaviour). |
| `(registration pending — PGS_OTEL_ENDPOINT)` | `Option\<std::ffi::CString` | `None` | F10 (v0.37.0): OTLP/gRPC endpoint for OpenTelemetry span export. |
| `(registration pending — PGS_OUTBOX_CLAIM_CHECK_BATCH_SIZE)` | `i32` | `1000` | OUTBOX-4 (v0.28.0): Batch size for claim-check acknowledgement processing. |
| `(registration pending — PGS_OUTBOX_DRAIN_BATCH_SIZE)` | `i32` | `500` | OUTBOX-1 (v0.28.0): Batch size for outbox drain operations. |
| `(registration pending — PGS_OUTBOX_DRAIN_INTERVAL_SECONDS)` | `i32` | `60` | OUTBOX-1 (v0.28.0): Seconds between background outbox drain sweeps (0 = disabled). |
| `(registration pending — PGS_OUTBOX_ENABLED)` | `bool` | `true` | OUTBOX-1 (v0.28.0): Master enable/disable for the transactional outbox feature. |
| `(registration pending — PGS_OUTBOX_FORCE_RETENTION)` | `bool` | `false` | OUTBOX-1 (v0.28.0): When true, the outbox retains rows beyond retention_hours if any consumer group has not yet consumed them. |
| `(registration pending — PGS_OUTBOX_INLINE_THRESHOLD_ROWS)` | `i32` | `10000` | OUTBOX-3 (v0.28.0): Maximum delta rows before switching to claim-check mode. |
| `(registration pending — PGS_OUTBOX_RETENTION_HOURS)` | `i32` | `24` | OUTBOX-1 (v0.28.0): Default outbox row retention in hours. |
| `(registration pending — PGS_OUTBOX_SKIP_EMPTY_DELTA)` | `bool` | `true` | OUTBOX-3 (v0.28.0): When true, skip writing an outbox row for empty-delta refreshes. |
| `(registration pending — PGS_OUTBOX_STORAGE_CRITICAL_MB)` | `i32` | `1024` | OUTBOX-5 (v0.28.0): Storage threshold (MB) at which outbox is considered critical. |
| `(registration pending — PGS_PARALLEL_REFRESH_MODE)` | `Option\<std::ffi::CString` | `"on"` | - `"on"` (default as of v0.11.0): Enable true parallel refresh via   dynamic workers. |
| `(registration pending — PGS_PART3_MAX_SCAN_COUNT)` | `i32` | `5` | Default: 5 (matches the previously hardcoded `PART3_MAX_SCAN_COUNT`). |
| `(registration pending — PGS_PER_DATABASE_WORKER_QUOTA)` | `i32` | `0` | Set to 0 (default) to disable per-database quotas — all databases share `max_dynamic_refresh_workers` on a first-come-first-served basis, bounded per coordinator by `max_concurrent_refreshes`. |
| `(registration pending — PGS_PLANNER_AGGRESSIVE)` | `bool` | `true` | Replaces the old `merge_planner_hints` and `merge_work_mem_mb` GUCs (both still accepted but emit deprecation warnings). |
| `(registration pending — PGS_PREDICTION_MIN_SAMPLES)` | `i32` | `5` | When fewer than this many data points exist, the predictor falls back to the existing fixed-threshold logic. |
| `(registration pending — PGS_PREDICTION_RATIO)` | `f64` | `1.5` | When `predicted_diff_ms > last_full_ms × prediction_ratio`, the scheduler overrides the strategy to FULL refresh. |
| `(registration pending — PGS_PREDICTION_WINDOW)` | `i32` | `60` | The forecaster fits `duration_ms ~ delta_rows` over this many minutes of `pgt_refresh_history` data per stream table. |
| `(registration pending — PGS_PUBLICATION_LAG_WARN_BYTES)` | `i32` | `0` | Set to 0 to disable subscriber lag tracking (default). |
| `(registration pending — PGS_REFRESH_STRATEGY)` | `Option\<std::ffi::CString` | `"auto"` | This GUC is a cluster-wide override. |
| `(registration pending — PGS_SCHEDULER_INTERVAL_MS)` | `i32` | `1000` | Scheduler wake interval in milliseconds. |
| `(registration pending — PGS_SCHEDULE_ALERT_COOLDOWN_SECONDS)` | `i32` | `300` | Prevents alert spam when the cost model consistently predicts SLA breach. |
| `(registration pending — PGS_SCHEDULE_RECOMMENDATION_MIN_SAMPLES)` | `i32` | `20` | When fewer samples are available, `confidence` is returned as 0.0 and the recommendation fields are NULL or conservative defaults. |
| `(registration pending — PGS_SLA_WINDOW_HOURS)` | `i32` | `24` | Default: 24 hours. |
| `(registration pending — PGS_SLOT_LAG_CRITICAL_THRESHOLD_MB)` | `i32` | `1024` | When a WAL-mode source retains more than this amount of WAL, `pgtrickle.check_cdc_health()` reports a `slot_lag_exceeds_threshold` alert for the source. |
| `(registration pending — PGS_SLOT_LAG_WARNING_THRESHOLD_MB)` | `i32` | `100` | When a WAL-mode source retains more than this amount of WAL, pg_trickle: - emits a `slot_lag_warning` NOTIFY event from the scheduler, and - reports a WARN row in `pgtrickle.health_check()`. |
| `(registration pending — PGS_SPILL_CONSECUTIVE_LIMIT)` | `i32` | `3` | When a stream table accumulates this many consecutive differential refreshes where `temp_blks_written > spill_threshold_blocks`, the scheduler marks the ST for reinitialization (FULL refresh) on the next cycle. |
| `(registration pending — PGS_SPILL_THRESHOLD_BLOCKS)` | `i32` | `0` | Set to 0 to disable spill detection (default). |
| `(registration pending — PGS_TEMPLATE_CACHE)` | `bool` | `true` | In transaction-pooling mode, rely on L2 rather than L0 warm-up for cross-connection performance. |
| `(registration pending — PGS_TEMPLATE_CACHE_MAX_AGE_HOURS)` | `i32` | `168` | Prevents stale entries accumulating after ALTER QUERY without DROP or source-OID renumbering. |
| `(registration pending — PGS_TEMPLATE_CACHE_MAX_ENTRIES)` | `i32` | `0` | When the cache reaches this size, the least-recently-used entry is evicted. |
| `(registration pending — PGS_TEMPORAL_STREAM_TABLES)` | `bool` | `false` | Default: `false` (standard non-temporal storage). |
| `(registration pending — PGS_TICK_WATERMARK_ENABLED)` | `bool` | `true` | Disable only if you need stream tables to always advance to the very latest available LSN regardless of cross-source consistency. |
| `(registration pending — PGS_TIERED_SCHEDULING)` | `bool` | `true` | Default changed to `true` in v0.12.0 (PERF-3) — prevents large deployments from wasting CPU refreshing cold STs at full speed. |
| `(registration pending — PGS_TRACE_ID)` | `Option\<std::ffi::CString` | `None` | F10 (v0.37.0): Session-level W3C traceparent header for trace context propagation. |
| `(registration pending — PGS_UNLOGGED_BUFFERS)` | `bool` | `false` | Default `false` — change buffers remain WAL-logged and crash-safe. |
| `(registration pending — PGS_USER_TRIGGERS)` | `Option\<std::ffi::CString` | `"auto"` | - `"auto"` (default): Detect user-defined row-level triggers on the   stream table and automatically use explicit DML (DELETE + UPDATE +   INSERT) so triggers fire with correct `TG_OP`, `OLD`, and `NEW`. |
| `(registration pending — PGS_USE_PREPARED_STATEMENTS)` | `bool` | `true` | Disable if prepared-statement parameter sniffing produces poor plans (e.g., highly skewed LSN distributions). |
| `(registration pending — PGS_USE_SQLSTATE_CLASSIFICATION)` | `bool` | `true` | The SQLSTATE-based classification is locale-safe: it works correctly regardless of `lc_messages`. |
| `(registration pending — PGS_VOLATILE_FUNCTION_POLICY)` | `Option\<std::ffi::CString` | `"reject"` | Controls how volatile functions in defining queries are handled: - `"reject"` (default): Error — volatile functions are rejected. |
| `(registration pending — PGS_WAKE_DEBOUNCE_MS)` | `i32` | `10` | **Note:** `pg_trickle.event_driven_wake` is deprecated and has no effect. |
| `(registration pending — PGS_WAL_MAX_CHANGES_PER_POLL)` | `i32` | `10000` | Default: 10 000. |
| `(registration pending — PGS_WAL_TRANSITION_TIMEOUT)` | `i32` | `300` | Maximum time (seconds) to wait for the WAL decoder to catch up during transition from triggers to WAL-based CDC before falling back to triggers. |
| `(registration pending — PGS_WATERMARK_HOLDBACK_TIMEOUT)` | `i32` | `0` | Set to 0 to disable stuck-watermark detection (default). |
| `(registration pending — PGS_WORKER_POOL_SIZE)` | `i32` | `0` | Set to 0 (default) to use the existing spawn-per-task model. |
| `pg_trickle.enabled` | `i32` | `65536` | Default: 65 536 (64 KiB). |
| `pg_trickle.enabled` | `i32` | `256` | Default: 256. |
| `pg_trickle.enabled` | `i32` | `128` | Default: 128. |
| `pg_trickle.enabled` | `bool` | `false` | Off by default — use static quotas. |
