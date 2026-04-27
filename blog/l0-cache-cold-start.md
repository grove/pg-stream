[← Back to Blog Index](README.md)

# The 45ms Cold-Start Tax and How L0 Cache Eliminates It

## Why connection poolers pay a hidden penalty — and the process-local cache that fixes it

---

You're using PgBouncer in transaction mode. Everything is fast. Then you look at the p99 refresh latency and notice occasional 50ms spikes on stream tables that normally refresh in 5ms.

The spikes aren't random. They happen when a refresh runs on a PostgreSQL backend that hasn't seen that stream table before. The backend needs to parse the delta query template, prepare execution plans, and load metadata. This cold-start overhead is ~45ms — invisible in a dedicated-connection world, but it shows up when connection poolers recycle backends across different workloads.

pg_trickle's L0 cache (introduced in v0.36.0) eliminates this. It's a process-local, in-memory cache that stores parsed templates and metadata. When a backend is reused, the template is already there. Cold start becomes warm start.

---

## The Cold-Start Problem

Each PostgreSQL backend is an independent process. When a backend first executes a stream table refresh, it needs to:

1. **Read the catalog entry:** Query `pgtrickle.pgt_stream_tables` for the defining query, schedule, refresh mode, and dependencies.
2. **Parse the delta query template:** Convert the defining query into a delta query with change buffer joins, aggregate adjustments, and MERGE logic.
3. **Prepare the execution plan:** PostgreSQL's planner creates a query plan for the delta query.
4. **Load dependency metadata:** Frontier positions, change tracking state, CDC mode for each source table.

Steps 1–4 take ~45ms on a typical workload. After the first refresh, PostgreSQL caches the prepared statement (step 3), so subsequent refreshes on the same backend skip the planning. But steps 1 and 2 — the catalog read and template parse — happen every time a new backend handles the stream table.

With a dedicated connection per backend (the traditional model), this 45ms penalty happens once: at first refresh after startup. With PgBouncer in transaction mode, it happens every time the refresh is assigned to a backend that hasn't seen it before — which can be every cycle if the pool is large enough.

---

## The L0 Cache

The L0 cache is a per-backend, in-memory hash map that stores:

- **Parsed delta query templates** keyed by `(pgt_id, cache_generation)`.
- **Catalog metadata snapshots** (schedule, refresh mode, dependency list).
- **Pre-computed MERGE SQL** for each stream table.

```
L0 Cache: RwLock<HashMap<(pgt_id, cache_generation), CachedTemplate>>
```

When a backend needs to refresh a stream table:

1. Check the L0 cache for the template.
2. If found and the `cache_generation` matches the current catalog generation → use the cached template. Skip parsing and catalog reads.
3. If not found or generation mismatch → parse the template, cache it, proceed.

**Cache generation** is a monotonically increasing counter that bumps whenever a stream table's definition changes (ALTER, DROP, CREATE). This ensures stale templates are invalidated without explicit cache eviction.

---

## Performance Impact

Benchmarks on a 4-core PostgreSQL 18 instance with PgBouncer (transaction mode, 20-connection pool):

| Metric | Without L0 Cache | With L0 Cache |
|--------|-----------------|---------------|
| First refresh on new backend | 47ms | 47ms (cache miss) |
| Subsequent refresh, same backend | 5ms | 5ms (no change) |
| Refresh after backend recycled | 47ms | 5ms (cache hit) |
| p50 latency (steady state) | 5ms | 5ms |
| p99 latency (steady state) | 48ms | 6ms |

The p99 improvement is dramatic. Without L0 cache, the p99 is dominated by cold-start events (backends seeing the stream table for the first time after recycling). With L0 cache, the parsed template survives backend recycling.

---

## How It Survives Backend Recycling

"Wait — if PgBouncer recycles backends, doesn't the in-memory cache get destroyed?"

