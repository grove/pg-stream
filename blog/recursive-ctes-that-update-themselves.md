[← Back to Blog Index](README.md)

# Recursive CTEs That Update Themselves

## Incremental view maintenance for graph queries, bill-of-materials, and org charts

---

Most IVM systems bail out when they see `WITH RECURSIVE`. The query involves an unknown number of iterations, the result set can grow or shrink unpredictably based on a single edge change, and the naive approach — recompute from scratch — defeats the purpose of incremental maintenance.

pg_trickle doesn't bail out. It maintains recursive CTEs incrementally using two strategies, chosen automatically based on the workload: **semi-naive evaluation** for insert-only tables, and **Delete-and-Rederive** for tables with mixed inserts, updates, and deletes.

This post explains how both work, when each applies, and what the practical limits are.

---

## The Recursive CTE Recap

A recursive CTE has two parts: a base case and a recursive step.

```sql
WITH RECURSIVE reachable AS (
  -- Base case: direct reports
  SELECT employee_id, manager_id, 1 AS depth
  FROM org_chart
  WHERE manager_id = 42

  UNION ALL

  -- Recursive step: transitive closure
  SELECT o.employee_id, o.manager_id, r.depth + 1
  FROM org_chart o
  JOIN reachable r ON o.manager_id = r.employee_id
  WHERE r.depth < 10
)
SELECT * FROM reachable;
```

PostgreSQL evaluates this by repeatedly executing the recursive step until no new rows are produced. For a deep org chart, this might iterate 8–10 times.

The problem for IVM: when someone changes managers (`UPDATE org_chart SET manager_id = 7 WHERE employee_id = 99`), the entire reachability set can change. An employee and all their reports might move from one subtree to another. The delta isn't just one row — it's an arbitrarily large cascade.

---

## Strategy 1: Semi-Naive Evaluation (Insert-Only)

When the source table only receives INSERTs (no updates or deletes), the incremental strategy is straightforward.

New rows inserted into `org_chart` can only *add* to the reachable set. They can't remove anything. So pg_trickle:

1. Takes the newly inserted rows as the "frontier."
2. Runs the recursive step using only the frontier as input.
3. Any new rows produced become the next frontier.
4. Repeats until the frontier is empty.
5. Inserts all discovered rows into the stream table.

This is semi-naive evaluation — the classic technique from Datalog. Each iteration only processes rows discovered in the previous iteration, not the full accumulated result.

```sql
-- You create the stream table
SELECT pgtrickle.create_stream_table(
  name  => 'reachable_from_ceo',
  query => $$
    WITH RECURSIVE reachable AS (
      SELECT employee_id, manager_id, 1 AS depth
      FROM org_chart WHERE manager_id = 1
      UNION ALL
      SELECT o.employee_id, o.manager_id, r.depth + 1
      FROM org_chart o
      JOIN reachable r ON o.manager_id = r.employee_id
      WHERE r.depth < 15
    )
    SELECT * FROM reachable
  $$,
  schedule => '5s'
);
```

If 3 new employees are added, pg_trickle doesn't re-traverse the entire org chart. It starts from those 3 rows, walks down their subtrees, and inserts only the newly reachable rows. For an org chart with 50,000 employees, adding 3 people touches maybe 3–15 rows instead of 50,000.

**Limitation:** pg_trickle uses the append-only fast path here. If you enable `append_only => true` and the source table later receives DELETEs or UPDATEs, pg_trickle will detect the violation and fall back to FULL refresh for that cycle.

---

## Strategy 2: Delete-and-Rederive (Mixed DML)

When the source table has updates and deletes, the problem is harder. Removing an edge from a graph can make previously reachable nodes unreachable — but only if there's no alternative path.

Consider: employee 99 reports to manager 42. Manager 42 reports to the CEO. If you delete the edge `99 → 42`, is 99 still reachable from the CEO? Only if 99 has another path (maybe through a dotted-line reporting relationship).

The Delete-and-Rederive algorithm handles this:

1. **Delete phase:** Identify all rows in the stream table that *might* be affected by the source changes. This is conservative — it may over-delete.
2. **Rederive phase:** Starting from the base case, re-derive the affected portion of the recursive result. Rows that are still reachable get re-inserted.
3. **Net delta:** The difference between what was deleted and what was re-derived is the actual change to apply.

This is more expensive than semi-naive evaluation because it touches more rows. But it's still cheaper than a full recomputation when the change is localized. Deleting one edge in a graph with 100,000 nodes might affect a subtree of 200 nodes. Delete-and-Rederive processes those 200 nodes, not all 100,000.

---

## Practical Example: Bill of Materials

A manufacturing BOM is a classic recursive structure. Parts contain sub-parts, which contain sub-sub-parts.

