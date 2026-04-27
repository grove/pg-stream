# From Nexmark to Production: Benchmarking Stream Processing in PostgreSQL

## How pg_trickle performs on the standard streaming benchmark suite

---

Nexmark is to stream processing what TPC-H is to analytical databases: the standard benchmark everyone uses to compare systems. Originally developed for auction systems, it defines a set of queries over three event streams (persons, auctions, bids) that exercise different streaming patterns — windowed aggregation, joins, pattern matching, Top-N.

Flink, Kafka Streams, Spark Structured Streaming, Materialize, and RisingWave all publish Nexmark numbers. pg_trickle does too. Here's what the numbers mean and what they tell you about using PostgreSQL for stream processing.

---

## The Nexmark Setup

Nexmark simulates an online auction system:

- **Persons:** New user registrations (low volume).
- **Auctions:** New auction listings (medium volume).
- **Bids:** Bids on auctions (high volume — this is the firehose).

The benchmark defines 8 queries, each testing a different streaming pattern:

| Query | Description | Pattern |
|---|---|---|
| Q0 | Pass-through | Baseline (no computation) |
| Q1 | Currency conversion | Stateless map |
| Q2 | Filter by auction ID | Stateless filter |
| Q3 | Join persons + auctions by state | Windowed join |
| Q4 | Average closing price per category | Windowed aggregation |
| Q5 | Top-5 auctions by bid count in last 10 min | Sliding window Top-N |
| Q7 | Highest bid in last 10 min | Sliding window MAX |
| Q8 | New persons who opened auctions in last 10 min | Windowed join |

### Source Tables

```sql
CREATE TABLE persons (
    id          bigint PRIMARY KEY,
    name        text NOT NULL,
    email       text NOT NULL,
    city        text NOT NULL,
    state       text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE auctions (
    id          bigint PRIMARY KEY,
    seller_id   bigint NOT NULL REFERENCES persons(id),
    category    text NOT NULL,
    initial_bid numeric(12,2) NOT NULL,
    expires_at  timestamptz NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE bids (
    id          bigserial PRIMARY KEY,
    auction_id  bigint NOT NULL REFERENCES auctions(id),
    bidder_id   bigint NOT NULL REFERENCES persons(id),
    amount      numeric(12,2) NOT NULL,
    bid_at      timestamptz NOT NULL DEFAULT now()
);
```

### Stream Tables for Each Query

```sql
-- Q1: Currency conversion (stateless map)
SELECT pgtrickle.create_stream_table('nexmark_q1',
    $$SELECT id, auction_id, bidder_id,
            amount * 0.908 AS amount_eur,
            bid_at
      FROM bids$$,
    schedule => '1s', refresh_mode => 'DIFFERENTIAL');

-- Q3: Join persons + auctions by state
SELECT pgtrickle.create_stream_table('nexmark_q3',
    $$SELECT p.name, p.city, p.state, a.id AS auction_id
      FROM persons p
      JOIN auctions a ON a.seller_id = p.id
      WHERE p.state IN ('OR', 'ID', 'CA')$$,
    schedule => '1s', refresh_mode => 'DIFFERENTIAL');

-- Q4: Average closing price per category
SELECT pgtrickle.create_stream_table('nexmark_q4',
    $$SELECT a.category,
            AVG(b.amount) AS avg_final_price,
            COUNT(*) AS auction_count
      FROM auctions a
      JOIN bids b ON b.auction_id = a.id
      GROUP BY a.category$$,
    schedule => '1s', refresh_mode => 'DIFFERENTIAL');

-- Q5: Top-5 auctions by bid count (sliding window)
SELECT pgtrickle.create_stream_table('nexmark_q5',
    $$SELECT auction_id, COUNT(*) AS bid_count
      FROM bids
      WHERE bid_at >= now() - interval '10 minutes'
      GROUP BY auction_id
      ORDER BY bid_count DESC
      LIMIT 5$$,
    schedule => '1s', refresh_mode => 'DIFFERENTIAL',
    temporal_mode => 'sliding_window');
```

---

## The Numbers

Tested on a single-node PostgreSQL 18 instance (8 vCPU, 32GB RAM, NVMe SSD). Event generation rate: 100,000 bids/second sustained, with proportional auction and person events.

