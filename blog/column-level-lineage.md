[← Back to Blog Index](README.md)

# Column-Level Lineage in One Function Call

## Know exactly which source columns feed your dashboard metrics

---

"If I drop the `discount_pct` column from `orders`, which stream tables break?"

This question seems simple, but answering it requires tracing through every stream table's defining query, following joins and aggregates, and mapping output columns back to source columns. For a DAG with 30 stream tables, doing this manually is an afternoon of SQL parsing.

pg_trickle's `stream_table_lineage()` does it in one function call.

---

## The API

```sql
SELECT * FROM pgtrickle.stream_table_lineage('revenue_by_region');
```

```
 output_column | source_table | source_column | transform
---------------+--------------+---------------+-----------
 region        | customers    | region        | direct
 revenue       | orders       | total         | SUM
 order_count   | orders       | id            | COUNT
 avg_order     | orders       | total         | AVG
```

Each row maps one output column of the stream table to the source table and column it derives from, along with the transformation applied.

---

## What Lineage Tells You

### Impact Analysis

Before altering a source table, check what depends on it:

```sql
-- Which stream tables reference orders.discount_pct?
SELECT DISTINCT l.stream_table
FROM pgtrickle.pgt_stream_tables st
CROSS JOIN LATERAL pgtrickle.stream_table_lineage(st.name) l
WHERE l.source_table = 'orders' AND l.source_column = 'discount_pct';
```

```
   stream_table
------------------
 order_summary
 revenue_by_region
 discount_analysis
```

Now you know: dropping `discount_pct` affects three stream tables. You can plan the migration — update the stream table queries first, then drop the column.

### GDPR Column Deletion

A user requests deletion of their data. You need to know everywhere their PII appears:

```sql
-- Which stream tables contain data derived from customers.email?
SELECT l.stream_table, l.output_column, l.transform
FROM pgtrickle.pgt_stream_tables st
CROSS JOIN LATERAL pgtrickle.stream_table_lineage(st.name) l
WHERE l.source_table = 'customers' AND l.source_column = 'email';
```

If `email` appears in a stream table's output (even through a join), the lineage trace finds it. You know exactly which stream tables need updating and which output columns contain PII.

### Documentation Generation

Auto-generate a data dictionary from lineage:

```sql
-- All lineage for all stream tables
SELECT
  st.name AS stream_table,
  l.output_column,
  l.source_table || '.' || l.source_column AS source,
  l.transform
FROM pgtrickle.pgt_stream_tables st
CROSS JOIN LATERAL pgtrickle.stream_table_lineage(st.name) l
ORDER BY st.name, l.output_column;
```

Export this as CSV or pipe it into your documentation system. Every time a stream table changes, re-run the query to get the updated lineage.

---

## Transformations

The `transform` column describes how the source column becomes the output column:

| Transform | Meaning |
|-----------|---------|
| `direct` | Column passed through unchanged (possibly renamed) |
| `SUM` | Aggregated via SUM |
| `COUNT` | Aggregated via COUNT |
| `AVG` | Aggregated via AVG |
| `MIN` / `MAX` | Aggregated via MIN/MAX |
| `expression` | Used in a computed expression (e.g., `total * quantity`) |
| `filter` | Used in WHERE/HAVING (doesn't appear in output, but affects result) |
| `join_key` | Used as a join condition |
| `window` | Used in a window function |

For computed expressions, the lineage may show multiple source columns mapping to the same output column:

```sql
-- query: SELECT customer_id, total * quantity AS line_total FROM ...
SELECT * FROM pgtrickle.stream_table_lineage('line_items_expanded');
```

```
 output_column | source_table | source_column | transform
---------------+--------------+---------------+------------
 customer_id   | orders       | customer_id   | direct
 line_total    | orders       | total         | expression
 line_total    | order_items  | quantity      | expression
```

Both `total` and `quantity` contribute to `line_total`.

---

## Chained Lineage

When stream tables reference other stream tables (A → B → C), lineage by default shows only the immediate sources:

```sql
SELECT * FROM pgtrickle.stream_table_lineage('dashboard_summary');
```

```
 output_column | source_table     | source_column | transform
---------------+------------------+---------------+-----------
 total_revenue | customer_metrics | lifetime_value| SUM
```

Here, `customer_metrics` is itself a stream table. To trace all the way back to base tables, call lineage recursively:

```sql
-- Transitive lineage: trace to base tables
WITH RECURSIVE full_lineage AS (
  -- Start with the target stream table
  SELECT
    'dashboard_summary' AS stream_table,
    output_column, source_table, source_column, transform
  FROM pgtrickle.stream_table_lineage('dashboard_summary')

  UNION ALL

  -- Recurse through intermediate stream tables
  SELECT
    fl.source_table,
    l.output_column, l.source_table, l.source_column, l.transform
  FROM full_lineage fl
  JOIN pgtrickle.pgt_stream_tables st ON st.name = fl.source_table
  CROSS JOIN LATERAL pgtrickle.stream_table_lineage(st.name) l
  WHERE l.output_column = fl.source_column
)
SELECT * FROM full_lineage
WHERE source_table NOT IN (SELECT name FROM pgtrickle.pgt_stream_tables);
```

This traces through the entire DAG and returns only the base-table lineage. For `dashboard_summary → customer_metrics → orders`, it returns the `orders` columns that ultimately feed the dashboard.

---

## Lineage for Debugging

When a stream table's results look wrong, lineage helps narrow down where the problem is:

1. Check lineage to identify which source columns feed the suspicious output column.
2. Query the source tables directly to verify the data.
3. If the source data is correct, the bug is in the transform (query logic).
4. If the source data is wrong, the problem is upstream.

This is especially useful for deep DAGs where a bug in a base table ripples through multiple levels.

---

## Performance

`stream_table_lineage()` parses the stream table's defining query and traces column references through the OpTree (pg_trickle's internal query representation). It doesn't execute any queries against the actual data.

The cost is proportional to the complexity of the defining query — number of columns, joins, and subqueries. For a typical 10-column query with 3 joins, it returns in under 1ms.

For a recursive trace across the full DAG, the cost multiplies by the number of stream tables in the chain. A 5-level DAG with 20 stream tables typically completes in under 10ms.

---

## Summary

`stream_table_lineage()` maps output columns to source columns in one function call. Use it for impact analysis before schema changes, GDPR compliance audits, documentation generation, and debugging.

For multi-level DAGs, compose it with a recursive CTE to trace all the way to base tables.

It's the kind of feature you don't think about until you're staring at 30 stream tables trying to figure out which one depends on the column you're about to drop. Then it's the most useful function in the extension.
