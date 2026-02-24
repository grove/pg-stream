# PLAN: LATERAL Join Support (Subqueries with LATERAL)

**Status:** Implemented

## Problem Statement

pg_stream supports set-returning functions in the FROM clause (`jsonb_array_elements`, `unnest`, etc.) via `T_RangeFunction` parsing and the `LateralFunction` operator. However, **explicit LATERAL subqueries** are not supported:

```sql
-- ❌ Currently fails or produces incorrect results
SELECT o.id, o.customer,
       latest.amount, latest.created_at
FROM orders o,
     LATERAL (
         SELECT amount, created_at
         FROM line_items li
         WHERE li.order_id = o.id
         ORDER BY created_at DESC
         LIMIT 1
     ) AS latest;

-- ❌ Also unsupported: LATERAL with explicit JOIN
SELECT d.id, d.name, stats.total, stats.cnt
FROM departments d
LEFT JOIN LATERAL (
    SELECT SUM(salary) AS total, COUNT(*) AS cnt
    FROM employees e
    WHERE e.dept_id = d.id
) AS stats ON true;
```

### Current State

The parser handles `T_RangeSubselect` nodes but **ignores the `lateral` flag** (`sub.lateral`). The subquery is treated as a non-correlated subquery wrapped in `OpTree::Subquery`, which delegates to `diff_subquery()` for transparent child delegation. Since the subquery's internal `ColumnRef` nodes reference columns from the outer FROM item, this produces invalid SQL when the diff engine generates the delta — the column references have no resolution context.

### PostgreSQL Parse Tree

```c
typedef struct RangeSubselect {
    NodeTag     type;
    bool        lateral;    // ← Currently ignored
    Node       *subquery;   // The SELECT statement
    Alias      *alias;      // Table alias + column aliases
} RangeSubselect;
```

When `lateral = true`:
- The subquery can reference columns from preceding FROM items
- PostgreSQL implicitly adds LATERAL for comma-syntax (`FROM t, (SELECT ... WHERE ref = t.col)`)
- The `LATERAL` keyword is explicit for JOIN syntax (`LEFT JOIN LATERAL (...)`)

### Why This Matters

LATERAL subqueries are the general form of correlated subqueries in FROM and are commonly used for:

1. **Top-N per group** — `LATERAL (SELECT ... ORDER BY ... LIMIT N)`
2. **Correlated aggregation** — `LATERAL (SELECT SUM(x) FROM child WHERE child.fk = parent.pk)`
3. **Conditional expansion** — `LEFT JOIN LATERAL (...) ON true` (keeping outer rows with NULLs when the subquery returns no rows)
4. **Multi-column derived values** — computing multiple values from a correlated subquery in a single pass

### Relationship to LateralFunction

The existing `LateralFunction` operator handles `T_RangeFunction` (SRFs in FROM) using row-scoped recomputation. LATERAL subqueries (`T_RangeSubselect` with `lateral = true`) are a superset:

| Feature | LateralFunction | LATERAL Subquery |
|---------|----------------|------------------|
| Parse node | `T_RangeFunction` | `T_RangeSubselect` with `lateral = true` |
| Content | Single SRF call | Full SELECT statement |
| Correlation | SRF args reference outer cols | WHERE/JOIN refs outer cols |
| Output cols | Fixed per SRF | Determined by SELECT list |
| ORDER BY / LIMIT | No | Yes (key use case) |
| Aggregation | No (done above) | Yes (inside subquery) |
| JOIN type | Implicit CROSS JOIN | CROSS JOIN, LEFT JOIN |

The diff strategy is the same: **row-scoped recomputation** — when an outer row changes, re-execute the correlated subquery for that row.

---

## Scope

This plan covers two implementation levels:

| Level | Capability | FULL | DIFFERENTIAL | Effort |
|-------|-----------|------|--------------|--------|
| **1** | Parse LATERAL subqueries, FULL only | ✅ | ❌ Reject | ~3 hours |
| **2** | Row-scoped recomputation DIFFERENTIAL | ✅ | ✅ (incremental) | ~8 hours total |

**Recommendation:** Implement both levels. Level 1 alone has limited value since FULL mode already works for any valid SQL. The real benefit is Level 2 — incremental maintenance for LATERAL subqueries.

---

## Level 1 — Parse LATERAL Subqueries, FULL Only

### Goal

Detect `lateral = true` on `T_RangeSubselect`, represent it as a new `OpTree::LateralSubquery` variant, allow FULL refresh, and reject DIFFERENTIAL with a clear error.

### Step 1.1: New OpTree Variant

Add after `LateralFunction`:

