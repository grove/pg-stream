# PLAN_APPLIANCE — pg_trickle as a Postgres-Shaped IVM Appliance

> **Status:** Research / Strategy
> **Created:** 2026-04-23
> **Priority:** High (long-horizon strategic bet)
> **Scope:** Operate pg_trickle in the same role as Epsio: spin up a
> dedicated PostgreSQL instance with the pg_trickle extension installed,
> point it at one or more primary databases (PG, managed PG, MySQL,
> MariaDB, MSSQL, Snowflake, BigQuery), and let it pull changes,
> compute materialized results, and write them back — all from inside
> that one Postgres process. The user treats the pg_trickle Postgres as
> a black box; no application code runs in it, no extra binary is needed
> on either side.
> **Related docs:**
> [GAP_ANALYSIS_EPSIO.md](GAP_ANALYSIS_EPSIO.md) ·
> [PLAN_CITUS.md](PLAN_CITUS.md) ·
> [PLAN_NEON.md](PLAN_NEON.md) ·
> [PLAN_CLOUDNATIVEPG.md](PLAN_CLOUDNATIVEPG.md)

---

## 1  Executive Summary

The user-facing model:

```
┌────────────┐                            ┌─────────────────────┐
│  PRIMARY   │  CDC stream (logical /     │  pg_trickle PG      │
│  DATABASE  │  binlog / CDC tables /     │  (the appliance)    │
│            │  STREAMS / CHANGES TVF)    │                     │
│  any kind  │ ─────────────────────────▶ │  • foreign sources  │
│            │                            │  • bgworkers ingest │
│            │  upserts / merges back     │  • DVM operator tree │
│            │  via FDW                   │  • stream tables    │
│            │ ◀─────────────────────────  │  • foreign dests    │
└────────────┘                            └─────────────────────┘
```

The appliance is a stock PostgreSQL 18 instance running:

1. `pg_trickle` (the IVM engine).
2. The relevant **foreign data wrapper** for each source (`postgres_fdw`,
   `mysql_fdw`, `tds_fdw` for MSSQL, `snowflake_fdw`, a multicorn-based
   `bigquery_fdw`).
3. Optionally `pg_cron` for human-readable schedules.

The user spins this Postgres up next to their primary DB, runs a
handful of `CREATE SERVER`, `IMPORT FOREIGN SCHEMA`, and
`CREATE STREAM TABLE` statements, and from then on it behaves like
Epsio: a black box that keeps maintained results fresh.

Key property: the appliance is, operationally, **just a Postgres**.
Backup, replication, monitoring, RBAC, HA — all the boring
infrastructure problems are already solved by the surrounding
ecosystem. pg_trickle's job is to add CDC pollers, foreign-table
plumbing, and the IVM engine that already exists.

---

## 2  How Sources Plug In: FDW + CDC Polling

For every supported source we need two halves:

- **Read path** — a foreign-table view that lets pg_trickle's delta SQL
  see source data as if it were local. The DVM emits PostgreSQL CTEs
  that join, group, and window over these foreign tables. The FDW pushes
  what it can to the source.
- **CDC path** — a bgworker that polls the source's native change
  stream and writes change rows into local
  `pgtrickle_changes.changes_<oid>` buffer tables. From there the
  existing differential-refresh pipeline runs unchanged.

The buffer tables, the catalog, the operator tree, the merge codegen —
none of that needs to know whether the source is local or remote. It
operates on OIDs of tables that happen to live in this Postgres. The
foreign tables **are** local PG objects with OIDs.

### 2.1  Source matrix

| Source                         | FDW                          | CDC mechanism                                  | Notes |
|--------------------------------|------------------------------|------------------------------------------------|-------|
| PG self-hosted                 | `postgres_fdw`               | logical replication slot consumed via libpq bgworker | A small generalisation of the existing in-process WAL decoder |
| RDS / Aurora / Cloud SQL PG    | `postgres_fdw`               | same; `rds_replication` / equivalent role      | Aurora: writer endpoint only; Cloud SQL: `cloudsql.logical_decoding=on` |
| Azure DB for PG / Supabase / Neon | `postgres_fdw`            | same; logical decoding default-on for Supabase/Neon | Neon scale-to-zero pauses WAL — surface as lag |
| MySQL / MariaDB                | `mysql_fdw`                  | row-based binlog polled by bgworker (`mysql_async`) | Need `binlog_format=ROW`, `binlog_row_image=FULL` |
| MSSQL                          | `tds_fdw`                    | `cdc.fn_cdc_get_all_changes_*` polled via TDS (`tiberius`) | SQL Server Agent must be enabled |
| Snowflake                      | `snowflake_fdw` (third-party) or multicorn adapter | Snowflake STREAMS polled via REST/JDBC | Warehouse-second billing → batch aggressively |
| BigQuery                       | multicorn-based `bigquery_fdw` | `CHANGES(TABLE …, TIMESTAMP, TIMESTAMP)` TVF polled via Storage API | Bytes-scanned billing → enforce per-source budget |

