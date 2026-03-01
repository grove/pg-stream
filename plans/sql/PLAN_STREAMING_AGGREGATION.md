# PLAN_STREAMING_AGGREGATION.md — Sub-Second Latency Path

> **Status:** Draft  
> **Target version:** Post-1.0 (Advanced SQL A2)  
> **Author:** pg_trickle project

---

## 1. Current Refresh Latency

The current refresh model is **poll-based**:

```
Source table INSERT/UPDATE/DELETE
  └─► CDC trigger writes to pgtrickle_changes.changes_<oid>
        └─► Background worker wakes on tick interval (default: 1s)
              └─► Full differential refresh executed via delta SQL
```

Minimum observable latency ≈ scheduler tick interval (configurable, default
`pg_trickle.scheduler_interval_ms = 1000`). Typical p99 ≈ tick × 2
(change arrives just after a tick fires).

For most analytical workloads, 1–2 second latency is acceptable. For
operational dashboards or CDC-downstream consumers, sub-second latency is
desirable without paying full differential refresh cost on every row change.

---

## 2. Approach: Incremental In-Trigger Accumulation

Instead of (or in addition to) the background worker refresh cycle, accumulate
aggregate state **synchronously inside the AFTER trigger** for supported
aggregate shapes:

```
INSERT into source table
  └─► pg_trickle CDC trigger (AFTER ROW)
        ├─► Append row to pgtrickle_changes (existing)
        └─► [NEW] Atomically update aggregate result table
              ─── SUM(v) += new_v
              ─── COUNT(*) += 1
```

This brings effective latency to **within the same transaction** as the source
DML (immediately visible to readers after the transaction commits).

---

## 3. Safely Invertible Aggregates

Only aggregates where the delta can be computed from the new/old row values
alone are candidates:

| Aggregate | Invertible | Delta formula |
|-----------|-----------|---------------|
| `COUNT(*)` | Yes | +1 (INSERT), -1 (DELETE), 0 (UPDATE if key unchanged) |
| `SUM(expr)` | Yes | +new_val - old_val |
| `AVG(expr)` | Partial | Maintain (sum, count) pair; derive avg |
| `MIN(expr)` | No | Removal of minimum requires full scan |
| `MAX(expr)` | No | Removal of maximum requires full scan |
| `PERCENTILE_*` | No | Requires sorted state |
| `ARRAY_AGG` | No | Requires ordered state |
| `COUNT(DISTINCT)` | No | Requires set state |
| `BOOL_AND` / `BOOL_OR` | No | Removal requires full scan |

**Rule:** Only `COUNT`, `SUM`, and derived `AVG` are eligible for the
synchronous fast path.

---

## 4. Integration with DVM OpTree

The DVM operator tree (`src/dvm/`) already produces differential SQL that
applies deltas. The streaming aggregation path is a specialization:

1. **Parser** (`src/dvm/parser.rs`) detects that the query is a "pure
   aggregate" — no JOINs, no subqueries, single GROUP BY (or no GROUP BY),
   all aggregate functions are in the invertible set.
2. **Code generator** produces a single `UPDATE target_table SET ... WHERE
   group_key = $1` statement instead of full delta SQL.
3. **CDC trigger** calls the generated statement directly, bypassing the
   change buffer for this table.

---

## 5. Required Catalog Changes

Add a column to `pgtrickle.pgt_stream_tables`:

```sql
ALTER TABLE pgtrickle.pgt_stream_tables
  ADD COLUMN fast_path_enabled bool NOT NULL DEFAULT false,
  ADD COLUMN fast_path_sql text;   -- pre-compiled UPDATE statement
```

`fast_path_sql` is compiled once at `CREATE STREAM TABLE` time and stored.
If the source or query changes (DDL), the fast path SQL is invalidated and
recompiled.

---

## 6. Interaction with WAL CDC

The synchronous fast path operates on **trigger-based CDC** only. WAL
decoding happens asynchronously in the background worker and is not compatible
with synchronous in-transaction updates.

When `cdc_mode = 'wal'`, the streaming aggregation fast path is disabled and
the table falls back to the standard background worker refresh cycle.

---

## 7. Implementation Phases

### Phase 1 — Detection and classification

- Extend `src/dvm/parser.rs` to detect pure-aggregate queries.
- Add `fast_path_eligible()` predicate.
- Unit tests for classification (no DB needed).

### Phase 2 — Fast-path SQL generation

- Generate parameterized `UPDATE` statement.
- Store in `fast_path_sql` column.
- Test round-trip: create → compile → inspect stored SQL.

### Phase 3 — Trigger integration

- Modify CDC trigger plpgsql template to call `fast_path_sql` for
  eligible tables.
- Benchmark: compare latency and throughput vs. standard path.

### Phase 4 — Background worker reconciliation

- Run background worker refresh on a longer interval for fast-path tables
  (reconciliation, not primary refresh).
- Handle `MIN`/`MAX` invalidation by falling back to full refresh.

---

## 8. Open Questions

1. **Concurrency:** Multiple concurrent writers hit the same aggregate row —
   does the `UPDATE ... WHERE group_key = $1` serialize correctly under row-
   locking? Need to verify no lost-update anomaly.
2. **GROUP BY cardinality:** High-cardinality GROUP BY (millions of keys) may
   make in-trigger updates as expensive as full refresh. Add a cardinality
   guard.
3. **Distributed transactions:** Does the fast path survive `ROLLBACK`? Yes —
   the trigger fires inside the same transaction, so rollback undoes the
   aggregate update automatically.

---

## References

- [src/dvm/parser.rs](../../src/dvm/parser.rs)
- [src/dvm/diff.rs](../../src/dvm/diff.rs)
- [src/cdc.rs](../../src/cdc.rs)
- [docs/DVM_OPERATORS.md](../../docs/DVM_OPERATORS.md)
- [plans/sql/GAP_SQL_PHASE_7.md](GAP_SQL_PHASE_7.md)