```rust
/// A LATERAL subquery in the FROM clause.
///
/// The subquery is correlated — it references columns from preceding
/// FROM items. The subquery body is stored as raw SQL because it
/// cannot be independently differentiated (it depends on outer row
/// context).
///
/// DVM strategy: row-scoped recomputation — for each changed outer row,
/// re-execute the subquery against the new outer row values.
LateralSubquery {
    /// The complete subquery body as SQL text.
    ///
    /// Example: `SELECT amount, created_at FROM line_items li
    ///           WHERE li.order_id = o.id ORDER BY created_at DESC LIMIT 1`
    subquery_sql: String,
    /// The FROM alias (e.g. `latest` from `... AS latest`).
    alias: String,
    /// Column aliases from `AS alias(c1, c2, ...)`, if any.
    column_aliases: Vec<String>,
    /// Output column names determined from the subquery's SELECT list.
    /// Used when `column_aliases` is empty.
    output_cols: Vec<String>,
    /// The left-hand FROM item that this subquery may reference
    /// (LATERAL dependency).
    child: Box<OpTree>,
}
```

**Key design decision: Store subquery as raw SQL.**

Unlike regular `Subquery` nodes (which wrap a parsed `OpTree`), a LATERAL subquery's body **cannot be independently parsed into an OpTree** because:

1. The subquery's WHERE/JOIN references columns from the outer scope (e.g., `WHERE li.order_id = o.id`). These `ColumnRef` nodes resolve to the outer FROM item, not to any table in the subquery's own FROM clause.

2. The subquery may use ORDER BY + LIMIT (which are normally rejected for stream tables). These are valid inside a LATERAL subquery because they apply per-outer-row, not to the entire stream table.

3. During delta generation, the subquery must be re-executed verbatim against each changed outer row — it's an opaque computation, not a composable operator tree.

This mirrors the `LateralFunction` approach where `func_sql` stores the SRF call as raw text.

### Step 1.2: Deparse Subquery to SQL

We need to convert the `SelectStmt` parse node back to SQL text. Options:

**Option A: Use `pg_sys::nodeToString()` + cleanup** — produces debug representation, not valid SQL. Not usable.

**Option B: Use `pgrx` deparse infrastructure** — pgrx 0.17 does not expose `deparse_query()`. Not available.

**Option C: Build a `deparse_select_stmt()` helper** — reconstruct SQL from `SelectStmt` fields. This is the approach.

The helper needs to handle:
- `SELECT <targetList>` — reuse `node_to_expr()` + `ResTarget` alias extraction
- `FROM <fromClause>` — reuse `deparse_from_item()` (new helper, handles `RangeVar` + `JoinExpr`)
- `WHERE <whereClause>` — reuse `node_to_expr()`
- `GROUP BY <groupClause>` — reuse `node_to_expr()` per item
- `HAVING <havingClause>` — reuse `node_to_expr()`
- `ORDER BY <sortClause>` — extract `SortBy` nodes (column ref + direction)
- `LIMIT <limitCount>` / `OFFSET <limitOffset>` — reuse `node_to_expr()`

```rust
/// Deparse a SelectStmt back to SQL text.
///
/// This is specifically for LATERAL subquery bodies. It handles the
/// common SQL constructs that appear inside LATERAL subqueries.
///
/// # Safety
/// Caller must ensure `stmt` points to a valid `pg_sys::SelectStmt`.
unsafe fn deparse_select_stmt_to_sql(
    stmt: *const pg_sys::SelectStmt,
) -> Result<String, PgStreamError> {
    let s = unsafe { &*stmt };
    let mut parts = Vec::new();

    // SELECT clause
    let targets = unsafe { deparse_target_list(s.targetList)? };
    parts.push(format!("SELECT {targets}"));

    // FROM clause
    let from = unsafe { deparse_from_clause(s.fromClause)? };
    if !from.is_empty() {
        parts.push(format!("FROM {from}"));
    }

    // WHERE clause
    if !s.whereClause.is_null() {
        let expr = unsafe { node_to_expr(s.whereClause)? };
        parts.push(format!("WHERE {}", expr.to_sql()));
    }

    // GROUP BY clause
    let group_list = unsafe { PgList::<pg_sys::Node>::from_pg(s.groupClause) };
    if !group_list.is_empty() {
        let groups = deparse_expr_list(&group_list)?;
        parts.push(format!("GROUP BY {groups}"));
    }

    // HAVING clause
    if !s.havingClause.is_null() {
        let expr = unsafe { node_to_expr(s.havingClause)? };
        parts.push(format!("HAVING {}", expr.to_sql()));
    }

    // ORDER BY clause (SortBy nodes)
    let sort_list = unsafe { PgList::<pg_sys::Node>::from_pg(s.sortClause) };
    if !sort_list.is_empty() {
        let sorts = unsafe { deparse_sort_clause(&sort_list)? };
        parts.push(format!("ORDER BY {sorts}"));
    }

    // LIMIT / OFFSET
    if !s.limitCount.is_null() {
        let expr = unsafe { node_to_expr(s.limitCount)? };
        parts.push(format!("LIMIT {}", expr.to_sql()));
    }
    if !s.limitOffset.is_null() {
        let expr = unsafe { node_to_expr(s.limitOffset)? };
        parts.push(format!("OFFSET {}", expr.to_sql()));
    }

    Ok(parts.join(" "))
}
```

**New helper functions needed:**

| Helper | Purpose |
|--------|---------|
| `deparse_target_list(List *) → String` | Convert ResTarget nodes to `expr AS alias, ...` |
| `deparse_from_clause(List *) → String` | Convert FROM items to `table1, table2 JOIN table3 ON ...` |
| `deparse_from_item_to_sql(Node *) → String` | Convert a single FROM item (RangeVar, JoinExpr, RangeSubselect) to SQL |
| `deparse_sort_clause(PgList) → String` | Convert SortBy nodes to `col1 ASC, col2 DESC` |

These helpers reuse existing `node_to_expr()` for expressions and `extract_func_name()` / `deparse_func_call()` for function calls. The key addition is handling `SortBy` nodes:

```rust
/// Deparse a SortBy node to SQL (e.g., `created_at DESC`).
unsafe fn deparse_sort_by(node: *const pg_sys::SortBy) -> Result<String, PgStreamError> {
    let sb = unsafe { &*node };
    let expr = unsafe { node_to_expr(sb.node)? };
    let dir = match sb.sortby_dir {
        pg_sys::SortByDir::SORTBY_ASC => " ASC",
        pg_sys::SortByDir::SORTBY_DESC => " DESC",
        _ => "",
    };
    let nulls = match sb.sortby_nulls {
        pg_sys::SortByNulls::SORTBY_NULLS_FIRST => " NULLS FIRST",
        pg_sys::SortByNulls::SORTBY_NULLS_LAST => " NULLS LAST",
        _ => "",
    };
    Ok(format!("{}{dir}{nulls}", expr.to_sql()))
}
```

### Step 1.3: Extract Output Columns

Determine the subquery's output columns from the `targetList`:

```rust
/// Extract output column names from a SelectStmt's target list.
///
/// For each ResTarget:
/// - If it has an explicit alias (`AS name`), use that
/// - If it's a ColumnRef, use the column name
/// - Otherwise, generate a positional name (`column1`, `column2`, ...)
unsafe fn extract_select_output_cols(
    target_list: *mut pg_sys::List,
) -> Result<Vec<String>, PgStreamError> {
    let targets = unsafe { PgList::<pg_sys::Node>::from_pg(target_list) };
    let mut cols = Vec::new();
    for (i, node) in targets.iter_ptr().enumerate() {
        let rt = unsafe { &*(node as *const pg_sys::ResTarget) };
        if !rt.name.is_null() {
            let name = unsafe { CStr::from_ptr(rt.name) }.to_str().unwrap_or("");
            cols.push(name.to_string());
        } else if let Ok(expr) = unsafe { node_to_expr(rt.val) } {
            cols.push(expr.output_name());
        } else {
            cols.push(format!("column{}", i + 1));
        }
    }
    Ok(cols)
}
```

### Step 1.4: Parse T_RangeSubselect with `lateral = true`

Modify the existing `T_RangeSubselect` branch in `parse_from_item()`:

```rust
} else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeSubselect) } {
    let sub = unsafe { &*(node as *const pg_sys::RangeSubselect) };
    // ... existing null checks ...

    if sub.lateral {
        // ── LATERAL subquery: store as raw SQL ─────────────────────
        let sub_stmt = unsafe { &*(sub.subquery as *const pg_sys::SelectStmt) };
        let subquery_sql = unsafe { deparse_select_stmt_to_sql(sub_stmt)? };

        // Extract output column names from the subquery's SELECT list
        let output_cols = unsafe { extract_select_output_cols(sub_stmt.targetList)? };

        // Extract alias
        let alias = /* ... same as existing code ... */;
        let column_aliases = /* ... same as existing code ... */;

        return Ok(OpTree::LateralSubquery {
            subquery_sql,
            alias,
            column_aliases,
            output_cols,
            // child is attached later in the FROM-list loop
            child: Box::new(OpTree::Scan { /* placeholder */ }),
        });
    }

    // ── Non-LATERAL subquery: existing code path ───────────────────
    // ... existing parse_select_stmt() delegation ...
}
```

### Step 1.5: FROM-list Attachment

Extend the FROM-list loop in `parse_select_stmt()` to handle `LateralSubquery` the same way as `LateralFunction`:

```rust
// In the FROM-list loop:
if let OpTree::LateralFunction { .. } = &right {
    // ... existing LateralFunction attachment ...
} else if let OpTree::LateralSubquery {
    subquery_sql,
    alias,
    column_aliases,
    output_cols,
    ..
} = right {
    tree = OpTree::LateralSubquery {
        subquery_sql,
        alias,
        column_aliases,
        output_cols,
        child: Box::new(tree),
    };
} else {
    tree = OpTree::InnerJoin { ... };
}
```

For explicit `JOIN LATERAL (...)` syntax: this is parsed as a `JoinExpr` where `rarg` is a `RangeSubselect` with `lateral = true`. The existing `T_JoinExpr` handler calls `parse_from_item(join.rarg)` recursively, which will now return `LateralSubquery`. We need to detect this in the join handler:

```rust
// In T_JoinExpr handler:
let right = unsafe { parse_from_item(join.rarg, cte_ctx)? };

