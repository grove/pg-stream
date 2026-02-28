# PLAN_MULTI_DATABASE.md — Multi-Database Support

> **Status:** Draft  
> **Target version:** Post-1.0 (Scale S3)  
> **Author:** pg_trickle project

---

## 1. Current Constraint

pg_trickle is locked to operating inside a **single PostgreSQL database** (the
one where `CREATE EXTENSION pg_trickle` was run). This is a fundamental
architectural constraint, not a missing feature.

Root causes:

| Constraint | Why it matters |
|-----------|---------------|
| `Spi::connect()` executes in the extension's current database | Cross-database SPI is impossible in PostgreSQL |
| Background worker registers at `_PG_init()` time | One BGW per `_PG_init` call — each database that loads `pg_trickle` gets its own BGW slot |
| `pg_shmem_init!()` allocates fixed shared memory | The slot count is fixed at server start; must accommodate N databases |
| Trigger catalog (`pg_trigger`) | Triggers target tables in the current database only |
| Change buffer schema (`pgtrickle_changes`) | Lives in the extension's database |

The multi-database constraint is therefore not a single code change but a set
of **orthogonal problems** that interact.

---

## 2. Option Analysis

### Option A: Per-database background worker (current approach, scaled)

**How it works today:** Each database that installs pg_trickle gets one BGW slot
when the server loads `shared_preload_libraries = 'pg_trickle'`. The BGW
connects to its own database.

**Extension:** This already gives multi-database isolation for free — each
database is independent. The constraint is that `max_worker_processes` limits
total BGW count across the whole server.

**Cost:** Low — no code change needed for basic multi-database use.  
**Limitation:** Each database consumes one BGW slot. 10 databases = 10 slots.

**Verdict: This is the correct first approach.** Document it and test it.

### Option B: Cross-database dblink polling

A single "coordinator" BGW polls other databases via `dblink`. Streaming tables
in "secondary" databases register with the coordinator.

**Cost:** Very high — requires dblink connectivity, cross-DB transaction
coordination, and a coordinator catalog.  
**Limitation:** dblink adds latency and complexity; foreign transactions are
not atomic with local triggers.

**Verdict: Not recommended.**

### Option C: External orchestrator (see REPORT_PARALLELIZATION.md §D)

A process outside PostgreSQL manages refresh scheduling across databases,
calling the refresh functions via libpq.

**Cost:** High — requires the external process, service discovery, and
connection management.  
**Verdict: Viable for 100+ databases at scale; overkill for typical use.**

### Option D: PostgreSQL logical replication + Subscriber

Stream changes from source databases to a central "hub" database using logical
replication. pg_trickle operates only in the hub.

**Cost:** High — additional replication slots, latency, schema synchronization.  
**Verdict: Viable as a separate architectural pattern, not an extension feature.**

---

## 3. Schema Isolation Requirements

Each database that installs pg_trickle has:

```
<database>/
  pgtrickle.pgt_stream_tables        -- catalog
  pgtrickle_changes.changes_<oid>    -- change buffers
  pgtrickle.<function>               -- SQL API
```

No cross-database state sharing is needed or desired. The per-database BGW
model (Option A) already satisfies this.

---

## 4. Shared Memory Considerations

`pg_shmem_init!()` allocates a fixed `PgLwLock` and `PgAtomic` block at
server start. If multiple databases load pg_trickle, each gets its own shmem
block because each `_PG_init()` call is isolated per-database. Confirm this
with a two-database smoke test.

---

## 5. Recommended Implementation Phases

### Phase 1 (v0.3.0): Test and document multi-database

- Add a multi-database E2E test: two databases on the same PG server, each
  with `pg_trickle` installed, verify independent BGWs.
- Document memory and BGW slot consumption per database.
- Expose `pg_trickle.enabled` per-database to allow disabling in low-priority DBs.

### Phase 2 (Post-1.0): BGW pool sizing guidance

- Add a GUC `pg_trickle.max_databases` (informational; actual limit is
  `max_worker_processes`).
- Add a monitoring view `pgtrickle.server_worker_inventory` showing all
  registered BGWs and their databases.

### Phase 3 (Post-1.0): External orchestrator integration

- Document the `pgtrickle.manual_refresh(stream_table_name)` pattern for
  external callers.
- See [REPORT_PARALLELIZATION.md](../performance/REPORT_PARALLELIZATION.md) §D.

---

## 6. Interaction with CNPG

CNPG clusters can host multiple PostgreSQL databases. The per-database BGW
model works transparently — each DB that has `pg_trickle` installed will have
an active BGW. The CNPG `cluster-example.yaml` should document the
`max_worker_processes` guidance.

See [CITUS.md](CITUS.md) for Citus (distributed PG) considerations, which
has similar but more complex multi-node constraints.

---

## References

- [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md) — BGW section
- [src/scheduler.rs](../../src/scheduler.rs)
- [plans/performance/REPORT_PARALLELIZATION.md](../performance/REPORT_PARALLELIZATION.md)
- [plans/infra/CITUS.md](CITUS.md)
- [cnpg/cluster-example.yaml](../../cnpg/cluster-example.yaml)
