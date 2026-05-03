[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# Streaming to Kafka Without Kafka Expertise

## How pgtrickle-relay bridges stream table deltas to external systems

---

You have pg_trickle maintaining stream tables in PostgreSQL. Now your analytics team wants those deltas in Kafka. Your mobile team wants webhook notifications. Your data science team wants events in NATS for a real-time ML feature store.

The traditional approach: write a Kafka producer in Python, poll the database, serialize the deltas, handle offsets, deal with exactly-once semantics, set up monitoring, and maintain it forever.

The pg_trickle approach: run a single binary.

---

## What pgtrickle-relay Does

`pgtrickle-relay` is a standalone Rust binary that reads from pg_trickle's outbox tables and writes to external messaging systems. It's not a library, not a framework, not an SDK. It's a process you run next to your PostgreSQL instance.

```
PostgreSQL                              External Systems
┌─────────────────┐                     ┌──────────────┐
│ stream table    │                     │ Kafka topic  │
│   ↓ delta       │                     └──────▲───────┘
│ outbox table    │──→ pgtrickle-relay ──→     │
│ (transactional) │         │                  │
└─────────────────┘         │           ┌──────▲───────┐
                            ├──→        │ NATS subject │
                            │           └──────────────┘
                            │           ┌──────────────┐
                            └──→        │ HTTP webhook  │
                                        └──────────────┘
```

Supported sinks: **Kafka**, **NATS JetStream**, **SQS**, **RabbitMQ**, **Redis Streams**, **HTTP webhooks**.

Supported sources (for the inbox direction): the same list, reversed. External events come in, land in an inbox table, and stream tables can read from them.

---

## Setup

### 1. Enable the outbox on a stream table

```sql
SELECT pgtrickle.enable_outbox('revenue_by_region');
```

This creates an outbox table that captures every delta produced by `revenue_by_region`. Each refresh cycle that produces non-empty changes writes an outbox row in the same transaction as the MERGE.

### 2. Configure the relay

```toml
# relay.toml
[global]
postgres_url = "postgres://user:pass@localhost/mydb"
metrics_bind = "0.0.0.0:9090"

[[pipeline]]
name = "revenue-to-kafka"
source = { type = "outbox", stream_table = "revenue_by_region" }
sink = { type = "kafka", brokers = "kafka:9092", topic = "revenue-deltas" }

[[pipeline]]
name = "orders-to-nats"
source = { type = "outbox", stream_table = "order_view" }
sink = { type = "nats", url = "nats://localhost:4222", subject = "orders.>" }

[[pipeline]]
name = "alerts-to-webhook"
source = { type = "outbox", stream_table = "fraud_alerts" }
sink = { type = "webhook", url = "https://hooks.example.com/fraud", method = "POST" }
```

### 3. Run it

```bash
pgtrickle-relay --config relay.toml
```

That's it. No Kafka consumer code. No NATS client library. No webhook retry logic.

---

## What Gets Sent

Each outbox message is a JSON envelope containing:

```json
{
  "stream_table": "revenue_by_region",
  "sequence": 42,
  "timestamp": "2026-04-27T10:15:03.412Z",
  "delta": {
    "inserted": [
      {"region": "europe", "day": "2026-04-27", "revenue": 150200.50, "order_count": 1203}
    ],
    "deleted": [
      {"region": "europe", "day": "2026-04-27", "revenue": 149800.00, "order_count": 1201}
    ]
  },
  "metadata": {
    "refresh_mode": "DIFFERENTIAL",
    "refresh_duration_ms": 12,
    "delta_rows": 2
  }
}
```

The delta is the exact set of rows that changed in the stream table — rows removed (old values) and rows added (new values). For an aggregate that was updated, the old aggregate value is in `deleted` and the new value is in `inserted`.

Consumers don't need to know about pg_trickle, PostgreSQL, or IVM. They receive a JSON message with the before/after state of the affected rows. They can build their own materialization from the delta stream.

---

## Subject Routing

For NATS and Kafka, you can route messages to different subjects/topics based on the delta content:

```toml
[[pipeline]]
name = "regional-revenue"
source = { type = "outbox", stream_table = "revenue_by_region" }
sink = {
  type = "nats",
  url = "nats://localhost:4222",
  subject_template = "revenue.{{ region }}"
}
```

A delta for the `europe` region goes to `revenue.europe`. A delta for `asia` goes to `revenue.asia`. Consumers subscribe to only the regions they care about.

---

## High Availability

In production you run multiple relay instances. They coordinate via PostgreSQL advisory locks:

```toml
[global]
ha_group = "relay-primary"
```

One instance acquires the advisory lock and becomes the active leader. The others are standby. If the leader crashes, a standby acquires the lock within seconds and continues from where the leader left off.

The outbox table stores the consumer offset. A new leader reads the last committed offset and resumes. No messages are lost. Some messages may be delivered twice during failover — consumers should be idempotent (which they should be anyway).

---

## The Inbox Direction

The relay also works in reverse. External events can flow into PostgreSQL:

```toml
[[pipeline]]
name = "payments-from-kafka"
source = { type = "kafka", brokers = "kafka:9092", topic = "payment-events", group_id = "pgtrickle-inbox" }
sink = { type = "inbox", inbox_name = "payment_events" }
```

Events land in `pgtrickle.inbox_payment_events`. You can build stream tables on top of the inbox:

```sql
-- Create the inbox
SELECT pgtrickle.create_inbox('payment_events');

-- Stream table over incoming payment events
SELECT pgtrickle.create_stream_table(
    'payment_summary',
    $$SELECT
        customer_id,
        SUM((payload->>'amount')::numeric) AS total_paid,
        COUNT(*) AS payment_count
      FROM pgtrickle.inbox_payment_events
      WHERE processed = false
      GROUP BY customer_id$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);
```

Events from Kafka are now queryable as a PostgreSQL table, with incremental aggregation on top.

---

## Monitoring

The relay exposes Prometheus metrics at `/metrics`:

```
# Messages delivered successfully
pgtrickle_relay_messages_delivered_total{pipeline="revenue-to-kafka"} 12847

# Delivery latency (histogram)
pgtrickle_relay_delivery_duration_seconds_bucket{pipeline="revenue-to-kafka",le="0.01"} 12500

# Consumer lag (messages pending)
pgtrickle_relay_consumer_lag{pipeline="revenue-to-kafka"} 3

# Errors
pgtrickle_relay_delivery_errors_total{pipeline="revenue-to-kafka"} 0
```

And a health endpoint at `/health` that returns the status of each pipeline.

---

## When to Use the Relay vs. Direct Outbox Polling

If your consumer is a PostgreSQL-native application (another service that queries the database), use `pgtrickle.poll_outbox()` directly. No relay needed.

If your consumer is an external system that speaks Kafka, NATS, HTTP, or any other messaging protocol — use the relay. It handles serialization, delivery, retries, offset tracking, and HA. Writing a custom consumer for each sink is the kind of infrastructure work that seems small and grows into a maintenance burden.

The relay is also useful when you need fan-out: one stream table's deltas going to multiple sinks. Each pipeline runs independently, with its own offset tracking.