// If right side is a LateralSubquery, wrap differently
if let OpTree::LateralSubquery { subquery_sql, alias, column_aliases, output_cols, .. } = right {
    match join.jointype {
        pg_sys::JoinType::JOIN_INNER => {
            // Attach left as child of the LateralSubquery
            Ok(OpTree::LateralSubquery {
                subquery_sql,
                alias,
                column_aliases,
                output_cols,
                child: Box::new(left),
            })
        }
        pg_sys::JoinType::JOIN_LEFT => {
            // LEFT JOIN LATERAL needs special handling — see Level 2
            // For Level 1 (FULL only), we can represent it as:
            Ok(OpTree::LeftLateralSubquery {
                subquery_sql,
                alias,
                column_aliases,
                output_cols,
                child: Box::new(left),
            })
            // For FULL refresh, the entire query is re-executed, so this
            // distinction doesn't matter at parse time. For DIFFERENTIAL,
            // the diff operator needs to know whether to LEFT JOIN or CROSS JOIN.
        }
        _ => Err(PgStreamError::UnsupportedOperator(
            "Only INNER JOIN LATERAL and LEFT JOIN LATERAL are supported".into(),
        )),
    }
}
```

**Design decision: Single variant with join type flag vs. separate variants.**

Use a single `LateralSubquery` variant with an optional `join_type` field:

```rust
LateralSubquery {
    subquery_sql: String,
    alias: String,
    column_aliases: Vec<String>,
    output_cols: Vec<String>,
    /// Whether this is a LEFT JOIN LATERAL (true) or CROSS JOIN LATERAL (false).
    /// LEFT JOIN preserves outer rows even when the subquery returns no rows.
    is_left_join: bool,
    child: Box<OpTree>,
}
```

### Step 1.6: Update OpTree Methods

Add `LateralSubquery` to all `match` arms:

| Method | Behavior |
|--------|----------|
| `alias()` | Return `alias` field |
| `node_kind()` | Return `"lateral subquery"` |
| `output_columns()` | `child.output_columns() + column_aliases` (or `output_cols` if aliases empty) |
| `source_oids()` | Delegate to `child.source_oids()` + parse subquery for additional source OIDs |
| `row_id_key_columns()` | Return `None` (no stable PK) |
| `check_ivm_support_inner()` | Delegate to `child` |

**Important: `source_oids()` challenge.** The LATERAL subquery may reference additional source tables (e.g., `FROM line_items` inside the subquery). These tables need CDC triggers too. Since the subquery is stored as raw SQL, we have two options:

1. **Parse the subquery body's FROM clause for OIDs** — extract table names from the deparsed SQL and resolve to OIDs using SPI. Complex and error-prone.

2. **Extract OIDs during parse time** — before deparsing, walk the subquery's parse tree to find all `RangeVar` nodes and resolve their OIDs. Store them alongside `subquery_sql`.

**Decision:** Option 2 — add a `subquery_source_oids: Vec<u32>` field extracted during parsing. The table OID resolution already exists in the `T_RangeVar` branch of `parse_from_item()`.

Updated variant:

```rust
LateralSubquery {
    subquery_sql: String,
    alias: String,
    column_aliases: Vec<String>,
    output_cols: Vec<String>,
    is_left_join: bool,
    /// Source table OIDs referenced by the subquery body.
    /// Needed for CDC trigger setup.
    subquery_source_oids: Vec<u32>,
    child: Box<OpTree>,
}
```

### Step 1.7: Block DIFFERENTIAL

In `diff_node()`:

```rust
OpTree::LateralSubquery { .. } => Err(PgStreamError::UnsupportedOperator(
    "LATERAL subqueries are not supported in DIFFERENTIAL mode. \
     Use FULL refresh mode instead."
        .into(),
)),
```

### Step 1.8: Tests (Level 1)

**Unit tests** (no database):

```rust
#[test]
fn test_lateral_subquery_output_columns_with_aliases() { ... }

