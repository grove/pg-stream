[← Back to Blog Index](README.md)

# Publishing Stream Tables via Logical Replication

## Your stream table as a replication origin — ship derived data to downstream PostgreSQL instances

---

You have a stream table that aggregates orders into regional revenue summaries. The primary database is in `us-east-1`. Your analytics team in Europe needs the same data. Your reporting service runs against a separate read replica.

Normally, you'd either replicate the raw `orders` table and re-aggregate on each downstream instance, or build a CDC pipeline with Debezium to capture the stream table changes.

There's a simpler option: **publish the stream table itself via PostgreSQL's built-in logical replication.**

A stream table is a regular PostgreSQL table with triggers and a scheduler. It participates in logical replication like any other table. You can create a publication, and downstream subscribers receive the inserts, updates, and deletes that each refresh applies.

---

## Setting It Up

### On the Primary (Publisher)

```sql
-- Create the stream table (if not already existing)
SELECT pgtrickle.create_stream_table(
  name  => 'revenue_by_region',
  query => $$
    SELECT region, date_trunc('day', created_at) AS day,
           SUM(total) AS revenue, COUNT(*) AS order_count
    FROM orders
    JOIN customers ON customers.id = orders.customer_id
    GROUP BY region, date_trunc('day', created_at)
  $$,
  schedule => '5s'
);

-- Create a publication for the stream table
CREATE PUBLICATION revenue_pub FOR TABLE pgtrickle.revenue_by_region;
```

### On the Subscriber

```sql
-- Create the target table (matching schema)
CREATE TABLE revenue_by_region (
  region TEXT,
  day TIMESTAMP,
  revenue NUMERIC,
  order_count BIGINT
);

-- Create the subscription
CREATE SUBSCRIPTION revenue_sub
  CONNECTION 'host=primary-db dbname=prod user=replicator'
  PUBLICATION revenue_pub;
```

That's it. The subscriber receives every change that the stream table refresh applies. When the MERGE step inserts 3 new region-day groups and updates 5 existing ones, the subscriber receives 3 INSERT and 5 UPDATE messages.

---

## What Gets Replicated

Logical replication captures DML operations on the published table. For a stream table, the DML happens during the MERGE step of each refresh:

| Refresh Operation | Replicated As |
|-------------------|---------------|
| New group appears in result | INSERT |
| Existing group changes (new sum, count) | UPDATE |
| Group disappears from result | DELETE |
| FULL refresh (truncate + reload) | TRUNCATE + INSERTs |

For DIFFERENTIAL refreshes, the subscriber sees fine-grained changes — only the groups that actually changed. For FULL refreshes, the subscriber sees a TRUNCATE followed by a full reload. Both are correct; DIFFERENTIAL produces smaller replication streams.

---

## Why This Is Useful

### Offloading Analytics Queries

The primary database handles transactional workloads. Analytical queries on the stream table compete for the same resources. By publishing the stream table to a downstream analytics instance, you separate the workloads:

```
primary: orders (writes) → stream table refresh (pg_trickle)
    ↓ logical replication
analytics: revenue_by_region (reads) → Grafana, reports
```

The analytics instance doesn't need pg_trickle installed. It's just a regular PostgreSQL database receiving replicated data.

### Multi-Region Data Distribution

For globally distributed applications, replicate the stream table to each regional database:

```
us-east-1 (primary): compute revenue_by_region
    ↓ logical replication
eu-west-1: revenue_by_region (read-only copy)
ap-southeast-1: revenue_by_region (read-only copy)
```

Each region gets sub-second updates without running its own aggregation pipeline. The primary computes once; subscribers receive the result.

### Feeding Non-PostgreSQL Systems

Logical replication can be consumed by tools like Debezium, which decode the replication stream and forward it to Kafka, Elasticsearch, or other systems. Publishing a stream table means Debezium captures the aggregated, processed data — not the raw source tables.

```
orders → pg_trickle → revenue_by_region → logical replication → Debezium → Kafka
```

The Kafka consumers receive clean, aggregated events. No need to re-aggregate downstream.

---

## CDC Implications

When a stream table is published via logical replication, there are two layers of change capture:

1. **CDC on source tables** (pg_trickle's trigger/WAL capture): feeds the stream table refresh.
2. **Logical replication on the stream table**: ships refresh results downstream.

These are independent. The first is managed by pg_trickle. The second is standard PostgreSQL logical replication.

If the stream table uses WAL-based CDC (`cdc_mode => 'wal'`), and you also publish it via logical replication, both consume from the WAL — but they use separate replication slots. The slots are independent; one falling behind doesn't affect the other.

---

## Replication Identity

Logical replication requires a **replication identity** to identify rows for UPDATE and DELETE. By default, PostgreSQL uses the primary key.

Stream tables may not have a primary key in the traditional sense. pg_trickle's internal row identity (`__pgt_row_id`) is a hidden column. For logical replication, you need an explicit identity:

**Option 1: Add a primary key on the stream table** (if the output has a natural key):

```sql
-- After creating the stream table
ALTER TABLE pgtrickle.revenue_by_region
  ADD PRIMARY KEY (region, day);
```

**Option 2: Use REPLICA IDENTITY FULL:**

```sql
ALTER TABLE pgtrickle.revenue_by_region
  REPLICA IDENTITY FULL;
```

This tells PostgreSQL to include all column values in UPDATE/DELETE WAL records. It's less efficient (larger WAL records) but works without a primary key.

**Recommendation:** If the stream table has a natural key (which most GROUP BY stream tables do — the group-by columns), add a primary key. It's better for replication performance and makes the subscriber's conflict resolution easier.

---

## Multiple Stream Tables in One Publication

You can publish multiple stream tables in a single publication:

```sql
CREATE PUBLICATION analytics_pub FOR TABLE
  pgtrickle.revenue_by_region,
  pgtrickle.customer_metrics,
  pgtrickle.product_rankings;
```

The subscriber creates matching tables and receives changes from all three. Each stream table's refresh produces independent changes; the subscriber applies them in commit order.

---

## Latency

The end-to-end latency from source change to subscriber visibility:

```
source DML → CDC capture (~0-15ms) → scheduler dispatch (~0-1000ms) →
  refresh execution (~5-500ms) → WAL write → logical replication (~1-50ms) →
  subscriber apply (~1-10ms)
```

Total: typically 100ms–2s. The dominant factor is the scheduler interval (how often the stream table is refreshed). With a 1-second schedule and DIFFERENTIAL mode, expect ~1–2 seconds end-to-end.

For IMMEDIATE mode stream tables, the refresh happens in the same transaction as the source DML. The subscriber sees the change as soon as the transaction commits and the WAL is shipped. End-to-end latency: typically 5–50ms.

---

## Conflict Handling on the Subscriber

If the subscriber table is read-only (no local writes), there are no conflicts. This is the recommended setup.

If the subscriber has local writes (not recommended for replicated stream tables), standard PostgreSQL logical replication conflict handling applies: duplicates cause the subscription to stall until resolved.

---

## Summary

Stream tables are regular PostgreSQL tables. They participate in logical replication like any other table. Create a publication, set up a subscriber, and the stream table's refresh deltas ship downstream.

Use this for:
- Offloading analytics queries to a separate instance
- Multi-region distribution of aggregated data
- Feeding Debezium/Kafka with clean, aggregated events

Set a replication identity (preferably a primary key on the group-by columns) and you're done. The primary computes, subscribers receive, no custom CDC pipeline required.
