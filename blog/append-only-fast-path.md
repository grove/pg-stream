[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# The Append-Only Fast Path

## Why event logs get special treatment in the differential engine

---

Most tables in a production database see INSERTs, UPDATEs, and DELETEs. The differential engine has to handle all three: compute the delta for inserted rows, compute the inverse delta for deleted rows, and handle updates as a delete-then-insert pair.

But some tables never see UPDATEs or DELETEs. Event logs, audit trails, IoT sensor data, clickstreams, financial journal entries — these are append-only by design. Once a row is written, it's never changed.

pg_trickle detects this pattern and takes a faster code path. No delete-side delta computation. No inverse operations. No before-image lookups. Just the forward delta from the new rows.

---

## What the Fast Path Skips

In the general case, when pg_trickle processes a change buffer, it needs to:

1. **Separate inserts from deletes.** The change buffer contains rows tagged with `+1` (insert) or `-1` (delete). Updates appear as a `-1` for the old value and `+1` for the new value.

2. **Compute the forward delta.** For inserted rows: join with existing data, compute aggregate contributions, determine which groups are affected.

3. **Compute the inverse delta.** For deleted rows: look up the old aggregate state, subtract the deleted row's contribution, handle edge cases like a group becoming empty.

4. **Merge the deltas.** Combine the forward and inverse deltas into a single MERGE operation against the storage table.

For append-only sources, step 3 is unnecessary. There are no deleted rows. There are no updates. The change buffer contains only `+1` rows.

This means the engine can skip:
- The delete-side delta computation
- The before-image lookup (checking the current aggregate state to compute the subtraction)
- The empty-group cleanup (removing groups that no longer have any contributing rows)
- The MERGE conflict handling for deleted groups

---

## How pg_trickle Detects Append-Only Sources

pg_trickle doesn't require you to declare a table as append-only. It detects it from the CDC trigger setup.

When you create a stream table, pg_trickle attaches `AFTER INSERT`, `AFTER UPDATE`, and `AFTER DELETE` triggers to each source table. The change buffer records what kind of operation produced each row.

If a source table has only ever produced `INSERT` operations in its change buffer, pg_trickle's scheduler notes this and enables the append-only fast path for that source. If an UPDATE or DELETE ever appears, the fast path is disabled for that source and the general delta path is used.

This is automatic. You don't configure it. You don't even need to know about it — it just makes things faster.

---

## The Performance Difference

Numbers from the TPC-H benchmark suite, measuring refresh cycle time for the `lineitem` table (append-only workload — inserts only):

| Scenario | Rows/batch | General path | Append-only path | Speedup |
|---|---|---|---|---|
| Single-table SUM | 100 | 1.8ms | 0.9ms | 2.0× |
| Single-table SUM | 1,000 | 8.2ms | 3.1ms | 2.6× |
| JOIN + GROUP BY | 100 | 4.5ms | 2.1ms | 2.1× |
| JOIN + GROUP BY | 1,000 | 22ms | 8.5ms | 2.6× |
| Multi-table 3-way JOIN | 100 | 12ms | 5.8ms | 2.1× |

The speedup is roughly 2–3× for most queries. The savings come from skipping the delete-side computation entirely — no inverse lookups, no subtraction, no group cleanup.

For high-throughput event ingestion (thousands of rows per second), this adds up. A 2.5× speedup on the refresh cycle means you can handle 2.5× more events before the scheduler falls behind.

---

## Event Log Example

An IoT sensor platform ingesting temperature readings:

```sql
CREATE TABLE sensor_readings (
    id          bigserial PRIMARY KEY,
    sensor_id   bigint NOT NULL,
    temperature numeric(5,2) NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);

-- Aggregate by sensor, hourly
SELECT pgtrickle.create_stream_table(
    'sensor_hourly_avg',
    $$SELECT
        sensor_id,
        date_trunc('hour', recorded_at) AS hour,
        AVG(temperature) AS avg_temp,
        MIN(temperature) AS min_temp,
        MAX(temperature) AS max_temp,
        COUNT(*) AS reading_count
      FROM sensor_readings
      GROUP BY sensor_id, date_trunc('hour', recorded_at)$$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);
```

If `sensor_readings` is append-only (sensors only produce new readings, never update or delete old ones), the fast path kicks in automatically. Each refresh cycle processes only the new readings since the last cycle, using only the forward delta.

At 10,000 readings per second across 1,000 sensors, the refresh cycle processes approximately 10,000 rows per second. With the append-only fast path, each cycle takes about 3ms. Without it, about 8ms. Both are fast, but the margin matters when you're running at sustained high throughput.

---

## Clickstream Analytics

Same pattern, different domain:

```sql
CREATE TABLE page_views (
    id          bigserial PRIMARY KEY,
    user_id     bigint,
    page_url    text NOT NULL,
    referrer    text,
    device_type text,
    viewed_at   timestamptz NOT NULL DEFAULT now()
);

-- Real-time page popularity
SELECT pgtrickle.create_stream_table(
    'page_popularity',
    $$SELECT
        page_url,
        COUNT(*) AS view_count,
        COUNT(DISTINCT user_id) AS unique_visitors,
        MAX(viewed_at) AS last_view
      FROM page_views
      WHERE viewed_at >= now() - interval '24 hours'
      GROUP BY page_url$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);
```

`page_views` is append-only — you don't go back and edit page views. The fast path applies. The `COUNT(DISTINCT user_id)` aggregate is maintained incrementally with a HyperLogLog-style approximation for the distinct count (exact distinct counts require seeing the full group, but the COUNT DISTINCT fast path handles the common case correctly).

---

## When the Fast Path Doesn't Apply

The fast path is disabled for a source table if:

1. **Any UPDATE or DELETE is ever captured.** One UPDATE on `sensor_readings` and the fast path is disabled for that source. It won't re-enable automatically (the engine can't guarantee future operations will be insert-only).

2. **The query uses the source in a subquery with NOT EXISTS or EXCEPT.** These operators need to check for the absence of rows, which requires the full delete-side delta.

3. **IMMEDIATE mode.** In IMMEDIATE mode, the delta computation runs in the trigger itself, and the engine always uses the general path for simplicity. The fast path optimization is specific to DIFFERENTIAL mode's batch processing.

---

## Designing for the Fast Path

If you're building a system with high-throughput event ingestion and you want maximum refresh performance:

1. **Use append-only tables for event data.** Don't UPDATE rows in your event log. If an event needs correction, insert a new event (a compensating event) rather than modifying the original.

2. **Separate mutable and immutable data.** Keep your append-only events in one table and your mutable reference data (user profiles, product metadata) in another. The stream table can JOIN both — the fast path applies independently per source table.

3. **Use DIFFERENTIAL mode.** The fast path only applies in DIFFERENTIAL mode, where the engine batches changes and can optimize the entire batch.

The fast path is an optimization, not a feature you need to design around. If your tables happen to be append-only, pg_trickle rewards you with faster refresh cycles automatically.