#[test]
fn test_lateral_subquery_output_columns_defaults_to_output_cols() { ... }

#[test]
fn test_lateral_subquery_source_oids_includes_child_and_subquery() { ... }

#[test]
fn test_lateral_subquery_alias() { ... }

#[test]
fn test_lateral_subquery_node_kind() { ... }

#[test]
fn test_lateral_subquery_is_left_join_flag() { ... }
```

**E2E tests** (require database):

```rust
#[tokio::test]
async fn test_lateral_subquery_top_n_full_mode() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE lat_orders (id INT PRIMARY KEY, customer TEXT)").await;
    db.execute("CREATE TABLE lat_items (id INT PRIMARY KEY, order_id INT, amount INT, created_at TIMESTAMP DEFAULT now())").await;
    db.execute("INSERT INTO lat_orders VALUES (1, 'Alice'), (2, 'Bob')").await;
    db.execute("INSERT INTO lat_items VALUES (1, 1, 100, '2024-01-01'), (2, 1, 200, '2024-01-02'), (3, 2, 50, '2024-01-01')").await;

    db.create_st(
        "lat_top_item",
        "SELECT o.id, o.customer, latest.amount \
         FROM lat_orders o, \
         LATERAL (SELECT amount FROM lat_items li WHERE li.order_id = o.id ORDER BY created_at DESC LIMIT 1) AS latest",
        "1m",
        "FULL",
    ).await;

    assert_eq!(db.count("public.lat_top_item").await, 2);
}

#[tokio::test]
async fn test_lateral_subquery_left_join_full_mode() {
    // LEFT JOIN LATERAL — outer rows preserved with NULLs when subquery returns no rows
    ...
}

#[tokio::test]
async fn test_lateral_subquery_correlated_aggregate_full_mode() {
    // LATERAL (SELECT SUM(x) FROM child WHERE child.fk = parent.pk)
    ...
}