No. PgBouncer doesn't destroy backends. It reassigns them. The PostgreSQL process continues running; it just handles a different client connection. The L0 cache lives in the process's memory, which persists across connection reassignments.

The cache is invalidated only when:
- The backend process exits (rare in normal operation).
- The `cache_generation` changes (stream table was altered).
- The cache reaches `template_cache_max_entries` and evicts the least-recently-used entry.
- The cache entry exceeds `template_cache_max_age_hours` and is considered stale.

---

## Configuration

The L0 cache is enabled by default since v0.36.0:

```sql
-- Cache size (entries)
SHOW pg_trickle.template_cache_max_entries;
-- 128

-- Maximum age before eviction
SHOW pg_trickle.template_cache_max_age_hours;
-- 24
```

**Sizing:** Each cached template uses ~1–5KB of memory, depending on query complexity. With `max_entries = 128`, the cache uses at most ~640KB per backend. For a 100-connection pool, that's ~64MB total — negligible on any modern server.

**For large deployments (500+ stream tables):** Increase `max_entries` to avoid eviction:

```sql
SET pg_trickle.template_cache_max_entries = 1024;
```

---

## Monitoring Cache Effectiveness

```sql
SELECT * FROM pgtrickle.cache_stats();
```

```
 backend_pid | entries | hits    | misses | hit_rate | oldest_entry_age
-------------+---------+---------+--------+----------+-------------------
 12345       | 15      | 4,521   | 17     | 99.6%    | 2h 14m
 12346       | 23      | 8,302   | 25     | 99.7%    | 4h 01m
 12347       | 8       | 1,203   | 9      | 99.3%    | 0h 45m
```

**Target:** `hit_rate > 99%`. If it's lower, the cache is too small (entries being evicted) or stream tables are being altered frequently (generation bumps invalidate entries).

**Diagnosis:**
- `hit_rate < 95%` with high `misses` → increase `max_entries`.
- `hit_rate < 95%` with low `entries` → stream tables are being altered frequently. This is expected during development; in production it should stabilize.

---

## Without PgBouncer: Does L0 Cache Still Help?

Yes, but less dramatically.

Without a connection pooler, each backend is dedicated to one connection. The cold-start happens once (at first refresh) and never again. The L0 cache just makes that first refresh slightly faster by caching across `pg_trickle.enabled` toggles or after `ALTER EXTENSION UPDATE`.

The big win is with connection poolers: PgBouncer, pgcat, Supavisor, odyssey. Any pooler that reassigns backends between clients will see the p99 improvement.

---

## The Broader Picture: Caching Layers

pg_trickle has multiple caching layers:

| Layer | Scope | What's Cached | Lifetime |
|-------|-------|---------------|----------|
| **L0** | Per-backend (process-local) | Parsed templates, catalog metadata | Until backend exit, generation bump, or LRU eviction |
| **PostgreSQL plan cache** | Per-backend | Query execution plans | Until backend exit or invalidation |
| **Shared buffers** | Shared across backends | Table/index pages | LRU eviction |
| **OS page cache** | System-wide | Disk blocks | LRU eviction |

L0 sits above the PostgreSQL plan cache. Even if the plan cache has the execution plan, the template parsing (steps 1–2) is L0's domain. Both layers contribute to refresh performance; L0 handles the pg_trickle-specific overhead that the plan cache doesn't cover.

---

## Summary

The L0 process-local template cache eliminates the ~45ms cold-start penalty that connection-pooler workloads pay when a backend handles a stream table for the first time.

It's a `RwLock<HashMap>` keyed by `(pgt_id, cache_generation)`. Hits are <1ms. Misses pay the full parse cost once and cache the result. Generation tracking ensures stale entries are invalidated without explicit eviction.

If you're running PgBouncer in transaction mode, the L0 cache is the difference between a 5ms p99 and a 48ms p99. It's enabled by default. Check `cache_stats()` to verify it's working.
