# PLAN_PARTITIONING_SPIKE.md — Partitioned Stream Tables Design Spike (STRETCH-1)

> **Status:** ✅ A1-1 + A1-2 + A1-3 + A1-4 complete — full Partitioning Design Spike delivered
> **Date:** 2026-03-25
> **Branch:** `0.11-partitioning-spike`
> **Effort:** 2–4 days (spike) + 1–2 wk (A1-1) + 1 wk (A1-2 + A1-3) + 0.5 wk (A1-4)
> **Next:** A2 (query-time partition pruning via RANGE predicate pushdown)

---

## 1. Problem Statement

When a stream table has 10M+ rows and changes affect only 0.1% of rows concentrated
in 2–3 date-range or region-range partitions, the current MERGE statement scans the
**entire** stream table because it joins on `__pgt_row_id` (a content hash). PostgreSQL's
partition pruning does not activate from a hash-join predicate alone.

**Bottleneck:** `MERGE` execution dominates 70–97% of refresh time (per Part 8
profiling). For large stream tables, reducing the MERGE scan surface is the highest-ROI
optimization available.

**Target:** For a 10M-row stream table partitioned into 100 RANGE partitions with a
0.1% change rate concentrated in 2–3 partitions, the MERGE should scan ~100K rows
instead of 10M — a **100× reduction** in MERGE I/O.

---

## 2. Core Constraint: Partition Pruning Does Not Flow Through `__pgt_row_id`

PostgreSQL partition pruning during `MERGE` activates **only** when the join predicate
or a `WHERE` clause aligns with the partition key. In pg_trickle, the MERGE joins on
`__pgt_row_id` — a SHA-256-derived content hash completely unrelated to any user column.

The planner has no basis to prune partitions from the `__pgt_row_id` join predicate.
Therefore, **partition pruning requires an explicit predicate injection step**.

---

## 3. Implementation Approaches

### Approach 1 — Partition-Key Predicate Injection (Recommended for RANGE/HASH)

**Mechanism:**
1. User declares `PARTITION BY partition_key_column` when creating the stream table.
2. The stream table is auto-created as a `PARTITION BY RANGE (partition_key_column)`
   PostgreSQL table.
3. At each refresh tick, the scheduler scans the delta CTE for `MIN(partition_key_column)`
   and `MAX(partition_key_column)` — a single scan of the (typically small) delta.
4. The MERGE is rewritten to include `WHERE st.partition_key_column BETWEEN :min AND :max`,
   forcing the planner to prune all non-overlapping partitions.

**Correctness Guarantee:** The `BETWEEN :min AND :max` range is computed from the delta
itself, so it provably contains every delta row's partition key. No correct rows can be
excluded from the MERGE.

**Performance:** For concentrated changes (the common case), only 1–3 of 100 partitions
are visited. I/O reduction matches the partition fraction (e.g., 2/100 = 98% reduction).

**Risk:** If delta rows span all partitions (e.g., a bulk UPDATE across all dates), the
predicate becomes the full range and no partitions are pruned — correctness maintained,
no performance regression vs. current behavior.

**Applicable to:** `RANGE` and `LIST` partitioned stream tables. Not useful for `HASH`
partitioning (the hash function is not invertible from the data range).

### Approach 2 — Per-Partition MERGE Loop in Rust

**Mechanism:**
1. User declares partition key as above.
2. At refresh time, Rust code queries which child partitions contain any delta rows.
3. For each affected partition, issue a separate targeted MERGE against just that
   child partition table.

**Advantages:**
- No planner risk — each MERGE targets a concrete physical table (no partition routing).
- Works for all partition types including HASH.
- Fine-grained: exactly the affected partitions, no range over-estimation.

**Disadvantages:**
- More round-trips: N MERGEs instead of 1 (latency overhead for N > 10).
- More complex Rust code: must discover child partitions, route deltas, handle
  `ATTACH PARTITION` / `DETACH PARTITION` lifecycle.
- Transaction boundaries: multiple MERGEs within one transaction still work correctly
  under PostgreSQL MVCC, but serialization failures become more likely for hot partitions.

**Recommendation:** Start with Approach 1 (predicate injection) for RANGE partitions.
Add Approach 2 as a separate option if benchmarking shows the range over-estimation
is significant (rare in practice — most workloads have temporally/geographically
clustered write patterns).

---