#[tokio::test]
async fn test_lateral_subquery_rejected_in_differential() {
    // Verify DIFFERENTIAL mode is rejected with clear error
    ...
}
```

### Files to Change (Level 1)

| File | Change |
|------|--------|
| `src/dvm/parser.rs` | New `OpTree::LateralSubquery` variant; modify `T_RangeSubselect` branch to detect `lateral`; `deparse_select_stmt_to_sql()` + helpers; FROM-list attachment for `LateralSubquery`; `T_JoinExpr` handler for LATERAL right sides; all `match` arms |
| `src/dvm/diff.rs` | `diff_node()` arm returning `UnsupportedOperator` for `LateralSubquery` |
| `tests/e2e_lateral_tests.rs` | Add FULL-mode tests for explicit LATERAL subqueries |
| `docs/SQL_REFERENCE.md` | Document LATERAL subquery support |
| `README.md` | Update SQL Support table |

---

## Level 2 — Row-Scoped Recomputation DIFFERENTIAL

### Goal

Enable DIFFERENTIAL mode for queries containing `LateralSubquery` by re-executing the subquery only for outer rows that changed. This mirrors the `LateralFunction` diff operator's strategy.

### Step 2.1: New Operator File

Create `src/dvm/operators/lateral_subquery.rs`:

```rust
/// Differentiate a LateralSubquery node via row-scoped recomputation.
///
/// Strategy:
/// 1. Get child delta (changed outer rows)
/// 2. Find old ST rows matching changed outer rows (DELETE them)
/// 3. Re-execute the subquery for new/updated outer rows (INSERT results)
/// 4. Handle LEFT JOIN semantics (NULL-padded rows when subquery returns empty)
pub fn diff_lateral_subquery(
    ctx: &mut DiffContext,
    op: &OpTree,
) -> Result<DiffResult, PgStreamError> { ... }
```

### Step 2.2: CTE Chain

The CTE chain is structurally similar to `diff_lateral_function`, but the re-expansion step executes a full subquery instead of calling a SRF:

```sql
-- CTE 1: Changed outer rows from child delta
WITH lat_sq_changed AS (
    SELECT DISTINCT "__pgs_row_id", "__pgs_action", <child_cols>
    FROM <child_delta>
),

-- CTE 2: Old ST rows matching changed outer rows (to be deleted)
lat_sq_old AS (
    SELECT st."__pgs_row_id", st.<all_output_cols>
    FROM <st_table> st
    WHERE EXISTS (
        SELECT 1 FROM lat_sq_changed cs
        WHERE st.<child_col1> IS NOT DISTINCT FROM cs.<child_col1>
          AND st.<child_col2> IS NOT DISTINCT FROM cs.<child_col2>
    )
),

-- CTE 3: Re-execute subquery for new/updated outer rows
lat_sq_expand AS (
    SELECT
        pg_stream_hash(cs.<child_cols>::TEXT || '/' || sub.<sub_cols>::TEXT) AS "__pgs_row_id",
        cs.<child_cols>,
        sub.<subquery_output_cols>
    FROM lat_sq_changed cs,
         LATERAL (<subquery_sql>) AS <alias>(<columns>)
    WHERE cs."__pgs_action" = 'I'
),

-- CTE 4: Final delta
lat_sq_final AS (
    SELECT "__pgs_row_id", 'D' AS "__pgs_action", <all_cols> FROM lat_sq_old
    UNION ALL
    SELECT "__pgs_row_id", 'I' AS "__pgs_action", <all_cols> FROM lat_sq_expand
)
```

### Step 2.3: LEFT JOIN LATERAL Handling

For `LEFT JOIN LATERAL`, if the subquery returns no rows for an outer row, a NULL-padded row must be emitted. This requires modifying the expand CTE:

```sql
-- CTE 3 (LEFT JOIN variant): Use LEFT JOIN LATERAL instead of comma syntax
lat_sq_expand AS (
    SELECT
        pg_stream_hash(cs.<child_cols>::TEXT || '/' || COALESCE(sub.<sub_cols>::TEXT, '')) AS "__pgs_row_id",
        cs.<child_cols>,
        sub.<subquery_output_cols>
    FROM lat_sq_changed cs
    LEFT JOIN LATERAL (<subquery_sql>) AS <alias>(<columns>) ON true
    WHERE cs."__pgs_action" = 'I'
)
```

When the subquery returns no rows, `LEFT JOIN LATERAL ... ON true` produces a single row with NULLs for all subquery columns. This preserves the outer row in the stream table.

### Step 2.4: Column Reference Rewriting

The subquery SQL contains column references to the outer table using its original alias (e.g., `o.id` in `WHERE li.order_id = o.id`). In the expansion CTE, the outer row comes from the `lat_sq_changed` CTE, aliased as `cs`. We have several strategies:

**Option A: Let PostgreSQL resolve through CTE naming.**
Wrap the changed-sources CTE with the original outer alias:

```sql
lat_sq_expand AS (
    SELECT ...
    FROM lat_sq_changed AS o,     -- Use original outer alias!
         LATERAL (...) AS sub
    WHERE o."__pgs_action" = 'I'
)
```

This is the **simplest approach** — the subquery's column references (`o.id`) resolve naturally because the CTE is aliased as `o`. The original outer alias is available from `child.alias()`.

**Decision:** Option A. It's simple and robust because we reuse the outer table's original alias.

### Step 2.5: Source OID Tracking for CDC

The subquery references tables that also need CDC triggers. During parsing, we extract OIDs from the subquery's FROM clause. In the diff operator, these OIDs are used to ensure change buffers exist.

For the delta query, the subquery references **current** source tables (not change buffers) — this is correct because we re-execute the subquery against the live source data for changed outer rows.

### Step 2.6: Row Identity

Content-based hash: `hash(outer_row_columns || '/' || subquery_result_columns)`.

For LEFT JOIN with NULL results: `hash(outer_row_columns || '/' || '')` — the COALESCE ensures a stable hash for NULL-padded rows.

### Step 2.7: Register Operator

- Add `lateral_subquery.rs` to `src/dvm/operators/mod.rs`
- Update `diff_node()` in `diff.rs`:
  ```rust
  OpTree::LateralSubquery { .. } => {
      operators::lateral_subquery::diff_lateral_subquery(self, op)
  }
  ```

### Step 2.8: Tests (Level 2)

**Unit tests** (16+):

```rust
#[test]
fn test_diff_lateral_subquery_basic() { ... }

