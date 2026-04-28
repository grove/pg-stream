[← Back to Blog Index](README.md)

# Time-Series Downsampling Without TimescaleDB

## Keep hourly, daily, and monthly rollups in sync with raw data — using stream tables instead of a dedicated TSDB

---

Every IoT platform, observability stack, and financial system hits the same wall. Raw sensor data accumulates at thousands of rows per second. Dashboards need to display trends over hours, days, and months. Reading raw data for a 30-day chart means scanning hundreds of millions of rows. The standard answer is to pre-aggregate: maintain rollup tables at different time granularities. The non-standard part is keeping those rollups correct as late data arrives, corrections are applied, and backfills happen.

TimescaleDB solves this with continuous aggregates — materialized views that automatically refresh over time buckets. It's a good product. But it requires a dedicated extension, a specific table format (hypertables), and its own mental model. If your data is already in regular PostgreSQL tables and you want rollups that maintain themselves incrementally, pg_trickle gives you the same capability with standard SQL.

---

## The Rollup Problem

Consider a temperature monitoring system. Sensors report every 5 seconds:

```sql
CREATE TABLE sensor_readings (
    sensor_id   integer,
    recorded_at timestamptz,
    temperature numeric(5,2),
    humidity    numeric(5,2)
);
```

You need three rollup levels:

1. **Hourly** — average temperature and humidity per sensor per hour
2. **Daily** — min, max, average per sensor per day
3. **Monthly** — average and percentile distributions per sensor per month

The naive approach is a cron job that truncates and rebuilds each rollup table every hour. This works until it doesn't — until the rebuild takes longer than an hour, until a backfill invalidates three months of daily aggregates, until a dashboard user notices a 45-minute gap between reality and the chart.

The incremental approach maintains each rollup as a stream table. When a sensor reading is inserted, the hourly bucket it falls into is updated in the same transaction (or within milliseconds via the background scheduler). Late data that arrives for yesterday? The daily rollup for yesterday is corrected. Backfill three months of historical data? The monthly rollups rebuild differentially, processing only the buckets that received new data.

---

## Hourly Rollups

```sql
SELECT pgtrickle.create_stream_table(
    'sensor_hourly',
    $$
    SELECT
        sensor_id,
        date_trunc('hour', recorded_at) AS hour,
        AVG(temperature) AS avg_temp,
        AVG(humidity) AS avg_humidity,
        COUNT(*) AS reading_count,
        MIN(temperature) AS min_temp,
        MAX(temperature) AS max_temp
    FROM sensor_readings
    GROUP BY sensor_id, date_trunc('hour', recorded_at)
    $$
);
```

This stream table tracks every hourly bucket that has been touched. When 100 new readings arrive for sensor 42 in the 14:00 hour, the incremental refresh only recomputes the aggregate for that single bucket — not all 8,760 hourly buckets across the year. The cost is proportional to the number of distinct buckets affected, not the total data volume.

For a system ingesting 1,000 readings per second across 500 sensors, a 5-second refresh window touches approximately 500 buckets (one per sensor for the current hour). The refresh processes only those 5,000 new readings and updates 500 aggregate rows. Compare that to scanning 31 billion readings (one year of data) to rebuild from scratch.

---

## Daily Rollups From Hourly Data

Here's where the cascade becomes powerful. The daily rollup can be defined on top of the hourly rollup:

```sql
SELECT pgtrickle.create_stream_table(
    'sensor_daily',
    $$
    SELECT
        sensor_id,
        date_trunc('day', hour) AS day,
        AVG(avg_temp) AS avg_temp,
        MIN(min_temp) AS daily_min_temp,
        MAX(max_temp) AS daily_max_temp,
        SUM(reading_count) AS total_readings
    FROM sensor_hourly
    GROUP BY sensor_id, date_trunc('day', hour)
    $$
);
```

Because `sensor_hourly` is itself a stream table, pg_trickle understands the dependency chain. When raw readings are inserted, the hourly rollup is updated first, and then the daily rollup is updated from the hourly changes. This cascade happens automatically — the DAG scheduler ensures correct ordering.

The daily rollup never touches raw data. It only processes changes to the 24 hourly buckets that make up a day. Even for a backfill of a million historical readings, the daily rollup only processes the distinct hourly buckets those readings fall into.

---

## Handling Late Data

Late-arriving data is the nightmare scenario for traditional rollup systems. A sensor was offline for 6 hours and suddenly reports all its buffered readings with timestamps from the past. A data correction replays yesterday's data with fixed calibration values.

With pg_trickle, late data is not special. It's just an insert (or update) with a timestamp that happens to fall in a past bucket. The incremental engine processes it exactly like any other change: it identifies which hourly bucket is affected, computes the delta to the aggregate, and propagates that delta up the chain.