## 4. Catalog Schema Requirements (A1-1)

### New Column: `st_partition_key TEXT`

Store the user-declared partition key column. `NULL` means the stream table is not
partitioned.

```sql
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS st_partition_key TEXT;
```

- **Type:** `TEXT` (column name as a string)
- **Nullable:** Yes — `NULL` = not partitioned
- **Scope:** Single column name. Multi-column partition keys are a future extension
  (tracked as A1-1b). For now, only single-column RANGE partitioning is in scope.
- **Validation at create time:** The named column must appear in the SELECT output of
  the defining query. Validated by `validate_partition_key()` in `api.rs`.

### Result in `StreamTableMeta`

```rust
/// A1-1: Partition key column for partitioned stream tables.
/// `None` means not partitioned. When set, the stream table storage is
/// created as a declaratively partitioned table (RANGE on this column),
/// and the refresh path injects a partition-key range predicate (A1-3).
pub st_partition_key: Option<String>,
```

### Physical Storage (A1-1 DDL)

When `partition_by` is provided to `CREATE STREAM TABLE`, the storage table is created
with declarative partitioning:

```sql
CREATE TABLE <schema>.<name> (
    __pgt_row_id TEXT NOT NULL,
    __pgt_count  SMALLINT NOT NULL DEFAULT 1,
    <columns from defining query...>,
    PRIMARY KEY (__pgt_row_id, <partition_key_column>)
) PARTITION BY RANGE (<partition_key_column>);
```

**PostgreSQL partitioning constraint:** The partition key must be part of any `PRIMARY KEY`
or `UNIQUE` constraint on the table. Since `__pgt_row_id` is the de-facto PK, the
composite `(__pgt_row_id, partition_key)` primary key satisfies this constraint.

**Default partition:** A default catch-all partition is created automatically to prevent
`INSERT` failures when the delta contains rows outside defined partition bounds:

```sql
CREATE TABLE <schema>.<name>_default
    PARTITION OF <schema>.<name> DEFAULT;
```

Users create additional partitions (e.g., monthly ranges) using standard PostgreSQL DDL:

```sql
ALTER TABLE <schema>.<name>
    ATTACH PARTITION <schema>.<name>_2026_01
    FOR VALUES FROM ('2026-01-01') TO ('2026-02-01');
```

---

## 5. API Design (A1-1)

### `create_stream_table()`

Add optional `partition_by` parameter (default `NULL` = not partitioned):

```sql
SELECT pgtrickle.create_stream_table(
    'myschema.orders_summary',
    'SELECT region, order_date, SUM(amount) AS total FROM orders GROUP BY region, order_date',
    partition_by => 'order_date'   -- NEW
);
```

### `create_stream_table_if_not_exists()`

Same `partition_by` parameter added at the same position.

### `alter_stream_table()`

**Not supported** in A1-1. Changing the partition key of an existing stream table requires
a full DROP + CREATE cycle (PostgreSQL does not allow changing the partitioning of an
existing table). Adding `alter_stream_table(partition_by => ...)` as a shortcut for the
DROP+CREATE cycle is tracked as A1-1c.

### Validation

`validate_partition_key(partition_key, &columns)` checks:
1. The supplied column name is not empty.
2. The column appears in the stream table's SELECT output (from `ValidatedQuery.columns`).
3. The value is a valid PostgreSQL identifier.

Returns `PgTrickleError::InvalidArgument` with a descriptive message if any check fails.

---

## 6. Storage Table Creation Changes (A1-1)

`initialize_st()` in `api.rs` currently always creates the storage table as a plain
(non-partitioned) table. When `st_partition_key` is `Some(...)`:

1. Emit `CREATE TABLE ... PARTITION BY RANGE (partition_key)` instead of `CREATE TABLE`.
2. Create the default catch-all partition: `CREATE TABLE ... PARTITION OF ... DEFAULT`.
3. The `PRIMARY KEY` becomes `(__pgt_row_id, <partition_key>)` (composite).

The auxiliary index on `__pgt_row_id` is still created (for point lookups by row identity),
but does **not** enforce uniqueness across partitions (PostgreSQL does not support global
unique indexes on partitioned tables). This is acceptable — the refresh MERGE is
correctness-gated by the content hash, not by a uniqueness constraint.

---

## 7. Migration Approach (A1-1)

### From 0.10.0 to 0.11.0