#[test]
fn test_diff_lateral_subquery_left_join() { ... }

#[test]
fn test_diff_lateral_subquery_uses_original_alias() { ... }

#[test]
fn test_diff_lateral_subquery_old_rows_join_condition() { ... }

#[test]
fn test_diff_lateral_subquery_expand_filters_inserts() { ... }

#[test]
fn test_diff_lateral_subquery_hash_includes_all_columns() { ... }

#[test]
fn test_diff_lateral_subquery_output_columns() { ... }

#[test]
fn test_diff_lateral_subquery_not_deduplicated() { ... }

#[test]
fn test_diff_lateral_subquery_error_on_wrong_node() { ... }

#[test]
fn test_diff_lateral_subquery_source_oids() { ... }
```

**E2E tests** (12+):

```rust
// ── FULL Mode ──────────────────────────────────────────────────────
#[tokio::test]
async fn test_lateral_subquery_top_n_full() { ... }

#[tokio::test]
async fn test_lateral_subquery_left_join_full() { ... }

#[tokio::test]
async fn test_lateral_subquery_correlated_agg_full() { ... }

// ── DIFFERENTIAL Mode ──────────────────────────────────────────────
#[tokio::test]
async fn test_lateral_subquery_differential_initial() { ... }

#[tokio::test]
async fn test_lateral_subquery_differential_outer_insert() {
    // Insert new outer row → subquery runs for new row → expanded rows added
}

#[tokio::test]
async fn test_lateral_subquery_differential_outer_delete() {
    // Delete outer row → all expanded rows for that outer row removed
}

#[tokio::test]
async fn test_lateral_subquery_differential_inner_update() {
    // Update inner table → outer rows referencing changed inner rows
    // are re-evaluated (this tests CDC on subquery source tables)
}

#[tokio::test]
async fn test_lateral_subquery_differential_mixed_dml() {
    // Insert + update + delete in one batch
}

#[tokio::test]
async fn test_lateral_subquery_left_join_differential() {
    // LEFT JOIN: outer row with no matching inner rows → NULL row preserved
}

#[tokio::test]
async fn test_lateral_subquery_left_join_null_to_match() {
    // Previously NULL match → inner row added → NULL row replaced with real values
}

#[tokio::test]
async fn test_lateral_subquery_top_n_order_changes() {
    // ORDER BY LIMIT N inside LATERAL — verify top-N recalculated when inner data changes
}

#[tokio::test]
async fn test_lateral_subquery_empty_result() {
    // Subquery returns 0 rows (CROSS JOIN) → outer row not in ST
}
```

### Files to Change (Level 2)

| File | Change |
|------|--------|
| `src/dvm/operators/lateral_subquery.rs` | **New file** — diff operator with 4-CTE chain |
| `src/dvm/operators/mod.rs` | Register `lateral_subquery` module |
| `src/dvm/diff.rs` | Update `diff_node()` dispatch |
| `tests/e2e_lateral_tests.rs` | Add DIFFERENTIAL-mode tests |
| `docs/DVM_OPERATORS.md` | Add "Lateral Subquery" section |
| `docs/ARCHITECTURE.md` | Add file to tree |

---

## Key Challenges

### 1. Deparsing SelectStmt to SQL

This is the largest new piece of infrastructure. The `deparse_select_stmt_to_sql()` function must handle the SQL constructs commonly used inside LATERAL subqueries:

| Construct | Difficulty | Notes |
|-----------|-----------|-------|
| Simple SELECT + WHERE | Low | Reuse `node_to_expr()` |
| FROM with single table | Low | Extract from `RangeVar` |
| FROM with joins | Medium | Recursive `RangeVar` / `JoinExpr` |
| ORDER BY | Medium | New `SortBy` deparsing |
| LIMIT / OFFSET | Low | Reuse `node_to_expr()` |
| GROUP BY / HAVING | Low | Reuse existing infrastructure |
| Aggregates in SELECT | Low | `node_to_expr()` handles `FuncCall` |
| Subqueries in FROM | High | Recursive deparsing (rare in practice) |
| DISTINCT | Low | Check `distinctClause` flag |

**Risk mitigation:** Start with the common patterns (SELECT + FROM single table + WHERE + ORDER BY + LIMIT) and expand as needed. LATERAL subqueries with complex FROM clauses (nested joins, sub-subqueries) are rare in practice.

### 2. Column Reference Resolution for Outer Table

When the LATERAL subquery references the outer table (e.g., `WHERE li.order_id = o.id`), the column reference `o.id` must resolve correctly in the expansion CTE. As described in Step 2.4, we use the outer table's original alias when generating the expansion CTE.

**Edge case:** Multiple preceding FROM items:
```sql
FROM a, b, LATERAL (SELECT ... WHERE x.fk = a.pk AND x.fk2 = b.pk) AS sub
```
Both `a` and `b` are part of the `child` tree. The expansion CTE needs access to columns from both. Since the child delta contains all child columns, this works naturally — the subquery references `a.pk` and `b.pk`, which are available as columns in the child's output.

**Complication:** The child is an `InnerJoin(a, b)` whose output columns are `[a.col1, a.col2, b.col1, b.col2]` — but the subquery references them as `a.pk` and `b.pk`, not as unqualified names. The expansion CTE aliases the changed-sources row with the **last from-item's alias**, but we need **both** aliases accessible.

**Solution:** In the expansion CTE, join back against the source tables to provide the correct aliases:

```sql
lat_sq_expand AS (
    SELECT ...
    FROM lat_sq_changed cs
    -- Reconstruct original FROM context by joining on child columns
    JOIN <source_table_a> a ON a.<pk> = cs.<a_pk_col>
    JOIN <source_table_b> b ON b.<pk> = cs.<b_pk_col>
    -- Now the subquery's column refs resolve naturally
    , LATERAL (<subquery_sql>) AS sub
    WHERE cs."__pgs_action" = 'I'
)
```

**However**, this is complex and the multi-table LATERAL case is rare. For the initial implementation, **limit support to single-table LATERAL** (one preceding FROM item) and error on multi-source LATERAL.

**Simpler alternative for single source:** Alias the CTE with the outer table's alias:

```sql
FROM lat_sq_changed AS <outer_alias>,
     LATERAL (<subquery_sql>) AS <sub_alias>
