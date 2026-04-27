# Reactive Alerts Without Polling

## PostgreSQL as a Push System — No Polling Loop Required

---

Your fraud detection system checks suspicious transactions every minute. Your inventory management system pings a "low stock" endpoint every 30 seconds. Your SLA monitoring service queries open support tickets every 15 seconds and sends a Slack message when something crosses a threshold.

All three of these are polling loops. They're querying the same data over and over, waiting for a condition to become true, then doing something.

Polling loops work. They're also wasteful by design — they spend 99% of their CPU on queries that return "nothing changed yet." They add latency equal to half the polling interval on average. They create a thundering herd problem when many services poll the same data. And they require someone to own the poll interval: too slow and you miss SLAs, too fast and you hammer the database.

There's a better model. It's called reactive subscriptions, and pg_trickle ships it in v0.39.

---

## What Reactive Subscriptions Mean in Practice

The idea is simple: instead of asking "has this condition become true?" repeatedly, you *subscribe* to the condition and receive a notification when it fires.

```sql
-- Create a stream table for SLA monitoring
SELECT pgtrickle.create_stream_table(
  name         => 'ticket_sla',
  query        => $$
    SELECT
      t.id          AS ticket_id,
      t.team_id,
      t.status,
      t.priority,
      t.created_at,
      t.sla_deadline,
      t.sla_deadline < NOW() AS breached,
      t.sla_deadline - NOW() AS time_to_breach
    FROM tickets t
    WHERE t.status != 'resolved'
  $$,
  schedule     => '30 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

-- Subscribe to the condition: tickets that just breached SLA
SELECT pgtrickle.subscribe(
  stream_table  => 'ticket_sla',
  condition     => 'breached = true AND OLD.breached = false',
  channel       => 'sla_breach_alerts',
  payload       => '{"ticket_id": ticket_id, "team_id": team_id, "priority": priority}'
);
```

When a ticket crosses its SLA deadline — which pg_trickle detects during the next refresh cycle — a notification fires on `sla_breach_alerts`. Your application, connected via PostgreSQL `LISTEN`, receives it.

No polling. No "check every 30 seconds." The notification fires once, at the right time, with exactly the data you need.

---

## The Mechanics

pg_trickle's reactive subscriptions work at the stream table layer, not the raw table layer.

When the DVM engine applies a delta to a stream table, it evaluates each active subscription condition against every changed row. The condition can reference both `OLD.*` and `NEW.*` — the row values before and after the delta — enabling you to express *transitions* rather than just states.

This is the key capability that raw PostgreSQL triggers lack. A trigger fires on every change. A subscription fires only when the condition transitions from false to true.

### State Transitions vs. Continuous Conditions

A continuous condition query:
```sql
WHERE breached = true
```
This fires on every refresh for every breached ticket — every 30 seconds until the ticket is resolved. That's not what you want for an alert.

A state transition:
```sql
WHERE breached = true AND OLD.breached = false
```
This fires exactly once per ticket, when it crosses from non-breached to breached. Subsequent refreshes see `OLD.breached = true` and don't re-fire.

pg_trickle maintains the `OLD.*` state implicitly — the previous values come from the stream table's current contents before the delta is applied.

---

## Real-World Use Cases

### Inventory Alerts

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'inventory_status',
  query        => $$
    SELECT
      p.id         AS product_id,
      p.name,
      p.sku,
      i.qty,
      i.reorder_point,
      i.qty <= i.reorder_point AS below_reorder
    FROM products p
    JOIN inventory i ON i.product_id = p.id
  $$,
  schedule     => '1 minute',
  refresh_mode => 'DIFFERENTIAL'
);

-- Alert when a product drops below reorder point
SELECT pgtrickle.subscribe(
  stream_table => 'inventory_status',
  condition    => 'below_reorder = true AND OLD.below_reorder = false',
  channel      => 'inventory_alerts',
  payload      => '{"product_id": product_id, "sku": sku, "qty": qty, "reorder_point": reorder_point}'
);

-- Alert when a product hits zero
SELECT pgtrickle.subscribe(
  stream_table => 'inventory_status',
  condition    => 'qty = 0 AND OLD.qty > 0',
  channel      => 'stockout_alerts',
  payload      => '{"product_id": product_id, "sku": sku, "product_name": name}'
);
```

Both alerts fire exactly once per event, not once per polling cycle.

### Fraud Detection

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'customer_velocity',
  query        => $$
    SELECT
      customer_id,
      COUNT(*)                                       AS tx_count_1h,
      SUM(amount)                                    AS tx_volume_1h,
      COUNT(DISTINCT merchant_id)                    AS distinct_merchants_1h,
      COUNT(DISTINCT SPLIT_PART(ip_address, '.', 1)
            || '.' || SPLIT_PART(ip_address, '.', 2)) AS distinct_ip_class_b
    FROM transactions
    WHERE created_at > NOW() - INTERVAL '1 hour'
    GROUP BY customer_id
  $$,
  schedule     => '10 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

-- Flag when a customer exceeds velocity thresholds
SELECT pgtrickle.subscribe(
  stream_table => 'customer_velocity',
  condition    => $$(
    (tx_count_1h > 20 AND OLD.tx_count_1h <= 20)
    OR (tx_volume_1h > 5000 AND OLD.tx_volume_1h <= 5000)
    OR (distinct_merchants_1h > 10 AND OLD.distinct_merchants_1h <= 10)
  )$$,
  channel      => 'fraud_review_queue',
  payload      => '{"customer_id": customer_id, "tx_count": tx_count_1h, "volume": tx_volume_1h}'
);
```

