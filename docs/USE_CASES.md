# Use Cases

pg_trickle is for any team that has wanted PostgreSQL to keep a derived
table up to date *automatically* — without writing a refresh cron, an
external streaming pipeline, or a custom CDC consumer.

This page is a gallery of the most common things people build with
stream tables. Each section tells you what the pattern looks like, gives
you a minimal SQL example, and links to a deeper guide.

> New to stream tables? Read **[What is pg_trickle?](introduction.md)**
> first, or jump straight to the
> **[5-minute Quickstart](QUICKSTART_5MIN.md)**.

---

## At a glance

| Use case | Best refresh mode | Deeper guide |
|---|---|---|
| [Real-time dashboards](#1-real-time-dashboards) | DIFFERENTIAL with `1s`–`5s` schedule | [Patterns §5](PATTERNS.md#pattern-5-real-time-dashboards) |
| [Operational read models / CQRS](#2-operational-read-models--cqrs) | IMMEDIATE | [Patterns §1](PATTERNS.md) |
| [Fraud detection / alerting / anomaly](#3-fraud-detection-alerting-anomaly) | DIFFERENTIAL `1s` | [Demo](DEMO.md) |
| [Leaderboards & TopK](#4-leaderboards--topk) | DIFFERENTIAL or IMMEDIATE | [SQL Reference – TopK](SQL_REFERENCE.md) |
| [Bronze / Silver / Gold (medallion)](#5-bronze--silver--gold) | DIFFERENTIAL with chained STs | [Patterns §1](PATTERNS.md#pattern-1-bronze--silver--gold-materialization) |
| [Event-driven services (outbox / inbox)](#6-event-driven-services) | IMMEDIATE for the table; DIFFERENTIAL for the views | [Outbox](OUTBOX.md) · [Inbox](INBOX.md) |
| [Cross-system replication](#7-cross-system-replication) | DIFFERENTIAL | [Publications](PUBLICATIONS.md) · [Relay](RELAY_GUIDE.md) |
| [Slowly-changing dimensions](#8-slowly-changing-dimensions-scd) | DIFFERENTIAL | [Patterns §3](PATTERNS.md#pattern-3-slowly-changing-dimensions-scd) |
| [Multi-tenant analytics](#9-multi-tenant-analytics) | DIFFERENTIAL with RLS | [Multi-tenant](integrations/multi-tenant.md) |
| [Citus distributed analytics](#10-citus-distributed-analytics) | DIFFERENTIAL | [Citus](CITUS.md) |
| [dbt-managed warehouse models](#11-dbt-managed-warehouse-models) | AUTO | [dbt integration](integrations/dbt.md) |

---

## 1. Real-time dashboards

You want KPI tiles ("orders today", "revenue per region", "active
users this hour") to update within a second or two of the underlying
data changing. With pg_trickle you write the SQL once and any number
of dashboards (Grafana, Metabase, Looker, a custom React app) just
read from the stream table.

```sql
SELECT pgtrickle.create_stream_table(
    'kpi_revenue_today',
    $$SELECT region, SUM(amount) AS revenue
      FROM orders
      WHERE created_at >= date_trunc('day', now())
      GROUP BY region$$,
    schedule => '2s'
);
```

Why it's a fit: aggregates over high-cardinality groups are exactly
where DIFFERENTIAL refresh wins biggest.

---

## 2. Operational read models / CQRS

A microservice writes to a normalised event/order/customer table; a
read API needs the *denormalised projection* (one row per order with
customer name, current status, latest payment). Most teams build this
with a separate read database and a CDC pipeline. With pg_trickle the
projection is just a stream table sitting next to the write tables.

```sql
SELECT pgtrickle.create_stream_table(
    'order_view',
    $$SELECT o.id, o.placed_at, c.name AS customer,
             p.status AS payment_status,
             COALESCE(s.shipped_at, NULL) AS shipped_at
      FROM orders o
      JOIN customers c ON c.id = o.customer_id
      LEFT JOIN payments p ON p.order_id = o.id
      LEFT JOIN shipments s ON s.order_id = o.id$$,
    refresh_mode => 'IMMEDIATE'
);
```

Why it's a fit: `IMMEDIATE` mode gives you read-your-writes
consistency without a second database.

---

## 3. Fraud detection, alerting, anomaly

Define a stream table that flags suspicious activity (large
transactions, velocity rules, unusual geographies). Subscribe an
alerter to that stream table — either via PostgreSQL `LISTEN/NOTIFY`,
a [downstream publication](PUBLICATIONS.md), or by polling.

```sql
SELECT pgtrickle.create_stream_table(
    'high_velocity_accounts',
    $$SELECT account_id, COUNT(*) AS txn_count, SUM(amount) AS total
      FROM transactions
      WHERE occurred_at >= now() - interval '5 minutes'
      GROUP BY account_id
      HAVING COUNT(*) > 20$$,
    schedule => '1s'
);
```

The [demo](DEMO.md) ships a full 9-node fraud-detection DAG you can
run locally with `docker compose up`.

---

## 4. Leaderboards & TopK

`ORDER BY ... LIMIT N` stream tables are a special case where
pg_trickle stores only the top N rows and updates them with scoped
recomputation when the changes affect the leaderboard.

```sql
SELECT pgtrickle.create_stream_table(
    'top_10_customers',
    $$SELECT customer_id, SUM(amount) AS lifetime_spend
      FROM orders
      GROUP BY customer_id
      ORDER BY lifetime_spend DESC
      LIMIT 10$$,
    schedule => '5s'
);
```

---

## 5. Bronze / Silver / Gold

A medallion architecture in PostgreSQL alone:

- **Bronze** – raw ingest table (a regular table you write to).
- **Silver** – cleaned and deduplicated stream table.
- **Gold** – business-level aggregates stream table that depends on
  Silver.

Set the schedule only on Gold; pg_trickle propagates the cadence
upstream automatically (CALCULATED scheduling).

See [Patterns §1](PATTERNS.md#pattern-1-bronze--silver--gold-materialization).

---

## 6. Event-driven services

The transactional outbox/inbox pattern, native to PostgreSQL:

- **Outbox** — write events in the same transaction as your business
  data; an external system or the [relay](RELAY_GUIDE.md) drains them
  to Kafka / NATS / SQS / a webhook.
- **Inbox** — receive events idempotently from an external system;
  stream tables give you live views of pending work, retries, and a
  dead-letter queue.

See [Transactional Outbox](OUTBOX.md) and [Transactional Inbox](INBOX.md).

---

## 7. Cross-system replication

Once a stream table exists, you can expose it as a standard PostgreSQL
logical-replication publication. Anything that speaks logical
replication — Debezium, Kafka Connect, a downstream Postgres replica,
a Spark Structured Streaming job, a custom WAL consumer — can
subscribe to live changes.

```sql
SELECT pgtrickle.stream_table_to_publication('order_view');
-- Subscribers can now subscribe to publication pgt_pub_order_view
```

See [Downstream Publications](PUBLICATIONS.md).

---

## 8. Slowly-changing dimensions (SCD)

Type 2 SCDs (one row per version, with `valid_from` / `valid_to`) fall
out naturally from a stream table over an event log. See
[Patterns §3](PATTERNS.md#pattern-3-slowly-changing-dimensions-scd).

---

## 9. Multi-tenant analytics

If your application multi-tenants by `tenant_id`, you can build
per-tenant aggregates as a single stream table grouped by `tenant_id`
and protect access with PostgreSQL Row-Level Security. See
[Multi-tenant integration](integrations/multi-tenant.md) and the
[Row-Level Security tutorial](tutorials/ROW_LEVEL_SECURITY.md).

---

## 10. Citus distributed analytics

pg_trickle works on Citus-distributed source tables. The scheduler
polls per-worker WAL slots via `dblink`, merges changes on the
coordinator, and applies the delta — automatically and idempotently.
See [Citus](CITUS.md).

---

## 11. dbt-managed warehouse models

Use the `stream_table` materialization in dbt. No custom adapter
needed — works with the standard `dbt-postgres` adapter.

```sql
{{ config(materialized='stream_table', schedule='5m', refresh_mode='AUTO') }}
SELECT customer_id, SUM(amount) AS total
FROM {{ source('raw', 'orders') }}
GROUP BY customer_id
```

See [dbt integration](integrations/dbt.md).

---

## When pg_trickle is *not* the right fit

A short, honest list — knowing when to look elsewhere is a feature.

- **Pure OLTP with no derived state.** If you don't have anything to
  materialize, you don't need a materializer.
- **Sub-millisecond latency derived state.** IMMEDIATE mode is fast,
  but it pays the differential cost inside your transaction. If you
  need every transaction to commit in < 1 ms, benchmark first.
- **Stateless event transformation only.** If you want "transform each
  Kafka event" with no stored state, a stream processor (Flink, Bytewax)
  is closer to that shape.
- **Cross-database joins at scale.** pg_trickle reads from local
  PostgreSQL tables (or Citus distributions). For federated joins
  across many heterogeneous databases, consider a streaming engine.
- **Workloads where you cannot install extensions.** Some managed
  PostgreSQL services don't allow third-party extensions. Check
  [Installation](INSTALL.md) for the support matrix.

---

**See also:**
[Patterns](PATTERNS.md) ·
[Performance Cookbook](PERFORMANCE_COOKBOOK.md) ·
[SQL Reference](SQL_REFERENCE.md) ·
[Comparisons](COMPARISONS.md)