The new `st_partition_key` column is `NULL`-able and defaults to `NULL`, so the migration
is a simple `ADD COLUMN IF NOT EXISTS`:

```sql
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS st_partition_key TEXT;
```

No data backfill is needed — all existing stream tables are non-partitioned (`NULL`).

### Physical Storage Migration

Existing stream tables are **not** converted to partitioned tables during upgrade.
Partitioning is opt-in via the `partition_by` parameter at create time. Retrofitting
an existing stream table with partitioning requires DROP + CREATE (documented in
`docs/UPGRADING.md`).

---

## 8. Refresh Path Integration (A1-2 + A1-3)

### A1-2: Delta Min/Max Inspection

Before building the MERGE SQL, inspect the delta CTE:

```rust
fn extract_partition_range(
    delta_table: &str,
    partition_key: &str,
) -> Result<Option<(PgValue, PgValue)>, PgTrickleError> {
    let sql = format!(
        "SELECT MIN({key}), MAX({key}) FROM {delta}",
        key = quote_identifier(partition_key),
        delta = delta_table,
    );
    // Execute via SPI, return None if delta is empty (short-circuit)
}
```

If the delta is empty, return `None` — the scheduler already short-circuits on empty deltas,
but this provides an extra guard.

### A1-3: MERGE Predicate Injection

Current MERGE structure (simplified):

```sql
MERGE INTO <schema>.<name> AS tgt
USING delta_cte AS src ON tgt.__pgt_row_id = src.__pgt_row_id
WHEN MATCHED AND src.__pgt_count = 0 THEN DELETE
WHEN MATCHED THEN UPDATE SET ...
WHEN NOT MATCHED THEN INSERT ...
```

With predicate injection (when `st_partition_key` is set):

```sql
MERGE INTO <schema>.<name> AS tgt
USING delta_cte AS src ON tgt.__pgt_row_id = src.__pgt_row_id
                       AND tgt.<partition_key> BETWEEN :min_val AND :max_val      -- NEW
WHEN MATCHED AND src.__pgt_count = 0 THEN DELETE
WHEN MATCHED THEN UPDATE SET ...
WHEN NOT MATCHED THEN INSERT ...
```

The `AND tgt.<partition_key> BETWEEN :min_val AND :max_val` predicate is added to the ON
clause (not a separate WHERE). This is necessary because MERGE's partition pruning is
driven by the join ON clause — a standalone WHERE clause after the MERGE is applied
post-join and does not prune partitions.

**Correctness proof:** Every row in `delta_cte` has `partition_key` in `[min_val, max_val]`
by construction of the min/max query. Therefore every row that needs updating is reachable
in the ON clause. The predicate only prunes partitions that cannot contain any affected
rows — correctness is preserved.

---

## 9. Benchmark Plan (A1-4)

### Test Setup

```sql
-- Create a large source table partitioned by month
CREATE TABLE orders_large (
    order_id BIGINT PRIMARY KEY,
    order_date DATE NOT NULL,
    region TEXT NOT NULL,
    amount NUMERIC
) PARTITION BY RANGE (order_date);

-- 24 monthly partitions + default
-- Insert 10M rows (uniform distribution across 24 months)

-- Create partitioned stream table
SELECT pgtrickle.create_stream_table(
    'public.orders_summary',
    'SELECT region, order_date, SUM(amount) AS total_amount, COUNT(*) AS order_count
     FROM orders_large
     GROUP BY region, order_date',
    partition_by => 'order_date'
);
```

### Benchmark Scenarios

| Scenario | Delta Size | Partitions Hit | Expected Speedup |
|----------|-----------|----------------|-----------------|
| Hot partition (1 day of current month) | 0.1% rows | 1/24 partitions | ~24× |
| Two months | 0.2% rows | 2/24 partitions | ~12× |
| Quarter (3 months) | 0.3% rows | 3/24 partitions | ~8× |
| Full backfill (all months) | 100% rows | 24/24 partitions | 1× (no regression) |

### Exit Criteria

- ✅ Single hot-partition refresh (0.1% row delta): MERGE scans ≤ 1% of total rows
- ✅ Full-range refresh (all rows in delta): No performance regression vs. non-partitioned
- ✅ Correct results: All benchmark scenarios produce identical output to reference
  non-partitioned stream table

---

## 10. Non-Goals (Out of Scope for A1-1)

