# pg_trickle — Feature Descriptions: v0.28.0 through v0.33.0

> **Audience:** Product managers, stakeholders, and technically curious readers
> who want to understand what each release delivers and why it matters —
> without needing to read Rust code or SQL specifications.

---

## Quick reference

| Version | Theme | Status | Scope |
|---------|-------|--------|-------|
| [v0.28.0](#v0280--transactional-inbox--outbox-patterns) | Reliable event messaging built into PostgreSQL | ✅ Released | Large |
| [v0.29.0](#v0290--relay-cli-pgtrickle-relay) | Off-the-shelf connector to Kafka, NATS, SQS, and more | Planned | Large |
| [v0.30.0](#v0300--pre-ga-correctness--stability-sprint) | Quality gate before 1.0 — correctness, stability, and docs | Planned | Medium |
| [v0.31.0](#v0310--performance--scheduler-intelligence) | Smarter scheduling and faster hot paths | Planned | Medium |
| [v0.32.0](#v0320--reactive-subscriptions--zero-downtime-operations) | Live push notifications and safe live schema changes | Planned | Medium |
| [v0.33.0](#v0330--temporal-ivm--columnar-materialization) | Time-travel queries and analytic storage | Planned | Medium |

---

## v0.28.0 — Transactional Inbox & Outbox Patterns

**Status: ✅ Released**

### What problem does this solve?

Modern applications often need to communicate between services: "an order was
placed, notify the warehouse system." The naive approach — write to the
database, *then* publish the event to a message broker — has a fatal flaw:
if the application crashes between those two steps, either the database row
exists with no event published, or the event was published but the database
write failed. This is called the *dual-write problem*, and it causes data
inconsistencies that are extremely difficult to debug.

v0.28.0 solves this entirely inside PostgreSQL, with no external
infrastructure required.

### The Transactional Outbox

Think of the **outbox** as a guaranteed delivery queue built directly into
your database. When a stream table is refreshed and its result changes,
pg_trickle automatically writes a record of those changes into a companion
table — the outbox — *in the same database transaction as the refresh*.
Either both succeed, or neither does. There is no window where the data
changed but the event was not recorded.

An external relay process (your code, or the built-in one from v0.29.0)
then reads the outbox and publishes to whatever downstream system you use:
Kafka, NATS, a webhook, an SQS queue, or anything else. The relay can
crash and restart safely — it just picks up where it left off.

For large batches of changes (more than 10,000 rows by default), pg_trickle
uses a *claim-check* pattern: the outbox row carries only a lightweight
summary, and the full row data is stored in a companion table that the relay
reads in bounded memory chunks. This means delivery is never blocked by the
size of a delta.

### The Transactional Inbox

The **inbox** is the mirror image: a production-grade table for *receiving*
events from outside. Call `create_inbox('my_inbox')` and pg_trickle
automatically creates:

- A **pending messages** stream table showing all unprocessed events
- A **dead-letter queue** stream table for messages that have failed too
  many times
- A **statistics** stream table tracking processing throughput and error
  rates

Applications insert events into the inbox with `ON CONFLICT DO NOTHING`
for automatic deduplication — the same event published twice only creates
one row. If a message processor crashes mid-flight, the message stays
pending and will be picked up again.

### Consumer Groups (Kafka-style, built into PostgreSQL)

For high-throughput scenarios where multiple relay processes share a single
outbox, **consumer groups** let them coordinate safely — exactly like Kafka
consumer groups, but with zero extra infrastructure. Each relay claims a
batch under a *visibility timeout* (similar to Amazon SQS), and if the relay
crashes its batch automatically becomes available for another relay to claim
after the timeout expires.

Live dashboards of consumer health — lag, last heartbeat, active leases —
are maintained as stream tables that can feed directly into Grafana.

### Ordered Message Processing

For use cases where the order of messages matters — financial transactions,
audit trails, order management — `enable_inbox_ordering()` creates a
`next_<inbox>` stream table that surfaces *only the next expected message*
for each entity (customer, order, account). Out-of-order arrivals are
withheld until the preceding message has been processed. A separate gap
detection stream table automatically alerts when a message appears to be
permanently missing.

Priority queues let critical messages use a one-second refresh schedule
while background messages use thirty seconds, with no interference between
tiers.

### Scope

v0.28.0 is a substantial release: six weeks of solo engineering effort
covering the full outbox/inbox stack, consumer groups, ordered processing,
benchmarks, and documentation. The result is a self-contained, reliable
event-driven messaging system that needs nothing outside PostgreSQL.

---

## v0.29.0 — Relay CLI (`pgtrickle-relay`)

**Status: Planned**

### What is this?

v0.28.0 built the mailbox. v0.29.0 builds the postman.

`pgtrickle-relay` is a standalone command-line tool written in Rust that
connects pg_trickle outboxes and inboxes to external messaging systems.
Without this tool, users have to write their own relay process from scratch.
With it, connecting a pg_trickle stream table to Kafka, NATS, or an SQS
queue is a matter of minutes and a few lines of SQL configuration.

### Forward mode (outbox → external system)

The relay polls the pg_trickle outbox and publishes each delta to an
external sink. Supported sinks at launch:

| Sink | Notes |
|------|-------|
| NATS JetStream | With `Nats-Msg-Id` deduplication header |
| Apache Kafka | Idempotent producer, SASL/SSL |
| HTTP webhook | Per-event or batched, with `Idempotency-Key` header |
| Redis Streams | `XADD` with configurable stream key |
| Amazon SQS | `SendMessageBatch`, FIFO dedup |
| Remote PostgreSQL inbox | `ON CONFLICT` deduplication |
| RabbitMQ AMQP | Manual ack/nack |
| stdout / file | JSON-Lines, JSON pretty-print, CSV |

### Reverse mode (external system → inbox)

The relay also works in reverse: it consumes messages from an external
source and writes them into a pg_trickle inbox, with automatic deduplication.
The same eight backends are supported as sources.

This enables patterns like: a NATS message arrives, the relay writes it
to the pg_trickle inbox, the inbox pending stream table updates, and a
database-side processor handles it — all with exactly-once delivery
guaranteed by the three-layer deduplication chain (broker ID → inbox
`ON CONFLICT` → outbox idempotency key).

### Configuration

All relay pipelines are configured with SQL, not config files. There is no
YAML or TOML to maintain. A pipeline is created with:

```sql
SELECT pgtrickle.set_relay_outbox('my_pipeline', ...);
SELECT pgtrickle.enable_relay('my_pipeline');
```

The relay binary picks up changes at runtime via PostgreSQL `LISTEN/NOTIFY` —
no restart needed.

### Sub-100 ms latency

The relay wakes up instantly when new outbox rows are written, thanks to the
`pg_notify` signal emitted by the outbox (introduced in v0.28.0). Poll
intervals become the fallback rather than the primary wake-up mechanism.

### Scope

v0.29.0 is another large release — approximately five weeks of engineering
effort. It ships as a separate binary alongside pg_trickle, distributed as
a Docker container, Homebrew formula, and pre-built binaries for Linux and
macOS on both x86-64 and ARM.

---

## v0.30.0 — Pre-GA Correctness & Stability Sprint

**Status: Planned**

### What is this?

v0.30.0 is a *quality gate* release — no new user-visible features, but
a mandatory milestone before the 1.0 stable release. Its purpose is to close
every known correctness defect, operational failure mode, and documentation
gap so that v1.0.0 ships from a clean baseline.

### Key areas of work

**Correctness fixes**

- A subtle edge case in multi-table JOIN differential updates (EC-01) can
  cause phantom rows to appear or disappear in rare multi-cycle mutation
  sequences. The full fix is completed here with a deterministic test that
  reproduces the issue on demand.

- Snapshot and restore operations are wrapped in a proper database
  sub-transaction, so a crash partway through can no longer leave the
  database in a partially-restored state.

**Stability and safety**

- The internal caches that hold compiled query templates are now properly
  bounded. Without eviction limits they can grow without bound during a
  long-running session with many schema changes.

- Error classification logic that currently matches English text fragments
  (and silently breaks on non-English PostgreSQL installations) is replaced
  with standard five-character SQLSTATE codes.

**Documentation backfill**

- The upgrade guide (`UPGRADING.md`) is extended to cover every version
  from v0.15.0 through v0.27.0.
- All configuration settings introduced since v0.23.0 are documented in
  `CONFIGURATION.md`.
- New error codes have clear explanations, hints, and remediation steps in
  `ERRORS.md`.
- A first-party Grafana dashboard JSON file ships in the repository.

**Test coverage**

- New fuzz targets exercise the WAL decoder, MERGE template generator, and
  snapshot SQL builder against random inputs for 24 hours without crashing.
- A multi-database soak test is promoted from a separate stability workflow
  into the standard CI pipeline.

### Scope

v0.30.0 is a medium-sized release — approximately seven weeks of work
weighted heavily towards test coverage and documentation rather than new
code. It is a prerequisite for v1.0.0 and cannot be skipped.

---

## v0.31.0 — Performance & Scheduler Intelligence

**Status: Planned**

### What is this?

v0.31.0 makes pg_trickle's internal scheduler significantly smarter and
faster, with no changes to the SQL API.

### Adaptive batching

Today the scheduler checks each stream table independently for new changes.
If five stream tables all watch the same source table, that source table is
scanned five times per refresh cycle. Adaptive batching coalesces those
scans: one scan per source table per tick, with the results distributed to
all downstream stream tables. Expected throughput improvement: 10–30% for
deployments with many stream tables sharing sources.

### Plan-aware refresh strategy

When refreshing a stream table, pg_trickle currently uses a fixed
`merge_strategy` setting that must be tuned manually. Plan-aware routing
automatically inspects the PostgreSQL query plan after each differential
refresh and switches strategy for the next cycle when the plan data suggests
a different approach would be faster. This eliminates a common manual
tuning step.

### Faster IVM trigger functions

PostgreSQL 18 introduced a way to reference in-flight row data (transition
tables) directly by name inside a trigger function, without first copying
that data into a temporary table. pg_trickle currently uses temporary tables,
which adds overhead on every data change. This release switches to the new
direct-reference approach, reducing the cost of the change-capture hot path.

### Better observability for lock contention

A new Prometheus counter tracks how often pg_trickle falls back to an
unnecessarily broad database lock because it cannot fully analyse a
complex query. Operators can use this metric to identify stream tables
that are causing more contention than expected.

### Scope

v0.31.0 is a medium-sized release. The improvements are internally focused
and invisible to users at the SQL level, but they make a measurable
difference at scale — particularly for deployments running hundreds or
thousands of stream tables.

---

## v0.32.0 — Reactive Subscriptions & Zero-Downtime Operations

**Status: Planned**

### What is this?

v0.32.0 adds two long-requested capabilities:

1. **Push notifications** — applications can subscribe to changes in a
   stream table and receive instant notifications, enabling real-time
   dashboards, live UIs, and event-driven microservices.

2. **Zero-downtime query changes** — modifying the defining query of a
   large stream table no longer requires a multi-minute lock on the table.

### Reactive subscriptions

`pgtrickle.subscribe('my_stream_table', 'my_notification_channel')` registers
a listener. After every successful refresh that produces at least one change,
pg_trickle sends a PostgreSQL `NOTIFY` message to the named channel with a
payload like:

```json
{"name": "my_stream_table", "inserted_count": 12, "deleted_count": 3}
```

Any application holding a standard PostgreSQL connection and listening on
that channel receives this signal immediately, without polling. This powers
real-time dashboards, event-driven microservices, and reactive frontends —
using nothing but a standard PostgreSQL driver, with no Kafka, no Debezium,
no Hasura required.

A configurable coalescence window prevents notification storms when a stream
table refreshes at high frequency.

### Shadow-ST: zero-downtime query evolution

Today, calling `alter_query()` on a large stream table triggers a full
re-computation of the entire result set. For a stream table with millions of
rows, this can lock the table for minutes — an unacceptable operation in
production.

The new `shadow_build := true` parameter to `alter_query()` changes how
this works:

1. A parallel "shadow" stream table is created from the new query, invisible
   to users.
2. The shadow table is refreshed to convergence in the background, with no
   lock on the live table. The live table continues to serve reads and accept
   writes normally throughout.
3. When the shadow table has caught up, the storage is swapped atomically.
4. The new query goes live at the next refresh cycle. The shadow table is
   dropped.

The live table is readable and writable from start to finish.

### Scope

v0.32.0 is a medium-sized release. The shadow-ST feature touches the refresh
orchestrator — the most change-sensitive module in the codebase — and ships
behind a feature flag with a full TPC-H validation pass before the flag is
removed.

---

## v0.33.0 — Temporal IVM & Columnar Materialization

**Status: Planned**

### What is this?

v0.33.0 opens two new classes of analytic workloads that pg_trickle cannot
currently serve.

### Temporal IVM — time-travel queries

Normally, a stream table shows the *current* state of the world. Temporal
mode changes this: the stream table maintains a full history of how every
row has changed over time. Rows are never physically deleted; instead, each
row carries a `valid_from` timestamp and an optional `valid_to` timestamp
that records when a version was replaced.

This enables queries like "what did this table look like at 3 PM on Tuesday?"
without any external audit log infrastructure. The pattern is known as
**SCD Type 2** (Slowly Changing Dimension Type 2) in data warehousing, and
it is used for:

- Customer history ("what address was on file when this order shipped?")
- Regulatory audit trails ("what were the account balances at quarter-end?")
- Slowly-changing dimension tables in analytics pipelines

Creating a temporal stream table is a single parameter:

```sql
SELECT pgtrickle.create_stream_table(
    'customer_history',
    query := 'SELECT id, name, address FROM customers',
    temporal := true
);
```

Queries against the stream table with `AS OF TIMESTAMP $1` automatically
resolve against the historical row versions.

### Columnar materialization

Stream tables currently store their results in standard PostgreSQL heap
storage — optimised for row-by-row reads and writes. Analytic queries that
scan millions of rows to compute aggregates are better served by *columnar*
storage, where all values for a single column are stored together on disk.
This dramatically reduces I/O for aggregate queries (summing a column, for
example, only reads that column, not the entire row).

The `storage_backend := 'columnar'` parameter to `create_stream_table()`
tells pg_trickle to store the materialised result in Citus columnar storage
or pg_mooncake. The differential refresh machinery continues to work —
pg_trickle automatically routes the MERGE to use the `delete_insert` strategy
that columnar storage requires, with no manual configuration.

The result: analytic dashboards and reporting queries that consume the
materialised stream table see dramatically lower I/O, smaller storage
footprint, and faster aggregate performance.

### Combined use

Temporal mode and columnar storage can be combined: a slowly-changing
dimension table stored in columnar format with full history, queryable at
any point in time. This is listed as a stretch goal and is not a hard
requirement for the release.

### Scope

v0.33.0 is a medium-sized release. The temporal IVM work requires extending
the core frontier model — the internal mechanism that tracks which changes
have been processed — from a single LSN cursor to a two-dimensional
`(LSN, timestamp)` pair. A design spike in v0.32.0 is a prerequisite before
committing this feature to the milestone.

---

## How these versions fit together

```
v0.28.0  ─── Reliable event messaging (outbox + inbox)
    │
v0.29.0  ─── Relay CLI connecting that messaging to Kafka, NATS, etc.
    │
v0.30.0  ─── Quality gate: correctness, stability, docs (required for 1.0)
    │
v0.31.0  ─── Scheduler intelligence and hot-path performance
    │
v0.32.0  ─── Live push notifications + zero-downtime schema changes
    │
v0.33.0  ─── Time-travel history + analytic columnar storage
    │
v1.0.0   ─── Stable release, PostgreSQL 19, package registries
```

v0.28.0 and v0.29.0 together deliver the event-driven integration story.
v0.30.0 is a mandatory correctness and polish gate before 1.0. v0.31.0
through v0.33.0 each add a distinct new capability — improved efficiency,
reactive UIs, and analytic workloads respectively — while the core IVM
engine underneath remains stable.
