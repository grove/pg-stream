# The Outbox Pattern, Turbocharged

## Transactionally Consistent Event Emission from PostgreSQL Without Dual-Write

---

The outbox pattern exists because distributed systems lie.

The lie: "I'll save the record to the database and send the event to Kafka in the same operation." The reality: one of those can fail while the other succeeds. You commit the database record but the Kafka send times out. Or the Kafka send succeeds but your process crashes before the database commits. Either way, downstream systems have a different view of the world than your database.

The outbox pattern is the solution: write the event to a `outbox` table in the *same* transaction as the business record. A separate process reads the outbox and publishes to Kafka/SQS/whatever. The outbox is durable because it's in PostgreSQL. The publication is idempotent because you can retry. Exactly-once-delivery becomes achievable.

This pattern is well-understood. It's implemented by Debezium, AWS EventBridge Pipes, and a dozen ORM libraries. It works.

What it doesn't do: maintain the outbox entries incrementally as derived data changes. If the event you need to emit is "the customer's order total changed" — a derived aggregate — you need to compute that aggregate, write it to the outbox, and keep it fresh as orders change.

This is where pg_trickle stream tables change the model.

---

## The Standard Outbox and Its Limitations

The standard outbox implementation:

```sql
CREATE TABLE outbox (
  id          bigserial PRIMARY KEY,
  aggregate_type  text NOT NULL,    -- e.g., 'Order', 'Customer'
  aggregate_id    bigint NOT NULL,
  event_type      text NOT NULL,    -- e.g., 'OrderPlaced', 'TotalUpdated'
  payload         jsonb NOT NULL,
  created_at      timestamptz DEFAULT NOW(),
  published_at    timestamptz,
  published       boolean DEFAULT false
);

-- Application code
BEGIN;
INSERT INTO orders (customer_id, total, ...) VALUES (...);
INSERT INTO outbox (aggregate_type, aggregate_id, event_type, payload)
VALUES ('Order', $new_order_id, 'OrderPlaced', $payload::jsonb);
COMMIT;
```

The outbox relay picks up unpublished rows, sends them, marks them published. Clean and correct.

**Limitation 1: Complex derived events require application logic.**

Emitting an `OrderTotalChanged` event when an order's total changes due to an item update requires detecting the change, computing the new total, constructing the event, and writing it to the outbox — all in the application layer. This logic is duplicated for every code path that can change an order's total.

**Limitation 2: Aggregate-level events require aggregation.**

Emitting a `CustomerRevenueUpdated` event — the customer's total revenue across all orders — requires aggregating in the application, which means either an extra SELECT or maintaining the aggregate as denormalized state. If the aggregate is maintained via triggers, you're back to the trigger-maintenance problem.

**Limitation 3: Multi-table derived events are fragile.**

If the event payload should include denormalized fields from related tables (shipping address, product name, account tier), the application must join them at event creation time. This works but creates tight coupling between the event schema and the application code at write time.

---

## Stream Tables as Event Sources

pg_trickle stream tables are maintained tables. When a stream table row changes, the change is a computable event. You can attach an outbox relay directly to stream table changes.