The velocity aggregates are maintained incrementally by the DVM engine. The subscription fires when any threshold is crossed. The fraud review service subscribes to `fraud_review_queue` via `LISTEN` and processes each notification.

No Kafka. No Redis Streams. No lambda function polling DynamoDB. Just PostgreSQL.

### Vector Distance Alerts

This is where it gets interesting for ML workloads.

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'user_drift',
  query        => $$
    SELECT
      u.id AS user_id,
      vector_avg(i.embedding) AS current_taste_vec,
      ut.baseline_taste_vec,
      vector_avg(i.embedding) <=> ut.baseline_taste_vec AS taste_drift
    FROM user_likes ul
    JOIN items i ON i.id = ul.item_id
    JOIN user_taste_baselines ut ON ut.user_id = ul.user_id
    JOIN users u ON u.id = ul.user_id
    WHERE ul.liked_at > NOW() - INTERVAL '7 days'
    GROUP BY u.id, ut.baseline_taste_vec
  $$,
  schedule     => '5 minutes',
  refresh_mode => 'DIFFERENTIAL'
);

-- Notify when a user's taste has drifted significantly from their baseline
SELECT pgtrickle.subscribe(
  stream_table => 'user_drift',
  condition    => 'taste_drift > 0.3 AND (OLD.taste_drift IS NULL OR OLD.taste_drift <= 0.3)',
  channel      => 'taste_drift_events',
  payload      => '{"user_id": user_id, "drift": taste_drift}'
);
```

When a user's recent listening/watching/browsing history has shifted enough from their baseline profile, a notification fires. Your recommendation service receives it and triggers a profile refresh. No polling, no `CASE WHEN taste_drift > 0.3` check in a cron job.

---

## Connecting Your Application

On the application side, you use PostgreSQL `LISTEN`:

```python
import psycopg2
import json

conn = psycopg2.connect(dsn)
conn.set_isolation_level(0)  # AUTOCOMMIT, required for LISTEN
cur = conn.cursor()
cur.execute("LISTEN sla_breach_alerts")

while True:
    conn.poll()
    while conn.notifies:
        notify = conn.notifies.pop(0)
        payload = json.loads(notify.payload)
        handle_sla_breach(
            ticket_id=payload['ticket_id'],
            team_id=payload['team_id'],
            priority=payload['priority']
        )
```

The `LISTEN` connection is cheap — a long-lived idle connection that wakes up only when there's a notification. This is the fundamental difference from polling: the database pushes to you rather than you pulling from it.

For high-volume workloads where a single `LISTEN` connection can't process fast enough, notifications can be routed to a PostgreSQL-backed queue (pg_trickle integrates with pgmq) and consumed in parallel.

---

## Why Not PostgreSQL Triggers?

You can implement a version of this with raw PostgreSQL triggers today. The trigger fires on INSERT/UPDATE, checks the condition, and calls `pg_notify()` if it's true.

The limitations:
- Triggers fire on raw table changes, not on derived state. If your alert condition depends on an aggregate (total volume in the last hour, count of open tickets, user taste drift), you can't express it as a trigger on a raw table.
- Transitions are hard. Expressing "fired only when transitioning from false to true" requires the trigger to query the previous state, which means an extra SELECT inside the trigger — adding latency to every write on the source table.
- Complexity. A fraud detection trigger that checks velocity aggregates inside itself is a trigger that does a GROUP BY on every transaction insert. The performance implications are significant.

Reactive subscriptions in pg_trickle are different because the condition evaluates against the *stream table* — derived state — not the raw source. The aggregate computation happens in the background worker. The subscription check is a comparison on already-computed values.

---

## The Latency Story

With a 10-second refresh interval, you'll see a maximum alert latency of 10 seconds. The average is 5 seconds.

Is that too slow? For most alert use cases — SLA monitoring, inventory management, business KPI dashboards — no. For true real-time applications (payment fraud that must be blocked in-flight, stock trading algorithms), a streaming system like Kafka Streams or Apache Flink at the millisecond level is the right tool.

pg_trickle's reactive subscriptions cover the 90% of alert use cases that don't need sub-second latency but have been paying the cost of polling anyway. If your team currently runs polling loops at 30-second or 1-minute intervals and wishes they were faster, reactive subscriptions are the right answer.

---

## What This Changes Architecturally

The polling loop paradigm produces a certain kind of system:
- Each service owns its own polling schedule
- The same data is read repeatedly by many services
- Latency is bounded by the poll interval, not by when data changes
- The alert fires at most once per interval, even if conditions have been true for longer

The reactive subscription paradigm produces a different kind of system:
- The database tracks when conditions become true
- Services receive notifications when they're relevant
- Latency is bounded by the refresh interval plus processing time
- The alert fires exactly when the condition first becomes true, not on a schedule

The second system is more correct, more efficient, and more composable. The transition is mechanical: replace each polling loop with a `pgtrickle.subscribe()` call and a `LISTEN` connection.

The polling loop was always a workaround for the lack of a good subscription primitive. Now there is one.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