The CDC poller for non-PG sources is a new pg_trickle bgworker per
source kind. It is small in shape: open a client to the source, poll
the change endpoint, translate rows into our existing change-buffer
format, write via SPI, advance the watermark in the catalog, sleep.
The DVM never sees the difference.

### 2.2  Why FDW for the read path

A typical differential refresh produces delta SQL like:

```sql
WITH delta_orders AS (
  SELECT * FROM source_orders
  WHERE order_id IN (SELECT pk FROM pgtrickle_changes.changes_<oid> WHERE …)
)
INSERT INTO stream_top_customers
SELECT customer_id, sum(amount)
FROM delta_orders JOIN source_customers USING (customer_id)
GROUP BY customer_id;
```

If `source_orders` and `source_customers` are foreign tables, the FDW
pushes the `WHERE pk IN (…)` filter to the remote, fetches only the
matching rows, and the rest of the query runs locally in the
appliance. That is exactly what differential refreshes need: small,
key-bounded lookups against the source for each batch of changes. The
push-down machinery in `postgres_fdw`, `mysql_fdw`, and `tds_fdw` is
already very good at this shape; warehouse FDWs are weaker but still
useful for small probes.

### 2.3  Why a bgworker for the CDC path

FDWs are pull-based query engines — they do not provide change
streams. The change stream is always a separate channel:

- PG logical replication needs `pg_logical_slot_get_changes` or a
  streaming replication client.
- MySQL binlog needs a binlog client over the replication protocol.
- MSSQL CDC needs polling of `cdc.<schema>_<table>_CT` tables.
- Snowflake STREAMS need polling via SQL.
- BigQuery CHANGES needs polling via SQL.

All of these fit naturally into pg_trickle's existing bgworker model
(see [src/scheduler.rs](../../src/scheduler.rs) and the WAL decoder
in [src/wal_decoder.rs](../../src/wal_decoder.rs)). One new bgworker
per source kind, registered on `shared_preload_libraries =
'pg_trickle'` startup, gated by `pg_trickle.enabled` and per-source
GUCs.

### 2.4  Long-tail sources via Debezium + relay

For source databases we do **not** ship a native poller for — Oracle,
Db2, MongoDB, Cassandra, Vitess, Spanner, Yugabyte, Informix — the
official answer is "run Debezium Server externally, point it at the
relay, set `wire_format = debezium`."

Concretely:

```
Oracle / Db2 / Mongo / …
        │
        ▼
Debezium Server (external, user-operated)
        │  (Kafka / NATS / Redis Streams / RabbitMQ / HTTP)
        ▼
pgtrickle-relay (reverse pipeline, wire_format = debezium)
        │
        ▼
inbox table on the appliance ──▶ stream table treats inbox as source
```

The decoder lives in [pgtrickle-relay](../../pgtrickle-relay) and is
specified in [../relay/PLAN_RELAY_WIRE_FORMATS.md](../relay/PLAN_RELAY_WIRE_FORMATS.md),
which treats Debezium support as **bidirectional** — the same
plan also covers emitting Debezium-shaped messages on the destination
side, see §3.

**Crucially, no JVM enters the appliance** — Debezium Server runs in
the user's existing infrastructure, the relay stays pure Rust, and
the appliance still only sees PG inbox rows.

This is the escape hatch that turns "we support 5 sources natively"
into "we support 5 sources natively + everything Debezium covers."
It does not absolve us of building the native pollers — latency,
operational simplicity, and the no-broker story still favour native
where we have it — but it gives users a credible answer for the long
tail without years of connector engineering on our side.

---

## 3  How Destinations Plug In: FDW Writes or Debezium Emit