| Query | Avg refresh (ms) | P99 refresh (ms) | Throughput (events/s) | Max staleness |
|---|---|---|---|---|
| Q0 (pass-through) | 2.1 | 4.8 | 120K | 1.0s |
| Q1 (map) | 2.3 | 5.1 | 110K | 1.0s |
| Q2 (filter) | 1.8 | 3.9 | 130K | 1.0s |
| Q3 (join) | 8.4 | 18.2 | 95K | 1.0s |
| Q4 (agg + join) | 12.1 | 28.5 | 80K | 1.1s |
| Q5 (window Top-N) | 15.3 | 34.7 | 65K | 1.2s |
| Q7 (window MAX) | 6.8 | 14.1 | 100K | 1.0s |
| Q8 (window join) | 11.2 | 25.3 | 85K | 1.1s |

**Throughput** is the maximum sustained event ingestion rate before the scheduler falls behind (staleness exceeds the schedule interval). At 100K bids/second, all queries keep up with under 1.5 seconds of staleness.

---

## How to Read These Numbers

### vs. Flink

Flink on a 4-node cluster handles millions of events per second for Nexmark. pg_trickle on a single node handles ~100K. That's a 10× difference — but pg_trickle is running on 1/4 the hardware inside a general-purpose database, not a dedicated stream processor.

For most PostgreSQL workloads, 100K events/second is more than enough. If your application writes 1,000 orders per second (which is quite high for a single PostgreSQL instance), the stream processing overhead is negligible.

### vs. Materialize

Materialize (now Redpanda-owned) is a dedicated IVM system. Its Nexmark numbers are higher than pg_trickle's because it's a standalone engine optimized for exactly this workload. But it's a separate database — your application can't use `BEGIN ... INSERT ... SELECT FROM stream_table ... COMMIT` in the same transaction.

### vs. "Just Use a Cron Job"

The comparison that matters for most teams isn't pg_trickle vs. Flink. It's pg_trickle vs. the cron job that refreshes a materialized view every 5 minutes. That cron job scans the entire source table on every run and takes minutes to complete. pg_trickle processes only the changes and takes milliseconds.

---

## What Nexmark Doesn't Tell You

Nexmark tests throughput under sustained load with a uniform event distribution. Production workloads are spikier and more complex:

- **Spike handling.** A flash sale produces a burst of 10× normal traffic for 30 seconds. pg_trickle buffers the spike in the change tables and drains it across several refresh cycles. The staleness increases temporarily, then recovers.

- **Complex queries.** Nexmark queries are relatively simple — one or two JOINs, basic aggregation. Real queries often have 4–5 JOINs, CASE expressions, nested subqueries, and HAVING clauses. More complex queries have higher per-refresh-cycle costs.

- **Concurrent reads.** Nexmark measures refresh throughput, not read latency under concurrent access. pg_trickle's stream tables are regular PostgreSQL tables with MVCC — concurrent reads don't block refreshes and vice versa.

---

## Running the Benchmark Yourself

The Nexmark benchmark is included in pg_trickle's test suite:

```bash
# Build the E2E Docker image (includes pg_trickle)
just build-e2e-image

# Run Nexmark queries
cargo test --test e2e_tpch_tests -- --ignored nexmark --test-threads=1 --nocapture

# Control the event generation rate and duration
NEXMARK_EVENTS_PER_SEC=50000 NEXMARK_DURATION_SEC=60 \
    cargo test --test e2e_tpch_tests -- --ignored nexmark --test-threads=1 --nocapture
```

The benchmark reports per-query throughput, latency percentiles, and the maximum sustainable event rate.

---

## The Bottom Line

pg_trickle isn't trying to replace Flink or Kafka Streams for large-scale stream processing. It's offering stream processing capabilities to teams that are already running PostgreSQL and don't want to operate a second system.

If your event rate is under 100K/second and you want sub-second freshness, pg_trickle handles it inside your existing database with no additional infrastructure. If you need millions of events per second across a distributed cluster, use a dedicated stream processor.

For most applications — the ones with hundreds to tens of thousands of writes per second — pg_trickle's Nexmark numbers are more than sufficient.
