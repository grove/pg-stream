# DVM SQL Rewrite Rules

This document describes the transformation pipeline in
`src/dvm/parser/rewrites.rs` that prepares a defining query for
differentiation by the DVM (Differential View Maintenance) engine.

Each rewrite pass targets a specific SQL pattern, transforms it into a
form the DVM engine can differentiate, and has a formal algebraic
correctness argument.

---

## Rewrite Pipeline Order

The rewrite passes are applied in sequence. Each pass may be iterated
until a fixed point (no further changes) is reached.

1. **View Inlining** — Replace view references with their definitions
2. **Grouping Sets Expansion** — Expand CUBE/ROLLUP into UNION ALL
3. **EXISTS → Anti/Semi-Join** — Convert correlated EXISTS to join operators
4. **Scalar Sublink Hoisting** — Lift scalar subqueries to CTEs
5. **Delta Key Restriction** — Push join key filters into R_old snapshots

---

## 1. View Inlining (`rewrite_views_inline`)

**Input Pattern:** `SELECT ... FROM my_view v WHERE ...`

**Transformation:** Replace `my_view` with its `pg_get_viewdef()` body
as a subquery: `SELECT ... FROM (SELECT ... FROM base_tables) v WHERE ...`

**Correctness:** A view is semantically equivalent to its definition.
Inlining is required because the DVM engine needs to see the base
tables to generate per-table change buffer references.

---

## 2. Grouping Sets Expansion (`rewrite_grouping_sets`)

**Input Pattern:** `SELECT ... GROUP BY CUBE(a, b)` or `GROUP BY ROLLUP(a, b)`

**Transformation:** Expand into a `UNION ALL` of individual `GROUP BY`
combinations. CUBE(a, b) → GROUP BY (a, b) UNION ALL GROUP BY (a)
UNION ALL GROUP BY (b) UNION ALL GROUP BY ().

**Correctness:** CUBE/ROLLUP is algebraically equivalent to the union of
all grouping combinations. The DVM engine differentiates each branch
independently, and the UNION ALL operator merges the deltas.

**Guard:** `pg_trickle.max_grouping_set_branches` (default 64) limits
explosion for high-dimensional CUBE expressions.

---

## 3. EXISTS → Anti/Semi-Join Conversion

**Input Pattern:**
```sql
SELECT ... FROM t1 WHERE EXISTS (SELECT 1 FROM t2 WHERE t2.key = t1.key)
SELECT ... FROM t1 WHERE NOT EXISTS (SELECT 1 FROM t2 WHERE t2.key = t1.key)
```

**Transformation:** Convert to `OpTree::SemiJoin` or `OpTree::AntiJoin`
with the extracted condition as the join predicate.

**Correctness:** `EXISTS (correlated subquery)` is equivalent to a
semi-join; `NOT EXISTS` is equivalent to an anti-join. The DVM engine
has specialized delta operators for both.

---

## 4. Scalar Sublink Hoisting (`rewrite_scalar_subqueries`)

**Input Pattern:** Scalar subqueries in SELECT or WHERE:
```sql
SELECT a, (SELECT max(b) FROM t2 WHERE t2.key = t1.key) FROM t1
```

**Transformation:** Hoist the scalar subquery to a CTE and replace
with a reference:
```sql
WITH __pgt_scalar_1 AS (SELECT key, max(b) AS val FROM t2 GROUP BY key)
SELECT a, s.val FROM t1 LEFT JOIN __pgt_scalar_1 s ON s.key = t1.key
```

**Correctness:** A correlated scalar subquery is equivalent to a
left join to its grouped equivalent. The CTE form allows the DVM engine
to differentiate the subquery as a separate operator node.

---

## 5. Delta Key Restriction (DI-6)

**Input Pattern:** Anti-join / semi-join R_old snapshots that scan the
full right table.

**Transformation:** Push equi-join key filters from the delta into the
R_old snapshot:
```sql
-- Before: R_old scans all of right_table
-- After:  R_old WHERE key IN (SELECT key FROM delta_right)
```

**Correctness:** Only right-side rows matching changed keys can affect
the anti/semi-join output. Restricting R_old to changed keys preserves
correctness while reducing the scan from O(n) to O(Δ).

---

## Adding New Rewrite Passes

To add a new rewrite pass:

1. Add the function in `src/dvm/parser/rewrites.rs`
2. Add unit tests asserting the expected SQL output for a reference input
3. Insert the pass at the correct position in the pipeline
4. Document the pass in this file with input pattern, transformation,
   and correctness argument

---

## See Also

- [docs/DVM_OPERATORS.md](DVM_OPERATORS.md) — Per-operator differentiation rules
- [docs/PERFORMANCE_COOKBOOK.md](PERFORMANCE_COOKBOOK.md) — Performance tuning
- [src/dvm/parser/rewrites.rs](https://github.com/grove/pg-trickle/blob/main/src/dvm/parser/rewrites.rs) — Implementation
