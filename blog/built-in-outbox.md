[← Back to Blog Index](README.md)

# The Outbox You Don't Have to Build

## pg_trickle's built-in outbox: consumer groups, offset tracking, exactly-once delivery

---

The transactional outbox pattern is well-known: write an event row in the same transaction as the business data, then have a poller read the event table and publish to a message broker. It guarantees that events are published if and only if the business transaction commits.

But building a correct outbox is tedious. You need a polling loop, offset tracking, consumer groups (if multiple consumers read the same events), and cleanup of consumed messages. Most teams spend a week building this and then spend a year maintaining it.

pg_trickle has a built-in outbox that takes one function call to enable. Stream table deltas — the rows added and removed by each refresh — are automatically captured as outbox messages. Consumer groups, offset tracking, and exactly-once delivery are provided out of the box.

This is distinct from the [outbox pattern post](outbox-pattern-turbocharged.md), which describes using stream tables *as* the outbox. This post is about the outbox API itself — the machinery that makes it work.

---

## Enabling the Outbox

```sql
-- Create a stream table
SELECT pgtrickle.create_stream_table(
  name  => 'order_summary',
  query => $$ SELECT customer_id, COUNT(*), SUM(total) FROM orders GROUP BY customer_id $$,
  schedule => '5s'
);

-- Enable outbox on it
SELECT pgtrickle.enable_outbox('order_summary');
```

That's it. From now on, every refresh of `order_summary` that produces changes (rows inserted, updated, or deleted) writes those changes as outbox messages.

---

## What Gets Written

Each outbox message contains:

| Field | Description |
|-------|-------------|
| `outbox_id` | Monotonically increasing sequence number |
| `refresh_id` | Which refresh cycle produced this message |
| `op` | Operation: `I` (insert), `D` (delete), `U` (update) |
| `row_data` | The row as JSONB |
| `old_row_data` | For updates: the previous row values |
| `created_at` | Timestamp of the refresh |

For a refresh that adds 3 customers and updates 1:

```
 outbox_id | refresh_id | op | row_data                          | old_row_data
-----------+------------+----+-----------------------------------+------------------
 1001      | 42         | I  | {"customer_id":5,"count":1,...}   | NULL
 1002      | 42         | I  | {"customer_id":6,"count":2,...}   | NULL
 1003      | 42         | I  | {"customer_id":7,"count":1,...}   | NULL
 1004      | 42         | U  | {"customer_id":3,"count":15,...}  | {"customer_id":3,"count":14,...}
```

---

## Consumer Groups

Multiple consumers can read from the same outbox independently. Each consumer group tracks its own offset.

```sql
-- Register a consumer group
SELECT pgtrickle.create_consumer_group('order_summary', 'analytics_pipeline');
SELECT pgtrickle.create_consumer_group('order_summary', 'search_indexer');
```

Each consumer group reads from the beginning and advances at its own pace. The `analytics_pipeline` can be 10 messages behind while the `search_indexer` is caught up. They don't interfere with each other.

---

## Polling

```sql
-- Fetch next batch of messages
SELECT * FROM pgtrickle.poll_outbox('order_summary', 'search_indexer', batch_size => 100);
```

This returns up to 100 unread messages for the `search_indexer` consumer group, starting from its last committed offset.

The messages are returned in `outbox_id` order — guaranteed monotonic and gap-free within a single stream table.

---

## Committing Offsets

After processing a batch, commit the offset:

```sql
SELECT pgtrickle.commit_offset('order_summary', 'search_indexer', 1004);
```

This marks all messages up to `outbox_id = 1004` as consumed for the `search_indexer` group. The next `poll_outbox()` call will start from 1005.

**Important:** Offset commit is idempotent. Committing the same offset twice is a no-op. Committing a lower offset than the current one is also a no-op (offsets only move forward).

---

## Exactly-Once Processing

The outbox provides exactly-once *delivery* semantics via the offset mechanism. Each message is delivered exactly once to each consumer group (assuming the consumer commits the offset after processing).

For exactly-once *processing*, you need idempotency on the consumer side. The standard approach:

```python
# Consumer pseudocode
while True:
    messages = poll_outbox('order_summary', 'search_indexer', batch_size=100)
    if not messages:
        time.sleep(1)
        continue

    for msg in messages:
        # Process idempotently (e.g., upsert to search index)
        process(msg)

    # Commit after successful processing
    commit_offset('order_summary', 'search_indexer', messages[-1].outbox_id)
```

If the consumer crashes between processing and committing, the next poll re-delivers the uncommitted messages. The consumer must handle re-delivery gracefully (idempotent upserts, deduplication by outbox_id, etc.).

---

## Consumer Lag

Monitor how far behind a consumer is:

```sql
SELECT * FROM pgtrickle.consumer_lag('order_summary');
```

```
 consumer_group     | committed_offset | latest_offset | lag  | lag_seconds
--------------------+------------------+---------------+------+-------------
 analytics_pipeline | 998              | 1004          | 6    | 12.4
 search_indexer     | 1004             | 1004          | 0    | 0.0
```

`lag` is the number of unconsumed messages. `lag_seconds` is the time between the consumer's committed offset and the latest message. If lag grows continuously, the consumer can't keep up.

---

## Outbox Status

```sql
SELECT * FROM pgtrickle.outbox_status('order_summary');
```

```
 stream_table   | enabled | total_messages | oldest_unconsumed | consumer_groups
----------------+---------+----------------+-------------------+-----------------
 order_summary  | t       | 15,247         | 998               | 2
```

`oldest_unconsumed` is the lowest outbox_id that any consumer group hasn't committed yet. Messages below this point can be cleaned up.

---

## Message Cleanup

Outbox messages accumulate. pg_trickle doesn't automatically delete them because it doesn't know when all consumers are done.

You can clean up consumed messages manually:

```sql
-- Delete messages that all consumer groups have committed past
SELECT pgtrickle.cleanup_outbox('order_summary');
```

This deletes all messages with `outbox_id < min(committed_offsets across all consumer groups)`. It's safe — no consumer can ever re-read those messages.

For automated cleanup, schedule it:

```sql
-- pg_cron: clean up every hour
SELECT cron.schedule('outbox-cleanup', '0 * * * *', $$
  SELECT pgtrickle.cleanup_outbox('order_summary');
$$);
```

---

## Outbox + Relay

The outbox API is the foundation for [pgtrickle-relay](streaming-to-kafka-without-kafka.md). The relay binary is a consumer that polls the outbox and publishes to external brokers (Kafka, NATS, SQS, etc.).

Under the hood, relay:
1. Creates a consumer group: `pgtrickle.create_consumer_group('order_summary', 'relay_kafka')`.
2. Polls in a loop: `pgtrickle.poll_outbox(...)`.
3. Publishes to Kafka.
4. Commits the offset after the Kafka ack.

You can use the outbox API directly for custom consumers, or use relay for standard broker integrations. They compose — one stream table can have both relay and custom consumers reading from the same outbox.

---

## Disabling the Outbox

```sql
SELECT pgtrickle.disable_outbox('order_summary');
```

This stops writing new outbox messages. Existing messages remain in the table until cleaned up. Consumer groups remain registered (they'll see no new messages).

To fully clean up:

```sql
SELECT pgtrickle.disable_outbox('order_summary');
SELECT pgtrickle.cleanup_outbox('order_summary');
-- Drop consumer groups if no longer needed
```

---

## When to Use the Built-In Outbox

**Use the outbox when:**
- You need to propagate stream table changes to external systems (search indexes, caches, notification services).
- Multiple consumers need independent reads of the same changes.
- You want offset tracking and replay without building it yourself.

**Don't use the outbox when:**
- You just need to query the stream table from SQL. The stream table itself is the API.
- You're using IMMEDIATE mode and need synchronous notification. Use PostgreSQL `LISTEN/NOTIFY` or reactive subscriptions instead.
- The stream table refreshes infrequently and changes are small. Direct polling of the stream table may be simpler.

---

## Summary

pg_trickle's built-in outbox captures stream table deltas as consumable messages with consumer groups, offset tracking, and exactly-once delivery.

One function call to enable. One function call to poll. One function call to commit. Consumer lag monitoring, cleanup, and relay integration are all included.

If you're building an outbox from scratch on top of PostgreSQL, stop. The one you need already exists.