There are two destination paths, picked per stream table:

1. **FDW write path** — the appliance writes maintained results
   directly to a local or foreign table via
   [src/refresh/merge.rs](../../src/refresh/merge.rs)'s `INSERT … ON
   CONFLICT DO UPDATE` (or `MERGE`). Used when the destination is a
   database the FDW catalogue covers.
2. **Debezium emit path** — the appliance writes maintained results
   into an **outbox table**; the relay encodes them as Debezium
   messages and pushes them onto Kafka / NATS / Redis Streams /
   RabbitMQ / HTTP. Used when the destination is a tool that already
   speaks the Debezium envelope.

| Destination                                            | Writer path                                       |
|--------------------------------------------------------|---------------------------------------------------|
| Same PG appliance (default)                            | Local table, no FDW needed                        |
| Source PG itself                                       | Foreign table via `postgres_fdw`, `INSERT … ON CONFLICT` |
| Other PG (e.g. serving tier)                           | Same                                              |
| MySQL / MariaDB                                        | Foreign table via `mysql_fdw` (FDW translates)    |
| MSSQL                                                  | Foreign table via `tds_fdw`, `MERGE` translation  |
| Snowflake / BigQuery                                   | Foreign table via warehouse FDW, batched MERGE    |
| Apache Iceberg                                         | Outbox → relay → Debezium → Kafka → Iceberg sink connector |
| Apache Hudi / Delta                                    | Outbox → relay → Debezium → Kafka → lakehouse sink |
| ClickHouse / Apache Pinot / Apache Doris / StarRocks   | Outbox → relay → Debezium → Kafka → OLAP-engine sink |
| Apache Druid                                           | Outbox → relay → Debezium → Kafka → indexing service |
| Materialize / Flink CDC / ksqlDB                       | Outbox → relay → Debezium → Kafka, consumed as a CDC source |
| Snowflake (via Kafka Connector instead of FDW)         | Outbox → relay → Debezium → Kafka → Snowflake Kafka Connector |
| Anything else that consumes the Debezium envelope      | Outbox → relay → Debezium → transport of choice    |

The Debezium-emit path means we do **not** ship Iceberg / Hudi /
ClickHouse / Pinot / Doris writers ourselves — the relay emits a
format the existing connector ecosystem already consumes, and the
destination is reached through tools the user likely already operates.
This quietly reopens the lakehouse / OLAP destination story without
forcing us to write per-destination writers.

### 3.1  Worked example

```sql
CREATE EXTENSION pg_trickle;
CREATE EXTENSION postgres_fdw;

CREATE SERVER primary_pg
  FOREIGN DATA WRAPPER postgres_fdw
  OPTIONS (host 'primary.internal', dbname 'app', port '5432');

CREATE USER MAPPING FOR pgtrickle SERVER primary_pg
  OPTIONS (user 'replicator', password '…');

IMPORT FOREIGN SCHEMA public
  LIMIT TO (orders, customers, line_items)
  FROM SERVER primary_pg INTO src;