```sql
-- The source table
CREATE TABLE bom (
  parent_part_id INT REFERENCES parts(id),
  child_part_id  INT REFERENCES parts(id),
  quantity       INT NOT NULL
);

-- The stream table: exploded BOM with cumulative quantities
SELECT pgtrickle.create_stream_table(
  name  => 'exploded_bom',
  query => $$
    WITH RECURSIVE exploded AS (
      SELECT parent_part_id, child_part_id, quantity, 1 AS level
      FROM bom
      WHERE parent_part_id IN (SELECT id FROM parts WHERE is_top_level)

      UNION ALL

      SELECT e.parent_part_id, b.child_part_id,
             e.quantity * b.quantity, e.level + 1
      FROM exploded e
      JOIN bom b ON b.parent_part_id = e.child_part_id
      WHERE e.level < 20
    )
    SELECT parent_part_id,
           child_part_id,
           SUM(quantity) AS total_quantity,
           MAX(level) AS max_depth
    FROM exploded
    GROUP BY parent_part_id, child_part_id
  $$,
  schedule => '10s'
);
```

When a supplier changes and you update a sub-component relationship, only the affected branch of the BOM tree is re-derived. The rest — potentially thousands of part relationships — stays untouched.

---

## The Depth Guard

Recursive CTEs can diverge. A cycle in the graph (`A → B → C → A`) causes infinite recursion. PostgreSQL handles this with a `WHERE depth < N` guard or a `CYCLE` clause.

pg_trickle requires one of these guards. If it detects a recursive CTE without a termination condition, it rejects the query at creation time:

```
ERROR: recursive CTE 'reachable' has no termination guard
HINT: Add a WHERE depth < N or CYCLE clause to prevent infinite recursion
```

This is a deliberate design choice. An unbounded recursive CTE in a stream table could consume unbounded resources on every refresh cycle.

---

## When to Use FULL Instead

Recursive CTEs with DIFFERENTIAL mode work well when:

- Changes are localized (a few edges added/removed per cycle)
- The graph is deep but sparse
- The recursion depth is bounded

They work poorly when:

- A single change can cascade to most of the result (e.g., changing the root node)
- The graph is dense and fully connected
- The result set is small enough that full recomputation is fast anyway

For the last case, pg_trickle's AUTO mode handles this automatically. If the cost model predicts that Delete-and-Rederive will touch more than 10% of the result, it falls back to FULL for that cycle.

---

## Graph Reachability, Transitive Closure, Shortest Paths

The recursive CTE support covers several common graph patterns:

**Transitive closure** (who can reach whom):
```sql
WITH RECURSIVE closure AS (
  SELECT src, dst FROM edges
  UNION
  SELECT c.src, e.dst
  FROM closure c JOIN edges e ON c.dst = e.src
)
SELECT * FROM closure;
```

**Shortest path** (with depth tracking):
```sql
WITH RECURSIVE paths AS (
  SELECT src, dst, 1 AS hops
  FROM edges WHERE src = 'A'
  UNION ALL
  SELECT p.src, e.dst, p.hops + 1
  FROM paths p JOIN edges e ON p.dst = e.src
  WHERE p.hops < 10
)
SELECT dst, MIN(hops) AS shortest
FROM paths GROUP BY dst;
```

**Ancestor queries** (all ancestors of a node):
```sql
WITH RECURSIVE ancestors AS (
  SELECT id, parent_id FROM categories WHERE id = 42
  UNION ALL
  SELECT c.id, c.parent_id
  FROM categories c JOIN ancestors a ON c.id = a.parent_id
)
SELECT * FROM ancestors;
```

All three patterns work as stream tables with incremental maintenance. The depth guard applies to all of them.

---

## Performance Characteristics

For a realistic benchmark — an org chart with 50,000 employees, 8 levels deep:

| Scenario | FULL refresh | DIFFERENTIAL refresh |
|----------|-------------|---------------------|
| 1 new hire (leaf) | 45ms | 2ms |
| 5 transfers (mid-level) | 45ms | 12ms |
| Reorg: move entire department (500 people) | 45ms | 38ms |
| Change CEO (affects everyone) | 45ms | 52ms |

The crossover point — where DIFFERENTIAL becomes more expensive than FULL — is around 15–20% of the tree being affected. pg_trickle's AUTO mode detects this and switches strategies.

---

## Summary

Recursive CTEs are the SQL feature most people assume can't be maintained incrementally. pg_trickle does it with two algorithms:

1. **Semi-naive evaluation** for insert-only workloads — processes only new frontier rows.
2. **Delete-and-Rederive** for mixed DML — conservatively deletes affected rows, then re-derives them.

Both require a depth guard. Both are automatic — you write the recursive CTE, pg_trickle picks the strategy. And when the change is large enough that incremental maintenance isn't worth it, AUTO mode falls back to FULL.

If your data has a graph structure — org charts, BOMs, category trees, network topologies — and you're currently recomputing the closure on a timer, this is the post that should make you reconsider.