```sql
-- Late data arrives: sensor 42 reports readings from 6 hours ago
INSERT INTO sensor_readings (sensor_id, recorded_at, temperature, humidity)
VALUES
    (42, now() - interval '6 hours', 22.5, 55.0),
    (42, now() - interval '6 hours' + interval '5 seconds', 22.6, 54.8),
    -- ... hundreds more
;

-- Next refresh: only the affected hourly and daily buckets are updated
SELECT pgtrickle.refresh_stream_table('sensor_hourly');
-- sensor_daily is automatically refreshed via dependency chain
```

No special "backfill mode." No invalidation of downstream caches. No need to identify which buckets were affected and selectively rebuild them. The differential engine handles it.

---

## Monthly Summaries and Percentiles

For monthly reporting, you often need more than simple aggregates. Percentiles, distribution widths, and trend indicators require access to the underlying data distribution.

```sql
SELECT pgtrickle.create_stream_table(
    'sensor_monthly',
    $$
    SELECT
        sensor_id,
        date_trunc('month', day) AS month,
        AVG(avg_temp) AS monthly_avg_temp,
        MIN(daily_min_temp) AS monthly_min,
        MAX(daily_max_temp) AS monthly_max,
        SUM(total_readings) AS monthly_readings,
        MAX(daily_max_temp) - MIN(daily_min_temp) AS temp_range
    FROM sensor_daily
    GROUP BY sensor_id, date_trunc('month', day)
    $$
);
```

The three-level cascade — raw → hourly → daily → monthly — means that inserting a single reading at the bottom can propagate all the way to the monthly summary in one refresh cycle. The total cost is three incremental updates (one per level), each touching a single group. Compare that to scanning the raw table three times with different `date_trunc` granularities.

---

## Comparison With TimescaleDB Continuous Aggregates

| Feature | TimescaleDB | pg_trickle |
|---------|-------------|------------|
| Requires hypertables | Yes | No (any table) |
| Refresh granularity | Time bucket window | Per-changed-row differential |
| Cascading rollups | Manual (materialized on top of materialized) | Automatic DAG scheduling |
| Late data handling | Re-materializes entire bucket | Incremental delta on affected bucket |
| Works with JOINs | Limited (single hypertable) | Full SQL including multi-table joins |
| Extension dependency | timescaledb | pg_trickle |

The key difference is granularity. TimescaleDB refreshes an entire time bucket when any row in that bucket changes. If your hourly bucket has 10,000 rows and one arrives late, all 10,000 are re-read. pg_trickle computes the delta from the single new row and adjusts the aggregate arithmetically. For high-cardinality time series (many sensors, many metrics), this difference is substantial.

---

## Real-World Sizing

For a production IoT deployment with:
- 10,000 sensors
- 1 reading per sensor per 10 seconds
- 1,000 readings/second sustained

The data volumes are:
- **Raw table:** ~2.6 billion rows/month
- **Hourly rollup:** 7.2M rows/month (10,000 sensors × 720 hours)
- **Daily rollup:** 300K rows/month (10,000 sensors × 30 days)
- **Monthly rollup:** 10,000 rows/month

With pg_trickle maintaining all three rollup levels, the refresh cost per second is approximately:
- Process 1,000 new raw readings
- Update ~1,000 hourly buckets (one per sensor for the current hour, but only those that received new data)
- Update ~100 daily buckets (sensors whose hourly aggregate actually changed meaningfully)
- Monthly rollup: updated once daily via scheduled refresh

Total CPU cost: a few milliseconds per second of ingested data. The dashboards are never more than a few seconds stale, and the database never performs a full scan of billions of rows.

---

## Getting Started

```sql
-- Just create your regular table (no hypertable conversion needed)
CREATE TABLE metrics (
    device_id  integer,
    ts         timestamptz DEFAULT now(),
    value      double precision
);

-- Define your rollups as stream tables
SELECT pgtrickle.create_stream_table(
    'metrics_hourly',
    $$
    SELECT device_id,
           date_trunc('hour', ts) AS hour,
           AVG(value) AS avg_val,
           COUNT(*) AS samples
    FROM metrics
    GROUP BY device_id, date_trunc('hour', ts)
    $$
);

-- Insert data normally
INSERT INTO metrics (device_id, value)
SELECT (random() * 100)::int, random() * 50
FROM generate_series(1, 10000);

-- Refresh — only new data is processed
SELECT pgtrickle.refresh_stream_table('metrics_hourly');
```

Your time-series rollups are live. No hypertable conversion, no extension-specific table types, no refresh policies to configure. Just SQL.

---

*Stop rebuilding rollups from scratch. Let the differential engine propagate only what changed.*
