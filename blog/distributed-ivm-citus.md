# Distributed IVM with Citus

## Incremental view maintenance across sharded PostgreSQL

---

Citus distributes PostgreSQL tables across multiple worker nodes. You get horizontal write scaling, parallel query execution, and the ability to store more data than fits on a single machine.

What you lose is the ability to maintain derived data easily. `CREATE MATERIALIZED VIEW` doesn't work across distributed tables in any incremental way. `REFRESH MATERIALIZED VIEW` on a Citus coordinator scans every shard, pulls the data to the coordinator, computes the result, and writes it back. At scale, this is slow and resource-intensive.

pg_trickle's Citus integration (v0.32–v0.34) solves this. It maintains stream tables across a Citus cluster with shard-aware CDC, distributed delta routing, and automatic recovery after shard rebalances.

---

## How It Works

### CDC on Workers

In a standard (non-Citus) setup, pg_trickle attaches triggers to source tables on a single PostgreSQL instance. In a Citus setup, the source tables are distributed — each worker node holds a subset of the shards.

pg_trickle installs CDC triggers on every worker that hosts a shard of the source table. Each worker captures changes into a local change buffer. The coordinator's scheduler polls these change buffers via `dblink` connections to each worker.

```
Worker 1 (shards 1-4):      Worker 2 (shards 5-8):
  orders_1 → changes_1        orders_5 → changes_5
  orders_2 → changes_2        orders_6 → changes_6
  orders_3 → changes_3        orders_7 → changes_7
  orders_4 → changes_4        orders_8 → changes_8
       ↓                            ↓
       └──────── coordinator ───────┘
                      ↓
              delta computation
                      ↓
              stream table (on coordinator or distributed)
```

### Delta Computation

The coordinator merges change buffers from all workers, computes the delta using the standard DVM engine, and applies it to the stream table. The stream table itself can be:

- **Reference table:** Replicated to all nodes. Good for small lookup tables and aggregates.
- **Distributed table:** Sharded across workers. Good for large stream tables that need to be co-located with queries.

```sql
-- Create a distributed source table
SELECT create_distributed_table('orders', 'customer_id');
SELECT create_distributed_table('customers', 'id');

-- Create a stream table over distributed sources
SELECT pgtrickle.create_stream_table(
    'revenue_by_region',
    $$SELECT c.region, SUM(o.amount) AS revenue, COUNT(*) AS order_count
      FROM orders o
      JOIN customers c ON c.id = o.customer_id
      GROUP BY c.region$$,
    schedule     => '3s',
    refresh_mode => 'DIFFERENTIAL'
);
```

pg_trickle detects that the source tables are Citus-distributed and automatically sets up the per-worker CDC infrastructure.

---

## Shard-Aware Delta Routing

The key optimization: not all shards contribute to every delta.

If an order is inserted on Worker 1, only the change buffers on Worker 1 contain new data. pg_trickle's scheduler knows this — it checks each worker's change buffer depth before polling. Workers with no changes are skipped entirely.

For a 16-worker cluster where only 3 workers have new data since the last refresh, the coordinator only polls 3 workers instead of 16. This reduces network overhead and coordinator CPU time proportionally.

---

## Co-Located Joins

Citus is fastest when JOINs are co-located — when the joined tables are distributed on the same column, so the JOIN can execute locally on each worker without shuffling data.

pg_trickle respects this. If `orders` and `customers` are both distributed on `customer_id` (or `id` for customers, co-located via a distribution column that matches), the delta computation pushes down to the workers:

```
Worker 1:
  local_orders_change + local_customers → local_delta
Worker 2:
  local_orders_change + local_customers → local_delta
...
Coordinator:
  merge(local_delta_1, local_delta_2, ...) → stream table MERGE
```

The expensive part — the JOIN — happens on the workers in parallel. The coordinator only needs to merge the per-worker deltas, which is typically a small aggregation.

---

## Handling Shard Rebalances

When you add a node to a Citus cluster or rebalance shards, data moves between workers. This invalidates the change buffers on the old and new worker for the affected shards.

pg_trickle detects shard rebalances by monitoring `pg_dist_shard_placement`. When a shard moves:

1. The scheduler pauses refresh for affected stream tables.
2. CDC triggers are installed on the new shard placement.
3. Change buffers for the moved shard are reset.
4. A targeted full refresh is run for the affected groups.
5. Normal differential refresh resumes.

This happens automatically. From the application's perspective, the stream table might be slightly stale during the rebalance (the refresh is paused), but it never returns incorrect data.

---

## Multi-Tenant Analytics

Citus's most common pattern is multi-tenant: each tenant's data is on the same shard, co-located by `tenant_id`. Stream tables work naturally with this pattern:

```sql
-- Distributed by tenant_id
SELECT create_distributed_table('events', 'tenant_id');

-- Per-tenant aggregation
SELECT pgtrickle.create_stream_table(
    'tenant_event_summary',
    $$SELECT
        tenant_id,
        event_type,
        COUNT(*) AS event_count,
        MAX(created_at) AS last_event
      FROM events
      WHERE created_at >= now() - interval '7 days'
      GROUP BY tenant_id, event_type$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);

-- Distribute the stream table on the same column
SELECT create_distributed_table('tenant_event_summary', 'tenant_id');
```

Each tenant's events are on a single worker. The CDC and delta computation for that tenant happen entirely on that worker. The coordinator only handles the final MERGE. This scales linearly with the number of workers.

---

## Cross-Shard Aggregation

Not all queries are shard-local. Global aggregates (total revenue across all tenants, system-wide event counts) require data from every worker:

```sql
-- Global aggregate across all tenants
SELECT pgtrickle.create_stream_table(
    'global_event_counts',
    $$SELECT event_type, COUNT(*) AS total_events
      FROM events
      GROUP BY event_type$$,
    schedule => '5s', refresh_mode => 'DIFFERENTIAL'
);
```

For cross-shard aggregates, each worker computes a partial delta locally, and the coordinator combines them. This is more expensive than shard-local queries (it requires data transfer from every worker), but the differential approach means only the changed groups are transferred — not the entire dataset.

---

## Limitations

- **Maximum workers:** pg_trickle has been tested with up to 32 Citus workers. Beyond that, the coordinator's `dblink` polling becomes the bottleneck. For larger clusters, consider increasing the polling interval.

- **Non-co-located JOINs:** If the JOIN requires shuffling data between workers (non-co-located distribution columns), the delta computation falls back to the coordinator. This is slower but still correct.

- **IMMEDIATE mode:** Not supported on Citus-distributed stream tables. The trigger would need to perform cross-shard operations within the source transaction, which Citus doesn't support transactionally. Use DIFFERENTIAL mode.

- **Schema changes on distributed tables:** `ALTER STREAM TABLE ... QUERY` works, but the full refresh during schema migration reads from all workers sequentially. Plan for a longer migration window on large clusters.

---

## When to Use Citus vs. Single-Node

If your data fits on a single PostgreSQL instance (up to a few TB with good hardware), you don't need Citus. pg_trickle on a single node is simpler, faster, and has no cross-node coordination overhead.

Use Citus + pg_trickle when:
- Your source data exceeds single-node capacity
- You need horizontal write scaling (more workers = more write throughput)
- Your tenants have strict data isolation requirements (each tenant on a dedicated shard)
- Your analytics queries benefit from parallel execution across workers