WHERE <outer_alias>."__pgs_action" = 'I'
```

### 3. CDC for Subquery Source Tables

LATERAL subqueries reference tables that need CDC triggers. For changes to the **inner** table (e.g., `line_items`), the outer row hasn't changed — but the subquery result may have changed.

**Problem:** The row-scoped recomputation only triggers re-evaluation for changed *outer* rows. If an inner table row changes, no outer row appears in the child delta, so no re-evaluation occurs.

**Solution:** Treat this as a multi-source problem:

**Option A: Full recomputation when inner table changes.**
When the inner table has changes but the outer table doesn't, fall back to full recomputation for the affected operator. This is correct but expensive.

**Option B: Track inner table changes separately.**
Add inner source tables to the CDC tracking and trigger re-evaluation for outer rows that are affected. This requires knowing which outer rows are correlated with the changed inner rows — which requires executing the subquery in reverse (impractical).

**Option C: Always use child delta as trigger, ignore inner table changes.**
Only re-evaluate when the outer table changes. Inner table changes are picked up on the next full refresh or the next time the outer row changes.

**Option D: Join inner delta against outer table to find affected outer rows.**
For each changed inner row, find which outer rows it's correlated with, then re-evaluate those. This requires understanding the correlation predicate.

**Decision for initial implementation:** Option A — when the inner table has changes but the outer table doesn't, fall back to full recomputation of the LATERAL subquery. The full recomputation means: for all outer rows, re-execute the subquery and diff against storage. This is expensive but correct.

**Better approach for a follow-up:** Option D — parse the correlation predicate from the subquery (e.g., `li.order_id = o.id`), use it to join the inner delta against the outer table, and produce a set of "affected outer rows" to re-evaluate. This is the optimal incremental approach but requires correlation predicate extraction.

### 4. Deparse Fidelity

The deparsed SQL must be semantically identical to the original. Known pitfalls:

- **Type casts**: `(e.value)::int` must deparse correctly. The existing `node_to_expr()` handles `TypeCast` → `Expr::Raw`.
- **Operator expressions**: `p.data->'key'` must deparse with correct operator syntax. Already handled by `BinaryOp`.
- **String literals**: Must be properly quoted. Already handled by `A_Const` deparsing.
- **Schema-qualified names**: `public.orders` must be preserved. Already handled in `RangeVar` deparsing.

---

## Common LATERAL Subquery Patterns

| Pattern | Example | Priority | Both levels? |
|---------|---------|----------|--------------|
| **Top-N per group** | `LATERAL (SELECT ... ORDER BY ... LIMIT N)` | High | ✅ |
| **Correlated aggregate** | `LATERAL (SELECT SUM(x) FROM t WHERE t.fk = p.pk)` | High | ✅ |
| **Existence with data** | `LEFT JOIN LATERAL (SELECT ... WHERE ...) ON true` | High | ✅ |
| **Multi-column lookup** | `LATERAL (SELECT a, b, c FROM t WHERE t.fk = p.pk LIMIT 1)` | Medium | ✅ |
| **Correlated with GROUP BY** | `LATERAL (SELECT type, COUNT(*) FROM t WHERE t.fk = p.pk GROUP BY type)` | Medium | ✅ |
| **Multi-source LATERAL** | `FROM a, b, LATERAL (SELECT ... WHERE ... a.id ... b.id ...)` | Low | Level 1 only* |

\* Multi-source LATERAL is supported in FULL mode (entire query re-executed). DIFFERENTIAL mode for multi-source LATERAL is deferred to a follow-up.

---

## Edge Cases

1. **Empty subquery result (CROSS JOIN):** When using comma syntax (`FROM t, LATERAL (...)`) and the subquery returns zero rows, the outer row is excluded from the output (like an inner join). The diff operator must handle this: no row emitted for that outer row.

2. **Empty subquery result (LEFT JOIN):** When using `LEFT JOIN LATERAL ... ON true` and the subquery returns zero rows, a NULL-padded row is emitted. The diff operator must generate the NULL-padded row.

3. **Multiple result rows from subquery:** A LATERAL subquery can return multiple rows per outer row (e.g., without LIMIT). Each produces a separate output row. The diff operator handles this naturally — the expansion CTE produces as many rows as the subquery returns.

4. **Subquery with no correlation:** `FROM t, LATERAL (SELECT 1 AS x)` — technically LATERAL but has no outer references. This is harmless; it works like a regular cross join. The diff operator still re-evaluates per outer row (slightly wasteful but correct).

5. **LATERAL subquery referencing another LATERAL:** `FROM t, LATERAL (...) AS a, LATERAL (SELECT a.col ...)` — chained LATERAL references. The second LATERAL depends on the first. This requires careful ordering in the parse tree. For Level 1 (FULL only), this works automatically. For Level 2, this is out of scope (reject with clear error).

6. **DISTINCT inside LATERAL subquery:** `LATERAL (SELECT DISTINCT ...)` — the deparsed SQL must include DISTINCT. Check `s.distinctClause`.

7. **Subquery with window functions:** `LATERAL (SELECT *, ROW_NUMBER() OVER () FROM ...)` — valid but unusual. The deparsed SQL must include the window clause.

---

## Implementation Order

```
Level 1 (FULL only):
  ✅ 1.1  Add OpTree::LateralSubquery variant + match arms
  ✅ 1.2  Implement deparse_select_stmt_to_sql() + helpers
          - deparse_target_list()
          - deparse_from_clause() / deparse_from_item_to_sql()
          - deparse_sort_clause() / deparse_sort_by()
          - extract_select_output_cols()
  ✅ 1.3  Modify T_RangeSubselect branch to detect lateral=true
  ✅ 1.4  FROM-list attachment for LateralSubquery
  ✅ 1.5  T_JoinExpr handler for LATERAL right sides (LEFT JOIN LATERAL)
  ✅ 1.6  Extract subquery source OIDs during parsing
  ✅ 1.7  DIFFERENTIAL supported (Level 2 implemented)
  ✅ 1.8  Unit tests (18) + E2E tests (12)
  ✅ 1.9  Documentation updates (SQL_REFERENCE, DVM_OPERATORS)