- **LIST partitioning:** Future extension (A1-1d). RANGE partitioning covers the
  most important use case (time-series, date-range data).
- **HASH partitioning:** Predicate injection is not applicable. Per-partition MERGE loop
  (Approach 2) would be needed — tracked as A1-3b.
- **Multi-column partition keys:** Tracked as A1-1b.
- **`alter_stream_table(partition_by => ...)`:** Tracked as A1-1c.
- **Automatic partition creation:** Time-series auto-partitioning (like TimescaleDB
  hypertables) is deferred to post-1.0.
- **Partition-aware CDC:** Change buffers remain per-source-table, not per-partition. The
  delta extraction already handles partitioned source tables correctly.
- **Cross-partition UPDATE:** When an UPDATE moves a row from one partition to another (UPDATE
  changes the partition key), this appears as a DELETE in the old partition + INSERT in the new.
  This is handled correctly by the existing MERGE logic — no special handling required.

---

## 11. Risk Analysis

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| ON-clause predicate does not trigger partition pruning | Low | Test with `EXPLAIN` during A1-3; fall back to per-partition loop | 
| Composite PK `(__pgt_row_id, partition_key)` breaks lookup correctness | Low | Unit-test: row identity lookup by `__pgt_row_id` still works |
| Default partition grows unboundedly | Medium | Document that users should create partition ranges before loading; add `WARNING` if default partition has rows |
| Global unique index requirement on keyless-source STs | Low | Keyless-source STs already use non-unique index; no change needed |
| `pg_dump` / `pg_restore` compatibility | Medium | `pg_extension_config_dump` is set for the catalog table; partitioned ST storage tables are dumped by pg_dump normally — test in upgrade E2E |

---

## 12. Implementation Plan (Phase Summary)

| Item | Description | Effort | Status |
|------|-------------|--------|--------|
| STRETCH-1 | This RFC document | 2–4d | ✅ Done |
| A1-1 | DDL: `CREATE STREAM TABLE … PARTITION BY`; `st_partition_key` catalog column; partitioned storage table creation (`PARTITION BY RANGE`); default catch-all partition; non-unique `__pgt_row_id` index; composite PK | 1–2 wk | ✅ Done |
| A1-2 | Delta min/max inspection: `extract_partition_range()` in `refresh.rs`; returns `None` on empty delta (fast path → skip MERGE) | 1 wk | ✅ Done |
| A1-3 | MERGE rewrite: inject `AND st.<key> BETWEEN <min> AND <max>` literal into ON clause via `__PGT_PART_PRED__` placeholder; stored in `CachedMergeTemplate.delta_sql_template`; D-2 prepared statements disabled for partitioned STs | 2–3 wk | ✅ Done |
| A1-4 | E2E benchmarks: 10M-row partitioned ST, 0.1%/0.2%/100% change scenarios; `EXPLAIN (ANALYZE, BUFFERS)` partition-scan verification | 1 wk | ✅ Done |

---

## 13. Go / No-Go Decision

**Go.** Based on this spike:

1. ✅ Partition-key predicate injection is feasible via the MERGE ON clause.
2. ✅ The catalog change (`st_partition_key TEXT`) is minimal and migration is trivial.
3. ✅ Correctness is provable: min/max range provably contains all delta rows.
4. ✅ No regression path: when all partitions are hit, the predicate spans the full range
   and partition pruning is effectively disabled (matching current behavior).
5. ✅ The composite PK `(__pgt_row_id, partition_key)` satisfies PostgreSQL's partitioning
   constraint without breaking row-identity semantics.

**Gating condition:** A1-3 must validate with `EXPLAIN (ANALYZE, BUFFERS)` that the
injected predicate actually triggers partition pruning (i.e., non-affected partitions
show 0 heap fetches). This is a one-hour validation task at the start of A1-3.

---

## References

- [PLAN_NEW_STUFF.md §A-1](plans/performance/PLAN_NEW_STUFF.md) — Original feature proposal
- PostgreSQL documentation: [Table Partitioning](https://www.postgresql.org/docs/18/ddl-partitioning.html)
- PostgreSQL documentation: [MERGE](https://www.postgresql.org/docs/18/sql-merge.html)
- TimescaleDB hypertable implementation: chunk-scoped operations prior art
- Materialize partitioned arrangements: partition-aware delta processing