CREATE STREAM TABLE serving.top_customers AS
SELECT customer_id, sum(amount) AS total
FROM   src.orders JOIN src.customers USING (customer_id)
GROUP  BY customer_id;
```

Behind that single `CREATE STREAM TABLE`, pg_trickle:

1. Parses and analyses the SQL using the local PG planner — foreign
   tables look like ordinary relations to the parser.
2. Discovers the source server from the foreign-table metadata.
3. Picks the right CDC poller (here: PG logical replication) and asks
   it to start streaming changes for `orders`, `customers`,
   `line_items`.
4. Runs the existing differential-refresh loop, with the only change
   being that snapshot reads on `src.*` go through the FDW.
5. Writes results to `serving.top_customers` (a local table; could be
   a foreign table on a serving-tier PG instead).

The user did not write a single line outside of `psql`.

---

## 4  Architectural Changes Inside pg_trickle

Most of the existing code already does the right thing. The targeted
changes are:

### 4.1  Source registry

A new catalog table `pgtrickle.pgt_sources` records, per source server:

| Column           | Purpose                                                   |
|------------------|-----------------------------------------------------------|
| `srvid`          | OID of the `pg_foreign_server`                            |
| `kind`           | `pg`, `mysql`, `mssql`, `snowflake`, `bigquery`           |
| `cdc_config`     | jsonb — slot name, binlog file/pos, CDC capture instance, etc. |
| `watermark`      | last acked LSN / binlog pos / stream offset / timestamp   |
| `cdc_status`     | `idle`, `polling`, `lagging`, `error`, `unsupported`      |

Source kind is detected from the FDW name; the catalog stores the
runtime state.

### 4.2  CDC pollers behind a trait

We add one bgworker per source kind, all conforming to a common
`ChangePoller` Rust trait:

```rust
trait ChangePoller {
    fn name(&self) -> &'static str;
    fn poll_once(&mut self, source: &Source) -> Result<Vec<Change>, PgTrickleError>;
    fn ack(&mut self, source: &Source, watermark: Watermark) -> Result<(), PgTrickleError>;
}
```

All implementations are in-process Rust inside the extension. Each
writes into the existing `pgtrickle_changes.changes_<oid>` buffer
tables via SPI. Nothing downstream of the buffer tables changes.

### 4.3  Snapshot reads through FDW

[src/dvm/diff.rs](../../src/dvm/diff.rs) generates delta SQL that
references source tables by their local PG OID. When that OID belongs
to a foreign table, PG's executor handles the rest via the FDW. No
change to the operator tree.

The one subtlety: per-operator dialect concerns (FILTER, FULL OUTER,
LATERAL, etc.) only matter for **destination writes** and for any
SQL we ask the *source* to evaluate beyond a simple
`SELECT … WHERE pk IN (…)`. We constrain the snapshot reader to that
shape — anything more complex is computed locally in the appliance,
where the full PG dialect is available. The DVM never has to learn
MySQL or T-SQL grammar.

### 4.4  Destination dialect

For DML on foreign tables, the FDW handles dialect translation; we
only need to choose between `INSERT … ON CONFLICT` (PG-style merge,
default) and `MERGE` (handled by `postgres_fdw` and others from
PG 15+). The existing merge codegen branch is sufficient.

### 4.5  Schema introspection

Foreign tables expose columns and types via `IMPORT FOREIGN SCHEMA`,
which pg_trickle reads through the existing `information_schema`
queries. Primary-key information for non-PG sources is not always
exposed via the FDW; we add per-source-kind probes (e.g. for MySQL,
query the source's `information_schema.STATISTICS` directly via the
bgworker's source client) that populate a new
`pgtrickle.pgt_source_keys` table.

### 4.6  GUCs

| GUC                                            | Default | Purpose                            |
|------------------------------------------------|---------|------------------------------------|
| `pg_trickle.cdc_poll_interval_default`         | `1s`    | Per-source poll cadence            |
| `pg_trickle.cdc_max_batch_rows`                | `10000` | Cap per poll cycle                 |
| `pg_trickle.snowflake_warehouse_seconds_budget`| `60`/h  | Cost guard                         |
| `pg_trickle.bigquery_bytes_budget`             | `1 GB`/h| Cost guard                         |
| `pg_trickle.fdw_pushdown_required`             | `true`  | Refuse stream tables whose snapshot SQL won't push down |

That is the entire engine-side change list. Everything else lives in
operational docs and connector-specific test suites.

---

## 5  Deployment Shapes

### 5.1  Black-box single appliance (the default)

One Postgres VM/container with pg_trickle + the FDWs you need. Sized
for: change-buffer storage + materialized stream-table size + headroom
for refresh CPU. Operationally identical to "we run a small Postgres."

Recommended floor: 4 vCPU / 16 GB RAM / fast SSD. Real sizing depends
on change rate × DVM operator complexity.

### 5.2  HA appliance pair

Standard PG streaming replication: a second appliance is a hot
standby. CDC pollers and refresh bgworkers run only on the primary
(they already gate on `pg_is_in_recovery() = false`). On promotion,
the new primary resumes from the watermark in `pgt_sources` — that
catalog row is replicated, so no state is lost as long as the source's
change retention covers the replication lag.

For PG sources, the logical-replication slot lives on the **source**,
not on the appliance, so failover of the appliance does not invalidate
the slot. For binlog/CDC/STREAMS/CHANGES sources, the watermark is
just an offset/timestamp the appliance owns; same story.

### 5.3  Citus appliance

If the maintained result tables outgrow a single PG, the appliance
itself becomes a Citus cluster. The user experience is unchanged —
they still `CREATE STREAM TABLE` — and the appliance distributes
results internally.

### 5.4  Per-source appliances

For multi-tenant or strict-isolation setups, one appliance per source
DB. They share nothing; failure isolation is total. Recommended shape
for shared-cloud users with mixed workloads.

---

## 6  What the Appliance Does Not Do

Being explicit prevents scope creep:

- **No application traffic.** Nobody connects to the appliance to run
  ad-hoc queries beyond inspecting maintained tables.
- **No DDL on the source.** The appliance never issues `CREATE
  TRIGGER` or `ALTER TABLE` against the upstream. PG sources need a
  publication; that is one DDL the user runs once on the source.
- **No write conflicts.** When the destination is the source, the
  maintained tables are pg_trickle-owned and the user must not write
  to them.
- **No transformation language beyond SQL.** The whole API surface is
  `CREATE STREAM TABLE … AS SELECT …`.

---

## 7  Operational Concerns

### 7.1  CDC durability

Each poller acks its watermark to `pgt_sources` **after** the changes
are durably written into the change-buffer table, in the same
transaction as the SPI insert. On crash recovery the bgworker resumes
from the persisted watermark. If the source's change retention has
been exceeded, the affected stream tables move to
`status = 'snapshot_required'` and the user is asked to trigger a full
refresh.

### 7.2  Lag and back-pressure

We expose per-source `lag_bytes` / `lag_seconds` / `lag_rows` metrics
through the existing `pgtrickle.metrics` view. Bgworkers slow their
poll cadence when refresh can't keep up; alerts fire at 50/80/95%
of `pg_trickle.change_buffer_max_rows`.

### 7.3  Schema drift

Three layers of detection:

1. **PG sources:** consume DDL events from logical replication
   metadata; on relevant ALTERs the appliance re-imports the foreign
   schema and validates affected stream tables.
2. **MySQL:** binlog DDL events trigger the same path.
3. **MSSQL / Snowflake / BigQuery:** poll `information_schema` on a
   slower cadence (default 60s); on diff, re-validate.

Stream tables whose schema has drifted go to
`status = 'schema_drift'` and stop refreshing until the user issues
`pgtrickle.rescan(stream_table)` or recreates the view.

### 7.4  Security

- DSNs and passwords live in PG `USER MAPPING`, ideally backed by a
  secret manager on the appliance host.
- The appliance role on the source has the **minimum** privileges:
  `SELECT` on the relevant tables and `REPLICATION` (PG) /
  `RELOAD, REPLICATION SLAVE, REPLICATION CLIENT` (MySQL) /
  `db_owner` on the CDC schema (MSSQL) / the documented Snowflake/BQ
  roles for STREAMS / CHANGES.
- The appliance never needs `SUPERUSER` on any source.
- Network: appliance pinned to the source's VPC; egress controlled by
  the cloud's normal NACLs.

### 7.5  Observability

The appliance is a Postgres, so all existing PG observability
(pg_stat, auto_explain, postgres_exporter) plus the pg_trickle
Prometheus exporter work without modification. We add per-source
counters (`pg_trickle_cdc_poll_lag_seconds{source=…}`,
`pg_trickle_fdw_rows_fetched{source=…,table=…}`,
`pg_trickle_warehouse_seconds_used{source=…}`).

---

## 8  Scaling

Five tactics, each independent. Pick what the workload demands.

### 8.1  Vertical

The appliance is a Postgres. Give it more CPU, RAM, faster disk. This
covers a surprising share of real workloads — the DVM is not memory-
hungry because there is no persistent operator state.

### 8.2  Per-source appliance

Run N independent appliances, each handling a subset of sources or
stream tables. No coordination needed because change buffers and
watermarks are per-source. Linear scaling for "I have many independent
views."

### 8.3  Read-replica destination

For a single hot maintained table with many readers, the destination
PG can be replicated to N hot standbys. The appliance writes to the
primary; readers go to the replicas. Plain PG, no pg_trickle
involvement.

### 8.4  Citus appliance for big result tables

When the maintained table itself is too big for one node, the
appliance becomes a Citus cluster. The result table is distributed by
the natural key (group key for aggregations, join key for joins).
The DVM merge step ends in `INSERT … ON CONFLICT` against the
distributed table; Citus routes each shard worth of rows to the right
worker. Reference dimensions become reference tables.

Critical sub-rule: when both source and destination are PG, choose
`source distribution key == destination distribution key` to avoid
shuffle. When the source is non-PG (or unsharded), the appliance
shuffles implicitly during the snapshot read — acceptable as long as
the result table is the bottleneck, not the snapshot read.

See [PLAN_CITUS.md](PLAN_CITUS.md) for the in-process Citus plan; it
applies to the appliance unchanged because the appliance **is** an
in-process pg_trickle.

### 8.5  DAG split across appliances

For a single complex stream table whose DVM plan has multiple stages,
intermediate stages can be materialized into a "boundary" PG. A second
appliance treats that boundary as its source and runs the downstream
stages. This is pipeline-parallelism inside a Postgres-only world.

Opt-in and rare. Only reach for it when (a) the DVM plan has a clear
bottleneck stage and (b) vertical scaling has been exhausted.

### 8.6  Cost-aware scheduling for warehouses

The Snowflake / BigQuery cases need explicit budgets (already in §4.6)
because every refresh costs real money. Schedule those stream tables
on a slower cadence (default 1 minute, configurable) and refuse to
schedule a refresh that would blow the per-source budget. Surface the
rejected refresh in the metrics view.

---

## 9  Roadmap

Each version follows the existing PLAN_<X>_<Y>_<Z>.md format with
concrete tickets and ADRs.

| Version | Theme                              | Deliverables                                              |
|---------|------------------------------------|-----------------------------------------------------------|
| 0.40    | Remote PG sources                  | `pgt_sources` registry, remote logical-slot poller, IMPORT FOREIGN SCHEMA wiring, end-to-end test against a second PG via `postgres_fdw` |
| 0.41    | Foreign destinations               | DML through `postgres_fdw`; documented "appliance writes back to source" pattern; merge codegen `MERGE` path |
| 0.42    | Managed-PG hardening               | Profiles for RDS / Aurora / Cloud SQL / Azure / Supabase / Neon; per-platform docs; CI matrix |
| 0.43    | MySQL / MariaDB                    | binlog poller bgworker, `mysql_fdw` integration, dialect gating for snapshot-shape SQL |
| 0.44    | MSSQL                              | CDC-tables poller, `tds_fdw` integration, T-SQL gating for destination writes |
| 0.45    | HA + Citus appliance               | Hot-standby docs and tests, Citus-distributed result tables, co-location validator |
| 0.46    | Snowflake                          | STREAMS poller, Snowflake FDW integration, warehouse-second budgets |
| 0.47    | BigQuery                           | CHANGES TVF poller, BQ FDW integration, bytes-scanned budgets |
| 0.48    | DAG split across appliances        | Documented multi-appliance pattern, intermediate-table contract, integration tests |

Lakehouse, OLAP-engine, and Kafka destinations are reached through
the **Debezium emit path** (relay 0.31+, see
[../relay/PLAN_RELAY_WIRE_FORMATS.md](../relay/PLAN_RELAY_WIRE_FORMATS.md))
rather than via custom output plugins. The appliance writes to a
local outbox table; the relay encodes Debezium messages and ships
them onto the user's transport of choice. No new code path inside
the extension.

---

## 10  Open Questions

1. **Snowflake / BigQuery FDW maturity.** The third-party FDWs are
   not as mature as `postgres_fdw`. We should evaluate writing the
   snapshot-read path for those sources directly through the
   bgworker's source client, bypassing the FDW for reads and using it
   only for destination writes (or skipping FDW entirely).
2. **Snapshot consistency across sources.** A single stream table
   that joins MySQL × PG cannot have a single LSN. Document the
   eventual-consistency guarantee per source kind and define the
   "transactional consistency window" explicitly.
3. **Schema-import automation.** Should `CREATE STREAM TABLE` against
   a not-yet-imported foreign table auto-import the schema?
   Convenient but surprising. Default off, opt-in GUC.
4. **PG-source slot lifecycle.** Does the appliance own creating /
   dropping the publication and slot on the source, or does the user
   pre-create them? Appliance creates if it has privileges, otherwise
   the docs spell out the SQL.
5. **MSSQL CDC vs Change Tracking.** Some users have one but not the
   other. Support CDC first; consider a Change-Tracking fallback
   later.

---

## 11  Risks

| Risk                                                  | Mitigation                                              |
|-------------------------------------------------------|---------------------------------------------------------|
| Per-source dialect divergence in destination DML      | FDW handles translation; we constrain ourselves to `INSERT … ON CONFLICT` / `MERGE` |
| Warehouse FDWs are weak                               | Bypass FDW for snapshot reads; use bgworker source clients directly |
| CDC polling falls behind on busy sources              | Multiple appliances, per-source partitioning, slow-down + alerts |
| Source DDL breaks stream tables silently              | Three-layer drift detection (§7.3), `status = 'schema_drift'` halts refresh |
| Logical-replication slot left dangling on source      | Appliance owns slot; documented cleanup on `DROP STREAM TABLE` |
| Aurora / Cloud SQL slot loss on failover              | Detect via libpq error codes, recreate slot on new writer, fall back to snapshot if WAL retention insufficient |
| Snowflake / BigQuery cost surprises                   | Per-source budgets, refusal to schedule, cost-calculator docs |
| FDW snapshot reads are slow under high change volume  | Per-source connection pool tuning; option to pre-replicate hot dimension tables locally via subscription |
| Network partitions appliance ↔ source                 | Pollers backoff + exponential retry; lag metrics surface to alerting |

---

## 12  Honest Verdict

The appliance idea is genuinely good but it is not a free lunch. Here
is the candid case for and against, separated cleanly.

### Where the fit is genuine

- **The compute substrate is just a Postgres.** Backup, replication,
  HA, RBAC, monitoring, secret management, packaging — all the boring
  infrastructure problems are solved by the surrounding ecosystem. We
  inherit decades of operational maturity for free. Epsio has to build
  every one of those from scratch; we don't.
- **The DVM does not have to change.** The IVM engine treats foreign
  tables as ordinary relations. We get heterogeneous-source IVM
  without porting the operator graph to a new runtime — that is a real
  multiplier on existing R&D investment.
- **FDWs do the dialect work for the write path.** We do not have to
  emit MySQL / T-SQL / Snowflake / BigQuery DML — `INSERT … ON
  CONFLICT` flows through the FDW, which translates. The DVM stays
  PG-flavoured everywhere.
- **PG-fidelity SQL stays the moat.** All the work in
  [GAP_ANALYSIS_EPSIO.md](GAP_ANALYSIS_EPSIO.md) — 39+ aggregates,
  full window functions, recursive CTEs, LATERAL, correlated
  subqueries — applies unchanged. Nothing about going multi-source
  forces us to give up SQL-surface superiority.
- **Onboarding is brutally simple.** "Spin up a Postgres, install the
  extension, write a `CREATE STREAM TABLE`." That is a story we can
  put on the README and have people succeed with in 15 minutes.

### Where the fit is genuinely uneasy

- **FDW push-down is the load-bearing assumption.** If a stream
  table's snapshot SQL doesn't push down — for example, a `WHERE
  jsonb_path_exists(...)` against a MySQL source, or a window
  function in a CTE that the FDW pulls back to the appliance — every
  refresh ends up dragging an unbounded slice of the source over the
  network. This is silently expensive and silently slow. The
  `fdw_pushdown_required` GUC helps catch it at view-creation time,
  but only for the patterns we know how to detect statically. Users
  *will* hit this in production.
- **Foreign tables are not free at planner time.** PG's planner asks
  the FDW for cost estimates and column statistics; for non-PG FDWs
  these are often fabricated or extremely conservative. The DVM
  generates non-trivial CTEs that join multiple foreign tables; bad
  estimates lead to bad plans, and bad plans on remote data are
  painful.
- **Type-system mismatches are death by a thousand cuts.** MySQL has
  no proper boolean, TIMESTAMP semantics differ across all four
  non-PG sources, MSSQL uses VARBINARY(MAX) for what we'd call BYTEA,
  Snowflake VARIANT does not round-trip to JSONB cleanly. Every
  connector ships with a mapping table, and every mismatch surfaces
  as a runtime cast error during a refresh — far from where the user
  wrote the SQL.
- **"Treat it as a black box" is a story, not a guarantee.** The
  appliance has GUCs, slot lifecycles, watermark recovery, CDC retention
  budgets, FDW user mappings, and managed-cloud quirks. The promise
  of black-box simplicity holds only as long as nothing breaks. When
  something does break, the user is debugging a Postgres they didn't
  design, with extension internals they don't understand. The
  diagnostic UX has to be *exceptional* or the black box becomes a
  closed coffin.
- **CDC retention is a foot-gun on managed clouds.** RDS PG keeps WAL
  for 5 minutes by default. Snowflake stream retention is 14 days but
  starts the clock on first read. BigQuery time travel maxes at 7
  days. If the appliance falls behind for any reason, a stream table
  silently transitions to "needs full refresh", which on a warehouse
  source can cost real money. The mitigations are documented but
  cannot be made automatic everywhere.
- **Per-source CDC pollers are the long pole.** Each is multi-week
  work done well: replication-protocol clients, type mapping, auth
  matrices (Kerberos for MSSQL, key-pair for Snowflake, ADC for BQ),
  TLS quirks, backoff, fixtures, soak. The 0.43–0.47 cadence is
  optimistic by ~1.5×. We will spend an honest year on connectors
  alone if we ship them all.
- **Warehouse FDWs are the weakest link.** `snowflake_fdw` is
  third-party and lightly maintained; the multicorn route adds a
  Python runtime dependency that operators will hate. We may end up
  writing thin native FDWs ourselves to keep ownership of the
  connector quality story. That is plannable but it is not free.
- **The market is contested.** Materialize, RisingWave, Feldera,
  Epsio, ClickHouse + MaterializedView, dbt incremental on the
  warehouse — there is no quiet niche. Our differentiation is
  "PostgreSQL-shaped operational story + true PG SQL fidelity + OSS",
  which is real but not obvious to a buyer comparing checkboxes.

### Net opinion

**Yes, the appliance is a good fit, but it is a good fit only for the
PG-source / PG-destination case at first.** That single slice
already beats Epsio on three axes (OSS, SQL surface, operational
shape) and it leverages the part of pg_trickle that already works.
Shipping that slice well — 0.40 + 0.41 + 0.42 — is high-confidence
work, low-risk delivery, and a coherent product on its own.

The non-PG sources (MySQL, MSSQL, Snowflake, BigQuery) are where the
fit gets honest. They are achievable, they extend the strategic
position, and they are the right long-term direction. But each one is
a real connector engineering project, the FDW push-down assumption
gets shakier with every backend, and operationally the "one Postgres"
story degrades into "one Postgres plus N flavours of CDC plumbing
your team has to understand." That is still better than two products,
but it is not the marketing story.

Recommendation, prioritised:

1. **Ship 0.40 + 0.41 as the proof of the appliance shape.** Remote
   PG source, foreign destination, end-to-end on stock managed PG.
   This is the smallest shippable artefact that validates the whole
   thesis. If it doesn't sing, abandon the rest.
2. **Then 0.42 (managed-PG matrix).** Real users are on RDS / Aurora /
   Cloud SQL / Supabase / Neon. The connector code is the same; the
   work is platform-specific docs, IAM, and CI fixtures.
3. **Then 0.43 (MySQL).** Largest non-PG market, best CDC ergonomics
   (full row images), validates the bgworker poller pattern for a
   truly different backend.
4. **Pause and re-assess after 0.43.** If the FDW snapshot story is
   holding, push to 0.44 (MSSQL). If it is wobbling, double down on
   tooling (planner-time push-down validation, better diagnostics,
   `EXPLAIN STREAM TABLE`) before adding more sources.
5. **Treat 0.46 / 0.47 (Snowflake, BigQuery) as design-partner
   driven.** Build only when a real user with a real workload signs
   up. The build cost is high, the cost-per-refresh is real, and the
   value is workload-specific.
6. **Treat 0.45 (Citus) as opportunistic.** The PLAN_CITUS work is
   independent; do not architect around it.
7. **Lakehouse / OLAP / Kafka destinations come for free via the
   relay's Debezium emit path** — not via custom writers in the
   extension. The cost is in the relay, not the appliance, and is
   covered by
   [../relay/PLAN_RELAY_WIRE_FORMATS.md](../relay/PLAN_RELAY_WIRE_FORMATS.md).

One-line verdict: **the appliance shape is right; the PG-source slice
is a high-confidence ship; non-PG sources are the strategic prize but
also the place where the simplicity story has to be defended every
release.** Worth doing, with eyes open about which parts are easy and
which parts are sustained connector engineering for years.
