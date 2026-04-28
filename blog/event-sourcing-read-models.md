[← Back to Blog Index](README.md)

# Event Sourcing Read Models Without Replay

## Project live read-optimized views from an append-only event store — no replay required

---

Event sourcing is architecturally elegant and operationally painful. The elegance is in the append-only event log: every state change is recorded as an immutable fact, the full history is preserved, and you can derive any view of the data by replaying events from the beginning. The pain is in that last part — "replaying events from the beginning."

When your event store has 500 million events and you need to add a new read model (say, "revenue by product category for the last 90 days"), you face a choice. You can replay 500 million events through your new projection, which takes hours and requires careful orchestration. Or you can implement the projection going forward and accept that it only has data from its creation date onward. Neither option is great.

pg_trickle eliminates this trade-off. Define your read model as a SQL query over the events table, register it as a stream table, and the initial materialization happens once (a full query against the events table). After that, every new event is processed incrementally — the read model stays current within milliseconds of the event being committed, without replaying anything.

---

## The Event Store Pattern

A typical PostgreSQL event store looks like this:

```sql
CREATE TABLE events (
    event_id     bigserial PRIMARY KEY,
    stream_id    uuid NOT NULL,          -- aggregate ID
    event_type   text NOT NULL,
    payload      jsonb NOT NULL,
    metadata     jsonb DEFAULT '{}',
    created_at   timestamptz DEFAULT now(),
    version      integer NOT NULL        -- per-stream sequence number
);

CREATE INDEX ON events (stream_id, version);
CREATE INDEX ON events (event_type, created_at);
```

Events are immutable. You never update or delete them. New state is expressed by appending new events. An order goes through `OrderPlaced → OrderConfirmed → OrderShipped → OrderDelivered`, each as a separate row in the events table.

Read models (projections) materialize the current state by folding over these events. The "current order status" projection looks at the latest event per stream. The "revenue by region" projection aggregates `OrderPlaced` events. The "inventory levels" projection sums `ItemAdded` and `ItemRemoved` events per product.

---

## Traditional Projection Approaches

**In-memory projectors:** A service subscribes to the event stream, maintains state in memory, and projects events as they arrive. Fast, but state is lost on restart (requires full replay), and you need one service per projection.

**Catch-up subscription:** A service reads events from a position marker, processes them, and advances the marker. Handles restarts but is sequential — adding a new projection means replaying from the beginning.

**Periodic snapshot + replay:** Take a snapshot of the projection state periodically, replay from the snapshot position on restart. Reduces replay cost but adds complexity around snapshot management.

All of these approaches share a problem: the projection logic lives in application code, separate from the database. It must handle ordering, idempotency, and failure recovery. It's another service to deploy, monitor, and scale.

---

## Read Models as Stream Tables

With pg_trickle, a read model is just a SQL query materialized as a stream table:

```sql
-- Current order status (latest event per order)
SELECT pgtrickle.create_stream_table(
    'order_status',
    $$
    SELECT DISTINCT ON (stream_id)
        stream_id AS order_id,
        payload->>'status' AS status,
        payload->>'customer_id' AS customer_id,
        payload->>'total' AS total,
        created_at AS last_updated
    FROM events
    WHERE event_type IN ('OrderPlaced', 'OrderConfirmed', 'OrderShipped', 'OrderDelivered', 'OrderCancelled')
    ORDER BY stream_id, version DESC
    $$
);
```

When a new `OrderShipped` event is appended for order `abc-123`, the incremental refresh:
1. Detects the new event row
2. Evaluates the `DISTINCT ON` logic for stream `abc-123`
3. Updates the single row in `order_status` for that order

It does not re-read the other 499,999,999 events. It does not replay the history of order `abc-123`. It processes one new event and produces one row update. The cost is constant regardless of how many historical events exist.

---

## Revenue Analytics Without ETL

A common read model for e-commerce is revenue aggregation:

```sql
SELECT pgtrickle.create_stream_table(
    'revenue_by_category',
    $$
    SELECT
        payload->>'category' AS category,
        date_trunc('day', created_at) AS day,
        SUM((payload->>'amount')::numeric) AS revenue,
        COUNT(*) AS order_count
    FROM events
    WHERE event_type = 'OrderPlaced'
    GROUP BY payload->>'category', date_trunc('day', created_at)
    $$
);
```

New `OrderPlaced` events are processed incrementally. The SUM and COUNT for the affected category and day are adjusted. No ETL pipeline, no separate analytics database, no nightly batch job. The read model is always within seconds of the event store.

For more sophisticated analytics — running totals, moving averages, cohort breakdowns — you can cascade stream tables:

```sql
-- Daily revenue per category (from above)
-- → Monthly summary with growth rate
SELECT pgtrickle.create_stream_table(
    'monthly_category_performance',
    $$
    SELECT
        category,
        date_trunc('month', day) AS month,
        SUM(revenue) AS monthly_revenue,
        SUM(order_count) AS monthly_orders,
        SUM(revenue) / NULLIF(SUM(order_count), 0) AS avg_order_value
    FROM revenue_by_category
    GROUP BY category, date_trunc('month', day)
    $$
);
```

The cascade handles the dependency ordering automatically. New events flow through: event → daily aggregate → monthly aggregate. Each step is incremental.

---

## Inventory Projections

Inventory is the canonical event sourcing example: items are added and removed, and the current level is the sum of all additions minus all removals.

```sql
SELECT pgtrickle.create_stream_table(
    'inventory_levels',
    $$
    SELECT
        payload->>'product_id' AS product_id,
        payload->>'warehouse_id' AS warehouse_id,
        SUM(
            CASE event_type
                WHEN 'ItemReceived' THEN (payload->>'quantity')::integer
                WHEN 'ItemShipped' THEN -(payload->>'quantity')::integer
                WHEN 'ItemAdjusted' THEN (payload->>'adjustment')::integer
                ELSE 0
            END
        ) AS current_stock
    FROM events
    WHERE event_type IN ('ItemReceived', 'ItemShipped', 'ItemAdjusted')
    GROUP BY payload->>'product_id', payload->>'warehouse_id'
    $$
);
```

Every `ItemReceived` event increments the stock for that product-warehouse pair. Every `ItemShipped` event decrements it. The stream table maintains the running total without ever replaying the full history. If your warehouse processes 10,000 shipments per hour, the inventory levels update 10,000 times per hour — each update touching exactly one row in the projection.

---

## Adding New Projections Without Replay

The traditional pain point of event sourcing is adding a new projection to an existing system. With a catch-up subscription, you need to replay from event zero. With pg_trickle, you define a new stream table and run the initial materialization:

```sql
-- New requirement: track customer lifetime value
SELECT pgtrickle.create_stream_table(
    'customer_ltv',
    $$
    SELECT
        payload->>'customer_id' AS customer_id,
        COUNT(*) AS total_orders,
        SUM((payload->>'amount')::numeric) AS lifetime_value,
        MIN(created_at) AS first_order,
        MAX(created_at) AS latest_order
    FROM events
    WHERE event_type = 'OrderPlaced'
    GROUP BY payload->>'customer_id'
    $$
);
```

The first `refresh_stream_table` call performs a full materialization — it reads all `OrderPlaced` events and computes the aggregates. This is a one-time cost, equivalent to the initial replay. After that, every new `OrderPlaced` event is processed incrementally. The read model is live from that moment forward.

The critical difference from a catch-up subscription: the "replay" is just a SQL query that PostgreSQL optimizes and executes in parallel. No single-threaded event processor. No position markers. No at-least-once vs. exactly-once semantics to worry about. The database handles it.

---

## Consistency Guarantees

One of the subtle advantages of running projections inside the database is transactional consistency. When a new event is committed and the stream table is refreshed, the read model update happens in the same transactional context (or at a known, bounded lag if using background refresh).

This eliminates the classic event sourcing consistency problem: a user places an order, immediately queries their order list, and doesn't see it because the projection service hasn't processed it yet. With pg_trickle's IMMEDIATE refresh mode, the read model is updated before the transaction commits. The user always sees their own writes.

```sql
-- IMMEDIATE mode: projection updates in the same transaction as the event
SELECT pgtrickle.alter_stream_table('order_status', refresh_mode := 'IMMEDIATE');
```

For high-throughput projections where same-transaction consistency isn't needed, use DEFERRED mode and the background scheduler. The projection will be at most a few seconds behind — still far better than the minutes-to-hours lag typical of catch-up subscription systems.

---

## When This Replaces Your Projection Service

You don't need a separate projection service when:

- Your read models can be expressed as SQL queries (aggregations, joins, filters, windowing)
- You want transactional consistency between writes and reads
- You want new projections to be deployable without replaying history
- You want projections to handle late events and corrections automatically
- You don't want to operate Kafka, RabbitMQ, or an event bus for internal projections

You still need a separate service when:

- Your projection logic involves external API calls or side effects
- You need to send emails or notifications as part of projection processing
- Your projection requires custom business logic that can't be expressed in SQL
- You're projecting across multiple databases or services

For most CRUD-heavy applications with analytics requirements, the SQL-based approach covers 80–90% of projection needs. The remaining projections (those with side effects) still benefit from the event store — they just read from it using a traditional subscriber.

---

*Event sourcing gives you the history. pg_trickle gives you the read models — live, consistent, and without the replay tax.*
