# Changelog

What's new in pg_trickle — written for everyone, not just developers.

For future plans and upcoming features, see [ROADMAP.md](ROADMAP.md).

## Table of Contents

<!-- TOC start -->
- [0.33.0 — Citus: Distributed Source CDC & Stream Tables](#0330--citus-distributed-source-cdc--stream-tables)
- [0.32.0 — Citus: Stable Naming & Per-Source Frontier Foundation](#0320--citus-stable-naming--per-source-frontier-foundation)
- [0.31.0 — Performance & Scheduler Intelligence](#0310--performance--scheduler-intelligence)
- [0.30.0 — Pre-GA Correctness & Stability Sprint](#0300--pre-ga-correctness--stability-sprint)
- [0.29.0 — Relay CLI (pgtrickle-relay)](#0290--relay-cli-pgtrickle-relay)
- [0.28.0 — Transactional Inbox & Outbox Patterns](#0280--transactional-inbox--outbox-patterns)
- [0.27.0 — Operability, Observability & DR](#0270--operability-observability--dr)
- [0.26.0 — Test & Concurrency Hardening](#0260--test--concurrency-hardening)
- [0.25.0 — Scheduler Scalability & Pooler Performance](#0250--scheduler-scalability--pooler-performance)
- [0.24.0 — Join Correctness & Durability Hardening](#0240--join-correctness--durability-hardening)
- [0.23.0 — Performance Tuning & Diagnostics](#0230--performance-tuning--diagnostics)
- [0.22.0 — Downstream CDC, Parallel Refresh & Predictive Cost Model](#0220--downstream-cdc-parallel-refresh--predictive-cost-model)
- [0.21.0 — Reliability, Safety & Operational Tools](#0210--reliability-safety--operational-tools)
- [0.20.0 — Self Monitoring](#0200--self-monitoring)
- [0.19.0 — Security, Scheduler Performance & Operator Convenience](#0190--security-scheduler-performance--operator-convenience)
- [0.18.0 — Hardening & Delta Performance](#0180--hardening--delta-performance)
- [0.17.0 — Query Intelligence & Stability](#0170--query-intelligence--stability)
- [0.16.0 — Performance & Refresh Optimization](#0160--performance--refresh-optimization)
- [0.15.0 — Interactive TUI, Bulk Create & Runaway-Refresh Protection](#0150--interactive-tui-bulk-create--runaway-refresh-protection)
- [0.14.0 — Tiered Scheduling, Diagnostics & TUI](#0140--tiered-scheduling-diagnostics--tui)
- [0.13.0 — Scalability Foundations & Full TPC-H Coverage](#0130--scalability-foundations--full-tpc-h-coverage)
- [0.12.0 — Join Correctness, Diagnostics & Reliability](#0120--join-correctness-diagnostics--reliability)
- [0.11.0 — Event-Driven Latency, Chain IVM & Observability Stack](#0110--event-driven-latency-chain-ivm--observability-stack)
- [0.10.0 — Cloud Deployment, PgBouncer & Query Engine Correctness](#0100--cloud-deployment-pgbouncer--query-engine-correctness)
- [0.9.0 — Incremental Aggregates & Smarter Scheduling](#090--incremental-aggregates--smarter-scheduling)
- [0.8.0 — Backup, Pooler Compatibility & Reliability](#080--backup-pooler-compatibility--reliability)
- [0.7.0 — Watermark Gating, Circular Pipelines & SQL Broadening](#070--watermark-gating-circular-pipelines--sql-broadening)
- [0.6.0 — Idempotent DDL, Partitioned Sources & dbt Integration](#060--idempotent-ddl-partitioned-sources--dbt-integration)
- [0.5.0 — Row-Level Security, Source Gating & Append-Only Fast Path](#050--row-level-security-source-gating--append-only-fast-path)
- [0.4.0 — Parallel Refresh & Statement-Level CDC Triggers](#040--parallel-refresh--statement-level-cdc-triggers)
- [0.3.0 — Incremental Correctness & Security Tooling](#030--incremental-correctness--security-tooling)
- [0.2.3 — Per-Table CDC Mode & WAL Lag Monitoring](#023--per-table-cdc-mode--wal-lag-monitoring)
- [0.2.2 — AUTO Refresh Mode & Query Alteration](#022--auto-refresh-mode--query-alteration)
- [0.2.1 — Safe Upgrades & Scheduling Improvements](#021--safe-upgrades--scheduling-improvements)
- [0.2.0 — Monitoring, IMMEDIATE Mode & Diamond Consistency](#020--monitoring-immediate-mode--diamond-consistency)
- [0.1.3 — TPC-H Correctness, Window Functions & Aggregate Fixes](#013--tpc-h-correctness-window-functions--aggregate-fixes)
- [0.1.2 — Incremental Correctness Fixes & Project Rename](#012--incremental-correctness-fixes--project-rename)
- [0.1.1 — CloudNativePG Image & Test Hardening](#011--cloudnativepg-image--test-hardening)
- [0.1.0 — Initial Release](#010--initial-release)
<!-- TOC end -->

---

## [0.33.0] — Citus: Distributed Source CDC & Stream Tables

This release delivers world-class incremental view maintenance over Citus
distributed tables, and aligns with pg_ripple v0.58.0 Citus sharding support.
pg_trickle can now track changes on distributed source tables and write results
to distributed output tables, while leaving all non-Citus code paths completely
unchanged.

### pg_ripple Citus Co-location Helper

#### New: `pgtrickle.handle_vp_promoted(payload TEXT) → BOOLEAN`

Processes a `pg_ripple.vp_promoted` NOTIFY payload emitted by pg_ripple
v0.58.0 when a VP table is distributed via Citus.  Call this from any
regular backend session that is LISTENing to `pg_ripple.vp_promoted`:

```sql
LISTEN "pg_ripple.vp_promoted";
-- … receive notification …
SELECT pgtrickle.handle_vp_promoted(:'NOTIFY_PAYLOAD');
```

The function:
- Parses the payload JSON (`table`, `shard_count`, `shard_table_prefix`,
  `predicate_id`).
- Logs the promotion details.
- When the promoted table matches an active distributed CDC source in
  `pgt_change_tracking`, signals the scheduler to probe worker slots on the
  next tick without a full catalog scan.
- Returns `true` if a matching source was found, `false` otherwise.

`docs/integrations/citus.md` gains a new **pg_ripple Integration** section
covering co-location DDL, the `vp_promoted` notification contract, and
guidance on aligning `pgt_st_locks` lease expiry with
`pg_ripple.merge_fence_timeout_ms`.

### Distributed stream table output

`create_stream_table()` gains a new optional parameter
`output_distribution_column`. When provided, and Citus is installed, the
output storage table is converted to a Citus distributed table on that column
immediately after creation. Existing call sites without the parameter are
unaffected.

```sql
-- Co-locate the stream table with the source shards
CALL pgtrickle.create_stream_table(
    name                       => 'orders_summary',
    query                      => 'SELECT customer_id, count(*) FROM orders GROUP BY 1',
    output_distribution_column => 'customer_id'
);
```

### Per-worker WAL slot tracking (`pgt_worker_slots`)

A new catalog table `pgtrickle.pgt_worker_slots` records the logical
replication slot name and last-consumed frontier for each Citus worker node
per source table. This enables per-worker CDC polling and accurate lag
monitoring across all nodes in the cluster.

### Cross-node refresh coordination (`pgt_st_locks`)

A new catalog table `pgtrickle.pgt_st_locks` provides lightweight distributed
mutex semantics using `INSERT … ON CONFLICT DO NOTHING`. This replaces
advisory locks for distributed stream table refreshes, ensuring that only one
coordinator node applies changes at a time across a multi-coordinator Citus
setup.

### Citus observability view (`citus_status`)

`SELECT * FROM pgtrickle.citus_status` returns one row per
(stream table, source, worker) combination, showing the coordinator slot,
worker slot name, last consumed LSN, and source placement type. Use this view
to monitor replication lag and detect unreachable workers.

```sql
SELECT pgt_name, worker_name, worker_port, worker_slot, worker_frontier
FROM pgtrickle.citus_status;
```

### Correct apply path for distributed stream tables

Citus blocks cross-shard `MERGE` statements. pg_trickle now automatically
detects distributed output stream tables and switches to a
`DELETE + INSERT … ON CONFLICT DO UPDATE` apply path, which Citus supports
natively. Single-node and reference-table stream tables continue to use the
existing `MERGE` path.

### Pre-flight checks for Citus clusters

Two new pre-flight check functions are available via the Rust API:

- `check_citus_version_compat()` — verifies that all worker nodes are running
  the same pg_trickle version as the coordinator. Returns an error listing any
  mismatched workers.
- `check_worker_wal_levels()` — verifies that `wal_level = logical` is
  configured on all worker nodes. Returns an error if any worker has a lower
  WAL level, preventing silent slot-creation failures.

### Per-worker CDC helpers

The `poll_worker_slot_changes()` function drains a logical replication slot on
a remote Citus worker via `dblink` and writes the decoded changes into the
coordinator's local change buffer. The `ensure_worker_slot()` function creates
the slot if it does not already exist, making the setup idempotent on every
scheduler tick.

### Citus integration guide

A new documentation page at `docs/integrations/citus.md` covers prerequisites,
installation, placement options, the observability view, known failure modes
(unreachable workers, recycled WAL slots, shard rebalancing), and performance
considerations.

### Upgrade

Run the standard extension upgrade. The migration script adds the three new
catalog objects (`pgt_st_locks`, `pgt_worker_slots`, `citus_status`) and
replaces the three `create_stream_table` function signatures with versions that
include the new `output_distribution_column` parameter. Existing call sites
without the new parameter continue to work without change.

```sql
ALTER EXTENSION pg_trickle UPDATE TO '0.33.0';
```

---

## [0.32.0] — Citus: Stable Naming & Per-Source Frontier Foundation

This release lays the foundation for world-class Citus support by replacing
OID-based internal object names with stable hash-derived names and adding
Citus cluster detection helpers.

### Stable internal object naming

pg_trickle now names every internal object (change buffer tables, trigger
functions, WAL replication slots, publication names) using a short 16-character
hex string derived from the schema-qualified source table name:

```
changes_a3f7b2c1d0e5f9a8       -- was: changes_12345
pgt_cdc_fn_a3f7b2c1d0e5f9a8    -- was: pgt_cdc_fn_12345
pgtrickle_a3f7b2c1d0e5f9a8     -- was: pgtrickle_12345
```

This name is identical on every Citus node, survives `pg_dump`/restore cycles,
and survives OID reassignment after a major-version upgrade. Existing
installations are upgraded automatically by the migration script — all existing
objects are renamed in a single transaction with no downtime.

The change is invisible to end users: no SQL API changes, no configuration
changes, no behaviour changes on single-node PostgreSQL.

### Citus cluster detection

A new internal module (`src/citus.rs`) provides helpers to detect whether Citus
is loaded and how a given source table is distributed (local, reference, or
distributed). This information is stored in the catalog and will drive per-node
CDC and apply strategies in v0.35.0.

### New catalog columns

Three catalog tables gain new columns:

- `pgtrickle.pgt_stream_tables`: `st_placement TEXT DEFAULT 'local'`
- `pgtrickle.pgt_dependencies`: `source_stable_name TEXT`, `source_placement TEXT DEFAULT 'local'`
- `pgtrickle.pgt_change_tracking`: `source_stable_name TEXT`, `source_placement TEXT DEFAULT 'local'`, `frontier_per_node JSONB`

### New SQL function

`pgtrickle.source_stable_name(oid) → TEXT` — returns the 16-character stable
hash for any source relation by OID. Useful for diagnostics.

### Upgrade notes

The `0.31.0 → 0.32.0` migration script handles all object renames
automatically. Replication slots are renamed if the PostgreSQL version is 15+;
on older versions a manual rename step is logged as a NOTICE. Existing change
buffer data is preserved — only the table and function names change.

---

## [0.31.0] — Performance & Scheduler Intelligence

This release delivers measurable performance improvements for deployments with
many stream tables, along with new tools for monitoring scheduler behaviour
and reacting to processing backlogs before they become a problem.

### Faster immediate-mode updates

Stream tables configured in immediate mode — which update on every data change
rather than on a schedule — now handle those changes more efficiently.
Previously, every single data change caused PostgreSQL to create and destroy a
temporary table in the background, a fixed cost that adds up at high write
rates. That overhead has been eliminated.

This improvement is opt-in. Enable it with `pg_trickle.ivm_use_enr = true`
(requires PostgreSQL 18+).

### Fewer database round-trips for shared sources

When multiple stream tables all read from the same source table, pg_trickle
now scans their pending changes in a single database pass instead of once per
stream table. If you have ten stream tables all watching the same `orders`
table, pg_trickle makes one read instead of ten. The benefit scales with the
number of stream tables. This is on by default.

### Smarter update-strategy hints

Every refresh, pg_trickle chooses between two strategies for applying changes:
a merge approach (efficient for small change sets) and a delete-then-reinsert
approach (faster when large portions of the data have changed). Enabling
`pg_trickle.adaptive_merge_strategy` now logs a suggestion after each refresh
indicating whether the current strategy is optimal, based on the ratio of
changes to total rows. This makes performance tuning straightforward — no
restarts or code changes required.

### Silent fallbacks are now visible

When pg_trickle encounters a problem analysing certain query types, it falls
back to a slower, more conservative update mode. Previously this was invisible.
The count of such fallbacks is now tracked and surfaced in
`pgtrickle.metrics_summary()` under `ivm_lock_parse_error_count`, so you can
spot and address the underlying cause.

### Back-pressure alerts for overloaded pipelines

If data is arriving faster than pg_trickle can process it, the change buffer
grows. pg_trickle now watches this and, after 3 consecutive cycles above the
alert threshold (configurable via `pg_trickle.backpressure_consecutive_limit`),
raises a `change_buffer_backpressure` alert. Applications or monitoring systems
can listen for this event and respond — for example by slowing producers or
adding consumers.

### Coming soon: cross-database refresh coordination

A detailed design for a future cross-database refresh coordinator has been
published in `docs/research/multi_db_refresh_broker.md`. Implementation is
planned for v0.32.0.

### What changed

- Error messages are now categorised by standard SQL error code by default,
  making them easier to parse in automated monitoring. The previous behaviour
  can be restored with `pg_trickle.use_sqlstate_classification = false`.

### New settings

| Setting | Default | What it does |
|---------|---------|--------------|
| `pg_trickle.ivm_use_enr` | off | Eliminate temporary-table overhead in immediate mode (PostgreSQL 18+ only) |
| `pg_trickle.adaptive_batch_coalescing` | on | Scan change buffers for shared sources in a single pass |
| `pg_trickle.adaptive_merge_strategy` | off | Log update-strategy suggestions after each refresh |
| `pg_trickle.backpressure_consecutive_limit` | 3 | Consecutive over-threshold cycles before raising a back-pressure alert |

### Upgrade

Run `ALTER EXTENSION pg_trickle UPDATE TO '0.31.0';` — no manual changes
required. The faster immediate-mode path is opt-in; set
`pg_trickle.ivm_use_enr = true` to enable it.

---

## [0.30.0] — Pre-GA Correctness & Stability Sprint

This release is focused entirely on correctness and stability in preparation
for the 1.0 release. There are no new user-facing features — every change is
a fix, a safety guard, or a memory efficiency improvement.

### Fixed: phantom rows in join-based stream tables

Stream tables that join multiple source tables could silently accumulate stale
rows over time when a refresh was interrupted part-way through. Those rows are
now cleaned up automatically after every refresh, ensuring the result always
converges to the correct answer.

### Fixed: incorrect results for complex query patterns

Subqueries nested inside `CASE` expressions, `COALESCE` calls, and function
arguments are now correctly detected and handled. Previously, stream tables
using these patterns could produce wrong incremental refresh results.

### Safer snapshots

Snapshot creation and restore are now fully atomic. If anything goes wrong
mid-operation — a disk error, a timeout, a lost connection — the operation is
cleanly rolled back and no partial tables are left behind.

Restoring from a snapshot no longer relies on PostgreSQL's internal column
ordering, making restores safe across different PostgreSQL minor versions.

### Bounded memory for in-flight update data

The internal cache that stores update data between steps was previously
unbounded. On deployments with many stream tables, it could grow large over
time. The cache now enforces a configurable maximum and evicts the oldest
entries when full, keeping memory usage predictable.

Additionally, cached query templates now expire after a configurable age
(default: 7 days). Old plans are automatically removed during background
maintenance, preventing stale query plans from accumulating.

### Complexity cap for queries

A new `pg_trickle.max_parse_nodes` setting lets you cap query complexity.
Queries that exceed the limit are rejected immediately with a clear error
instead of consuming unexpected memory.

### New settings

| Setting | Default | What it does |
|---------|---------|--------------|
| `pg_trickle.use_sqlstate_classification` | off | Categorise errors by SQL error code (useful for automated retry logic) |
| `pg_trickle.template_cache_max_age_hours` | 168 (7 days) | Evict cached query plans older than this |
| `pg_trickle.max_parse_nodes` | 0 (disabled) | Reject queries that exceed this complexity limit |

### Upgrade

No schema changes. Upgrade from v0.29.0 with:

```sql
ALTER EXTENSION pg_trickle UPDATE TO '0.30.0';
```

---

## [0.29.0] — Relay CLI (pgtrickle-relay)

This release introduces `pgtrickle-relay` — a standalone companion tool that
connects pg_trickle to the outside world.

### What is pgtrickle-relay?

The relay bridges pg_trickle's inbox and outbox tables with external messaging
systems, handling the reliable "last mile" of getting data in and out of your
database.

- **Forward (outbox → external):** Watches your pg_trickle outbox tables and
  forwards new records to external systems as they arrive. Supported
  destinations include Kafka, NATS, HTTP webhooks, Redis Streams, AWS SQS,
  RabbitMQ, and plain text output.
- **Reverse (external → inbox):** Reads messages from external systems and
  writes them into your pg_trickle inbox tables, enabling fully bidirectional
  event-driven pipelines.

### Configured entirely through SQL

There are no YAML files or config files to manage. You set up and manage relay
pipelines with SQL:

| Function | What it does |
|----------|-------------|
| `pgtrickle.set_relay_outbox(...)` | Configure an outbox-to-external pipeline |
| `pgtrickle.set_relay_inbox(...)` | Configure an external-to-inbox pipeline |
| `pgtrickle.enable_relay(name)` | Start a relay pipeline |
| `pgtrickle.disable_relay(name)` | Pause a relay pipeline |
| `pgtrickle.delete_relay(name)` | Remove a relay pipeline |
| `pgtrickle.list_relay_configs()` | List all configured pipelines |

### Built for reliability

- **No duplicate messages:** Every destination uses a deduplication key to
  prevent the same message from being delivered more than once, even if the
  relay restarts mid-send.
- **High availability:** Multiple relay instances can run simultaneously and
  coordinate automatically using database-level locks — no external
  coordination service such as ZooKeeper or Redis is needed.
- **Live config updates:** Change relay configuration in SQL and it takes
  effect within seconds, with no restart.
- **Built-in monitoring:** Health check at `/health` and Prometheus metrics
  at `/metrics` (port 9090 by default).

### Upgrade notes

```sql
ALTER EXTENSION pg_trickle UPDATE TO '0.29.0';
```

The relay binary is distributed separately (see `Dockerfile.relay`). Existing
stream tables, views, and outbox/inbox APIs are unchanged.

---

## [0.28.0] — Transactional Inbox & Outbox Patterns

This release adds two complementary patterns for reliably integrating
pg_trickle with external systems.

### The problem these patterns solve

When you update a database and need to notify an external system — a message
queue, an API, a downstream service — you face a reliability challenge: what
happens if the database update succeeds but the notification fails? You can end
up with data in your database that the external system never heard about, or a
notification sent for a change that was rolled back.

The **outbox pattern** solves this: the notification is written in the same
database transaction as the data change, so they either both succeed or both
fail. pg_trickle then delivers the notification reliably once the transaction
has committed.

The **inbox pattern** is the reverse: external messages arrive into a managed
queue inside PostgreSQL, where they can be processed reliably, retried on
failure, and replayed if needed.

### Outbox

Enable the outbox on any stream table with `pgtrickle.enable_outbox()`. After
each refresh, pg_trickle writes a record to a dedicated outbox table. Your
application or the relay tool picks it up from there and forwards it to
external consumers.

Consumers can work in named **consumer groups** — similar to Kafka consumer
groups. Each consumer tracks its own position in the stream independently and
can be replayed, paused, or have its lease extended without affecting others.

| Function | What it does |
|----------|-------------|
| `pgtrickle.enable_outbox(name, retention_hours)` | Start capturing refresh output for external delivery |
| `pgtrickle.disable_outbox(name)` | Stop capturing |
| `pgtrickle.outbox_status(name)` | See the current outbox state |
| `pgtrickle.outbox_rows_consumed(stream_table, outbox_id)` | Acknowledge that records have been delivered |
| `pgtrickle.create_consumer_group(name, outbox, ...)` | Create a named group of consumers |
| `pgtrickle.drop_consumer_group(name)` | Remove a consumer group |
| `pgtrickle.poll_outbox(group, consumer, batch_size, ...)` | Claim the next batch of records |
| `pgtrickle.commit_offset(group, consumer, last_offset)` | Acknowledge processed records |
| `pgtrickle.extend_lease(group, consumer, ...)` | Hold onto a batch longer before it times out |
| `pgtrickle.seek_offset(group, consumer, new_offset)` | Jump to a specific position (for replay) |
| `pgtrickle.consumer_heartbeat(group, consumer)` | Signal that a consumer is still alive |
| `pgtrickle.consumer_lag(group)` | See how far behind each consumer is |

### Inbox

Create a named inbox with `pgtrickle.create_inbox()`. pg_trickle automatically
sets up a pending queue, a dead-letter queue (for messages that could not be
processed), and a stats table.

| Function | What it does |
|----------|-------------|
| `pgtrickle.create_inbox(name, ...)` | Create a managed inbox with pending queue and dead-letter queue |
| `pgtrickle.drop_inbox(name, ...)` | Remove an inbox |
| `pgtrickle.enable_inbox_tracking(name, ...)` | Attach inbox tracking to an existing table |
| `pgtrickle.inbox_health(name)` | Get a health summary for an inbox |
| `pgtrickle.inbox_status(name)` | Show queue depths and processing stats |
| `pgtrickle.replay_inbox_messages(name, event_ids)` | Reset specific messages for re-processing |

**Additional inbox capabilities:**

- **Ordered processing:** `pgtrickle.enable_inbox_ordering()` ensures messages
  for the same entity (e.g. the same customer or order ID) are processed in
  sequence, eliminating race conditions without any extra coordination in your
  application.
- **Priority tiers:** `pgtrickle.enable_inbox_priority()` marks messages as
  high or low priority so the scheduler processes urgent messages first.
- **Horizontal scaling:** `pgtrickle.inbox_is_my_partition()` provides
  consistent hash-based partition assignment for multi-worker inbox processing.
  Multiple workers can safely share an inbox without an external coordinator.
- **Gap detection:** `pgtrickle.inbox_ordering_gaps()` surfaces any sequence
  gaps per entity so you can detect and recover from missing messages.

### New settings

| Setting | Default | What it does |
|---------|---------|--------------|
| `pg_trickle.outbox_enabled` | on | Enable the outbox subsystem |
| `pg_trickle.outbox_retention_hours` | 24 | How long to keep delivered outbox records |
| `pg_trickle.outbox_drain_batch_size` | 1000 | Records to process per drain pass |
| `pg_trickle.outbox_skip_empty_delta` | on | Skip writing an outbox record when there are no changes |
| `pg_trickle.consumer_dead_threshold_hours` | 24 | Hours before a silent consumer is considered dead |
| `pg_trickle.inbox_enabled` | on | Enable the inbox subsystem |
| `pg_trickle.inbox_processed_retention_hours` | 72 | How long to keep processed inbox records |
| `pg_trickle.inbox_dlq_alert_max_per_refresh` | 10 | Alert when this many messages land in the dead-letter queue in one cycle |

### Upgrade

```sql
ALTER EXTENSION pg_trickle UPDATE TO '0.28.0';
```

---

## [0.27.0] — Operability, Observability & DR

This release focuses on three areas: disaster recovery tooling, better
visibility into multi-database deployments, and a more reliable built-in
metrics server.

### Snapshot and restore

You can now export a stream table's current data to an archive table and
restore it later. This is useful for bootstrapping a new read replica without
a full database dump, taking a point-in-time snapshot before a risky migration,
or recovering a stream table to a known-good state.

| Function | What it does |
|----------|-------------|
| `pgtrickle.snapshot_stream_table(name, target)` | Export a stream table to an archive table |
| `pgtrickle.restore_from_snapshot(name, source)` | Restore from an archive table |
| `pgtrickle.list_snapshots(name)` | List available snapshots with size and age |
| `pgtrickle.drop_snapshot(snapshot_table)` | Delete a snapshot |

Restore aligns the stream table's internal progress marker with the snapshot,
so incremental refresh resumes correctly without any manual steps.

### Predictive schedule recommendations

pg_trickle now analyses its own refresh history and recommends optimal refresh
intervals for each stream table.

- `pgtrickle.recommend_schedule(name)` returns a suggested interval and a
  confidence score. Confidence is low on new deployments and rises as history
  accumulates (at least 20 samples are needed before the score is meaningful).
- `pgtrickle.schedule_recommendations()` returns recommendations for all stream
  tables in one call.
- A **`predicted_sla_breach`** alert fires when the model predicts the next
  refresh is likely to miss your freshness target by more than 20%. The alert
  fires at most once every 5 minutes by default, to avoid flooding.

### Cluster-wide worker visibility

In deployments that run pg_trickle across multiple databases,
`pgtrickle.cluster_worker_summary()` shows which databases are consuming
background workers. This makes it easy to diagnose situations where one
database is crowding out others.

All Prometheus metrics now include database-level labels, so you can split a
single Grafana panel by database.

### Metrics server improvements

- A new `pgtrickle.metrics_summary()` SQL function returns cluster-wide refresh
  and error counts — useful for monitoring without a Prometheus scraper.
- Port conflicts now produce a clear error message instead of failing silently.
- Malformed HTTP requests now return a proper `400 Bad Request` response.

### New settings

| Setting | Default | What it does |
|---------|---------|--------------|
| `pg_trickle.schedule_recommendation_min_samples` | 20 | Minimum history samples before schedule confidence is meaningful |
| `pg_trickle.schedule_alert_cooldown_seconds` | 300 | Minimum seconds between consecutive `predicted_sla_breach` alerts |
| `pg_trickle.metrics_request_timeout_ms` | 5000 | Maximum time the metrics server waits for a request (ms) |

### Upgrade

This release upgrades the internal pgrx library to 0.18.0. This is transparent
to users. Run:

```sql
ALTER EXTENSION pg_trickle UPDATE TO '0.27.0';
```

---

## [0.26.0] — Test & Concurrency Hardening

This release is all about making pg_trickle more reliable and battle-tested.
There are no new SQL commands or user-facing features — every change is
internal: more tests, safer concurrent operations, cleaner code structure, and
better error messages.

### Safer under concurrent load

Running multiple operations at the same time — such as modifying a stream table
while it's actively refreshing, or dropping a table while its workers are still
running — is now explicitly tested and guaranteed to be safe. These scenarios
were handled before, but lacked the tests to prove it. That proof is now part
of every build.

- **Simultaneous alter + refresh** no longer risks a deadlock. The catalog
  stays consistent throughout.
- **Drop during refresh** aborts cleanly — no orphaned change buffers or
  dangling catalog rows left behind.
- **Parallel scheduler workers** are prevented from picking the same stream
  table for refresh at the same time — a hard guarantee, not just a convention.
- **Simultaneous buffer promotion** — when two workers race to promote a change
  buffer, exactly one succeeds and the metadata stays consistent.

### More stable SLA-based scheduling

The scheduler uses a predictive model to decide when to refresh stream tables,
balancing your freshness targets against system load. That model now holds its
ground under difficult workloads.

- **Bursty, sawtooth, and spike workloads** are all validated in a new
  dedicated test suite.
- **No more tier flapping** — the priority tier of a stream table (which
  controls how aggressively it is refreshed) now requires 3 consecutive
  breaches before downgrading and 3 consecutive successes before upgrading.
  This prevents the system from oscillating at the boundary, which caused
  unnecessary refresh churn in earlier releases.
- A **10,000-iteration randomised stress test** confirms the tier stays stable
  even under adversarial latency patterns.

### Fuzz testing and extreme-scale validation

The extension is now tested against malformed, random, and adversarial inputs
in three new fuzz test areas, preventing certain classes of unexpected input
from crashing the extension:

- Invalid cron schedule expressions
- Unrecognised or malformed configuration values
- Unexpected row shapes in change-capture triggers

Two new scale tests verify behaviour at extremes:

- A source table with **1,000 partitions** installs change-capture triggers and
  completes its first refresh within 60 seconds.
- A flooded worker pool does **not** starve high-priority stream tables in a
  second database — multi-database fairness is enforced under load.

### Cleaner internals: refresh module reorganised

The refresh orchestrator had grown into a single very large file. It has been
split into three focused modules with **no behaviour change**:

| Module | What it handles |
|--------|----------------|
| `orchestrator` | Deciding when and how to refresh — timing, cost model, recovery |
| `codegen` | Building the SQL queries and managing the query cache |
| `merge` | Executing the actual refresh — incremental, full, or TopK |

### Better error messages

Error messages throughout the extension now include more context — table names,
operation types, and hints such as "check system clock" on timestamp failures.
This makes it easier to diagnose problems from logs alone.

A new crash-recovery test verifies that a publication subscriber that was
active when the database was killed catches up with **zero data loss** after
restart.

---

## [0.25.0] — Scheduler Scalability & Pooler Performance

pg_trickle now comfortably manages **thousands of stream tables** on commodity
hardware — a significant jump from the practical ceiling of a few hundred in
earlier releases. The scheduler avoids reloading the full catalog on every
tick, change detection is batched into far fewer database round-trips, and a
new cache-sharing mechanism means connecting backends can skip expensive
query re-parsing entirely. If you use a connection pooler such as PgBouncer,
RDS Proxy, or Supabase Pooler, this release delivers the largest latency
improvement to date.

### Scales to thousands of stream tables

Previously, the scheduler queried the catalog on every tick — a process that
grew slower as the stream table count increased. Metadata is now cached per
backend and only reloaded when the dependency graph actually changes. Checking
whether source tables have new rows is batched across an entire refresh group
into a single query, down from one query per source per tick. Dependency-graph
rebuilds now happen in the background without blocking ongoing refreshes, so
you never get a stall when a stream table is created or dropped.

**New GUC: `pg_trickle.worker_pool_size`** (default `0` = spawn-per-task).
Set this to a positive number to keep that many background workers running
permanently, eliminating roughly 2 ms of spawn overhead per worker on
high-throughput deployments.

### Faster connections through poolers

A new shared-memory signal lets each connecting backend check whether the
query-template cache is already warm. If it is, the backend skips query
parsing entirely and jumps straight to the cached result. This matters most in
pooled environments — PgBouncer, RDS Proxy, Supabase — where backends connect
and disconnect frequently and re-parsing on every connection was a hidden cost.

The per-backend template cache is now bounded by
**`pg_trickle.template_cache_max_entries`** (default `0` = unbounded). When
the limit is reached, the least-recently-used entry is evicted automatically,
keeping memory usage predictable on servers with many concurrent backends.

A new SQL function, **`pgtrickle.clear_caches()`**, flushes all cache levels
in one call — useful after schema changes or when debugging unexpected
behaviour.

### Lower overhead on high-write workloads

Change fingerprinting — the hashing that identifies which rows changed —
now streams values directly into the hash function instead of building a
temporary string per row, eliminating one heap allocation per incoming change.
SQL buffers in the query-projection step are pre-sized rather than repeatedly
concatenated. Refresh timing data (how long full and incremental refreshes
take) is stored in shared memory so parallel workers can read it without a
catalog round-trip.

### More conservative refresh-mode predictions

The predictive model that decides when to fall back from incremental to full
refresh is now more stable. It waits for at least 60 seconds of history before
making any prediction — preventing erratic switches on fresh deployments —
removes statistical outliers before fitting, and keeps its output within a
reasonable band around recent observed timings.

### Subscriber lag tracking for downstream publications

If you use `stream_table_to_publication()` to feed a downstream system,
pg_trickle now monitors how far behind each subscriber's replication slot has
fallen. When a subscriber exceeds **`pg_trickle.publication_lag_warn_bytes`**,
a warning is logged and change-buffer cleanup is paused for that slot until it
catches up — preventing data loss for slow consumers.

A new SQL function, **`pgtrickle.worker_allocation_status()`**, returns
per-database worker usage, quotas, and queue depth across the cluster. Useful
for diagnosing scheduler starvation in multi-tenant deployments.

### Upgrade notes

- **Row ID change:** The internal hash function changed from xxh64 to xxh3.
  If your application relies on stable pg_trickle row ID values across
  versions, run `SELECT pgtrickle.reinitialize('<schema>.<table>')` on each
  affected stream table after upgrading.
- **No schema changes** beyond two new SQL functions (`clear_caches` and
  `worker_allocation_status`). No data migration required.

---

## [0.24.0] — Join Correctness & Durability Hardening

This release focuses on two themes: **correctness** — ensuring stream tables
that join multiple source tables always give you the right answer — and
**durability** — ensuring your data is never lost or skipped, even when the
server crashes or long-running transactions are in flight.

### More accurate results from multi-table joins

When a stream table combines rows from two or more source tables, pg_trickle
now guarantees that an incremental refresh produces exactly the same result as
a full recompute from scratch. A subtle bug in how rows were tracked across
refresh cycles could previously cause phantom rows to accumulate silently over
time. Those phantom rows are now detected automatically after every incremental
refresh and cleaned up.

### No data loss across crashes or restarts

pg_trickle now records its progress in a crash-safe sequence: it saves its
intent before writing data, then marks completion afterwards. If the server
goes down between those two steps, pg_trickle reconciles its position on
restart — no changes are processed twice and none are silently dropped. The
scheduler also persists its last known-safe position across restarts, closing
a narrow gap that existed in earlier versions.

### Long-running transactions no longer cause missed changes

If a database transaction stays open while pg_trickle is running a refresh,
the changes it is writing could previously be overlooked — they were captured
before the refresh started but not yet visible to it. pg_trickle now checks
for open transactions before advancing its read position and waits for them to
commit first.

- **`pg_trickle.frontier_holdback_mode`** — controls the holdback behaviour.
- **`pg_trickle.frontier_holdback_warn_seconds`** (default `60`) — logs a
  warning when a transaction has been blocking progress longer than this
  threshold.

### Works correctly on managed cloud databases

AWS RDS, Cloud SQL, and Azure Database for PostgreSQL restrict access to
certain monitoring views. pg_trickle now detects this automatically and tells
you exactly what to do:

```sql
GRANT pg_monitor TO <your_pg_trickle_role>;
```

Without this grant, pg_trickle previously behaved as if no transactions were
open — the same unsafe condition the holdback feature was built to prevent.
See `docs/TROUBLESHOOTING.md` section 14 for full diagnosis steps.

### Choose your durability level

The new **`pg_trickle.change_buffer_durability`** setting controls how
carefully incoming changes are stored before processing:

- **`unlogged`** (default) — fastest; change buffers do not survive a server
  crash.
- **`logged`** — survives crashes and replicates to standby servers.
- **`sync`** — maximum safety; every write is confirmed to disk before
  continuing.

### Automatic history clean-up

Old refresh history rows are now pruned automatically in small background
batches during idle time. Previously the history table grew without bound,
which could become noticeable on busy deployments.

### Alerts for frozen stream tables

The new **`pgtrickle.df_frozen_stream_tables`** view flags any stream table
that has not refreshed within 5× its expected interval, and sends a
notification on the `pgtrickle_alert` channel. Useful for catching a stuck or
disabled stream table before users notice stale data.

### New monitoring metrics

Two new Prometheus metrics expose holdback state:

- **`pg_trickle_frontier_holdback_lsn_bytes`** — how far behind the read
  position is being held, in bytes of WAL.
- **`pg_trickle_frontier_holdback_seconds`** — how long the oldest blocking
  transaction has been running.

> **Note:** All metrics now use the `pg_trickle_` prefix consistently. If
> your dashboards or alerting rules use the old `pgtrickle_` prefix, update
> them before upgrading.

---

## [0.23.0] — Performance Tuning & Diagnostics

This release gives you better tools to understand and control how pg_trickle
performs, with new settings for memory tuning and new functions for
inspecting what the extension is doing under the hood.

### See exactly what SQL is running

Turn on `pg_trickle.log_delta_sql` and pg_trickle will log the SQL it
generates for each incremental refresh. You can paste that SQL directly
into `EXPLAIN ANALYZE` to understand why a particular refresh is taking
longer than expected — no code changes required.

### Tune memory for refreshes without restarting

`pg_trickle.delta_work_mem` lets you give incremental refresh queries more
(or less) working memory without touching PostgreSQL's global settings or
restarting the server. Apply it instantly with:
```sql
ALTER SYSTEM SET pg_trickle.delta_work_mem = 256;
```

### Automatic statistics before each refresh

pg_trickle now runs a quick statistics pass on change buffers before
executing an incremental refresh. This gives PostgreSQL's query planner
accurate row counts and generally produces faster, more predictable query
plans with no manual intervention. Controlled by `pg_trickle.analyze_before_delta`
(on by default).

### Warning when incremental is unexpectedly slower than full

If an incremental refresh takes longer than the last full refresh,
pg_trickle now logs a warning that includes both timings. This surfaces
scenarios where incremental refresh has become counterproductive so you
can investigate and adjust thresholds.

### Alert when too many changes pile up

Set `pg_trickle.max_change_buffer_alert_rows` to a row count and pg_trickle
will warn you whenever any source table's pending change buffer exceeds
that threshold. This is useful for catching unexpected write bursts before
they slow down your refreshes.

### Refresh timing statistics at a glance

The new `pgtrickle.pgtrickle_refresh_stats()` function returns per-stream-table
refresh durations — average, 95th percentile, and 99th percentile — in a
single query. No need to manually aggregate the history table.

### Inspect generated SQL without running it

Call `pgtrickle.explain_diff_sql(name)` on any stream table to see the SQL
pg_trickle would use for an incremental refresh — without actually executing
it. Useful for understanding query structure and diagnosing performance issues.

---

## [0.22.0] — Downstream CDC, Parallel Refresh & Predictive Cost Model

This release makes it easier to feed stream table changes to other systems,
gives you a knob to control how many refreshes run at once, and adds
automatic intelligence for choosing between incremental and full refresh.

### Stream table changes can flow to other systems

`stream_table_to_publication(name)` creates a PostgreSQL logical replication
publication for a stream table. Any downstream tool that understands
PostgreSQL replication — Debezium, Kafka Connect, a read replica, or a
custom consumer — can then subscribe and receive changes as they happen.
Publications are removed automatically when the stream table is dropped.
Use `drop_stream_table_publication(name)` to remove one manually.

### Control how many tables refresh at once

`pg_trickle.max_parallel_workers` caps the number of stream tables that
can refresh simultaneously. The scheduler already runs independent refreshes
in parallel; this setting gives you an explicit limit if you want to reserve
database resources for your application.

### Automatic mode switching based on predicted cost

pg_trickle now learns from your refresh history. Before each incremental
refresh it predicts how long it will take based on recent timings. If that
prediction exceeds 1.5× the cost of a full refresh, it switches to full
refresh for that cycle automatically — no manual intervention needed. The
lookback window, threshold, and minimum sample count are all configurable:
- `pg_trickle.prediction_window` — how many recent refreshes to consider (default 60).
- `pg_trickle.prediction_ratio` — how much more expensive incremental must
  be before switching to full (default 1.5).
- `pg_trickle.prediction_min_samples` — minimum history before the model
  activates (default 5).

### Set a freshness target and let pg_trickle handle the rest

Call `set_stream_table_sla(name, interval)` with your target maximum data
age — for example `'5 seconds'` or `'1 minute'` — and pg_trickle assigns
the most appropriate refresh tier automatically. It re-evaluates the
assignment over time as real-world refresh performance changes.

---

## [0.21.0] — Reliability, Safety & Operational Tools

This release focuses on making pg_trickle safer and easier to operate day-to-day.
It eliminates hidden crash risks in the query analysis engine, adds new
operational commands for maintenance windows, and introduces a built-in
monitoring endpoint so you don't need extra software to observe pg_trickle.

### The extension can no longer crash your database

When pg_trickle analyses a query internally, it previously had hidden error
paths that could — in rare edge cases — abort a PostgreSQL backend process.
All of those paths now return a structured error instead of crashing.
Additionally, a compile-time rule now prevents production code from ever
calling the Rust equivalent of an unchecked assertion, so this class of
bug cannot be reintroduced silently.

### Warning for queries that shouldn't use incremental refresh

If you create a stream table with a query that calls time-sensitive or
non-deterministic functions such as `now()`, `random()`, or
`gen_random_uuid()`, pg_trickle now warns you at creation time. Those
functions produce a different result every time they run, which means
incremental refresh would produce wrong answers — the warning lets you
catch this before it becomes a data problem.

### Pause and resume everything at once

Two new functions let you halt and restart all active stream tables with
a single SQL call:

```sql
SELECT pgtrickle.pause_all();   -- stop all refreshes (e.g. before maintenance)
SELECT pgtrickle.resume_all();  -- restart them when you're done
```

### Refresh only when the data is actually stale

`pgtrickle.refresh_if_stale(name, max_age)` triggers a refresh only if the
stream table is older than your specified threshold. Returns `TRUE` when a
refresh ran, `FALSE` when the data was already fresh enough. Useful for
scripts and scheduled jobs that shouldn't over-refresh.

### Export a stream table's definition

`pgtrickle.stream_table_definition(name)` returns the complete
`CREATE STREAM TABLE` statement for any stream table. Handy for
documentation, disaster recovery playbooks, and migrations.

### Test query changes safely before going live

A three-step canary workflow lets you try a new query on a shadow copy of
your stream table and compare the results before committing to the change:

1. `canary_begin(name, new_query)` — creates a shadow stream table running
   the new query in parallel with the original.
2. `canary_diff(name)` — shows exactly which rows differ between the old
   and new queries.
3. `canary_promote(name)` — atomically switches the live stream table to
   the new query once you are satisfied with the results.

### Built-in monitoring endpoint

Set `pg_trickle.metrics_port = 9188` and pg_trickle serves a Prometheus-
compatible metrics endpoint directly — no extra exporter software needed.
Metrics include total refreshes, failures, rows changed per refresh, and
the number of active stream tables.

### Visibility into recursive query fallbacks

When a query containing a recursive clause cannot be refreshed incrementally
and falls back to a full refresh, pg_trickle now logs a notice and records
the reason in refresh history. Previously this happened silently.

### Upgrade

```sql
ALTER EXTENSION pg_trickle UPDATE TO '0.21.0';
```

---

## [0.20.0] — Self Monitoring

**pg_trickle now monitors itself.** Instead of you having to check on
pg_trickle's health manually, this release lets pg_trickle watch its own
performance, spot problems early, and even fix some of them on its own. Five
new stream tables sit in the `pgtrickle` schema and continuously analyse
refresh history — the same technology you use for your own data, pointed
inward. One SQL call sets everything up; one call tears it down.

We call this *self monitoring* — pg_trickle uses its own stream-table technology
to keep an eye on itself, just like it keeps your data views up to date.

### What's new

- **One-click self-monitoring** — run `SELECT pgtrickle.setup_self_monitoring()`
  and pg_trickle creates five monitoring stream tables that continuously track
  how well it is performing. Run `teardown_self_monitoring()` to remove them.
  Both are idempotent — safe to call as many times as you like, even during
  rolling upgrades.

- **Health at a glance** — the new `self_monitoring_status()` function shows the
  status of all five monitoring views in one query: whether each one exists,
  its refresh mode, and the last time it refreshed. Quick to run from a
  monitoring script or dashboard.

- **Threshold recommendations** — after enough refresh cycles accumulate
  (typically 10–20 minutes of activity), `df_threshold_advice` starts
  producing suggestions for each stream table. Each recommendation includes
  a confidence level (HIGH / MEDIUM / LOW) and a reason — for example,
  "DIFF is 73% faster — raise threshold to allow more DIFF". A
  `sla_headroom_pct` column shows exactly how much faster incremental refresh
  is versus full refresh for that table.

- **Automatic tuning** — set `pg_trickle.self_monitoring_auto_apply = 'threshold_only'`
  and pg_trickle will apply HIGH-confidence threshold recommendations
  automatically. Changes are rate-limited to once per 10 minutes per stream
  table, and every adjustment is logged to `pgt_refresh_history` with
  `initiated_by = 'SELF_MONITOR'` so you have a full audit trail.

- **Real-time alerts** — when pg_trickle detects an anomaly (duration spike
  exceeding 3× the baseline, or two or more recent failures), it sends a
  `NOTIFY` on the `pgtrickle_alert` channel with a JSON payload. Your
  application, Alertmanager webhook, or `LISTEN` client can act immediately
  without polling.

- **Scheduling interference detection** — `df_scheduling_interference` tracks
  pairs of stream tables that consistently overlap during refresh. When
  overlap is heavy, the scheduler automatically backs off its poll interval
  (up to 2× the configured base) to reduce contention.

- **Visual dependency graph** — the new `explain_dag()` function renders
  your full refresh pipeline as a Mermaid or Graphviz DOT diagram. User
  stream tables appear in blue, self-monitoring tables in green, suspended tables
  in red. Paste the output into any Mermaid renderer or `dot` to see exactly
  how your tables depend on each other.

- **Scheduler overhead report** — `scheduler_overhead()` returns metrics
  for the last hour: total refreshes, how many were self-monitoring, the
  fraction they represent, and average durations. Useful for confirming that
  self-monitoring adds negligible cost.

### What pg_trickle watches

| Monitoring view | What it tracks |
|-----------------|----------------|
| `df_efficiency_rolling` | Rolling-window refresh speed, change ratio, DIFF vs FULL counts |
| `df_anomaly_signals` | Duration spikes (> 3× baseline), error bursts, mode oscillation |
| `df_threshold_advice` | Per-table threshold recommendations with confidence level and reasoning |
| `df_cdc_buffer_trends` | Change-capture buffer growth rate per source table; alerts on burst spikes |
| `df_scheduling_interference` | Refresh overlap patterns; pairs with 3+ concurrent refreshes in the last hour |

### Faster and more reliable

- A new index on `pgt_refresh_history(pgt_id, start_time)` speeds up all
  self-monitoring queries and general history lookups. Applied automatically
  during the 0.19.0 → 0.20.0 upgrade.
- Old history records are now pruned in batches of 1,000 rows per transaction
  (previously one large DELETE), which avoids long lock holds on
  `pgt_refresh_history` during the nightly cleanup.
- `check_cdc_health()` is enriched with spill-risk alerts: if a source
  table's max burst delta exceeds 10× its average, you get an early warning
  before the buffer fills.
- `explain_st()` now shows two new properties: `self_monitoring_coverage`
  (none / partial / full) and `recommended_refresh_mode`, so diagnostics
  automatically surface self-monitoring data when it is available.

### New documentation and tooling

- **SQL Reference** — a new "Self Monitoring — Self-Monitoring" section covers
  all five stream tables, `setup_self_monitoring()`, `teardown_self_monitoring()`,
  confidence levels, and the `sla_headroom_pct` column.
- **Getting Started** — a new "Day 2 Operations" section walks through
  enabling self-monitoring, reading recommendations, enabling auto-apply, and
  visualising the DAG.
- **Configuration** — `pg_trickle.self_monitoring_auto_apply` is fully
  documented with values, rate-limiting behaviour, and the audit trail.
- A ready-made **Grafana dashboard** (`pg_trickle_self_monitoring.json`) with
  five panels covers refresh throughput, anomaly heatmap, threshold
  calibration, CDC buffer growth, and the scheduling interference matrix.
- A **dbt macro** (`pgtrickle_enable_monitoring`) enables monitoring as a
  post-hook with one line in `dbt_project.yml`.
- A **quick-start SQL script** at `sql/self_monitoring_setup.sql` walks through
  setup, auto-apply, alert listening, and status verification in six steps.

---

## [0.19.0] — Security, Scheduler Performance & Operator Convenience

**Safer, faster, easier to operate.** This release closes several security
and correctness gaps, adds new conveniences for operators and developers, and
significantly improves performance for deployments with many stream tables.
The background scheduler finds the next table to refresh 10–15× faster.
Four breaking changes are included — all easy to adapt to, each one
correcting behaviour that was a source of subtle bugs in production.

### Breaking changes

- **Only owners can modify their own stream tables** — other database users
  can no longer drop or alter a stream table they did not create. If shared
  access is intentional, grant superuser or explicitly add the user as owner.
  Superusers are unaffected.

- **Dropping a stream table no longer cascades** — `drop_stream_table()` now
  behaves like PostgreSQL's own `DROP TABLE`: it refuses to drop if dependent
  objects exist, unless you pass `cascade => true` explicitly. Previously it
  silently removed all dependents, which surprised operators after restructuring.

- **The refresh notification channel was renamed** — change `LISTEN pgtrickle_refresh`
  to `LISTEN pg_trickle_refresh` (note the added underscore). The old name
  was inconsistent with every other channel in the extension.

- **The `delete_insert` refresh strategy was removed** — this strategy could
  produce wrong results for queries containing aggregates or `DISTINCT`. If
  you had it configured, pg_trickle logs a warning and automatically switches
  to the safe `auto` strategy. No data is lost; the next refresh corrects
  any affected rows.

### New features

- **Installation health check** — `version_check()` returns the installed
  extension version, the loaded library version, and the PostgreSQL server
  version in one row. If the extension was upgraded but the server has not
  been restarted, you get an explicit warning. Useful in deploy scripts and
  smoke tests.

- **Write and refresh in one step** — `write_and_refresh(sql, st_name)`
  executes an arbitrary SQL statement and immediately refreshes the named
  stream table in the same transaction. Downstream readers see consistent
  results as soon as the transaction commits — no polling loop needed.

- **Better connection-pooler support** — the new
  `pg_trickle.connection_pooler_mode` GUC configures pg_trickle for
  PgBouncer, pgcat, or Supavisor at the cluster level. Previously each
  stream table had to be configured individually, which was error-prone on
  large deployments.

- **Automatic refresh history cleanup** — `pgt_refresh_history` is now
  trimmed automatically after 90 days (configurable with
  `pg_trickle.history_retention_days`; set to `0` to disable). Without
  this, the history table could grow by thousands of rows per day on
  busy deployments.

- **Schema migration tracking** — pg_trickle now records which upgrade
  scripts have been applied in `pgtrickle.pgt_schema_version`. This makes
  it straightforward to verify that a deployment is fully up to date and
  simplifies the rollback story.

- **Clearer skip messages** — when a refresh is skipped because another
  refresh of the same stream table is already running, you now see a
  `NOTICE: skipping refresh of <name> — already running` message instead
  of silence. Reduces confusion when debugging slow or stuck schedulers.

- **Deeper diagnostics** — `explain_st()` gains a `with_analyze` parameter.
  When set to `true`, it runs `EXPLAIN (ANALYZE, BUFFERS)` on the refresh
  query and returns actual row counts, timing, and buffer hit/miss ratios —
  the same information PostgreSQL's query planner provides for any query,
  but surfaced inside the stream-table diagnostic tool.

- **New deployment guides** — step-by-step documentation for PgBouncer,
  pgcat, Supavisor, CNPG, and Kubernetes deployments, plus an operational
  runbook for common Kubernetes failure modes.

### Bug fixes

- Fixed a constraint-validation inconsistency in databases upgraded from
  0.11.0 or earlier where `pgt_refresh_history` had a duplicate check entry
  in the catalog. Affected databases could see spurious constraint errors
  on busy write paths.

- Error messages throughout the extension now show human-readable table
  names (e.g. `public.orders`) instead of raw PostgreSQL OIDs. This affects
  "source table was dropped", "schema changed", and several other error
  paths that were previously unreadable without a catalog lookup.

### Performance

- **10–15× faster scheduler dispatch** — the scheduler now finds the next
  stream table to process with a direct lookup instead of scanning the full
  list on every poll cycle. On a deployment with 500 stream tables this
  drops from ~650 µs to ~45 µs per poll, reducing background CPU overhead
  significantly at scale.

- **Single-query change detection** — when the scheduler checks whether any
  source tables have changed, it now issues one query covering all sources
  at once instead of one query per source table. On deployments with 50+
  source tables this meaningfully reduces the overhead of each scheduler
  cycle, especially under PgBouncer transaction pooling.

---

## [0.18.0] — Hardening & Delta Performance

**Hardening & Delta Performance.** This release focuses on correctness,
reliability, and giving operators better visibility into what pg_trickle is
doing. Stream tables that group by columns containing NULL values now refresh
correctly in all cases. A new memory safety net prevents runaway refreshes
from consuming too much RAM. Error messages across the board now explain what
went wrong and suggest how to fix it. Two new SQL functions —
`health_summary()` and `cache_stats()` — give you a single-query overview of
the entire system, and updated Grafana dashboards make monitoring plug-and-play.
The TPC-H industry benchmark now runs as a nightly regression guard, and
property-based tests mathematically verify the core delta engine's arithmetic.

### Highlights

- **NULL values in GROUP BY now handled correctly** — previous versions could
  produce wrong results when a stream table grouped by a column that contained
  NULL values and rows were deleted. The root cause was that NULL group keys
  broke the internal row-matching logic. This is now fixed: NULL keys are
  matched correctly during both inserts and deletes, so aggregate stream
  tables always return the right answer regardless of NULLs in the data.

- **Memory safety net for large deltas** — if an unexpectedly large batch of
  changes arrives (for example, a bulk import into a source table), the
  incremental refresh could previously consume unbounded memory. A new
  configuration option (`pg_trickle.delta_work_mem_cap_mb`) lets you set a
  ceiling. When a refresh would exceed it, pg_trickle automatically falls
  back to a full refresh instead of risking an out-of-memory crash.

- **Early warning when refreshes spill to disk** — when the incremental
  refresh engine runs low on memory, PostgreSQL may spill intermediate data
  to temporary files on disk, which is much slower. pg_trickle now detects
  this and sends a notification so you can investigate before performance
  degrades. If spilling happens repeatedly, the scheduler automatically
  switches the affected stream table to full refresh.

- **One-query system health check** — the new `pgtrickle.health_summary()`
  function returns a single row with everything you need at a glance: how
  many stream tables are active, how many are in error or suspended state,
  the worst staleness across all tables, whether the scheduler is running,
  and the overall cache hit rate. Perfect for dashboards, alerting rules, or
  a quick manual check.

- **Cache performance visibility** — the new `pgtrickle.cache_stats()`
  function shows how effectively pg_trickle is reusing its internal query
  templates. You can see cache hit rates, eviction counts, and current cache
  size — useful for tuning `pg_trickle.template_cache_size` on busy systems.

- **Better error messages** — every error pg_trickle can raise now includes a
  standard PostgreSQL error code (SQLSTATE), a DETAIL line explaining the
  context, and a HINT suggesting what to do. Instead of a cryptic internal
  error, you get actionable guidance like "Table 'orders' was dropped while
  stream table 'order_summary' depends on it — recreate the source table or
  drop the stream table."

### Monitoring & dashboards

- **Updated Grafana dashboards** — the bundled `pg_trickle_overview.json`
  dashboard now includes panels for template cache hit rate, P99 and average
  refresh latency, hourly refresh success/failure counts, and cache eviction
  trends. Import it into Grafana and point it at your Prometheus instance for
  instant visibility.

- **Prometheus metric documentation** — all 8 new metrics exposed by
  `cache_stats()` and `health_summary()` are now fully documented in the
  monitoring guide, with ready-to-use PromQL queries.

### Correctness & testing

- **TPC-H regression guard** — all 22 queries from the TPC-H industry
  benchmark now run nightly against known-good expected output. If a code
  change causes any query to return different results, CI fails immediately.
  This catches subtle correctness regressions that targeted tests might miss.

- **Mathematical proof of delta arithmetic** — 6 property-based tests
  (2,000 random cases each) verify that the core engine's insert/delete
  accounting is correct: operations compose in the right order, groups cancel
  out properly, and no phantom rows appear after mixed workloads. An
  additional 4 end-to-end property tests exercise the full pipeline from
  change capture through to the final merged result.

- **CDC edge case coverage** — new tests cover composite primary keys,
  generated (computed) columns, NULL values in non-key columns, and domain
  types — real-world schema patterns that were previously untested.

- **dbt integration tests** — the dbt adapter now has regression tests for
  AUTO refresh mode, stream table health checks, and refresh history
  lifecycle — ensuring the dbt workflow stays reliable across releases.

### Scalability

- **Scaling guide** — a new `docs/SCALING.md` document covers how to
  configure pg_trickle for large deployments (200+ stream tables), including
  worker pool sizing, tiered scheduling, per-database quotas, and tuning
  profiles for different workload types.

- **Buffer growth stress tests** — new tests verify that the
  `max_buffer_rows` safety limit works correctly under sustained high write
  rates, including automatic recovery back to incremental refresh after a
  burst subsides.

### Testing infrastructure

- **Faster CI on pull requests** — 19 additional test files (~197 tests)
  were moved to the lightweight test runner that does not require building a
  custom Docker image. Pull request CI is now faster without sacrificing
  coverage.

- **Upgrade path tested** — the full upgrade chain from version 0.1.3
  through every release up to 0.18.0 is verified automatically in CI,
  including function availability, schema integrity, and data survival.

### Fixed

- **Upgrade script completeness** — the 0.17.0→0.18.0 upgrade migration now
  includes all new and changed functions (`pg_trickle_hash`, `cache_stats()`,
  `health_summary()`), so `ALTER EXTENSION pg_trickle UPDATE` works correctly.

---

## [0.17.0] — Query Intelligence & Stability

**Query Intelligence & Stability.** This release teaches pg_trickle to make
smarter decisions about how to refresh each stream table, reduces unnecessary
work when only a handful of columns actually changed, and proves correctness
through 10,000 automated random mutations every night. Large deployments
with hundreds of stream tables now handle schema changes much faster.
Alongside these improvements, three new documentation resources make it
easier to get started, troubleshoot problems, and migrate from pg_ivm.

### Highlights

- **Query-aware refresh decisions** — pg_trickle previously used a fixed
  threshold to decide between incremental and full refresh: if more than 50%
  of rows changed, switch to full. That works for simple queries but is
  poorly calibrated for joins or aggregates. The engine now classifies each
  query by its complexity (simple scan, filter, aggregate, join, or
  join+aggregate) and weights the cost estimate accordingly. Simple queries
  stay incremental even at high change rates; expensive join-heavy queries
  switch to full refresh sooner when the data is largely different. You can
  also pin a table to always use one strategy with the new
  `pg_trickle.refresh_strategy` setting (`'auto'` / `'differential'` /
  `'full'`), or tune the aggressiveness with `pg_trickle.cost_model_safety_margin`.

- **Skip columns that did not change** — when a row is updated in a wide
  source table (say, 50 columns) but only 2 columns that the stream table
  actually uses are modified, pg_trickle previously processed the full change
  anyway. It now tracks exactly which columns were modified and skips updates
  that touch none of the relevant columns. For aggregate stream tables the
  savings go further: a value-only update that does not affect group
  membership is applied as a single lightweight correction instead of a
  delete-then-insert pair. On write-heavy workloads with wide tables, this
  reduces the volume of data flowing through the refresh pipeline by 50–90%.

- **Faster schema changes on large deployments** — every time you create,
  alter, or drop a stream table, pg_trickle previously rebuilt the entire
  internal dependency graph from scratch. With 100 stream tables that takes
  only a few milliseconds, but at 1,000 it becomes noticeable. The graph is
  now updated incrementally — only the affected edges are touched, leaving
  everything else in place. At 1,000 stream tables the rebuild time drops
  from ~600 µs to ~116 µs and no longer scales with the total number of
  tables in the database.

- **Nightly correctness oracle** — a new automated test runs 10,000 random
  data mutations every night against a broad set of query shapes. For each
  mutation it compares the result of incremental refresh against a full
  recompute and fails if they ever disagree. This catches subtle correctness
  bugs that only surface after unusual sequences of inserts, updates, and
  deletes — the kind that hand-written tests rarely reach.

- **`ROWS FROM()` fully supported** — queries that use `ROWS FROM()` to call
  multiple set-returning functions side-by-side are now fully supported in
  incremental mode, including updates and deletes. This was previously
  restricted to insert-only workloads.

### New documentation

- **Try it in 60 seconds** — a new `playground/` directory contains a
  `docker compose up` environment with PostgreSQL 18 + pg_trickle pre-wired,
  sample data loaded, and five stream tables ready to query. No installation
  required beyond Docker.

- **Troubleshooting runbook** — `docs/TROUBLESHOOTING.md` covers 13
  real-world failure scenarios: scheduler not running, stream table stuck in
  SUSPENDED state, CDC triggers missing, WAL slot problems, out-of-memory,
  disk full, circular dependency convergence issues, unexpected schema
  changes, worker pool exhaustion, and blown fuses. Each scenario lists
  symptoms, diagnostic queries, and step-by-step resolution.

- **Migrating from pg_ivm** — `docs/tutorials/MIGRATING_FROM_PG_IVM.md`
  is a step-by-step guide for teams moving from the pg_ivm extension. It
  maps every pg_ivm API to its pg_trickle equivalent, explains behavioral
  differences, and includes ready-to-run SQL examples and a post-migration
  verification checklist.

- **New user FAQ** — the top 15 common questions are now answered at the
  top of `docs/FAQ.md` so new users find answers before scrolling through
  the full document.

- **Post-install verification script** — `scripts/verify_install.sql` walks
  through the complete setup: checks that pg_trickle is loaded, creates a
  test stream table, runs a refresh, verifies the result, and cleans up.
  Useful for confirming a fresh installation or diagnosing environment issues.

### Stability & code quality

- **Safer internal code** — the number of `unsafe` Rust blocks in the query
  parser was reduced from 690 to 441 (a 36% drop) by introducing two
  helper macros that wrap the most common unsafe patterns. No behavior change;
  this makes the codebase easier to audit and maintain.

- **Cleaner internal structure** — the largest source file (`api.rs`, ~9,400
  lines) was split into three focused modules. This has no user-visible
  effect but makes the codebase significantly easier to work with and
  reduces the risk of regressions from unrelated code being in the same file.

- **Refresh logic extracted and tested** — seven functions responsible for
  building the SQL used during refresh were extracted into standalone
  testable units and covered with 29 new unit tests. This catches
  regressions in generated SQL templates before they reach production.

---

## [0.16.0] — Performance & Refresh Optimization

**Performance & Refresh Optimization.** This release makes stream table
refreshes significantly faster across the board. Small changes to large
tables are now applied without expensive full-table scans. Tables that only
receive new rows (no updates or deletes) use a streamlined path that skips
unnecessary work. Aggregate queries like `SUM` and `COUNT` are refreshed
with pinpoint updates instead of recalculating entire groups. A new template
cache eliminates repeated startup work when database connections are recycled.
An automated benchmark system now prevents future changes from accidentally
slowing things down.

### Highlights

- **Smarter refresh for small changes** — when only a handful of rows change
  in a large stream table (less than 1% of total rows), pg_trickle now uses
  a faster strategy that skips the full-table comparison. This can reduce
  refresh time by up to 40% for common workloads where most data stays the
  same between refreshes. The system picks the best strategy automatically,
  but you can override it via the `merge_strategy` setting.

- **Insert-only fast path** — stream tables backed by append-only data sources
  (like event logs or audit trails that never update or delete rows) are now
  detected automatically and refreshed using a much simpler, faster path.
  No configuration is needed — pg_trickle observes your data patterns and
  switches to the fast path on its own. If an update or delete is later
  detected, it safely falls back to the standard approach with a warning.

- **Faster aggregate refreshes** — stream tables that use `SUM`, `COUNT`,
  `AVG`, or `STDDEV` aggregates now update individual groups directly instead
  of re-joining against the entire table. For queries with many distinct
  groups, this can be 5–20× faster. Non-invertible aggregates like `MIN`,
  `MAX`, and `STRING_AGG` continue using the standard path.

- **Template cache for faster cold starts** — the first time a database
  connection refreshes a stream table, pg_trickle normally spends ~45 ms
  preparing the refresh query. A new cross-connection cache stores these
  prepared queries so that subsequent connections (including those from
  connection poolers like PgBouncer) start refreshing in about 1 ms instead.

- **Automated performance regression checks** — every code change to
  pg_trickle is now automatically benchmarked before it can be merged. If any
  operation slows down by more than 10%, the change is blocked until the
  regression is fixed. This protects users from accidental performance
  degradation in future releases.

### New features

- **Error reference guide** — a new [error reference](docs/ERRORS.md) page
  documents every error message pg_trickle can produce, explains what caused
  it, and suggests how to fix it. Useful when troubleshooting unexpected
  behavior in production.

- **Change buffer growth protection** — if a stream table's refresh keeps
  failing, the backlog of unprocessed changes could previously grow without
  limit, consuming disk space. A new `max_buffer_rows` setting (default:
  1,000,000 rows) caps this growth. When the limit is reached, pg_trickle
  performs a full refresh to clear the backlog and warns you about the
  situation.

- **Automatic index creation control** — pg_trickle has always created helpful
  indexes on stream tables automatically. A new `auto_index` setting lets you
  disable this behavior when you want full control over indexing. Stream tables
  using `SELECT DISTINCT` now also get an automatic index on their distinct
  columns.

- **Compaction and predicate pushdown stats** — the `explain_st()` diagnostics
  function now shows additional information about change buffer compaction
  thresholds, merge strategy selection, append-only mode, aggregate fast-path
  status, and template cache hit rates.

### Improved

- **Configuration guidance** — the documentation now includes detailed tuning
  advice for the `planner_aggressive` and `cleanup_use_truncate` settings,
  especially for environments using connection poolers like PgBouncer or
  running under memory pressure.

- **Terminal dashboard improvements** — the `pgtrickle` TUI dashboard now shows
  the effective refresh mode for each stream table (e.g., when a table is
  temporarily downgraded from differential to full refresh). The Alerts tab
  has been restructured with a clearer table layout and better distinction
  between "stale data" and "no upstream changes" conditions.

### Fixed

- **Append-only detection with chained stream tables** — stream tables that
  feed into other stream tables (cascading dependencies) now correctly skip
  the append-only fast path to avoid data inconsistencies. Previously, a
  chained stream table could incorrectly use the insert-only path even when
  downstream tables needed the full change set.

- **Append-only heuristic accuracy** — the automatic detection of insert-only
  data sources now also checks the stream table's own change buffer for
  non-insert operations, avoiding false positives.

- **Full refresh fallback for mixed changes** — when both a stream table and
  its source table have pending changes in the same refresh cycle, pg_trickle
  now correctly falls back to a full refresh to avoid inconsistencies.

- **`resume_stream_table()` confirmed working** — the function referenced in
  error messages when a stream table enters `SUSPENDED` state was verified to
  exist and work correctly (present since v0.2.0).

### Testing & quality

- 13 new end-to-end tests covering JOIN correctness across update/delete
  cycles, window function differential behavior, differential-vs-full
  equivalence validation, and source table schema evolution resilience.
- 5 new benchmark scenarios covering semi-joins, anti-joins, multi-table join
  chains, and aggregate queries at varying group counts. Total: 22 benchmark
  functions.
- 1,700 unit tests pass (up from 1,630 in v0.15.0).

---

## [0.15.0] — Interactive TUI, Bulk Create & Runaway-Refresh Protection

0.15.0 brings the terminal dashboard to full operational capability, adds
safety features that protect against runaway refreshes, and broadens the
ecosystem with guides for popular migration and ORM frameworks. It also
includes a major internal refactoring of the query parser and a new streaming
benchmark suite.

### Highlights

- **Interactive terminal dashboard** — the `pgtrickle` TUI is no longer
  read-only. Refresh, pause, resume, and repair stream tables directly from
  the dashboard. A command palette (`:`) with fuzzy search makes common
  operations fast. The poller reconnects automatically after network
  interruptions.

- **Bulk creation** — `pgtrickle.bulk_create()` creates many stream tables in
  a single atomic transaction, ideal for CI/CD and dbt pipelines.

- **Runaway-refresh protection** — two new safety nets prevent expensive
  merges from spiralling: a pre-flight row-count estimate that downgrades to
  FULL refresh when deltas are too large (`max_delta_estimate_rows`), and a
  spill detector that forces FULL refresh after repeated temp-file writes
  (`spill_threshold_blocks`).

- **Stuck-watermark alerting** — if an upstream ETL pipeline stops advancing
  its watermark, pg_trickle now pauses affected stream tables and sends a
  `watermark_stuck` notification so the issue is surfaced immediately rather
  than silently producing stale data.

- **Integration guides** — new documentation for Flyway, Liquibase,
  SQLAlchemy, Django, and dbt Hub helps teams adopt pg_trickle alongside
  their existing tooling.

### New Features

- **Volatile function policy** — a new `volatile_function_policy` setting
  lets you choose whether volatile functions (like `random()` or
  `clock_timestamp()`) should be rejected (the default), allowed with a
  warning, or allowed silently when creating stream tables.

- **Bulk create API** — `pgtrickle.bulk_create(definitions)` accepts a JSON
  array of stream table definitions and creates them all in one transaction.
  If any definition fails, the entire batch is rolled back.

- **Enhanced diagnostics** — `pgtrickle.explain_st()` now shows refresh
  timing statistics (min/max/average duration), partition info for
  partitioned source tables, and a dependency graph you can render with
  Graphviz.

- **Join strategy override** — the `merge_join_strategy` setting lets you
  force a specific join method (`hash_join`, `nested_loop`, or `merge_join`)
  during delta merges, which can help when the automatic heuristic doesn't
  suit your workload.

- **Pre-flight delta estimation** — when `max_delta_estimate_rows` is set,
  pg_trickle counts the delta rows before merging. If the count exceeds the
  limit, it falls back to a FULL refresh and logs a notice, preventing
  out-of-memory conditions on unexpectedly large change sets.

- **Spill-aware refresh** — if differential merges spill to disk repeatedly
  (controlled by `spill_threshold_blocks` and `spill_consecutive_limit`),
  the scheduler switches to FULL refresh automatically.

- **Stuck watermark hold-back** — the `watermark_holdback_timeout` setting
  detects watermarks that have not advanced within a configurable window.
  Downstream stream tables are paused and a `watermark_stuck` notification
  is emitted until the watermark advances again.

- **Cascade drop** — `drop_stream_table()` now accepts an optional `cascade`
  parameter (default `true`). Setting it to `false` raises an error if
  dependent stream tables exist, matching PostgreSQL's RESTRICT behavior.

- **Nexmark benchmark suite** — a 10-query streaming benchmark (modelled on
  an online auction system) validates correctness under sustained
  high-frequency inserts, updates, and deletes.

- **17 new end-to-end tests** — 7 tests for multi-level stream-table chains
  (3- and 4-level cascades with mixed refresh modes) and 10 tests for
  diamond/fan-in topologies with IMMEDIATE mode. No deadlocks were found.

### Terminal Dashboard (TUI)

- **Write actions** — refresh, pause, resume, repair, reset fuse, and
  gate/ungate operations can now be performed without leaving the dashboard.
- **Command palette** — press `:` for fuzzy-matched command entry with
  tab-completion.
- **Automatic reconnection** — the dashboard reconnects with exponential
  back-off (up to 15 s) after a connection loss, with a visual indicator.
- **Richer views** — all 14 views now show additional live data (diagnostics,
  CDC health, refresh history with row-delta counts, error remediation hints,
  dependency-graph annotations, worker queue status, and watermark alignment).
- **Cross-view filtering** — the `/` search filter now persists across all
  10 list views.
- **Navigation re-fetch** — moving between rows in the Detail view
  immediately fetches fresh data for the selected table.
- **Toast messages** — write actions show confirmation and error toasts.
- **Sort cycling** — press `s` / `S` on the Dashboard to cycle through 6
  sort modes.
- **Mouse support** — `--mouse` enables scroll-wheel navigation.
- **Theme toggle** — `t` or `--theme dark|light` switches colour themes.
- **JSON export** — `Ctrl+E` or `:export` writes the current view to a file.
- **TLS support** — `--sslmode` and `--sslrootcert` flags.

### Documentation & Ecosystem

- **Flyway / Liquibase guide** — migration patterns for versioned and
  repeatable migrations, rollback blocks, and CI environments.
- **SQLAlchemy / Django guide** — read-only model patterns, write-blocking
  safeguards, DRF viewsets, and freshness checking.
- **dbt Hub readiness** — the `dbt-pgtrickle` package is version-synced and
  ready for dbt Hub submission.
- **Kubernetes / CNPG** — updated probe configuration and a new deployment
  section in the Getting Started guide.
- **Full documentation review** — configuration reference expanded from 23
  to 40+ settings, missing SQL reference entries filled in, outdated FAQ
  answers corrected.

### Internal Improvements

- **Parser modularisation** — the 21 000-line query parser has been split
  into 5 focused sub-modules (`types`, `validation`, `rewrites`, `sublinks`,
  and the main entry point). No behavior change — all 1 687 unit tests pass.
- **Unsafe audit** — every `unsafe` block in the codebase (~750 total) now
  has a `// SAFETY:` comment explaining why it is sound.
- **Shared-memory cache RFC** — an RFC for a DSM-based MERGE template cache
  has been written, informing the v0.16.0 implementation plan.
- **TRUNCATE handling verified** — TRUNCATE on source tables in trigger CDC
  mode already triggers a FULL refresh; this is now documented.
- **JOIN key-change fix verified** — the v0.14.0 correctness fix for
  simultaneous JOIN key updates and DELETEs has been verified working and
  the former known-limitation note replaced with a description of the fix.

### Bug Fixes

- Fixed a panic in the TUI when deserializing health-check data that
  returned 64-bit integers where 32-bit was expected.
- Fixed spurious "Error: db error" toasts in the TUI Detail view —
  background queries now degrade silently instead of surfacing transient
  errors.
- Fixed incorrect integer type annotations in two E2E tests for IMMEDIATE
  mode diamond topologies.

---

## [0.14.0] — Tiered Scheduling, Diagnostics & TUI

0.14.0 is the **Tiered Scheduling, Diagnostics & TUI** release. It gives you
fine-grained control over how often each stream table refreshes, adds tools
that recommend the best refresh strategy for your workload, introduces a
full-screen terminal dashboard for managing stream tables without SQL, and
includes important security and reliability fixes.

### Terminal Dashboard (TUI)

A new `pgtrickle` command-line tool lets you monitor and manage stream tables
from a terminal — no SQL required. Run it with no arguments to launch a
live-updating full-screen dashboard (think `htop` for stream tables), or use
one-shot subcommands like `pgtrickle list`, `pgtrickle status`, or
`pgtrickle refresh` for scripting and CI.

The interactive dashboard includes:

- **Live overview** — stream table statuses, refresh timing, and issue counts
  update every 2 seconds, with color-coded health indicators.
- **Dependency graph** — see how stream tables relate to each other in an
  ASCII tree view.
- **Diagnostics** — view refresh mode recommendations with confidence levels.
- **CDC health** — monitor change buffer sizes with warnings when they grow
  too large.
- **Alert feed** — real-time notification display with severity levels.
- **Issue detection** — automatically spots broken dependency chains, growing
  buffers, blown fuses, and stale data, with a persistent badge showing the
  issue count from any view.
- **Watch mode** — `pgtrickle watch` provides continuous non-interactive
  output suitable for log aggregation.
- **Output formats** — all CLI subcommands support `--format json`,
  `--format csv`, and human-readable table output.

See [docs/TUI.md](docs/TUI.md) for the full user guide.

### Tiered Refresh Scheduling

Stream tables can now be assigned to refresh tiers — **hot**, **warm**,
**cold**, or **frozen** — to control how frequently they refresh:

- **Hot** (default) — refreshes at the configured interval.
- **Warm** — refreshes at 2× the interval.
- **Cold** — refreshes at 10× the interval, ideal for infrequently accessed
  reports.
- **Frozen** — pauses automatic refresh entirely until promoted back.

Assign a tier with
`ALTER STREAM TABLE ... SET (tier = 'cold')`. A NOTICE is emitted when
demoting from Hot to Cold or Frozen so operators are aware of the change in
refresh frequency.

### Smarter Refresh Recommendations

Two new diagnostic functions help you choose the most efficient refresh
strategy for each stream table:

- **`pgtrickle.recommend_refresh_mode(name)`** — analyzes seven workload
  signals (change frequency, timing history, query complexity, table size,
  index coverage, and latency patterns) and recommends FULL or DIFFERENTIAL
  mode with a confidence level and plain-language explanation. Useful when
  you're unsure which mode will be faster for a particular table.

- **`pgtrickle.refresh_efficiency(name)`** — shows per-table refresh
  performance: how many FULL vs. DIFFERENTIAL refreshes have run, average
  timing for each, and the speedup factor. Good for monitoring dashboards
  and alerting.

A new tutorial — [Tuning Refresh Mode](docs/tutorials/tuning-refresh-mode.md)
— walks through the process step by step.

### Reduced Write Overhead with UNLOGGED Buffers

Enable `pg_trickle.unlogged_buffers = true` and newly created change buffer
tables will skip write-ahead logging, reducing WAL volume by roughly 30%.
This is ideal for workloads where you can tolerate a full re-sync after a
crash (the extension detects the crash and re-syncs automatically).

A utility function — `pgtrickle.convert_buffers_to_unlogged()` — converts
existing buffers in one call. Run it during a maintenance window since it
briefly locks each buffer table.

### Instant Error Detection

Previously, when a stream table's refresh hit a permanent error (for example,
a function that doesn't exist for the column type), the extension would retry
several times before giving up. Now it recognizes permanent errors immediately,
sets the stream table status to **ERROR** with a clear error message, and
stops retrying. You can see the error at a glance in the `stream_tables_info`
view or the TUI dashboard, and fix it by altering the stream table's query.

### Security Hardening

- **CDC trigger functions now use `SECURITY DEFINER`** — change-data-capture
  trigger functions run with the privileges of the extension owner rather
  than the current user, preventing privilege escalation through modified
  search paths.
- **Explicit `SET search_path`** — all CDC trigger functions now set
  `search_path` to `pgtrickle_changes, pg_catalog` to prevent search-path
  manipulation attacks.

### Other Improvements

- **Export definitions** — `pgtrickle.export_definition(name)` exports a
  stream table's full configuration as reproducible SQL (`DROP` + `CREATE` +
  `ALTER` statements), making it easy to version-control or migrate stream
  table definitions between environments.

- **Creation-time warnings** — when creating a stream table with aggregates
  like `MIN`, `MAX`, or `STRING_AGG` in DIFFERENTIAL mode, a warning now
  suggests that FULL or AUTO mode may be more efficient. For algebraic
  aggregates (`SUM`/`COUNT`/`AVG`), the warning only appears when the
  estimated number of groups is below a configurable threshold.

- **Simplified settings** — the `merge_planner_hints` and `merge_work_mem_mb`
  settings have been consolidated into a single `planner_aggressive` switch.
  The old setting names still work but are ignored in favor of the new one.

- **GHCR Docker image** — a multi-architecture Docker image
  (`ghcr.io/grove/pg_trickle`) with PostgreSQL 18.3 and pg_trickle
  pre-installed is now published automatically on each release.

- **Pre-deployment checklist** — new [PRE_DEPLOYMENT.md](docs/PRE_DEPLOYMENT.md)
  with a 10-point checklist for production deployments.

- **Best-practice patterns guide** — new [PATTERNS.md](docs/PATTERNS.md) with
  6 common patterns: Bronze/Silver/Gold materialization, event sourcing,
  slowly-changing dimensions, high-fan-out topology, real-time dashboards,
  and tiered refresh strategies.

- **Keyless dedup fix** — replaced `MAX(col)` with `array_agg(col)[1]` for
  deduplicating keyless scan results, which is more correct for non-orderable
  types.

### Bug Fixes

- **ST-on-ST differential refresh** — manually refreshing a stream table that
  reads from another stream table now uses true incremental (DIFFERENTIAL)
  refresh instead of falling back to a full re-scan. This matches the behavior
  of the automatic scheduler and is significantly faster for large tables.

- **Staleness tracking** — the staleness indicator now uses the actual last
  refresh time instead of an internal data timestamp, making the
  `pg_stat_stream_tables` view more accurate.

### Testing & Reliability

- **Soak test** — a new long-running stability test validates zero worker
  crashes, zero ERROR states, and stable memory usage under sustained mixed
  workload (configurable duration, default 10 minutes).

- **Multi-database isolation test** — verifies that two databases in the same
  PostgreSQL cluster run pg_trickle independently without interference.

- **140 TUI tests** — comprehensive unit, snapshot, and interaction tests for
  the terminal dashboard.

- **23 mixed-object E2E tests** — validates stream tables alongside regular
  PostgreSQL views, materialized views, and other objects.

- **Scheduler race fixes** — eliminated flaky test failures caused by
  scheduler timing races and GUC leak between tests.

### New SQL Functions

| Function | Purpose |
|----------|---------|
| `pgtrickle.recommend_refresh_mode(name)` | Workload-based refresh mode recommendation |
| `pgtrickle.refresh_efficiency(name)` | Per-table refresh performance metrics |
| `pgtrickle.export_definition(name)` | Export stream table as reproducible DDL |
| `pgtrickle.convert_buffers_to_unlogged()` | Convert logged change buffers to UNLOGGED |

### New Settings

| Setting | Default | Purpose |
|---------|---------|---------|
| `pg_trickle.planner_aggressive` | `true` | Consolidated switch for MERGE planner hints |
| `pg_trickle.unlogged_buffers` | `false` | Create new change buffers as UNLOGGED |
| `pg_trickle.agg_diff_cardinality_threshold` | `1000` | Warn about DIFFERENTIAL mode below this group count |

### Deprecated

- **`pg_trickle.merge_planner_hints`** — Use `pg_trickle.planner_aggressive`
  instead. Still accepted but ignored at runtime.
- **`pg_trickle.merge_work_mem_mb`** — Same; use `planner_aggressive` instead.

### Upgrading

Run `ALTER EXTENSION pg_trickle UPDATE;` after installing the new binaries.
The upgrade adds new catalog columns, functions, and the TUI workspace member.
No breaking changes — everything from v0.13.0 continues to work. See
[UPGRADING.md](docs/UPGRADING.md) for details.

---

## [0.13.0] — Scalability Foundations & Full TPC-H Coverage

0.13.0 is the **Scalability Foundations** release. It makes pg_trickle handle
large tables, complex queries, and multi-tenant deployments much more
efficiently — and it achieves a major milestone: **all 22 TPC-H benchmark
queries now run in incremental (DIFFERENTIAL) mode**, meaning the engine no
longer needs to fall back to slow full-refresh for any standard analytical
query pattern.

### Smarter Change Detection for Wide Tables

When you UPDATE a few columns in a large table — say, changing a `status`
column in a 60-column table — pg_trickle used to treat every column as
potentially changed, doing extra work to keep all downstream views up to date.

Now it knows the difference. Columns used in GROUP BY, JOIN, or WHERE clauses
are "key columns"; everything else is a "value column." When only value columns
change, the engine takes a shortcut: it sends a single correction row instead
of a full delete-and-reinsert pair. For wide-table workloads, this can cut the
volume of data processed by 50% or more.

### Shared Change Buffers

If you have several stream tables watching the same source table, each one used
to maintain its own private copy of the change log. That's wasteful. Now they
share a single change buffer per source, and each consumer simply tracks how
far it has read. The slowest reader protects the buffer for everyone.

You can see how this is working with the new `pgtrickle.shared_buffer_stats()`
function — it shows each buffer, who's reading from it, how many rows are
queued, and whether it's been automatically partitioned for performance.

### Automatic Buffer Partitioning

Set `pg_trickle.buffer_partitioning = 'auto'` and pg_trickle will start with
simple, unpartitioned change buffers. If a buffer starts accumulating a lot of
rows (high-throughput sources), it automatically converts to a partitioned
layout where old data can be removed almost instantly instead of deleting rows
one by one.

### More Partitioning Options for Stream Tables

Building on the RANGE partitioning added in v0.11.0, you can now partition
stream tables in three additional ways:

- **Multi-column keys** — partition by a combination of columns
  (`partition_by='region,year'`)
- **LIST partitioning** — for low-cardinality columns like `status` or `type`
  (`partition_by='LIST:status'`)
- **HASH partitioning** — for even distribution across a fixed number of
  partitions (`partition_by='HASH:customer_id:8'`)

You can also change the partition key of an existing stream table at runtime
with `alter_stream_table(partition_by => ...)` — data is preserved
automatically. If rows land in the default (catch-all) partition, a WARNING
is emitted to prompt you to add explicit partitions.

### All 22 TPC-H Queries Now Run Incrementally

The DVM (differential view maintenance) engine received its most significant
set of improvements yet, targeting the complex multi-table join patterns found
in standard analytical benchmarks:

- **Smarter pre-image lookups** — instead of reconstructing what the data
  looked like before a change by subtracting deltas (expensive for large
  tables), the engine now uses targeted index lookups that only touch the rows
  that actually changed.
- **Predicate pushdown** — WHERE conditions from the original query are now
  pushed into the delta computation, preventing unnecessary cross-products
  in multi-table joins.
- **Deep-join optimizations** — queries joining 5+ tables get automatic planner
  hints (more memory, smarter join strategies) to avoid spilling to disk.
- **Scan-count-aware strategy selector** — queries that exceed configurable
  join complexity or delta volume thresholds automatically fall back to full
  refresh on a per-query basis rather than failing.

The result: all 22 TPC-H queries pass at SF=0.01 in DIFFERENTIAL mode
with zero drift across 3 refresh cycles. The `DIFFERENTIAL_SKIP_ALLOWLIST`
(queries that previously required full refresh) is now empty.

### Refresh Performance Inspection Tools

Two new functions help you understand what pg_trickle is doing under the hood:

- **`pgtrickle.explain_delta(name, format)`** — shows you the query plan for
  the auto-generated delta SQL, the same way `EXPLAIN` works for regular
  queries. Available in text, JSON, XML, or YAML format.
- **`pgtrickle.dedup_stats()`** — reports how often concurrent writes produce
  duplicate entries that need pre-processing before the MERGE step.

### Multi-Tenant Worker Quotas

New setting: **`pg_trickle.per_database_worker_quota`** — if you run many
databases on one PostgreSQL cluster, this prevents a busy database from
monopolizing all the refresh workers. Workers are assigned by priority
(immediate-mode tables first, then hot, warm, and cold), with burst capacity
up to 150% when other databases are idle.

### TPC-H Benchmark Harness

You can now measure refresh performance across all 22 TPC-H queries in a
structured way. Run `just bench-tpch` to get per-query timing, FULL vs.
DIFFERENTIAL comparison, and P95 latency numbers. Five synthetic benchmarks
(`q01`, `q05`, `q08`, `q18`, `q21`) also measure the pure Rust delta-SQL
generation time without needing a database.

### Broader SQL Support

- **`IS JSON` predicates** (PG 16+) — expressions like
  `expr IS JSON OBJECT` now work in incremental mode.
- **SQL/JSON constructors** (PG 16+) — `JSON_OBJECT(...)`, `JSON_ARRAY(...)`,
  `JSON_OBJECTAGG(...)`, and `JSON_ARRAYAGG(...)` are now accepted.
- **Recursive CTEs** — recursive queries with non-monotone operators (like
  `EXCEPT`) correctly fall back to full refresh instead of producing
  wrong results.

### dbt Integration Updates

If you use dbt-pgtrickle, you can now set partitioning and fuse options
directly from dbt model config:

- `{{ config(partition_by='customer_id') }}` for partitioned stream tables
- `{{ config(fuse='auto', fuse_ceiling=100000, fuse_sensitivity=3) }}` for
  circuit-breaker protection

### Bug Fixes

- **Scheduler cascade fix** — stream tables downstream of FULL-mode upstream
  tables now detect changes correctly via a `last_refresh_at` fallback,
  preventing stale data in chains where the upstream uses full refresh.
- **SUM(CASE WHEN ...) drift fix** — aggregate expressions using CASE were
  occasionally producing slightly wrong incremental results; these are now
  correctly detected and processed via a group rescan.
- **Duplicate column DDL fix** — removed a duplicate column definition in the
  `pgt_stream_tables` DDL that could cause issues on fresh installs.

### Testing Improvements

- New regression test suite targeting 9 structural weaknesses: join multi-cycle
  correctness (7 tests), differential-equals-full equivalence (11 tests), DVM
  operator execution, failure recovery, and MERGE template unit tests.
- E2E test infrastructure now uses template databases, cutting per-test setup
  time significantly.

### New SQL Functions

| Function | Purpose |
|----------|---------|
| `pgtrickle.explain_delta(name, format)` | Show the query plan for the delta SQL |
| `pgtrickle.dedup_stats()` | MERGE deduplication frequency counters |
| `pgtrickle.shared_buffer_stats()` | Per-source change buffer status |
| `pgtrickle.explain_refresh_mode(name)` | Why a stream table uses its current refresh mode |
| `pgtrickle.reset_fuse(name)` | Reset a blown circuit-breaker fuse |
| `pgtrickle.fuse_status()` | Fuse state across all stream tables |

### New Catalog Columns

Ten new columns on `pgtrickle.pgt_stream_tables`:

| Column | Purpose |
|--------|---------|
| `effective_refresh_mode` | The actual refresh mode after AUTO resolution |
| `fuse_mode` | Circuit-breaker configuration (off / auto / manual) |
| `fuse_state` | Current fuse state (armed / blown) |
| `fuse_ceiling` | Maximum change count before fuse blows |
| `fuse_sensitivity` | Consecutive cycles above ceiling before triggering |
| `blown_at` | When the fuse last blew |
| `blow_reason` | Why the fuse blew |
| `st_partition_key` | Partition key specification |
| `max_differential_joins` | Maximum join count for differential mode |
| `max_delta_fraction` | Maximum delta-to-table ratio for differential mode |

### Upgrading

Run `ALTER EXTENSION pg_trickle UPDATE;` after installing the new binaries.
All new columns and functions are added automatically. No breaking changes —
everything from v0.12.0 continues to work as before. See
[UPGRADING.md](docs/UPGRADING.md) for details.

---

## [0.12.0] — Join Correctness, Diagnostics & Reliability

0.12.0 is a correctness, reliability, and developer-experience release built on
top of 0.11.0's major new features. It closes the last known wrong-answer bugs
for complex join queries, adds tools to help you understand and debug stream
table behavior, hardens the scheduler against several edge cases that could
cause stale data or crashes, and backs it all with thousands of new
automatically generated tests.

### Stale Rows Fixed in Stream-Table Chains

**What was the problem?** When a stream table (B) reads from another stream
table (A), each change in A is recorded as a small "what changed" entry — a
row added or removed. But the identity key used for those entries was computed
differently inside the change buffer than it was inside B's own storage. As a
result, when A changed via an upstream UPDATE, B's refresh could silently fail
to delete the old version of a row, leaving a stale duplicate.

**What changed?** The change buffer now computes row identity the same way B
does — using a hash of all the data columns rather than the upstream source's
primary key. Stale rows after UPDATE no longer appear in stream-table chains.
This bug was found and confirmed by the new property-based test suite (see
below).

### Phantom Rows Fixed for Complex Joins (TPC-H Q7 / Q8 / Q9)

**What was the problem?** When a stream table's query joins three or more
tables together and rows are deleted from more than one join side at the same
time, the incremental engine could silently drop the correction — leaving rows
in the stream table that should have been removed.

This affected TPC-H queries Q7, Q8, and Q9 (which all involve deep join
trees), and any user query with a similar multi-table join structure. A
temporary workaround (falling back to full refresh for wide joins) was in
place since v0.11.0 and has now been lifted.

**What changed?** The incremental engine now takes an individual "before
snapshot" for each leaf table in the join tree — each one cheaply computed
from a single-table comparison — and re-joins them after the delete. This
avoids writing multi-gigabyte temp files to disk (the root cause of the
original workaround) and eliminates the phantom-row bug entirely. Q7, Q8, and
Q9 now run in differential mode without any workarounds.

### Type Errors Fixed in Parallel Refresh Chains

**What was the problem?** When a chain of stream tables is fused into a single
execution unit for efficiency (the "bypass" optimisation added in v0.11.0),
the internal bypass table used `text` for every column regardless of the
actual column type. This caused an `operator does not exist: text > integer`
error whenever a downstream stream table had a type-sensitive WHERE clause
(e.g. `WHERE amount > 100`), making the parallel worker tests fail silently
across all topologies that included a fused chain.

**What changed?** Bypass tables now use the real column types. The six
parallel-worker benchmark tests now complete in 9–26 seconds rather than
timing out after 120 seconds.

### Scheduler Fixes for Diamond and ST-on-ST Topologies

Two scheduler bugs that caused incorrect refresh behavior with complex
dependency graphs were fixed:

- **Diamond timeout.** In a diamond topology (A → B, A → C, B+C → D), the L1
  arm stream tables (B and C) were created with a 1-minute fixed interval
  rather than a calculated schedule. This meant D never received updates within
  the test window. The scheduler also had a bug loading stream table records
  by ID that caused silent failures in parallel worker paths. Both are fixed.

- **ST-on-ST parallel workers.** When an upstream stream table changed, the
  parallel worker paths (singleton, atomic group, immediate closure, fused
  chain) were not forcing a full refresh on downstream stream tables the way
  the main scheduler loop did. This could leave downstream tables stale. The
  fix ensures all parallel paths treat upstream stream-table changes the same
  way.

### Four New Diagnostic Functions

When stream table behavior is unexpected — wrong refresh mode, a query being
rewritten in a surprising way, persistent errors — it previously required
reading server logs or source code to understand why. Four new SQL functions
expose that internal state directly in queries:

- **`pgtrickle.explain_query_rewrite(query TEXT)`** — shows exactly how
  pg_trickle rewrites your query for incremental refresh: which operators were
  applied, how delta keys are injected, and how aggregates are classified.
  Useful for understanding why a query got a particular refresh mode.

- **`pgtrickle.diagnose_errors(name TEXT)`** — shows the last 5 errors for a
  stream table, each classified by type (correctness, performance,
  configuration, infrastructure) with a suggested fix.

- **`pgtrickle.list_auxiliary_columns(name TEXT)`** — lists the internal
  `__pgt_*` columns that pg_trickle injects into a stream table's query plan,
  with an explanation of each one's purpose. Helpful when `SELECT *` returns
  unexpected extra columns.

- **`pgtrickle.validate_query(query TEXT)`** — analyses a SQL query and
  reports which refresh mode it would get, which SQL constructs were detected,
  and any warnings — all without creating a stream table.

### Multi-Column `IN (subquery)` Now Gives a Clear Error

**What was the problem?** A query like `WHERE (col_a, col_b) IN (SELECT x, y
FROM …)` passed validation but produced silently wrong results — the engine
was only matching on the first column and ignoring the second.

**What changed?** This construct is now detected at stream table creation time
and rejected with a clear error message that recommends rewriting it as
`EXISTS (SELECT 1 FROM … WHERE col_a = x AND col_b = y)`.

### IMMEDIATE Mode Proven Correct Under High Concurrency

IMMEDIATE mode (where the stream table updates inside the same transaction as
the source table change) now has a dedicated concurrency stress test: 100–120
concurrent transactions firing simultaneously against the same source table,
across five scenarios (all inserts, all updates to distinct rows, all updates
to the same row, all deletes, and a mixed workload). Zero lost updates, zero
phantom rows, and no deadlocks were observed in any run.

### Protection Against Pathological Queries

A new guard prevents a particularly deep or convoluted query from consuming
all available stack space and crashing the database backend. When the query
analyser recurses more than 64 levels deep (configurable via
`pg_trickle.max_parse_depth`), it now returns a clear `QueryTooComplex` error
instead of crashing.

### Tiered Scheduling Now On By Default

The tiered scheduling feature — which automatically slows down cold
(infrequently-read) stream tables and speeds up hot ones — is now enabled by
default. In large deployments this reduces the scheduler's CPU usage
significantly. Stream tables you query often continue refreshing at full speed.
Stream tables that nobody has read recently back off gracefully.

If you rely on all stream tables refreshing at the same rate regardless of
read frequency, set `pg_trickle.tiered_scheduling = off`.

### Thousands of Automatically Generated Tests

Two new automated testing systems were added to complement the hand-written
test suite:

- **Property-based tests** — the test framework automatically generates
  thousands of random DAG shapes, schedule combinations, and edge cases and
  checks that the scheduler's ordering guarantees hold for all of them. If any
  configuration would cause a table to refresh in the wrong order or get
  spuriously suspended, these tests catch it.

- **SQLancer fuzzing** — SQLancer generates random SQL queries and checks
  that pg_trickle's incremental result matches the result of running the same
  query directly in PostgreSQL. Any mismatch is automatically saved as a
  permanent regression test. A weekly CI job runs this continuously. At time
  of release, zero mismatches have been found.

### CDC Write-Side Benchmark Published

A new benchmark suite measures the overhead that pg_trickle's change capture
triggers add to your write workload. Results across five scenarios (single-row
INSERT, bulk INSERT, bulk UPDATE, bulk DELETE, concurrent writers) are
published in [docs/BENCHMARK.md](docs/BENCHMARK.md). Use these numbers to
estimate the impact before deploying pg_trickle on a write-heavy table.

### MERGE Template Validation at Test Startup

The SQL templates that pg_trickle generates for applying incremental changes
(the MERGE statements) are now validated with an `EXPLAIN` dry-run at every
test startup. If a code change accidentally produces a malformed MERGE
template, the tests catch it before any data is processed — rather than
manifesting as a cryptic runtime error.

---

## [0.11.0] — Event-Driven Latency, Chain IVM & Observability Stack

This is the biggest release since the initial launch. The headline features are
**34× lower latency** for real-time workloads, **stream-table chains that now
refresh incrementally** (no more forced full recomputation when one stream table
feeds another), **declarative partitioning** to cut I/O on large tables by up to
100×, a **ready-to-use Prometheus and Grafana monitoring stack**, and a **circuit
breaker** to protect production databases from runaway change bursts.

### 34× Lower Latency — Changes Arrive Instantly

**Previously**, the background worker woke up on a fixed timer every ~500 ms to
check for new data, even when nothing had changed. Every change had to wait up to
half a second in the change buffer before being processed.

**Now**, when a source table is modified, the change capture trigger immediately
wakes the background worker via a PostgreSQL notification channel. The worker
starts processing within ~15 ms of the write committing — a 34× improvement for
low-volume workloads. Under heavy DML, a 10 ms debounce window coalesces rapid
notifications so the worker isn't flooded.

Event-driven wake is on by default. You can turn it off
(`pg_trickle.event_driven_wake = off`) to revert to poll-based wake, and you can
tune the debounce window with `pg_trickle.wake_debounce_ms` (default `10`).

### Stream-Table-to-Stream-Table Chains Now Refresh Incrementally

**Previously**, when stream table B's query read from stream table A, pg_trickle
had to do a full recomputation of B every time A changed — even if only a few
rows in A actually changed. For long chains (A → B → C → D), every hop was a
full re-scan.

**Now**, stream tables can read from other stream tables incrementally. When A
refreshes, the rows it added and removed are recorded in a change buffer just like
a base table. B wakes up, reads only the changed rows from A, and applies a delta
— not a full recomputation. Even when A does a full refresh (e.g. because its
query does not support differential mode), a before/after snapshot diff is
captured automatically so downstream tables still receive a small insert/delete
delta rather than cascading full refreshes through the chain.

### Declaratively Partitioned Stream Tables

Stream tables can now be declared with a partition key:

```sql
SELECT create_stream_table(
  'monthly_sales',
  $$ SELECT month, region, SUM(amount) FROM orders GROUP BY 1, 2 $$,
  partition_by => 'month'
);
```

pg_trickle creates a range-partitioned storage table and, when refreshing,
automatically restricts the MERGE operation to only the partitions that contain
changed rows. For large tables where changes touch only 2–3 out of 100 monthly
partitions, this can reduce the MERGE I/O from 10 million rows to ~100,000 — a
100× improvement.

### Ready-to-Use Prometheus and Grafana Monitoring

A complete observability stack is now included in the `monitoring/` directory:

- **`monitoring/prometheus/pg_trickle_queries.yml`** — drop-in configuration for
  `postgres_exporter` that exports 14 metrics covering refresh performance,
  CDC buffer sizes, staleness, error rates, and per-table status.
- **`monitoring/prometheus/alerts.yml`** — 8 alerting rules that page you when a
  stream table goes stale (> 5 min), starts error-looping (≥ 3 consecutive
  failures), is suspended, or when the CDC buffer exceeds 1 GB.
- **`monitoring/grafana/dashboards/pg_trickle_overview.json`** — a pre-built
  Grafana dashboard with six sections: cluster overview, refresh latency
  time-series, staleness heatmap, CDC lag, per-table drill-down, and scheduler
  health.
- **`monitoring/docker-compose.yml`** — brings up PostgreSQL + pg_trickle +
  postgres_exporter + Prometheus + Grafana with one command
  (`docker compose up`). Grafana opens at http://localhost:3000; the dashboard
  shows live metrics generated by a seed workload of stream tables continuously
  refreshing synthetic order and product data (see `monitoring/init/01_demo.sql`).

No code changes are needed to use this stack with an existing pg_trickle
installation.

### Circuit Breaker (Fuse) — Protection Against Runaway Change Bursts

A new circuit breaker mechanism halts refresh for a stream table when its pending
change count exceeds a configurable threshold. This protects your database from
accidental mass-delete scripts, runaway migrations, or data imports that would
otherwise trigger an unexpectedly large and expensive refresh operation.

When the fuse blows, pg_trickle sends a `pgtrickle_alert` PostgreSQL notification
that you can subscribe to, and suspends the affected stream table. You then choose
how to recover using `reset_fuse()`:

- `reset_fuse(name, action => 'apply')` — process the backlog normally (default).
- `reset_fuse(name, action => 'reinitialize')` — clear the change buffer and
  repopulate the stream table from scratch.
- `reset_fuse(name, action => 'skip_changes')` — discard the pending changes and
  resume without reprocessing them.

Configure per-table with `alter_stream_table(fuse => 'on', fuse_ceiling => 10000)`
or set a global default with `pg_trickle.fuse_default_ceiling`. Use
`fuse_status()` to inspect the blown/active state of all stream tables at once.

### Wider Column Bitmask — No More 63-Column Limit

pg_trickle's change capture tracks which columns were actually modified in each
row so that stream tables that reference only a subset of columns can ignore
irrelevant updates. Previously, this optimization silently stopped working for
source tables with more than 63 columns — all updates were treated as touching
every column.

The bitmask has been extended from a 64-bit integer to an arbitrary-width
PostgreSQL `VARBIT` value, removing the column count cap entirely. Existing
deployments are migrated automatically (the old column value becomes `NULL`,
which the filter treats conservatively — no rows are silently dropped). Tables
with fewer than 64 columns are unaffected at the data level.

### Per-Database Worker Quotas

In multi-tenant environments where multiple databases share a single PostgreSQL
instance, all stream-table refresh workers previously competed for the same
concurrency pool. A single busy database could crowd out others.

A new GUC `pg_trickle.per_database_worker_quota` sets a soft concurrency limit
per database. When the rest of the cluster is lightly loaded (< 80% of available
capacity in use), a database can burst to 150% of its quota. When the cluster is
busy, each database is held to its base quota.

Refresh work is also now dispatched in priority order:
IMMEDIATE mode tables → atomic diamond groups → singleton tables.

### DAG Scheduling Performance

For deployments with chains of stream tables (A → B → C), several improvements
reduce end-to-end propagation latency:

- **Fused single-consumer chains.** When a stream table chain has exactly one
  downstream consumer at each hop, the scheduler fuses the chain into a single
  execution unit in one background worker. Intermediate deltas are stored in
  temporary in-memory tables instead of persistent change buffers, eliminating
  the WAL writes, index maintenance, and cleanup that would normally occur at
  each hop.
- **Batch coalescing.** Before a downstream table reads from an upstream change
  buffer, redundant insert/delete pairs for the same row are cancelled out. This
  prevents rapid-fire upstream refreshes from accumulating duplicate work for
  downstream tables.
- **Adaptive dispatch polling.** The parallel dispatch loop now backs off
  exponentially (20 ms → 200 ms) instead of using a fixed 200 ms poll, and
  resets to 20 ms as soon as any worker finishes. Cheap refreshes no longer
  wait a full 200 ms for the next tick.
- **Delta amplification warnings.** When a differential refresh produces many
  more output rows than input rows (default threshold: 100×), a `WARNING` is
  emitted with the table name, input and output counts, and a tuning hint.
  `explain_st()` now exposes `amplification_stats` from the last 20 refreshes.

### Smarter Diagnostics and Warnings

Several improvements to make problems visible earlier and easier to diagnose:

- **Know which refresh mode is actually running.** When a stream table is set to
  `AUTO`, pg_trickle now records which mode it actually chose at each refresh
  (`DIFFERENTIAL`, `FULL`, etc.) in a new `effective_refresh_mode` column on
  `pgt_stream_tables`. A new `explain_refresh_mode(name)` function reports the
  configured mode, the actual mode used, and the reason for any downgrade — all
  in one query.
- **Clearer warning when a stream table falls back to full refresh.** If a stream
  table cannot use differential mode, pg_trickle now emits a `WARNING` message
  naming the affected table and the reason. Previously this happened silently.
- **Warning when using aggregates that require full group rescans.** Aggregate
  functions like `STRING_AGG`, `ARRAY_AGG`, and `JSON_AGG` require re-aggregating
  the entire group whenever any member changes. pg_trickle now warns at stream
  table creation time when such aggregates are used in `DIFFERENTIAL` mode, and
  `explain_st()` classifies each aggregate's maintenance strategy
  (incremental, auxiliary-state, or group-rescan) so you can understand the cost.
- **Better error messages.** Errors for unsupported query patterns, cycle
  detection, upstream schema changes, and query parse failures now include a
  `DETAIL` field explaining what went wrong and a `HINT` field suggesting how to
  fix it.
- **Invalid parameter combinations are rejected at creation time.** For example,
  using `diamond_schedule_policy='slowest'` without `diamond_consistency='atomic'`
  now produces a clear error at `create_stream_table` / `alter_stream_table` time
  rather than silently doing the wrong thing at refresh time.
- **TopK queries validate their metadata on every refresh.** Stream tables defined
  with `ORDER BY ... LIMIT N` now recheck that the stored LIMIT/OFFSET metadata
  still matches the actual query on each refresh. On mismatch, they fall back to
  a full refresh with a `WARNING` rather than silently producing wrong results.

### Safety and Reliability Improvements

- **No more crashes from schema changes.** If a source table's schema changes
  while a refresh is running (e.g. a column is dropped), pg_trickle now catches
  the error, emits a structured `WARNING` with the table name and error details,
  and continues refreshing all other stream tables. The scheduler never crashes
  due to an individual table's error.
- **Failure injection tests.** New end-to-end tests deliberately drop columns and
  tables mid-refresh to verify that the scheduler stays alive and other stream
  tables continue processing correctly.
- **Safer defaults.** Three default settings have been updated to reflect
  production-safe behavior:
  - `parallel_refresh_mode` now defaults to `'on'` (was `'off'`). Parallel
    refresh has been stable for several releases; serial mode is now opt-in.
  - `block_source_ddl` now defaults to `true`. Accidental `ALTER TABLE` on a
    source table while a stream table depends on it is now blocked by default,
    with clear instructions on how to temporarily disable the guard if needed.
  - The invalidation ring capacity has been doubled from 32 to 128 slots,
    reducing the risk of invalidation events being silently discarded under
    rapid DDL.

### Getting Started Guide Restructured

`docs/GETTING_STARTED.md` has been reorganised into five progressive chapters:

1. **Hello World** — create your first stream table and watch it update.
2. **Joins, Aggregates & Chains** — multi-table dependencies and DAG patterns.
3. **Scheduling & Backpressure** — controlling refresh frequency and auto-backoff.
4. **Monitoring In Depth** — using the five key diagnostic functions and the
   Prometheus/Grafana stack.
5. **Advanced Topics** — FUSE circuit breaker, partitioned stream tables,
   IMMEDIATE (in-transaction) IVM, and multi-tenant worker quotas.

### TPC-H Correctness Gate Added to CI

Five queries derived from the TPC-H benchmark — covering single-table
GROUP BY, filter-aggregate, CASE WHEN inside SUM, a three-way join, and LEFT
OUTER JOIN with GROUP BY — now run in DIFFERENTIAL mode on every push to `main`
and daily. Any correctness mismatch between pg_trickle's incremental output and
plain PostgreSQL execution fails the CI build automatically.

### Docker Hub Image Improvements

The `Dockerfile.hub` image that is published to Docker Hub has been expanded
with a comprehensive set of GUC defaults fine-tuned for production use. A new
`just build-hub-image` recipe builds the image locally for testing.

### Bug Fixes

- **Scheduler crash after event-driven wake was enabled.** The background worker
  crashed immediately after startup when `event_driven_wake = on` (the default)
  because the `LISTEN` command was being issued outside of a transaction. Fixed
  by issuing `LISTEN` inside a short-lived SPI transaction at startup.
  (#296)
- **Spurious full refresh for non-recursive CTEs.** Stream tables containing
  `WITH` clauses that were not recursive (`WITH foo AS (SELECT ...)`) were being
  incorrectly forced to FULL refresh mode. Only truly recursive CTEs
  (`WITH RECURSIVE`) require this. Non-recursive CTEs now correctly use
  differential mode. (#298)
- **`DISTINCT ON` inside a CTE body caused a parse error.** When a stream table's
  defining query contained a `WITH` clause whose body used `DISTINCT ON (...)`,
  the DVM query analyser failed with a parse error. The `DISTINCT ON` clause is
  now rewritten before analysis so it no longer interferes. (#300)
- **Full-refresh fallback warning now names the affected table.** When pg_trickle
  falls back from differential to full refresh, the emitted `WARNING` now
  includes the stream table name and the reason, making it straightforward to
  identify which table you need to investigate. (#301)

---

## [0.10.0] — Cloud Deployment, PgBouncer & Query Engine Correctness

The headline features of 0.10.0 are **cloud deployment compatibility**, **query
engine correctness**, **refresh performance**, and **improved developer
experience for `auto_backoff`**. pg_trickle now works reliably
behind PgBouncer — the connection pooler used by default on Supabase, Railway,
Neon, and other managed PostgreSQL platforms. A broad set of correctness issues
in the incremental query engine are fixed. And several performance optimizations
cut refresh time for large tables and busy deployments.

### `auto_backoff` Is Now Much Friendlier on Developer Machines

When `pg_trickle.auto_backoff = true` is enabled, the scheduler automatically
slows down stream tables whose refresh cost exceeds their schedule budget — a
good safeguard in production. This release makes the feature safe to use
alongside short schedules (e.g. `'1s'`) in developer and CI environments:

- **Trigger threshold raised from 80 % → 95 %.** Backoff now only activates
  when a refresh consumes more than 95 % of the schedule window. A 900 ms
  refresh on a 1-second schedule (90 %) used to trigger backoff; it no longer
  does. EC-11 operator alerting continues to fire at 80 % (unchanged)
  so you still get an early warning before the scheduler is actually stuck.

- **Maximum slowdown reduced from 64× → 8×.** In the worst case, a stream
  table's effective refresh interval is now capped at 8× its configured
  schedule (e.g. 8 seconds for a `'1s'` table) instead of 64 seconds. The
  cap self-heals immediately: a single on-time refresh resets the factor to 1×.

- **Backoff events now emit `WARNING` instead of `INFO`.** When the scheduler
  stretches or resets a stream table's effective interval, you will see a
  `WARNING` message in your PostgreSQL client, including the new effective
  interval — rather than a silent slowdown with no explanation.

- **`auto_backoff` now defaults to `on`.** With the above improvements in place,
  the feature is safe in all environments. New installations get CPU runaway
  protection out of the box. To restore the old opt-in behaviour, set
  `pg_trickle.auto_backoff = off`.

### Works Behind PgBouncer

PgBouncer is the most popular PostgreSQL connection pooler. In "transaction
mode" — the default setting on most cloud PostgreSQL platforms — it hands a
fresh database connection to every transaction, which breaks anything that
assumes the same connection stays open between calls (session locks, prepared
statements). pg_trickle previously relied on both. This release makes pg_trickle
work correctly in such deployments.

- **Session locks replaced with row-level locking.** The background scheduler
  now acquires a short-lived row-level lock on each stream table's catalog entry
  instead of a session-level advisory lock. Row-level locks are released
  automatically at transaction end — exactly what PgBouncer transaction mode
  requires. If a concurrent refresh is already running for a given stream table,
  the scheduler skips that cycle and retries, rather than blocking.

- **New `pooler_compatibility_mode` option per stream table.** Setting
  `pooler_compatibility_mode => true` when creating or altering a stream table
  disables prepared statements and NOTIFY emissions for that table. Leave it off
  (the default) if you're not behind a pooler — behaviour is unchanged from
  v0.9.0.

- **PgBouncer tested end-to-end.** A new automated test suite boots PgBouncer in
  transaction-pool mode alongside pg_trickle and exercises the full lifecycle:
  create, refresh, alter, drop — all through the pooler. Run with
  `just test-pgbouncer`.

### Query Engine Correctness Fixes

Several SQL patterns that appeared to work correctly could produce wrong results
silently under the incremental query engine. All of the following are now fixed:

- **Recursive queries (WITH RECURSIVE) update correctly when rows are deleted.**
  Recursive queries are used for organisation hierarchies, bill-of-materials
  roll-ups, graph traversals, and similar structures. In DIFFERENTIAL mode,
  deleting a row from the source previously caused a full recomputation
  (correct, but expensive — O(n)). Now pg_trickle uses the Delete-and-Rederive
  algorithm, updating only affected rows at O(delta) cost. Computed expressions
  like `ancestor.path || ' > ' || node.name` update correctly when any ancestor
  is renamed or moved.

- **SUM over a FULL OUTER JOIN no longer returns 0 instead of NULL.** When
  matched rows on both join sides transition to matched on one side only (creating
  null-padded rows), the incremental SUM formula previously returned 0 instead of
  NULL. pg_trickle now tracks how many non-null values exist in each group and
  produces the correct answer without any full-group rescan.

- **Multi-source delta merging is now correct for diamond-shaped queries.** A
  "diamond" topology is when two separate paths through the dependency graph both
  feed into the same stream table (e.g. table A → both B and C → D). Simultaneous
  changes on both paths could previously cause some corrections to be silently
  discarded, leaving D with wrong values. Now uses proper weight aggregation
  (Z-set algebra) so every correction is applied. Six property-based tests verify
  this for different diamond shapes.

- **Statistical aggregates (CORR, COVAR, REGR_*) now update in constant time.**
  All twelve SQL correlation and regression functions — `CORR`, `COVAR_POP`,
  `COVAR_SAMP`, and the ten `REGR_*` variants — now update incrementally using
  running totals (Welford-style accumulation) instead of rescanning the whole
  group. Each changed row is processed once regardless of group size.

- **LATERAL subqueries only re-examine correlated rows.** When data changes in
  the inner part of a LATERAL JOIN, pg_trickle previously re-ran the subquery for
  every row in the outer table. Now it re-runs it only for outer rows that
  actually correlate with the changed inner data, reducing work from
  proportional-to-table-size to proportional-to-changes.

- **Materialized view sources now work in DIFFERENTIAL mode.** Stream tables
  can use a PostgreSQL materialized view as their data source when
  `pg_trickle.matview_polling = on` is set. Changes are detected by comparing
  snapshots, the same mechanism used for foreign table sources.

- **Six correctness bugs in the query rewriting engine fixed.** These all
  involved edge cases in how the incremental engine translates SQL:
  - SQL comment fragments such as `/* unsupported ... */` that were being
    injected into generated SQL and causing runtime syntax errors are now
    replaced with clear extension-level errors.
  - When a column-rename step (e.g. `EXTRACT(year FROM orderdate) AS o_year`)
    sits between an aggregate and its source, GROUP BY and aggregate expressions
    now resolve correctly.
  - `EXCEPT` queries wrapped in a projection no longer silently lose their row
    multiplicity tracking.
  - A placeholder row identifier value of zero could collide with real row
    hashes; changed to a sentinel value (`i64::MIN`) outside the normal hash
    range.
  - Empty scalar subqueries now raise a clear error instead of silently
    emitting NULL.

- **Change capture (CDC) fixes.** The UPDATE trigger now correctly handles rows
  with NULL values in their primary key columns (previously those rows were
  silently dropped from the change buffer). WAL logical replication publications
  are automatically rebuilt when a source table is converted to partitioned after
  the publication was set up — previously this caused the stream table to silently
  stop updating. TRUNCATE followed by INSERT is handled atomically so
  post-TRUNCATE inserts are never lost.

### Faster Refreshes

- **Automatic covering index on stream table row IDs.** Stream tables with eight
  or fewer output columns now automatically get a covering index with `INCLUDE
  (col1, col2, ...)` on the internal `__pgt_row_id` column. This lets the MERGE
  step use index-only scans — no heap lookups for matched rows — reducing refresh
  time by roughly 20–50% in small-delta / large-table scenarios.

- **Change buffer compaction.** When the pending change buffer grows beyond
  `pg_trickle.compact_threshold` (default 100,000 rows), pg_trickle compacts it
  before the next refresh cycle. INSERT→DELETE pairs that cancel each other out
  are eliminated; multiple sequential changes to the same row are collapsed to a
  single net change. Reduces delta scan overhead by 50–90% for high-churn tables.
  Uses `change_id` (not `ctid`) for safe operation under concurrent VACUUM.

- **Tiered refresh scheduling.** Large deployments can assign stream tables to
  one of four tiers: Hot (refresh at the configured interval), Warm (2× interval),
  Cold (10× interval), or Frozen (skip until manually promoted). Gate the feature
  with `pg_trickle.tiered_scheduling = on` (default off). Set per stream table
  via `ALTER STREAM TABLE ... SET (tier => 'warm')`. Frozen stream tables are
  entirely skipped by the scheduler until you promote them.

- **Incremental dependency-graph updates.** When a stream table is created,
  altered, or dropped, the internal dependency graph now updates only the affected
  entries instead of rebuilding the entire graph from scratch. Reduces the latency
  impact of DDL operations from roughly 50 ms to roughly 1 ms in deployments with
  1,000+ stream tables.

- **Smarter topo-sort caching inside a scheduler tick.** The ordering in which
  stream tables are refreshed (topological order through the dependency graph) is
  now computed once per scheduler tick and reused across all internal callers,
  eliminating redundant work.

### Better Visibility Into What pg_trickle Is Doing

Several behaviours that previously happened silently now produce a short,
actionable message at the moment they occur:

- **`ORDER BY` without `LIMIT` warns you at creation time.** Adding `ORDER BY`
  to a stream table's defining query without also adding `LIMIT` has no effect:
  stream table storage has no guaranteed row order. pg_trickle now emits a
  `WARNING` pointing you toward the TopK pattern or suggesting you remove the
  `ORDER BY`.

- **`append_only` mode reversions are visible.** When pg_trickle automatically
  exits append-only mode (because deletions or updates were detected in the
  source), the notice is now emitted at `WARNING` level (was `INFO`, normally
  suppressed) and also dispatched as a `pgtrickle_alert` notification.

- **Cleanup failures escalate after 3 consecutive attempts.** If the background
  worker fails to clean up a source table 3 times in a row, the message is
  promoted from `DEBUG1` (normally invisible) to `WARNING` so it appears in the
  server log.

- **Diamond dependency with `diamond_consistency='none'` now advises you.** When
  you create a stream table that forms a diamond in the dependency graph and
  explicitly set `diamond_consistency='none'`, a `NOTICE` advises you to consider
  `diamond_consistency='atomic'` for consistent cross-branch reads.

- **`diamond_consistency` now defaults to `'atomic'`.** New stream tables get
  atomic group semantics by default, meaning all branches of a diamond are
  refreshed together in a single savepoint before the convergence node is
  updated. This prevents a read from the convergence node seeing one branch
  partially updated and the other stale. To restore the old independent behavior,
  pass `diamond_consistency => 'none'` explicitly.

- **Adaptive fallback is visible at the default log level.** When a differential
  refresh falls back to a full refresh because the delta is too large, the
  message is now emitted at `NOTICE` level (the default `client_min_messages`
  threshold) instead of `INFO` (usually suppressed in the client session).

- **`CALCULATED` schedule without downstream dependents warns you.** When a
  stream table is created with `schedule='calculated'` but no existing stream
  table references it as a downstream dependent, a `NOTICE` explains that the
  schedule will fall back to `pg_trickle.default_schedule_seconds`.

- **Internal `__pgt_*` auxiliary columns are now documented.** The hidden
  columns that the refresh engine may add to stream table physical storage are
  described in a new section of [SQL_REFERENCE.md](docs/SQL_REFERENCE.md). This
  covers all variants from the always-present `__pgt_row_id` primary key through
  the aggregate-specific auxiliary columns for AVG, STDDEV, CORR, COVAR, REGR_*,
  window functions, and recursive CTE depth.

### Bug Fixes

- **Scheduler no longer permanently misses stream tables created under a
  stale snapshot.** `signal_dag_invalidation` is called inside the creating
  transaction before it commits. If the background scheduler happened to
  start a new tick and capture a catalog snapshot at that exact instant, the
  DAG rebuild query would not see the new stream table — yet the version
  counter was already advanced, so the scheduler would never rebuild again.
  The affected stream table would then never be scheduled for refresh.
  Fixed by verifying that every invalidated `pgt_id` is present in the
  rebuilt DAG after each rebuild. If any are missing the scheduler signals
  a full-rebuild for the next tick (which starts a fresh transaction that
  includes all committed data) rather than accepting the stale version.
  Fixes CI test `test_autorefresh_diamond_cascade`.

### Upgrade Notes

- **New catalog columns.** The `0.9.0 → 0.10.0` upgrade migration adds
  `pooler_compatibility_mode BOOLEAN` and `refresh_tier TEXT` to
  `pgt_stream_tables`. Run `ALTER EXTENSION pg_trickle UPDATE TO '0.10.0'`
  after replacing the extension files. Verification script:
  `scripts/check_upgrade_completeness.sh`.

- **Hidden auxiliary columns for statistical aggregates.** Stream tables using
  `CORR`, `COVAR_POP`, `COVAR_SAMP`, or any `REGR_*` aggregate will get hidden
  `__pgt_aux_*` columns when created or altered under 0.10.0. These are
  invisible to normal queries (excluded by the `NOT LIKE '__pgt_%'` convention)
  and managed automatically.

- **`pooler_compatibility_mode` is off by default.** Existing stream tables are
  unaffected. Enable it only for stream tables accessed through PgBouncer
  transaction-mode pooling.

### Additional Bug Fixes (2026-03-24)

**Scheduler stability:**

- **Scheduler no longer crashes when concurrent refreshes compete.** The
  internal function that decides whether to skip a refresh cycle was running a
  locking query outside a transaction boundary — a strict PostgreSQL requirement.
  It now runs inside a proper subtransaction, eliminating the crash.

- **Auto-backoff no longer causes a transaction conflict in the background
  worker.** When the auto-backoff feature stretches a stream table's refresh
  interval, it previously tried to open a new transaction inside the background
  worker's already-open transaction. PostgreSQL does not allow this nesting; the
  code path is now restructured to avoid it.

**Query engine correctness:**

- **Queries that filter on hidden columns now produce correct results.** For
  example, `SELECT name FROM users WHERE internal_id > 5` — where `internal_id`
  is not part of the output — could return wrong rows during incremental updates.
  Fixed.

- **JOIN results are correct when both joined tables change at the same time.**
  Simultaneous changes to two stream tables connected by a JOIN could leave the
  output with stale or duplicated rows. Fixed.

- **`NULLIF(a, b)` expressions now work in incremental queries.** `NULLIF`
  returns NULL when its two arguments are equal. It was not recognised by the
  incremental parser, causing a fallback error. Fixed.

- **`LIKE` and `ILIKE` pattern matching now work in filter conditions.** Filter
  expressions such as `WHERE name LIKE 'A%'` or `WHERE description ILIKE
  '%widget%'` were not handled by the incremental engine. Fixed.

- **Subqueries with `ORDER BY`, `LIMIT`, or `OFFSET` are now preserved
  correctly.** When the incremental engine reconstructed a subquery, those
  clauses were silently dropped. The incremental result no longer differs from a
  full refresh for such queries.

- **Scalar subqueries using `LIMIT` or `OFFSET` are now handled gracefully.**
  Rather than producing a runtime error, the engine falls back to a full refresh
  for those cases and continues.

**SQL parser:**

- **Wildcard column references (`table.*`) now work for qualified names.** A
  two- or three-part column reference such as `schema.table.*` or `alias.*`
  caused a parser crash. Fixed.

**Change capture and WAL:**

- **State transitions no longer stall when the WAL replication slot is behind.**
  When a stream table moves through the TRANSITIONING state, pg_trickle now
  advances the WAL replication slot up-front. This eliminates a lag-check stall
  that could cause the transition to hang indefinitely under write-heavy
  workloads.

**Security:**

- Several low-severity code quality and security scanner alerts from Semgrep and
  CodeQL are resolved. No user-visible behaviour changes.

---

## [0.9.0] — Incremental Aggregates & Smarter Scheduling

The headline feature of 0.9.0 is **incremental aggregate maintenance**: when a
single row changes inside a group of 100,000 rows, pg_trickle no longer has to
re-scan all 100,000 rows to update COUNT, SUM, AVG, STDDEV, or VAR results.
Instead it keeps running totals and adjusts them in constant time. Only MIN/MAX
still needs a rescan — and only when the deleted value happens to be the current
extreme.

Beyond aggregates, this release contains a broad set of performance
optimizations that reduce wasted I/O during every refresh cycle, two new
configuration knobs, a refresh-group management API, and several bug fixes.

### Faster Aggregates

- **Constant-time COUNT, SUM, AVG**: Changed rows are now applied
  algebraically (`new_sum = old_sum + inserted − deleted`) instead of
  re-aggregating the whole group.  AVG uses hidden auxiliary SUM and COUNT
  columns maintained automatically on the stream table.
- **Constant-time STDDEV and VAR**: Standard-deviation and variance
  aggregates (`STDDEV_POP`, `STDDEV_SAMP`, `VAR_POP`, `VAR_SAMP`) now
  use a sum-of-squares decomposition with a hidden auxiliary column,
  achieving the same constant-time update as COUNT/SUM/AVG.
- **MIN/MAX safety guard**: Deleting the row that currently holds the
  minimum (or maximum) value correctly triggers a rescan of that group.
  Property-based tests verify this boundary.
- **Floating-point drift reset**: A new setting
  (`pg_trickle.algebraic_drift_reset_cycles`) periodically forces a full
  recomputation to correct any floating-point rounding drift that
  accumulates over many incremental cycles.

### Smarter Refresh Scheduling

- **Automatic backoff for overloaded streams**: The `pg_trickle.auto_backoff`
  GUC was introduced here (default off at the time). See the v0.10.0 entry
  for the improved thresholds, reduced cap, and the flip to `on` by default.
- **Index-aware MERGE**: A new threshold setting
  (`pg_trickle.merge_seqscan_threshold`, default 0.001) tells PostgreSQL
  to use an index lookup instead of a full table scan when only a tiny
  fraction of the stream table's rows are changing.

### Less Wasted I/O

- **Skip unchanged columns**: The scan operator now checks the CDC
  trigger's per-row bitmask to skip UPDATE rows where none of the columns
  your query actually uses were modified.  For wide tables where you only
  reference a few columns, most UPDATE processing is eliminated.
- **Skip unchanged sources in joins**: When a multi-source join query has
  three source tables but only one of them changed, the delta branches for
  the two unchanged sources are now replaced with `FALSE` at plan time.
  PostgreSQL's planner recognises those branches as empty and skips them
  entirely.
- **Push WHERE filters into the change scan**: If your stream table's
  defining query has a WHERE clause (e.g. `WHERE status = 'shipped'`),
  that filter is now applied immediately after reading the change buffer
  — before rows enter the join or aggregate pipeline.  Rows that don't
  match the filter are discarded right away.
- **Faster DISTINCT counting**: The per-row multiplicity lookup for
  `SELECT DISTINCT` queries now uses an index-driven scalar subquery
  instead of a LEFT JOIN, guaranteeing I/O proportional to the number of
  changed rows regardless of stream table size.
- **Scalar subquery short-circuit**: When a scalar subquery's inner source
  has no changes in the current cycle, the expensive outer-table snapshot
  reconstruction is skipped entirely.

### Refresh Group Management

- **New SQL functions** for grouping stream tables that should always be
  refreshed together (cross-source snapshot consistency):
  - `pgtrickle.create_refresh_group(name, members, isolation)`
  - `pgtrickle.drop_refresh_group(name)`
  - `pgtrickle.refresh_groups()` — lists all declared groups.

### Bug Fixes

- **Fixed a crash when internal status queries failed**: The
  `source_gates()` and `watermarks()` SQL functions previously crashed the
  entire PostgreSQL backend process on any internal error.  They now report
  a normal SQL error instead.
- **Clearer handling of window functions in expressions**: Queries like
  `CASE WHEN ROW_NUMBER() OVER (...) > 5 THEN ...` were silently accepted
  but failed at refresh time with a confusing error.  pg_trickle now
  automatically falls back to full refresh mode (in AUTO mode) or warns
  you at creation time (in explicit DIFFERENTIAL mode).

### Documentation

- Documented the known limitation that recursive CTE stream tables in
  DIFFERENTIAL mode fall back to full recomputation when rows are deleted
  or updated.  Workaround: use `refresh_mode = 'IMMEDIATE'`.
- Documented the `pgt_refresh_groups` catalog table schema and usage.
- Documented the O(partition_size) cost of window function maintenance with
  mitigation strategies.

### Deferred to v0.10.0

The following performance optimizations were evaluated and explicitly deferred.
In every case the current behaviour is **correct** — these items would make
certain workloads faster but carry enough implementation risk that they need
more design work first:
- Recursive CTE incremental delete/update in DIFFERENTIAL mode (P2-1)
- SUM NULL-transition shortcut for FULL OUTER JOIN aggregates (P2-2)
- Materialized view sources in IMMEDIATE mode (P2-4)
- LATERAL subquery scoped re-execution (P2-6)
- Welford auxiliary columns for CORR/COVAR/REGR_* aggregates (P3-2)
- Merged-delta weight aggregation for multi-source deduplication (B3-2/B3-3)

### Upgrade Notes

- **New SQL objects**: The `0.8.0 → 0.9.0` upgrade migration adds the
  `pgt_refresh_groups` table and the `restore_stream_tables` function.
  Run `ALTER EXTENSION pg_trickle UPDATE TO '0.9.0'` after replacing the
  extension files.
- **Hidden auxiliary columns**: Stream tables using AVG, STDDEV, or VAR
  aggregates will automatically get hidden `__pgt_aux_*` columns when
  created or altered.  These columns are invisible to normal queries
  (filtered by the existing `NOT LIKE '__pgt_%'` convention) and are
  managed automatically.
- **PGXN publishing**: Release artifacts are now automatically uploaded to
  PGXN via GitHub Actions.

---

## [0.8.0] — Backup, Pooler Compatibility & Reliability

This release focuses on making your streams easier to back up, far more reliable under complex scenarios, and solidifying the underlying core engine through massive testing improvements.

### Added
- **Backup and Restore Support**: You can now safely backup your database using standard `pg_dump` and `pg_restore` commands. The system will automatically reconnect all streams and data queues to eliminate downtime during disaster recovery.
- **Connection Pooler Opt-In**: Replaced the global PgBouncer pooler compatibility setting with a per-stream option. You can now enable connection pooling optimizations selectively on a stream-by-stream basis.

### Fixed
- **Cyclic Stream Reliability**: Fixed internal bugs that occasionally caused streams referencing each other in a loop to get stuck refreshing forever. Streams now accurately detect when row changes stop and naturally settle.
- **Large Dependency Chains**: Fixed a crash (stack overflow) that could happen if you attempted to drop an extremely large or heavily recursive chain of stream tables sequentially.
- **Special Character Support in SQL**: Handled an edge case causing errors when multi-byte characters or special non-ASCII symbols were parsed inside certain SQL commands.
- **Mac Support for Developer Tooling**: Addressed a minor internal tool error stopping test components from automatically building on Apple Silicon machines.

### Under the Hood Code and Testing Enhancements
- **Massive Testing Hardening**: We have fundamentally overhauled and upgraded how we test the system. Our internal test suite has been completely enhanced with tens of thousands of continuous automated checks ensuring query answers are perfect, no matter how complex the data joins or updates get.
- **Performance Migrations**: Began adopting new tools (`cargo nextest`) to speed up how fast we can iterate and develop the software in the background.

## [0.7.0] — Watermark Gating, Circular Pipelines & SQL Broadening

0.7.0 makes pg_trickle easier to trust in real-world data pipelines. The big
theme of this release is fewer surprises: the scheduler can now wait for late
arriving source data, some circular pipelines can run safely instead of being
blocked, more queries stay on incremental refresh, and the system does a better
job of deciding when incremental work is no longer worth it.

### Added

#### Multi-source data can wait until it is actually ready

pg_trickle can now delay a refresh until related source tables have all caught
up to roughly the same point in time. This is useful for ETL jobs where, for
example, `orders` arrives before `order_lines` and refreshing too early would
produce a half-finished report.

- New watermark APIs: `advance_watermark(source, watermark)`,
  `create_watermark_group(name, sources[], tolerance_secs)`, and
  `drop_watermark_group(name)`.
- New status helpers: `watermarks()`, `watermark_groups()`, and
  `watermark_status()`.
- The scheduler now skips gated refreshes when grouped sources are too far
  apart and records the reason in refresh history.
- New catalog tables store per-source watermarks and watermark group
  definitions.
- 28 end-to-end tests cover normal operation, bad input, tolerance windows,
  and scheduler behavior.

#### Some circular pipelines can now run safely

Stream tables that depend on each other in a loop are no longer always blocked.
If the cycle is monotone and uses DIFFERENTIAL mode, pg_trickle can now keep
refreshing the group until it stops changing.

- Circular refreshes run to a fixed point, with `pg_trickle.max_fixpoint_iterations`
  as a safety limit.
- Cycle creation and ALTER validation now check that every member is safe for
  convergence before allowing the loop.
- `pgtrickle.pgt_status()` now reports `scc_id`, and
  `pgtrickle.pgt_scc_status()` shows per-cycle-group status.
- `pgtrickle.pgt_stream_tables` now tracks `last_fixpoint_iterations` so it is
  easier to spot slow or unstable cycles.
- 6 end-to-end tests cover convergence, rejection of unsafe cycles,
  non-convergence handling, and cleanup.

#### More queries stay on incremental refresh

Several query shapes that used to fall back to FULL refresh, or fail outright,
now keep working in DIFFERENTIAL and AUTO mode.

- User-defined aggregates created with `CREATE AGGREGATE` now work through the
  existing group-rescan strategy, including common extension-provided
  aggregates.
- More complex `OR` plus subquery patterns are now rewritten correctly,
  including cases that need De Morgan normalization and multiple rewrite passes.
- The rewrite pipeline has a guardrail to stop runaway branch explosion.
- A dedicated 14-test end-to-end suite covers these previously missing cases.

#### Easier packaging ahead of 1.0

The release also adds infrastructure that makes evaluation and future
distribution simpler.

- `Dockerfile.hub` and a dedicated CI workflow can build and smoke-test a
  ready-to-run PostgreSQL 18 image with pg_trickle preinstalled.
- `META.json` adds PGXN package metadata with `release_status: "testing"`.
- CNPG smoke testing is now part of the documented pre-1.0 packaging story.

### Improved

#### Refresh strategy and performance decisions are smarter

The scheduler and refresh engine now make better choices when incremental work
is likely to help and back off sooner when it is not.

- Wide tables now use xxh64-based change detection instead of slower MD5-based
  comparisons.
- Aggregate stream tables can skip expensive incremental work and jump straight
  to FULL refresh when the pending change set is obviously too large.
- Strategy selection now combines a change-ratio signal with recent refresh
  history, which helps on workloads with uneven batch sizes.
- DAG levels are extracted explicitly, enabling level-parallel refresh
  scheduling.
- Small internal hot paths such as column-list building and LSN comparison were
  tightened to remove avoidable allocations.

#### Benchmarking is much easier to use and compare

The performance toolchain was expanded so regressions are easier to spot and
large-scale behavior is easier to study.

- Benchmarks now support per-cycle output, optional `EXPLAIN ANALYZE` capture,
  larger 1M-row runs, and more stable Criterion settings.
- New tooling covers cross-run comparison, concurrent writers, and extra query
  shapes such as window, lateral, CTE, and `UNION ALL` workloads.
- `just bench-docker` makes it easier to run Criterion inside the builder image
  when local linking is awkward.

### Changed
#### Internal Code Quality: Integration Test Suite Hardening

Completed a full hardening pass of the integration test suite, bringing all items in `PLAN_TEST_EVALS_INTEGRATION.md` to done:
- **Multiset validation** — Extracted `assert_sets_equal()` helper relying on EXCEPT/UNION ALL SQL logic and applied it to workflow tests to ensure storage table state correctly matches the defining query post-refresh.
- **Round-trip notifications** — `pg_trickle_alert` notifications now verify receipt end-to-end via `sqlx::PgListener`.
- **DVM operators** — Added unit coverage for complex semi/anti-join behaviors (multi-column, filtered, complementary), multi-table join chains for inner and full joins, and `proptest!` fuzz tests enforcing generated SQL invariants across INNER, SEMI, and ANTI joins.
- **Resilience and edge cases** — Test coverage for ST drop cascades verifying dependent object removal, exact error escalation thresholds, and scheduler job lifecycles across queued mock states.
- **Cleanups** — Standardized naming practices (`test_workflow_*`, `test_infra_*`) and eliminated clock-bound flakes by widening staleness assertions.


#### Internal low-level code is much safer to audit

This release cuts the amount of low-level `unsafe` Rust in half without
changing behavior.

- Unsafe blocks were reduced by 51%, from 1,309 to 641.
- Repeated patterns were consolidated into a small set of documented helper
  functions.
- 37 internal functions no longer need to be marked `unsafe`.
- Existing unit tests continued to pass unchanged after the refactor.

---

## [0.6.0] — Idempotent DDL, Partitioned Sources & dbt Integration

### Added

#### Idempotent DDL (`create_or_replace`)

New one-call function for deploying stream tables without worrying about
whether they already exist. Replaces the old "check if it exists, then drop
and recreate" pattern.

- **`create_or_replace_stream_table()`** — a single function that does the
  right thing automatically:
  - **Creates** the stream table if it doesn't exist yet.
  - **Does nothing** if the stream table already exists with the same query
    and settings (logs an INFO so you know it was a no-op).
  - **Updates settings** (schedule, refresh mode, etc.) if only config changed.
  - **Replaces the query** if the defining query changed — including
    automatic schema migration and a full refresh.
- **dbt uses it automatically.** The `stream_table` materialization now calls
  `create_or_replace_stream_table()` when running against pg_trickle 0.6.0+,
  with automatic fallback for older versions.
- **Whitespace-insensitive.** Cosmetic SQL differences (extra spaces, tabs,
  newlines) are correctly treated as no-ops — won't trigger unnecessary
  rebuilds.

#### dbt Integration Enhancements

- **Check stream table health from dbt.** New `pgtrickle_stream_table_status()`
  macro returns whether a stream table is healthy, stale, erroring, or paused.
  Pair it with the new built-in `stream_table_healthy` test in your
  `schema.yml` to fail CI when a stream table is behind or broken.
- **Refresh everything in the right order.** New `refresh_all_stream_tables`
  run-operation refreshes all dbt-managed stream tables in dependency order.
  Run it after `dbt run` and before `dbt test` in your CI pipeline.

#### Partitioned Source Tables

Stream tables now work with PostgreSQL's declarative table partitioning —
RANGE, LIST, and HASH partitioned tables all work as sources out of the box.

- **Changes in any partition are captured automatically.** CDC triggers fire
  on the parent table so inserts, updates, and deletes in any child partition
  are picked up.
- **ATTACH PARTITION triggers automatic rebuild.** When you attach a new
  partition, pg_trickle detects the structural change and rebuilds affected
  stream tables to include the new partition's pre-existing data.
- **WAL mode works with partitions.** Publications are configured with
  `publish_via_partition_root = true`, so all partitions report changes under
  the parent table's identity.
- **New tutorial** covering partitioned source tables, ATTACH/DETACH behavior,
  and known caveats (`docs/tutorials/PARTITIONED_TABLES.md`).

#### Circular Dependency Foundation

Lays the groundwork for stream tables that reference each other in a cycle
(A → B → A). The actual cyclic refresh execution is planned for v0.7.0 —
this release adds the detection, validation, and safety infrastructure.

- **Cycle detection.** pg_trickle can now identify groups of stream tables
  that form circular dependencies.
- **Safety checks at creation time.** Queries that can't safely participate
  in a cycle (those using aggregates, EXCEPT, window functions, or NOT EXISTS)
  are rejected with a clear error explaining why.
- **New settings:**
  - `pg_trickle.allow_circular` (default: off) — master switch for circular
    dependencies.
  - `pg_trickle.max_fixpoint_iterations` (default: 100) — prevents runaway
    loops.

#### Source Gating Improvements

- **`bootstrap_gate_status()` function.** Shows which sources are currently
  gated, when they were gated, how long the gate has been active, and which
  stream tables are waiting. Useful for debugging "why isn't my stream table
  refreshing?"
- **ETL coordination cookbook.** SQL Reference now includes five step-by-step
  recipes for common bulk-load patterns.

#### More SQL Patterns Supported

Two query patterns that previously required workarounds now just work:

- **Window functions inside expressions.** Queries like
  `CASE WHEN ROW_NUMBER() OVER (...) = 1 THEN 'top' ELSE 'other' END` or
  `COALESCE(SUM() OVER (...), 0)` are now accepted and produce correct
  results. Use **FULL** refresh mode for these queries — incremental
  (DIFFERENTIAL) refresh of window-in-expression patterns is not yet
  supported. Previously, the query was rejected entirely at creation time.

- **`ALL (subquery)` comparisons.** Queries like
  `WHERE price < ALL (SELECT price FROM competitors)` are now accepted in
  both FULL and DIFFERENTIAL modes. Supports all comparison operators
  (`>`, `>=`, `<`, `<=`, `=`, `<>`) and correctly handles NULL values per
  the SQL standard.

#### Operational Safety Improvements

- **Function changes detected automatically.** If a stream table's query
  calls a user-defined function and you update that function with
  `CREATE OR REPLACE FUNCTION`, pg_trickle detects the change and
  automatically rebuilds the stream table on the next cycle. No manual
  intervention needed.

- **WAL mode explains why it isn't activating.** When `cdc_mode = 'auto'`
  and the system stays on trigger-based tracking, the scheduler now
  periodically logs the exact reason (e.g., "`wal_level` is not `logical`")
  and `check_cdc_health()` reports the current mode so you can diagnose the
  issue.

- **WAL + keyless tables rejected early.** Creating a stream table with
  `cdc_mode = 'wal'` on a table that has no primary key and no
  `REPLICA IDENTITY FULL` is now rejected at creation time with a clear
  error — instead of silently producing incomplete results later.

- **Automatic recovery after backup/restore.** When a PostgreSQL server is
  restored from `pg_basebackup`, WAL replication slots are lost. pg_trickle
  now detects the missing slot, automatically falls back to trigger-based
  tracking, and logs a WARNING so you know what happened.

#### Documentation

- **ALL (subquery) worked example** in the SQL Reference with sample data
  and expected results.
- **Window-in-expression documentation** showing before/after examples of
  the automatic rewrite.
- **Foreign table sources tutorial** — step-by-step guide for using
  `postgres_fdw` foreign tables as stream table sources.

### Fixed

- **`create_or_replace` whitespace handling.** Extra spaces, tabs, and
  newlines in queries no longer trigger unnecessary rebuilds.
- **`create_or_replace` schema incompatibility detection.** Incompatible
  column type changes (e.g., text → integer) are now properly detected
  and handled.

---

## [0.5.0] — Row-Level Security, Source Gating & Append-Only Fast Path

### Added

#### Row-Level Security (RLS) Support

Stream tables now work correctly with PostgreSQL's Row-Level Security feature,
which lets you control which rows different users can see.

- **Refreshes always see all data.** When a stream table is refreshed, it
  computes the full result regardless of RLS policies on the source tables.
  This matches how PostgreSQL's built-in materialized views work. You then
  add RLS policies directly on the stream table to control who can read what.
- **Internal tables are protected.** The internal change-tracking tables used
  by pg_trickle are shielded from RLS interference, so refreshes won't
  silently fail if you turn on RLS at the schema level.
- **Real-time (IMMEDIATE) mode secured.** Triggers that keep stream tables
  updated in real time now run with elevated privileges and a locked-down
  search path, preventing data corruption or security bypasses.
- **RLS changes are detected automatically.** If you enable, disable, or force
  RLS on a source table, pg_trickle detects the change and marks affected
  stream tables for a full rebuild.
- **New tutorial.** Step-by-step guide for setting up per-tenant RLS policies
  on stream tables (see `docs/tutorials/ROW_LEVEL_SECURITY.md`).

#### Source Gating for Bulk Loads

New pause/resume mechanism for large data imports. When you're loading a big
batch of data into a source table, you can temporarily "gate" it to prevent
the background scheduler from triggering refreshes mid-load. Once the load is
done, ungate it and everything catches up in a single refresh.

- **`gate_source('my_table')`** — pauses automatic refreshes for any stream
  table that depends on `my_table`.
- **`ungate_source('my_table')`** — resumes automatic refreshes. All changes
  made during the gate are picked up in the next refresh cycle.
- **`source_gates()`** — shows which source tables are currently gated, when
  they were gated, and by whom.
- **Manual refresh still works.** Even while a source is gated, you can
  explicitly call `refresh_stream_table()` if needed.
- Gating is idempotent — calling `gate_source()` twice is safe, and gating a
  source that's already gated is a no-op.

#### Append-Only Fast Path

Significant performance improvement for tables that only receive INSERTs
(event logs, audit trails, time-series data, etc.). When you mark a stream
table as `append_only`, refreshes skip the expensive merge logic (checking
for deletes, updates, and row comparisons) and use a simple, fast insert.

- **How to use:** Pass `append_only => true` when creating or altering a
  stream table.
- **Safe fallback.** If a DELETE or UPDATE is detected on a source table, the
  extension automatically falls back to the standard refresh path and logs a
  warning. It won't silently produce wrong results.
- **Restrictions.** Append-only mode requires DIFFERENTIAL refresh mode and
  source tables with primary keys.

#### Usability Improvements

- **Manual refresh history.** When you manually call `refresh_stream_table()`,
  the result (success or failure, timing, rows affected) is now recorded in
  the refresh history, just like scheduled refreshes.
- **`quick_health` view.** A single-row health summary showing how many stream
  tables you have, how many are in error or stale, whether the scheduler is
  running, and an overall status (`OK`, `WARNING`, `CRITICAL`). Easy to plug
  into monitoring dashboards.
- **`create_stream_table_if_not_exists()`.** A convenience function that does
  nothing if the stream table already exists, instead of raising an error.
  Makes migration scripts and deployment automation simpler.

#### Smooth Upgrade from 0.4.0

- Existing installations can upgrade with
  `ALTER EXTENSION pg_trickle UPDATE TO '0.5.0'`. All new features (source
  gating, append-only mode, quick health view, and the new convenience
  functions) are included in the upgrade script.
- The upgrade has been verified with automated tests that confirm all 40 SQL
  objects survive the upgrade intact.

---

## [0.4.0] — Parallel Refresh & Statement-Level CDC Triggers

### Added

#### Parallel Refresh (opt-in)

Stream tables can now be refreshed in parallel, using multiple background
workers instead of processing them one at a time. This can dramatically reduce
end-to-end refresh latency when you have many independent stream tables.

- **Off by default.** Set `pg_trickle.parallel_refresh_mode = 'on'` to enable.
  Use `'dry_run'` to preview what the scheduler would do without changing
  behavior.
- **Automatic dependency awareness.** The scheduler figures out which stream
  tables can safely refresh at the same time and which must wait for others.
  Stream tables connected by real-time (IMMEDIATE) triggers are always
  refreshed together to prevent race conditions.
- **Atomic groups.** When a group of stream tables must succeed or fail
  together (e.g. diamond dependencies), all members are wrapped in a single
  transaction — if one fails, the whole group rolls back cleanly.
- **Worker pool controls:**
  - `pg_trickle.max_dynamic_refresh_workers` (default 4) — cluster-wide cap on
    concurrent refresh workers.
  - `pg_trickle.max_concurrent_refreshes` — per-database dispatch cap.
- **Monitoring:**
  - `worker_pool_status()` — shows how many workers are active and the current
    limits.
  - `parallel_job_status(max_age_seconds)` — lists recent and active refresh
    jobs with timing and status.
  - `health_check()` now warns when the worker pool is saturated or the job
    queue is backing up.
- **Self-healing.** On startup, the scheduler automatically cleans up orphaned
  jobs and reclaims leaked worker slots from previous crashes.

#### Statement-Level CDC Triggers

Change tracking triggers have been upgraded from row-level to statement-level,
reducing write-side overhead for bulk INSERT and UPDATE operations. This is
now the default for all new and existing stream tables. A benchmark harness is
included so you can measure the difference on your own hardware.

#### dbt Getting Started Example

New `examples/dbt_getting_started/` project with a complete, runnable dbt
example showing org-chart seed data, staging views, and three stream table
models. Includes an automated test script.

### Fixed

#### Refresh Lock Not Released After Errors

Fixed a bug where `refresh_stream_table()` could get permanently stuck after
a PostgreSQL error (e.g. running out of temp file space). The internal lock
was session-level and survived transaction rollback, causing all future
refreshes for that stream table to report "another refresh is already in
progress". Refresh locks are now transaction-level, so they are automatically
released when the transaction ends — whether it succeeds or fails.

#### dbt Integration Fixes

- Fixed query quoting in dbt macros that broke when queries contained single
  quotes.
- Fixed `schedule = none` in dbt being incorrectly mapped to SQL NULL.
- Fixed view inlining when the same view was referenced with different aliases.

### Changed
#### Internal Code Quality: Integration Test Suite Hardening

Completed a full hardening pass of the integration test suite, bringing all items in `PLAN_TEST_EVALS_INTEGRATION.md` to done:
- **Multiset validation** — Extracted `assert_sets_equal()` helper relying on EXCEPT/UNION ALL SQL logic and applied it to workflow tests to ensure storage table state correctly matches the defining query post-refresh.
- **Round-trip notifications** — `pg_trickle_alert` notifications now verify receipt end-to-end via `sqlx::PgListener`.
- **DVM operators** — Added unit coverage for complex semi/anti-join behaviors (multi-column, filtered, complementary), multi-table join chains for inner and full joins, and `proptest!` fuzz tests enforcing generated SQL invariants across INNER, SEMI, and ANTI joins.
- **Resilience and edge cases** — Test coverage for ST drop cascades verifying dependent object removal, exact error escalation thresholds, and scheduler job lifecycles across queued mock states.
- **Cleanups** — Standardized naming practices (`test_workflow_*`, `test_infra_*`) and eliminated clock-bound flakes by widening staleness assertions.


- Updated to PostgreSQL 18.3 across CI and test infrastructure.
- Dependency updates: `tokio` 1.49 → 1.50 and several GitHub Actions bumps.

### Breaking Changes

These behavioural changes shipped in v0.4.0. They improve usability but may
require action from users upgrading from v0.3.0.

- **Schedule default changed from `'1m'` to `'calculated'`.**
  `create_stream_table` now defaults to `schedule => 'calculated'`, which
  auto-computes the refresh interval from downstream dependents instead of
  refreshing every 1 minute. If you relied on the implicit 1-minute default,
  explicitly pass `schedule => '1m'` to preserve the old behaviour.

- **`NULL` schedule input rejected.** Passing `schedule => NULL` to
  `create_stream_table` now returns an error. Use `schedule => 'calculated'`
  instead — it's explicit and self-documenting.

- **Diamond GUCs removed.** The cluster-wide GUCs
  `pg_trickle.diamond_consistency` and `pg_trickle.diamond_schedule_policy`
  have been removed. Diamond behaviour is now controlled per-table via
  parameters on `create_stream_table()` / `alter_stream_table()`:
  `diamond_consistency => 'atomic'`, `diamond_schedule_policy => 'slowest'`.

---

## [0.3.0] — Incremental Correctness & Security Tooling

This is a correctness and hardening release. No new SQL functions, tables, or
views were added — all changes are in the compiled extension code.
`ALTER EXTENSION pg_trickle UPDATE` is safe and a no-op for schema objects.

### Fixed

#### Incremental Correctness Fixes

All 18 previously-disabled correctness tests have been re-enabled (0
remaining). The following query patterns now produce correct results during
incremental (non-full) refreshes:

- **HAVING clause threshold crossing.** Queries with `HAVING` filters (e.g.
  `HAVING SUM(amount) > 100`) now produce correct totals when groups cross
  the threshold. Previously, a group gaining enough rows to meet the condition
  would show only the newly added values instead of the correct total.

- **FULL OUTER JOIN.** Five bugs affecting incremental updates for
  `FULL OUTER JOIN` queries are fixed: mismatched row identifiers, incorrect
  handling of compound GROUP BY expressions like
  `COALESCE(left.col, right.col)`, and wrong NULL handling for SUM aggregates.

- **EXISTS with HAVING subqueries.** Queries using
  `WHERE EXISTS(... GROUP BY ... HAVING ...)` now work correctly — the inner
  GROUP BY and HAVING were previously being silently discarded.

- **Correlated scalar subqueries.** Correlated subqueries in SELECT like
  `(SELECT MAX(e.salary) FROM emp e WHERE e.dept_id = d.id)` are now
  automatically rewritten into LEFT JOINs so the incremental engine can
  handle them correctly.

#### Background Worker Detection on PostgreSQL 18

Fixed a bug where `health_check()` and the scheduler reported zero active
workers on PostgreSQL 18 due to a column name change in system views.

#### Scheduler Stability

Fixed a loop where the scheduler launcher could get stuck retrying failed
database probes indefinitely instead of backing off properly.

### Added

#### Security Tooling

Added static security analysis to the CI pipeline:

- **GitHub CodeQL** — automated security scanning across all Rust source files.
  First scan: zero findings.
- **`cargo deny`** — enforces a license allow-list and flags unmaintained or
  yanked dependencies.
- **Semgrep** — custom rules that flag potentially dangerous patterns such as
  dynamic SQL construction and privilege escalation. Advisory-only (does not
  block merges).
- **Unsafe block inventory** — CI tracks the count of unsafe code blocks per
  file and fails if any file exceeds its baseline, preventing unreviewed
  growth of low-level code.

## [0.2.3] — Per-Table CDC Mode & WAL Lag Monitoring

### Added

- **Unsafe function detection.** Queries using non-deterministic functions like
  `random()` or `clock_timestamp()` are now rejected when creating incremental
  stream tables, because they can't produce reliable results. Functions like
  `now()` that return the same value within a transaction are allowed with a
  warning.

- **Per-table change tracking mode.** You can now choose how each stream table
  tracks changes (`'auto'`, `'trigger'`, or `'wal'`) via the `cdc_mode`
  parameter on `create_stream_table()` and `alter_stream_table()`, instead of
  relying only on the global setting.

- **CDC status view.** New `pgtrickle.pgt_cdc_status` view shows the change
  tracking mode, replication slot, and transition status for every source
  table in one place.

- **Configurable WAL lag thresholds.** The warning and critical thresholds for
  replication slot lag are now configurable via
  `pg_trickle.slot_lag_warning_threshold_mb` (default 100 MB) and
  `pg_trickle.slot_lag_critical_threshold_mb` (default 1024 MB), instead of
  being hard-coded.

- **`pg_trickle_dump` backup tool.** New standalone CLI that exports all your
  stream table definitions as replayable SQL, ordered by dependency. Useful
  for backups before upgrades or migrations.

- **Upgrade path.** `ALTER EXTENSION pg_trickle UPDATE` picks up all new
  features from this release.

### Changed
#### Internal Code Quality: Integration Test Suite Hardening

Completed a full hardening pass of the integration test suite, bringing all items in `PLAN_TEST_EVALS_INTEGRATION.md` to done:
- **Multiset validation** — Extracted `assert_sets_equal()` helper relying on EXCEPT/UNION ALL SQL logic and applied it to workflow tests to ensure storage table state correctly matches the defining query post-refresh.
- **Round-trip notifications** — `pg_trickle_alert` notifications now verify receipt end-to-end via `sqlx::PgListener`.
- **DVM operators** — Added unit coverage for complex semi/anti-join behaviors (multi-column, filtered, complementary), multi-table join chains for inner and full joins, and `proptest!` fuzz tests enforcing generated SQL invariants across INNER, SEMI, and ANTI joins.
- **Resilience and edge cases** — Test coverage for ST drop cascades verifying dependent object removal, exact error escalation thresholds, and scheduler job lifecycles across queued mock states.
- **Cleanups** — Standardized naming practices (`test_workflow_*`, `test_infra_*`) and eliminated clock-bound flakes by widening staleness assertions.


- After a full refresh, WAL replication slots are now advanced to the current
  position, preventing unnecessary WAL accumulation and false lag alarms.
- Change buffers are now flushed after a full refresh, fixing a cycle where
  the scheduler would alternate endlessly between incremental and full
  refreshes on bulk-loaded tables.
- IMMEDIATE mode now correctly rejects explicit WAL CDC requests with a clear
  error, since real-time mode uses its own trigger mechanism.
- The `pg_trickle.user_triggers` setting is simplified to `auto` and `off`.
  The old `on` value still works as an alias for `auto`.
- CI pipelines are faster on PRs — only essential tests run; the full suite
  runs on merge and daily schedule.

---

## [0.2.2] — AUTO Refresh Mode & Query Alteration

### Added

- **Change a stream table's query.** `alter_stream_table` now accepts a
  `query` parameter, so you can change what a stream table computes without
  dropping and recreating it. If the new query's columns are compatible, the
  underlying storage table is preserved — existing views, policies, and
  publications continue to work.

- **AUTO refresh mode (new default).** Stream tables now default to `AUTO`
  mode, which uses fast incremental updates when the query supports it and
  automatically falls back to a full recompute when it doesn't. You no longer
  need to think about whether your query is "incremental-compatible" — just
  create the stream table and it picks the best strategy.

- **Version mismatch warning.** The background scheduler now warns if the
  installed extension version doesn't match the compiled library, making it
  easier to spot a half-finished upgrade.

- **ORDER BY + LIMIT + OFFSET.** You can now page through top-N results, e.g.
  `ORDER BY revenue DESC LIMIT 10 OFFSET 20` to get the third page of
  top earners.

- **Real-time mode: recursive queries.** `WITH RECURSIVE` queries (e.g.
  org-chart hierarchies) now work in IMMEDIATE mode. A depth limit (default
  100) prevents infinite loops.

- **Real-time mode: top-N queries.** `ORDER BY ... LIMIT N` queries now work
  in IMMEDIATE mode — the top-N rows are recomputed on every data change.
  Maximum N is controlled by `pg_trickle.ivm_topk_max_limit` (default 1000).

- **Foreign table support.** Stream tables can now use foreign tables as
  sources. Changes are detected by comparing snapshots since foreign tables
  don't support triggers. Enable with `pg_trickle.foreign_table_polling = on`.

- **Documentation reorganization.** Configuration and SQL reference docs are
  reorganized around practical workflows. New sections cover DDL-during-refresh
  behavior, standby/replica limitations, and PgBouncer constraints.

### Changed
#### Internal Code Quality: Integration Test Suite Hardening

Completed a full hardening pass of the integration test suite, bringing all items in `PLAN_TEST_EVALS_INTEGRATION.md` to done:
- **Multiset validation** — Extracted `assert_sets_equal()` helper relying on EXCEPT/UNION ALL SQL logic and applied it to workflow tests to ensure storage table state correctly matches the defining query post-refresh.
- **Round-trip notifications** — `pg_trickle_alert` notifications now verify receipt end-to-end via `sqlx::PgListener`.
- **DVM operators** — Added unit coverage for complex semi/anti-join behaviors (multi-column, filtered, complementary), multi-table join chains for inner and full joins, and `proptest!` fuzz tests enforcing generated SQL invariants across INNER, SEMI, and ANTI joins.
- **Resilience and edge cases** — Test coverage for ST drop cascades verifying dependent object removal, exact error escalation thresholds, and scheduler job lifecycles across queued mock states.
- **Cleanups** — Standardized naming practices (`test_workflow_*`, `test_infra_*`) and eliminated clock-bound flakes by widening staleness assertions.


- Default refresh mode changed from `'DIFFERENTIAL'` to `'AUTO'`.
- Default schedule changed from `'1m'` to `'calculated'` (automatic).
- Default change tracking mode changed from `'trigger'` to `'auto'` — WAL-based
  tracking starts automatically when available, with trigger-based as fallback.

---

## [0.2.1] — Safe Upgrades & Scheduling Improvements

### Added

- **Safe upgrades.** New upgrade infrastructure ensures that
  `ALTER EXTENSION pg_trickle UPDATE` works correctly. A CI check detects
  missing functions or views in upgrade scripts, and automated tests verify
  that stream tables survive version-to-version upgrades intact. See
  [docs/UPGRADING.md](docs/UPGRADING.md) for the upgrade guide.

- **ORDER BY + LIMIT + OFFSET.** You can now create stream tables over paged
  results, like "the second page of the top-100 products by revenue"
  (`ORDER BY revenue DESC LIMIT 100 OFFSET 100`).

- **`'calculated'` schedule.** Instead of passing SQL `NULL` to request
  automatic scheduling, you can now write `schedule => 'calculated'`. Passing
  `NULL` now gives a helpful error message.

- **Documentation expansion.** Six new pages in the online book covering dbt
  integration, contributing guidelines, security policy, release process, and
  research comparisons with other projects.

- **Better warnings and safety checks:**
  - Warning when a source table lacks a primary key (duplicate rows are
    handled safely but less efficiently).
  - Warning when using `SELECT *` (new columns added later can break
    incremental updates).
  - Alert when the refresh queue is falling behind (> 80% capacity).
  - Guard triggers prevent accidental direct writes to stream table storage.
  - Automatic fallback from WAL to trigger-based change tracking when the
    replication slot disappears.
  - Nested window functions and complex `WHERE` clauses with `EXISTS` are now
    handled automatically.

- **Change buffer partitioning.** For high-throughput tables, change buffers
  can now be partitioned so that processed data is dropped efficiently.

- **Column pruning.** The incremental engine now skips source columns not used
  in the query, reducing I/O for wide tables.

### Changed
#### Internal Code Quality: Integration Test Suite Hardening

Completed a full hardening pass of the integration test suite, bringing all items in `PLAN_TEST_EVALS_INTEGRATION.md` to done:
- **Multiset validation** — Extracted `assert_sets_equal()` helper relying on EXCEPT/UNION ALL SQL logic and applied it to workflow tests to ensure storage table state correctly matches the defining query post-refresh.
- **Round-trip notifications** — `pg_trickle_alert` notifications now verify receipt end-to-end via `sqlx::PgListener`.
- **DVM operators** — Added unit coverage for complex semi/anti-join behaviors (multi-column, filtered, complementary), multi-table join chains for inner and full joins, and `proptest!` fuzz tests enforcing generated SQL invariants across INNER, SEMI, and ANTI joins.
- **Resilience and edge cases** — Test coverage for ST drop cascades verifying dependent object removal, exact error escalation thresholds, and scheduler job lifecycles across queued mock states.
- **Cleanups** — Standardized naming practices (`test_workflow_*`, `test_infra_*`) and eliminated clock-bound flakes by widening staleness assertions.


- Default `schedule` changed from `'1m'` to `'calculated'` (automatic).
- Minimum schedule interval lowered from 60 s to 1 s.
- Cluster-wide diamond consistency settings removed; per-table settings remain
  and now default to `'atomic'` / `'fastest'`.

### Fixed

- The 0.1.3 → 0.2.0 upgrade script was accidentally a no-op, silently
  skipping 11 new functions. Fixed.
- Queries combining `WITH` (CTEs) and `UNION ALL` now parse correctly.

---

## [0.2.0] — Monitoring, IMMEDIATE Mode & Diamond Consistency

### Added

- **Monitoring & health checks.** Six new functions for inspecting your stream
  tables at runtime (no superuser required):
  - `change_buffer_sizes()` — shows how much pending change data each stream
    table has queued up.
  - `list_sources(name)` — lists all base tables that feed a given stream
    table, with row counts and size estimates.
  - `dependency_tree()` — displays an ASCII tree of how your stream tables
    depend on each other.
  - `health_check()` — quick system triage that checks whether the scheduler
    is running, flags tables in error or stale, and warns about large change
    buffers or WAL lag.
  - `refresh_timeline()` — recent refresh history across all stream tables,
    showing timing, row counts, and any errors.
  - `trigger_inventory()` — verifies that all required change-tracking
    triggers are in place and enabled.

- **IMMEDIATE refresh mode (real-time updates).** New `'IMMEDIATE'` mode keeps
  stream tables updated within the same transaction as your data changes.
  There's no delay — the stream table reflects changes the instant they happen.
  Supports window functions, LATERAL joins, scalar subqueries, and aggregate
  queries. You can switch between IMMEDIATE and other modes at any time using
  `alter_stream_table`.

- **Top-N queries (ORDER BY + LIMIT).** Queries like
  `SELECT ... ORDER BY score DESC LIMIT 10` are now supported. The stream
  table stores only the top N rows and updates efficiently.

- **Diamond dependency consistency.** When multiple stream tables share common
  sources and feed into the same downstream table (a "diamond" pattern), they
  can now be refreshed as an atomic group — either all succeed or all roll
  back. This prevents inconsistent reads at convergence points. Controlled via
  the `diamond_consistency` parameter (default: `'atomic'`).

- **Multi-database auto-discovery.** The background scheduler now automatically
  finds and services all databases on the server where pg_trickle is installed.
  No manual `pg_trickle.database` configuration required — just install the
  extension and the scheduler discovers it.

### Fixed

- Fixed IMMEDIATE mode incorrectly trying to read from change buffer tables
  (which don't exist in that mode) for certain aggregate queries.
- Fixed type mismatches when join queries had unchanged source tables producing
  empty change sets.
- Fixed join condition column order being swapped when the right-side table was
  written first in the `ON` clause (e.g. `ON r.id = l.id`).
- Fixed dbt macros silently rolling back stream table creation because dbt
  wraps statements in a `ROLLBACK` by default.
- Fixed `LIMIT ALL` being incorrectly rejected as an unsupported LIMIT clause.
- Fixed false "query may produce incorrect incremental results" warnings on
  simple arithmetic like `depth + 1` or `path || name`.
- Fixed auto-created indexes using the wrong column name when the query had a
  column alias (e.g. `SELECT id AS department_id`).

---

## [0.1.3] — TPC-H Correctness, Window Functions & Aggregate Fixes

Major hardening release with 50 improvements across correctness, robustness,
operational safety, and test coverage.

### Added

- **DDL change tracking expanded.** `ALTER TYPE`, `ALTER POLICY`, and
  `ALTER DOMAIN` on source tables are now detected and trigger a rebuild of
  affected stream tables. Previously only column changes were tracked.
- **Recursive query safety guard.** Recursive CTEs (`WITH RECURSIVE`) are now
  checked for non-monotonic terms that could produce incorrect incremental
  results.
- **Read replica awareness.** The background scheduler detects when it's
  running on a read replica and skips refresh work, preventing errors.
- **Range aggregates rejected.** `RANGE_AGG` and `RANGE_INTERSECT_AGG` are
  now properly rejected in incremental mode with a clear error.
- **Refresh history: row counts.** Refresh history now records how many rows
  were inserted, updated, and deleted in each refresh cycle.
- **Change buffer alerts.** New `pg_trickle.buffer_alert_threshold` setting
  lets you configure when to be warned about growing change buffers.
- **`st_auto_threshold()` function.** Shows the current adaptive threshold
  that decides when to switch between incremental and full refresh.
- **Wide table optimization.** Tables with more than 50 columns use a hash
  shortcut during refresh merges, improving performance.
- **Change buffer security.** Internal change buffer tables are no longer
  accessible to `PUBLIC`.
- **Documentation.** PgBouncer compatibility, keyless table limitations, delta
  memory bounds, sequential processing rationale, and connection overhead are
  all now documented in the FAQ.

#### TPC-H Correctness Suite: 22/22 Queries Passing

The TPC-H-derived correctness test suite (22 industry-standard analytical
queries) now passes completely across multiple rounds of data changes. This
validates that incremental refreshes produce identical results to full
recomputation for complex real-world query patterns.

### Fixed

#### Window Function Correctness

Fixed incremental maintenance of window functions (ROW_NUMBER, RANK,
DENSE_RANK, NTILE, LAG/LEAD, SUM OVER, etc.) to correctly handle:
- Non-RANGE frame types
- Ranking functions over tied values
- Window functions wrapping aggregates (e.g. `RANK() OVER (ORDER BY SUM(x))`)
- Multiple window functions with different PARTITION BY clauses

#### INTERSECT / EXCEPT Correctness

Fixed incremental maintenance of `INTERSECT` and `EXCEPT` queries that
produced wrong results due to invalid SQL generation.

#### EXISTS / IN with OR Correctness

Fixed `EXISTS` and `IN` subqueries combined with `OR` in WHERE clauses that
produced wrong results.

#### Aggregate Correctness

- `MIN` / `MAX` now correctly rescan the source table when the current
  minimum or maximum value is deleted.
- `STRING_AGG(... ORDER BY ...)` and `ARRAY_AGG(... ORDER BY ...)` no longer
  silently drop the ORDER BY clause.

---

## [0.1.2] — Incremental Correctness Fixes & Project Rename

### Changed
#### Internal Code Quality: Integration Test Suite Hardening

Completed a full hardening pass of the integration test suite, bringing all items in `PLAN_TEST_EVALS_INTEGRATION.md` to done:
- **Multiset validation** — Extracted `assert_sets_equal()` helper relying on EXCEPT/UNION ALL SQL logic and applied it to workflow tests to ensure storage table state correctly matches the defining query post-refresh.
- **Round-trip notifications** — `pg_trickle_alert` notifications now verify receipt end-to-end via `sqlx::PgListener`.
- **DVM operators** — Added unit coverage for complex semi/anti-join behaviors (multi-column, filtered, complementary), multi-table join chains for inner and full joins, and `proptest!` fuzz tests enforcing generated SQL invariants across INNER, SEMI, and ANTI joins.
- **Resilience and edge cases** — Test coverage for ST drop cascades verifying dependent object removal, exact error escalation thresholds, and scheduler job lifecycles across queued mock states.
- **Cleanups** — Standardized naming practices (`test_workflow_*`, `test_infra_*`) and eliminated clock-bound flakes by widening staleness assertions.


#### Project Renamed from pg_stream to pg_trickle

Renamed the entire project from **pg_stream** to **pg_trickle** to avoid a
naming collision with an unrelated project. If you were using the old name,
all configuration prefixes changed from `pg_stream.*` to `pg_trickle.*`, and
the SQL schemas changed from `pgstream` to `pgtrickle`. The "stream tables"
terminology is unchanged.

### Fixed

Fixed numerous incremental computation bugs discovered while building a
comprehensive correctness test suite based on all 22 TPC-H analytical queries:

- **Inner join double-counting.** When both sides of a join had changes in
  the same refresh cycle, some rows were counted twice.
- **Shared source cleanup.** Cleaning up processed changes for one stream
  table could accidentally delete entries still needed by another stream
  table sharing the same source.
- **Scalar aggregate identity mismatch.** Queries like `SELECT SUM(amount)
  FROM orders` could produce mismatched row identifiers between the
  incremental and merge phases. AVG also failed to recompute correctly
  after partial group changes.
- **EXISTS / NOT EXISTS snapshots.** Incremental maintenance of `EXISTS` and
  `NOT EXISTS` subqueries missed pre-change state, producing wrong results.
- **Column resolution in complex joins.** Several fixes for column name
  resolution in multi-table joins and nested subqueries.
- **COUNT(*) rendering.** `COUNT(*)` was sometimes rendered as `COUNT()`
  (missing the star), causing SQL errors.
- **Subquery rewriting.** Several subquery patterns (correlated vs
  non-correlated scalar subqueries, derived tables in FROM) were incorrectly
  rewritten, blocking certain queries from being created.
- **Cleanup worker crash.** The background cleanup worker no longer crashes
  when it encounters entries for stream tables that were dropped mid-cycle.

### Added

#### TPC-H Correctness Test Suite

Added a comprehensive correctness test suite based on all 22 TPC-H analytical
queries. These tests verify that incremental refreshes produce identical
results to a full recompute after INSERT, DELETE, and UPDATE mutations.
20 of 22 queries can be created as stream tables; 15 pass full correctness
checks at this point (improved to 22/22 in v0.1.3).

---

## [0.1.1] — CloudNativePG Image & Test Hardening

### Changed
#### Internal Code Quality: Integration Test Suite Hardening

Completed a full hardening pass of the integration test suite, bringing all items in `PLAN_TEST_EVALS_INTEGRATION.md` to done:
- **Multiset validation** — Extracted `assert_sets_equal()` helper relying on EXCEPT/UNION ALL SQL logic and applied it to workflow tests to ensure storage table state correctly matches the defining query post-refresh.
- **Round-trip notifications** — `pg_trickle_alert` notifications now verify receipt end-to-end via `sqlx::PgListener`.
- **DVM operators** — Added unit coverage for complex semi/anti-join behaviors (multi-column, filtered, complementary), multi-table join chains for inner and full joins, and `proptest!` fuzz tests enforcing generated SQL invariants across INNER, SEMI, and ANTI joins.
- **Resilience and edge cases** — Test coverage for ST drop cascades verifying dependent object removal, exact error escalation thresholds, and scheduler job lifecycles across queued mock states.
- **Cleanups** — Standardized naming practices (`test_workflow_*`, `test_infra_*`) and eliminated clock-bound flakes by widening staleness assertions.


#### CloudNativePG Extension Image

Replaced the full PostgreSQL Docker image (~400 MB) with a minimal
extension-only image (< 10 MB) following the CloudNativePG Image Volume
Extensions specification. This means faster pulls and less disk usage in
Kubernetes deployments. The image contains just the extension files —
no full PostgreSQL server.

---

## [0.1.0] — Initial Release

Initial release of pg_trickle — a PostgreSQL extension that keeps query results
automatically up to date as your data changes.

### Core Concept

Define a SQL query and a schedule. pg_trickle creates a **stream table** that
stores the query's results and keeps them fresh — either on a schedule
(every N seconds) or in real time. When data in your source tables changes,
only the affected rows are recomputed instead of re-running the entire query.

### What You Can Do

- **Create stream tables** from `SELECT` queries — joins, aggregates,
  subqueries, CTEs, window functions, set operations, and more.
- **Automatic refresh** — a background scheduler refreshes stream tables in
  dependency order. You can also trigger refreshes manually.
- **Incremental updates** — the engine automatically figures out how to update
  only the rows that changed, instead of recomputing everything. This works
  for most query patterns including multi-table joins and aggregates.
- **Views as sources** — views referenced in your query are automatically
  expanded so change tracking works on the underlying tables.
- **Tables without primary keys** — supported via content hashing. Tables with
  primary keys get better performance.
- **Hybrid change tracking** — starts with lightweight triggers (no special
  PostgreSQL configuration needed). Can automatically switch to WAL-based
  tracking for lower overhead when `wal_level = logical` is available.
- **Multi-database support** — the scheduler automatically discovers all
  databases on the server where the extension is installed.
- **User triggers on stream tables** — your own `AFTER` triggers on stream
  tables fire correctly during incremental refreshes.
- **DDL awareness** — `ALTER TABLE`, `DROP TABLE`, `CREATE OR REPLACE
  FUNCTION`, and other DDL on source tables or functions used in your query
  are detected and handled automatically.

### SQL Support

Broad coverage of SQL features:

- **Joins:** INNER, LEFT, RIGHT, FULL OUTER, NATURAL, LATERAL subqueries,
  LATERAL set-returning functions (`unnest`, `jsonb_array_elements`, etc.)
- **Aggregates:** 39 functions including COUNT, SUM, AVG, MIN, MAX,
  STRING_AGG, ARRAY_AGG, JSON_ARRAYAGG, JSON_OBJECTAGG, statistical
  regression functions (CORR, COVAR_*, REGR_*), and ordered-set aggregates
  (MODE, PERCENTILE_CONT, PERCENTILE_DISC)
- **Window functions:** ROW_NUMBER, RANK, DENSE_RANK, NTILE, LAG, LEAD,
  SUM OVER, etc. with full frame clause support
- **Set operations:** UNION, UNION ALL, INTERSECT, EXCEPT
- **Subqueries:** in FROM, EXISTS/NOT EXISTS, IN/NOT IN, scalar subqueries
- **CTEs:** `WITH` and `WITH RECURSIVE`
- **Special syntax:** DISTINCT, DISTINCT ON, GROUPING SETS / CUBE / ROLLUP,
  CASE WHEN, COALESCE, JSON_TABLE (PostgreSQL 17+)
- **Unsafe function detection:** queries using non-deterministic functions
  like `random()` are rejected with a clear error

### Monitoring

- `explain_st()` — shows the incremental computation plan
- `st_refresh_stats()`, `get_refresh_history()`, `get_staleness()` — refresh
  performance and status
- `slot_health()` — WAL replication slot health
- `check_cdc_health()` — change tracking health per source table
- `stream_tables_info` and `pg_stat_stream_tables` views
- NOTIFY alerts for stale data, errors, and refresh events

### Documentation

- Architecture guide, SQL reference, configuration reference, FAQ,
  getting-started tutorial, and deep-dive tutorials.

### Known Limitations

- `TABLESAMPLE`, `LIMIT` / `OFFSET`, `FOR UPDATE` / `FOR SHARE` — not yet
  supported (clear error messages).
- Window functions inside expressions (e.g. `CASE WHEN ROW_NUMBER() ...`) —
  not yet supported.
- Circular stream table dependencies — not yet supported.
