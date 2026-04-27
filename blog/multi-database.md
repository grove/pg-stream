[← Back to Blog Index](README.md)

# One PostgreSQL, Five Databases, One Worker Pool

## Multi-database pg_trickle: per-database isolation with shared worker scheduling

---

You're running a SaaS product. Each customer has their own PostgreSQL database on a shared server. You've installed pg_trickle in all of them. Now you have 5 databases, each with its own stream tables, its own scheduler, and its own demand for refresh workers.

How does pg_trickle coordinate across databases without them stepping on each other?

The answer is a two-level architecture: **one launcher per server, one scheduler per database, one shared worker pool.**

---

## The Launcher

When PostgreSQL starts, pg_trickle registers a single background worker called the **launcher**. The launcher's job is discovery:

1. Connect to each database that has `pg_trickle` installed.
2. Start a scheduler process for each database.
3. Monitor the schedulers — restart them if they crash.

The launcher uses `pg_database` to enumerate databases and checks for the `pgtrickle` schema. Databases without pg_trickle are ignored.

```
PostgreSQL server
├── pg_trickle launcher (1 per server)
│   ├── scheduler: customer_a_db
│   ├── scheduler: customer_b_db
│   ├── scheduler: customer_c_db
│   ├── scheduler: internal_analytics_db
│   └── scheduler: staging_db
└── shared worker pool (N workers)
```

---

## Per-Database Isolation

Each database gets its own scheduler process. Schedulers are fully isolated:

- Each scheduler only sees stream tables in its own database.
- Each scheduler maintains its own DAG, its own tier classification, its own refresh history.
- A crash in one scheduler doesn't affect others. The launcher restarts it.
- Error states are per-database — a suspended stream table in `customer_a_db` doesn't affect `customer_b_db`.

This isolation is critical for multi-tenant deployments. Tenant A's misconfigured stream table (running in a loop, consuming resources) can't degrade Tenant B's refreshes.

---

## The Shared Worker Pool

Refresh workers are a shared resource. pg_trickle maintains a pool of `max_dynamic_refresh_workers` (default: 4) workers that are dispatched across databases.

When a scheduler determines that a stream table needs refreshing, it requests a worker from the shared pool. The worker connects to the scheduler's database, executes the refresh, and returns to the pool.

```sql
-- See current worker allocation
SHOW pg_trickle.max_dynamic_refresh_workers;
-- 4
```

---

## Fair-Share Scheduling

Without any controls, one busy database could monopolize the worker pool. If `customer_a_db` has 50 due refreshes and `customer_b_db` has 5, the pool would spend 90% of its time on Customer A.

pg_trickle prevents this with **per-database worker quotas**:

```sql
-- Each database gets at most 2 workers concurrently
SET pg_trickle.per_database_worker_quota = 2;
```

With 4 workers and a quota of 2 per database, at least 2 databases can refresh concurrently. No single database can starve the others.

The quota is a ceiling, not a reservation. If only one database has pending work, it can use all 4 workers. The quota only kicks in when there's contention.

---

## Configuration

Most pg_trickle GUCs are per-database (set in each database independently):

```sql
-- In customer_a_db:
SET pg_trickle.scheduler_interval_ms = 500;   -- fast scheduler
SET pg_trickle.max_concurrent_refreshes = 3;   -- up to 3 concurrent refreshes

-- In staging_db:
SET pg_trickle.scheduler_interval_ms = 5000;  -- slower scheduler
SET pg_trickle.max_concurrent_refreshes = 1;   -- sequential refreshes
```

Server-wide settings (in `postgresql.conf`) apply to all databases:

```
pg_trickle.max_dynamic_refresh_workers = 8      # Total pool size
pg_trickle.per_database_worker_quota = 3         # Per-DB ceiling
pg_trickle.enabled = on                          # Global switch
```

If `pg_trickle.enabled = off` in `postgresql.conf`, no database runs any refreshes. Individual databases can't override this.

---

## The Database-Per-Tenant Pattern

For SaaS products using database-per-tenant isolation:

```
tenant_1_db: 5 stream tables (real-time dashboard)
tenant_2_db: 12 stream tables (analytics pipeline)
tenant_3_db: 3 stream tables (inventory tracking)
...
tenant_50_db: 8 stream tables
```

Each tenant's stream tables are independent. The launcher discovers all 50 databases and starts 50 schedulers. The shared worker pool (say, 16 workers) services all of them with fair-share quotas.

**Scaling:**
- Add a new tenant → create database, install pg_trickle, create stream tables. The launcher discovers it automatically on the next discovery cycle.
- Remove a tenant → drop the database. The launcher detects the missing database and stops its scheduler.
- Tenant needs more throughput → increase their `max_concurrent_refreshes` within the database. They'll get more workers (up to their quota) when the pool has capacity.

---

## Monitoring Across Databases

Each database reports its own health:

```sql
-- In each database
SELECT * FROM pgtrickle.health_summary();
```

For a cross-database view, query each database from a monitoring system:

```bash
for db in customer_a_db customer_b_db customer_c_db; do
  psql -d $db -c "SELECT '$db' AS database, * FROM pgtrickle.health_summary();"
done
```

Or use Prometheus metrics (exposed per-database) and aggregate in Grafana.

---

## Failure Containment

The isolation model means failures are contained:

| Failure | Impact |
|---------|--------|
| Scheduler crash in `tenant_1_db` | Only `tenant_1_db` stops refreshing. Launcher restarts it. |
| Runaway query in `tenant_2_db` | Uses one worker. Other databases use remaining workers. |
| Database dropped | Launcher detects and stops the scheduler. No impact on others. |
| pg_trickle disabled in one DB | Only that DB stops. Others continue. |
| Shared worker pool exhausted | All databases queue refreshes. Fair-share ensures no single DB monopolizes. |

The worst case — worker pool exhaustion — affects all databases equally. This is by design: when the system is overloaded, everyone slows down proportionally. No single tenant can cause another tenant's stream tables to stop refreshing entirely.

---

## Sizing the Worker Pool

**Rule of thumb:** `max_dynamic_refresh_workers` should be at least equal to the number of databases with "hot" stream tables (tables that change every cycle).

For a server with:
- 10 databases
- 3 databases with real-time dashboards (hot)
- 7 databases with hourly reporting (cold)

Set `max_dynamic_refresh_workers = 6` (2× the hot count). The hot databases get immediate workers; cold databases share the remaining capacity.

For CPU-bound refreshes (complex aggregates, many joins), each worker consumes one PostgreSQL backend. Size the worker pool to leave headroom for user connections:

```
total_connections = max_connections - max_dynamic_refresh_workers - launcher - schedulers
```

---

## Summary

pg_trickle's multi-database architecture uses one launcher per server to discover databases, one scheduler per database for isolation, and one shared worker pool for resource efficiency.

Per-database quotas prevent monopolization. Failure is contained per-database. New databases are discovered automatically.

For SaaS products with database-per-tenant isolation, this is the architecture that lets pg_trickle scale horizontally without per-tenant infrastructure overhead. One PostgreSQL server, one extension, fair-share scheduling.