Level 2 (incremental DIFFERENTIAL):
  ✅ 2.1  Create lateral_subquery.rs operator
  ✅ 2.2  Implement CTE chain (4 CTEs)
  ✅ 2.3  Handle LEFT JOIN semantics (LEFT JOIN LATERAL ... ON true)
  ✅ 2.4  Outer-alias rewriting in expansion CTE
  ✅ 2.5  Inner table change detection (fall back to full recomputation)
  ✅ 2.6  Row identity hash (content-based)
  ✅ 2.7  Register operator in mod.rs + diff.rs
  ✅ 2.8  Unit tests (18) + E2E tests (12)
  ✅ 2.9  Documentation updates (DVM_OPERATORS)
```

---

## Estimated Effort

| Phase | Effort | Description |
|-------|--------|-------------|
| Level 1 — OpTree variant + match arms | Low | ~40 lines |
| Level 1 — Deparse infrastructure | High | ~200 lines: `deparse_select_stmt_to_sql()` + 4 helpers |
| Level 1 — Parser modifications | Medium | ~80 lines: LATERAL detection, FROM-list attachment, JoinExpr |
| Level 1 — OID extraction | Low | ~30 lines |
| Level 1 — Tests + docs | Medium | 6 unit + 4 e2e tests, 2 doc files |
| **Level 1 total** | **~3 hours** | |
| Level 2 — Diff operator | High | ~250 lines: CTE chain + LEFT JOIN + alias rewriting |
| Level 2 — Inner table fallback | Medium | ~60 lines: detect inner-only changes, trigger full recomp |
| Level 2 — Tests + docs | Medium | 10 unit + 12 e2e tests |
| **Level 2 total** | **~5 additional hours** | |
| **Grand total** | **~8 hours** | |

---

## Dependencies

- **Existing infrastructure reused:**
  - `node_to_expr()` — expression deparsing (covers most SQL expression types)
  - `extract_func_name()` — function name extraction from parse nodes
  - `deparse_func_call()` — SRF deparsing (added for LateralFunction)
  - `build_hash_expr()` — content-based row ID generation
  - `col_list()`, `quote_ident()` — SQL generation helpers
  - `extract_alias_colnames()` — column alias extraction from `Alias` nodes

- **New infrastructure needed:**
  - `deparse_select_stmt_to_sql()` — the key new helper
  - `deparse_target_list()` — ResTarget → SQL
  - `deparse_from_clause()` / `deparse_from_item_to_sql()` — FROM item → SQL
  - `deparse_sort_clause()` — SortBy → SQL
  - `extract_select_output_cols()` — target list → output column names
  - `extract_from_oids()` — walk FROM clause for table OIDs

---

## Follow-Up Improvements (Post-Implementation)

1. **Correlation predicate extraction** — Parse the subquery's WHERE clause to identify the join condition between inner and outer tables. Use this to efficiently determine which outer rows need re-evaluation when the inner table changes (eliminating the full-recomputation fallback).

2. **Multi-source LATERAL in DIFFERENTIAL** — Support LATERAL subqueries that reference multiple preceding FROM items.

3. **Chained LATERAL** — Support `FROM t, LATERAL (...) AS a, LATERAL (... a.col ...)`.

4. **LATERAL with set operations** — Support LATERAL subqueries that contain UNION/INTERSECT/EXCEPT.