```sql
-- A stream table that computes customer-level revenue state
SELECT pgtrickle.create_stream_table(
  name         => 'customer_revenue_state',
  query        => $$
    SELECT
      c.id            AS customer_id,
      c.email,
      c.account_tier,
      COUNT(o.id)     AS order_count,
      SUM(o.total)    AS total_revenue,
      MAX(o.created_at) AS last_order_at
    FROM customers c
    LEFT JOIN orders o ON o.customer_id = c.id
    GROUP BY c.id, c.email, c.account_tier
  $$,
  schedule     => '10 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

Every time a customer's order count or total revenue changes — whether from a new order, a refund, or a cancelled order — the corresponding row in `customer_revenue_state` is updated with the new aggregate values.

Now you can attach an outbox relay that watches for changes in `customer_revenue_state` and emits events:

```sql
-- Attach an outbox relay to the stream table
SELECT pgtrickle.create_outbox_relay(
  stream_table  => 'customer_revenue_state',
  outbox_table  => 'outbox',
  aggregate_type => 'Customer',
  aggregate_id_column => 'customer_id',
  event_type_fn  => $$
    CASE
      WHEN OLD.order_count IS NULL THEN 'CustomerFirstOrderPlaced'
      WHEN NEW.order_count > OLD.order_count THEN 'CustomerOrderAdded'
      WHEN NEW.total_revenue < OLD.total_revenue THEN 'CustomerRefundProcessed'
      ELSE 'CustomerRevenueUpdated'
    END
  $$,
  payload_fn => $$
    jsonb_build_object(
      'customer_id',    NEW.customer_id,
      'email',          NEW.email,
      'account_tier',   NEW.account_tier,
      'order_count',    NEW.order_count,
      'total_revenue',  NEW.total_revenue,
      'last_order_at',  NEW.last_order_at,
      'revenue_delta',  NEW.total_revenue - COALESCE(OLD.total_revenue, 0)
    )
  $$
);
```

When `customer_revenue_state` is updated by a refresh cycle, pg_trickle writes the corresponding event rows to `outbox` as part of the same transaction. Your existing outbox relay picks them up and publishes.

The derived aggregate — total revenue, order count — is computed once, by the DVM engine, not duplicated across every application code path that can change orders.

---

## The Mechanics

The outbox relay works at the stream table layer:

1. A refresh cycle computes the delta to `customer_revenue_state` — a set of rows to insert, update, or delete.
2. For each changed row, the relay evaluates `event_type_fn` and `payload_fn` against `(OLD.*, NEW.*)`.
3. The relay writes one `outbox` row per changed stream table row.
4. Steps 2–3 happen inside the same transaction that applies the delta.

This is transactionally safe: if the refresh cycle transaction rolls back (e.g., due to a conflict), the outbox entries are also rolled back. The outbox never contains events for changes that didn't commit.

The `OLD.*` and `NEW.*` semantics let you express transitions:
- `OLD IS NULL` → this is a new row (first-time state)
- `NEW IS NULL` → this row was deleted
- `NEW.total_revenue > OLD.total_revenue` → revenue increased
- `NEW.order_count > OLD.order_count` → new order placed

This is richer than a raw table trigger, which fires on every write to orders — including changes that don't affect the customer's aggregate state. The stream table relay fires only when the *aggregate state* actually changes.

---

## Enriched Event Payloads

A frequent frustration with the standard outbox: the payload at write time might not contain enough context for downstream consumers. "OrderPlaced" fires, but the consumer needs to know the order total, the product names, the customer's account tier. The application at write time may not have all that information ready.

With stream tables as event sources, the event payload is computed from the fully-denormalized stream table. You can include anything the stream table contains:

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'order_event_state',
  query => $$
    SELECT
      o.id          AS order_id,
      o.status,
      o.total,
      o.created_at,
      c.id          AS customer_id,
      c.email       AS customer_email,
      c.account_tier,
      s.name        AS shipping_address_name,
      s.city,
      s.country,
      array_agg(jsonb_build_object(
        'product_id',   p.id,
        'product_name', p.name,
        'quantity',     oi.quantity,
        'unit_price',   oi.unit_price
      ) ORDER BY oi.id) AS line_items
    FROM orders o
    JOIN customers c ON c.id = o.customer_id
    JOIN shipping_addresses s ON s.id = o.shipping_address_id
    JOIN order_items oi ON oi.order_id = o.id
    JOIN products p ON p.id = oi.product_id
    GROUP BY o.id, o.status, o.total, o.created_at,
             c.id, c.email, c.account_tier,
             s.name, s.city, s.country
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

The event payload includes the full order with line items, customer context, and shipping address — all pre-joined. The downstream consumer receives a self-contained event and doesn't need to query back to the database.

This is the "fat event" pattern: events that carry all the context consumers need, rather than just an ID that consumers have to resolve.

---

## The Deduplication Story

Outbox patterns require consumers to handle duplicate delivery (at-least-once delivery is the standard guarantee). When stream tables drive the outbox, there's an additional source of potential duplicates to think about.

If a customer places three orders in the same 10-second refresh cycle, the stream table will update once — to the aggregate state after all three orders. The outbox will have one event, reflecting the final state.

If the outbox relay fails to publish and the refresh worker retries (applying the same delta again), the outbox will have a duplicate row for the same state. The consumer's deduplication logic (typically, "have I seen an event with this aggregate_id and this state before?") handles this correctly.

The key insight: stream tables aggregate changes within a refresh cycle, so the outbox volume is naturally lower than it would be with row-level triggers on the source tables. Three new orders in 10 seconds produce one `CustomerRevenueUpdated` event, not three.

---

## Monitoring Outbox Health

```sql
-- Outbox relay status
SELECT
  relay_name,
  stream_table,
  events_emitted_last_cycle,
  avg_emit_ms,
  outbox_pending_count,
  oldest_pending_age_secs
FROM pgtrickle.outbox_relay_status();

-- Are events being published fast enough?
SELECT
  COUNT(*) FILTER (WHERE published = false) AS pending,
  COUNT(*) FILTER (WHERE published = false
                   AND created_at < NOW() - INTERVAL '5 minutes') AS old_pending,
  MIN(created_at) FILTER (WHERE published = false) AS oldest_pending_at
FROM outbox;
```

The `outbox_pending_count` growing over time indicates the outbox relay can't keep up with the event rate. Common causes: slow downstream (Kafka backpressure, API rate limits), slow database (outbox table index needs VACUUM or REINDEX), or the relay is down.

---

## When to Use This Pattern

The stream-table-backed outbox is the right choice when:
- The event you need to emit is a derived or aggregated value, not a raw row change
- The event payload benefits from denormalization (fat events)
- You want to decouple the event schema from the application write path
- Multiple code paths affect the same aggregate, and you want one canonical event source

It's overkill when:
- The event maps directly to a raw table change (e.g., `UserCreated` fires when a row is inserted into `users`)
- You need sub-second event latency (stream tables add up to `schedule` latency)
- You're doing event sourcing where every intermediate state matters (stream tables coalesce changes within a cycle)

For raw table events, a standard trigger-based outbox is simpler. For derived aggregate events, the stream-table-backed outbox is significantly more correct and easier to maintain.

---

## The Bigger Picture

The transactional outbox pattern solves the dual-write problem. Stream tables solve the derived-data freshness problem. Combined, they solve a third problem: emitting events that reflect aggregate state changes in a way that's both transactionally safe and computationally efficient.

Without this combination, teams typically end up with either:
- Events that fire on raw table changes and require consumers to compute aggregates (chatty, couples consumers to the database schema)
- Scheduled aggregate recomputation with a separate event emission step (latency, correctness concerns at the boundary)

The stream-table outbox gives you: aggregate events, computed correctly by the DVM engine, emitted transactionally, with the freshness controlled by your `schedule` parameter.

The reliability of the outbox pattern, the correctness of IVM, and the expressiveness of SQL. That combination is harder to build than it sounds, but pg_trickle makes it a configuration decision rather than an engineering project.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
