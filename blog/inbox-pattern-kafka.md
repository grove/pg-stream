[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# The Inbox Pattern: Receiving Events from Kafka into PostgreSQL

## Idempotent, ordered, exactly-once event ingestion without writing a consumer

---

The outbox pattern gets all the attention. You write an event to an outbox table in the same transaction as your business data, and an external process delivers it to Kafka/NATS/SQS. Problem solved: no dual-write, no inconsistency.

But what about the other direction? Your service needs to *receive* events from Kafka and process them. You need:

1. Events to land in PostgreSQL reliably.
2. Duplicate events to be handled idempotently.
3. Events to be processed in order.
4. Failed processing to not lose the event.

Most teams build this with a Kafka consumer in their application code: poll, deserialize, write to the database, commit the offset. It works, but it's another piece of infrastructure to maintain, monitor, and debug.

pg_trickle's inbox pattern moves the consumer into PostgreSQL.

---

## How the Inbox Works

### 1. Create an inbox

```sql
SELECT pgtrickle.create_inbox('payment_events');
```

This creates:
- `pgtrickle.inbox_payment_events` — the inbox table where events land
- Deduplication infrastructure (unique constraint on event ID)
- An ordering guarantee (sequence number per partition)

### 2. Configure the relay to deliver events

```toml
# relay.toml
[[pipeline]]
name = "payments-inbound"
source = { type = "kafka", brokers = "kafka:9092", topic = "payment.completed", group_id = "pgtrickle-payments" }
sink = { type = "inbox", inbox_name = "payment_events" }
```

The relay reads from Kafka and writes to the inbox table. Each Kafka message becomes a row in `pgtrickle.inbox_payment_events`.

### 3. Build stream tables on top of the inbox

```sql
-- Aggregate payment events into per-customer totals
SELECT pgtrickle.create_stream_table(
    'customer_payment_totals',
    $$SELECT
        (payload->>'customer_id')::bigint AS customer_id,
        SUM((payload->>'amount')::numeric) AS total_paid,
        COUNT(*) AS payment_count,
        MAX((payload->>'completed_at')::timestamptz) AS last_payment
      FROM pgtrickle.inbox_payment_events
      GROUP BY (payload->>'customer_id')::bigint$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);
```

Events from Kafka are now a PostgreSQL table, with incremental aggregation on top.

---

## Deduplication

Kafka guarantees at-least-once delivery. The same event can be delivered multiple times — network retries, consumer rebalances, relay restarts.

The inbox handles deduplication using the event's unique identifier:

```sql
-- Inbox table structure (simplified)
CREATE TABLE pgtrickle.inbox_payment_events (
    inbox_seq       bigserial PRIMARY KEY,
    event_id        text NOT NULL UNIQUE,  -- Kafka message key or a field from the payload
    partition_key   text,
    payload         jsonb NOT NULL,
    received_at     timestamptz NOT NULL DEFAULT now(),
    processed       boolean NOT NULL DEFAULT false
);
```

The `UNIQUE` constraint on `event_id` means that if the relay delivers the same event twice, the second INSERT is silently dropped (using `ON CONFLICT DO NOTHING`). No duplicates in the inbox, no duplicates in the stream table.

The event ID comes from the Kafka message key by default. You can configure the relay to extract it from the payload:

```toml
[[pipeline]]
name = "payments-inbound"
source = { type = "kafka", brokers = "kafka:9092", topic = "payment.completed", group_id = "pgtrickle-payments" }
sink = { type = "inbox", inbox_name = "payment_events", dedup_key = "$.payment_id" }
```

---

## Ordering

Events from the same Kafka partition arrive in order. The inbox preserves this ordering with a `partition_key` column and a sequence number.

If your application needs to process events in order per customer:

```sql
SELECT pgtrickle.enable_inbox_ordering('payment_events', partition_key => 'customer_id');
```

This ensures that the relay inserts events for the same customer sequentially, and the stream table's delta computation respects the ordering within each partition.

For most use cases — aggregations, counts, sums — ordering doesn't matter. The aggregate is the same regardless of insertion order. But for stateful processing (where event N depends on event N-1), ordering matters and the inbox preserves it.

---

## Dead-Letter Queue

If the relay can't deserialize a Kafka message (malformed JSON, unexpected schema), it routes the message to a dead-letter table:

```sql
-- Automatically created alongside the inbox
-- pgtrickle.inbox_payment_events_dlq
SELECT * FROM pgtrickle.inbox_payment_events_dlq;
```

| inbox_dlq_seq | event_id | raw_payload | error_message | received_at |
|---|---|---|---|---|
| 1 | pay_99 | `{invalid json` | "unexpected end of JSON input" | 2026-04-27 10:00:01 |

Dead-letter events don't affect the main inbox or the stream tables. You can inspect them, fix the upstream producer, and replay them manually if needed.

---

## A Complete Example: Order Fulfillment

Your order service publishes events to Kafka when orders are placed. Your inventory service (running PostgreSQL + pg_trickle) needs to track orders per SKU:

```toml
# Inventory service relay config
[[pipeline]]
name = "orders-inbound"
source = { type = "kafka", brokers = "kafka:9092", topic = "orders.placed", group_id = "inventory-service" }
sink = { type = "inbox", inbox_name = "incoming_orders", dedup_key = "$.order_id" }
```

```sql
-- Create the inbox
SELECT pgtrickle.create_inbox('incoming_orders');

-- Stream table: pending fulfillment per SKU
SELECT pgtrickle.create_stream_table(
    'pending_fulfillment',
    $$SELECT
        item->>'sku' AS sku,
        SUM((item->>'quantity')::int) AS total_quantity,
        COUNT(DISTINCT (payload->>'order_id')) AS order_count
      FROM pgtrickle.inbox_incoming_orders,
           jsonb_array_elements(payload->'items') AS item
      WHERE NOT processed
      GROUP BY item->>'sku'$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);
```

When the order service publishes an event, it flows through Kafka → relay → inbox → stream table. The `pending_fulfillment` table shows how many units of each SKU need to be shipped.

When the warehouse marks an order as shipped, the application updates the inbox row (`processed = true`), and the stream table's delta removes that order's contribution from the aggregate.

---

## Inbox vs. Direct Kafka Consumer

| Aspect | Direct Kafka consumer | pg_trickle inbox |
|---|---|---|
| Code to write | Consumer class, deserialization, DB writes, offset management | TOML config + SQL |
| Deduplication | Application-level (you implement it) | Built-in (UNIQUE constraint) |
| Dead-letter queue | Application-level | Built-in |
| Aggregation | Application-level queries | Stream tables (incremental) |
| Monitoring | Custom metrics | pg_trickle monitoring (built-in) |
| Ordering | Kafka partition ordering + application logic | Preserved in inbox |
| Scaling | Consumer group + application instances | Relay instances + advisory lock HA |

The inbox pattern is particularly useful when:
- The events end up in PostgreSQL anyway (you just need them in a table)
- You want to aggregate the events incrementally
- You don't want to write and maintain consumer code
- You need exactly-once semantics in the database

If your event processing requires complex business logic that can't be expressed in SQL (calling external APIs, sending emails, orchestrating workflows), a direct consumer is more appropriate. The inbox is for data ingestion and aggregation, not arbitrary computation.
