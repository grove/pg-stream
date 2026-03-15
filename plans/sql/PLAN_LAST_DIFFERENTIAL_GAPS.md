# Plan: Last Differential Mode Gaps

Date: 2026-03-15
Status: Complete — G1 + G3 fully implemented (including De Morgan + multi-pass); G2 was done in a prior release

---

## 1. Problem Statement

pg_trickle's DIFFERENTIAL refresh mode covers a very wide range of SQL
constructs. However, three categories of queries still silently fall back to
FULL when `AUTO` mode is selected (or produce hard errors when explicit
`DIFFERENTIAL` is requested):

| # | Gap | Estimated Frequency | Effort |
|---|-----|---------------------|--------|
| G1 | User-defined aggregates (UDAs) | Medium (analytics, PostGIS, pgvector) | Medium |
| G2 | Window functions nested in expressions | Medium (ranking/pagination queries) | Medium |
| G3 | Sublinks in deeply nested OR | Low (complex filter trees) | Small–Medium |

All three gaps share a common trait: the DVM engine already has the
infrastructure to handle them (group-rescan for aggregates, partition
recomputation for windows, UNION rewrite for OR+sublinks), but the parser
rejects the patterns before that infrastructure can be applied.

### Out of Scope

The following FULL-only patterns are **fundamentally** incompatible with
differential semantics and are not addressed here:

- **Materialized views / foreign tables as sources** — no CDC triggers.
- **VOLATILE functions** (`random()`, `clock_timestamp()`) — non-deterministic.
- **TABLESAMPLE** — non-deterministic sampling.
- **FOR UPDATE / FOR SHARE** — row-level locking has no differential meaning.
- **LIMIT without ORDER BY** — non-deterministic row selection.
- **LATERAL with RIGHT/FULL JOIN** — rejected by PostgreSQL itself (lateral
  reference dependency conflicts with RIGHT/FULL semantics).

---

## 2. Current State

### How AUTO Fallback Works

`validate_and_parse_query()` in `src/api.rs` (L540) runs five sequential
checkpoints. If any checkpoint fails and mode is `AUTO`, it downgrades to
`FULL`:

1. `reject_unsupported_constructs()` — TABLESAMPLE, FOR UPDATE, nested OR
   sublinks
2. `reject_materialized_views()` — matviews, foreign tables
3. `validate_immediate_mode_support()` — IMMEDIATE-specific checks
4. `parse_defining_query_full()` + `check_ivm_support_with_registry()` — parse
   into OpTree; any failure → FULL
5. Volatility check — VOLATILE → hard error; STABLE → warning

Gaps G1, G2, and G3 all fail at checkpoint 4 — the OpTree parser rejects
them, and AUTO gracefully falls back to FULL.

### Key Code Locations

| Component | File | Entry Point |
|-----------|------|-------------|
| AUTO fallback orchestrator | `src/api.rs:540` | `validate_and_parse_query()` |
| Aggregate classification | `src/dvm/parser.rs:13558` | `is_known_aggregate()` |
| UDA detection | `src/dvm/parser.rs:13534` | `is_user_defined_aggregate()` |
| Aggregate extraction | `src/dvm/parser.rs:13772` | `extract_aggregates()` rejection |
| Window-in-expr detection | `src/dvm/parser.rs:12787` | `node_contains_window_func()` |
| Window-in-expr rejection | `src/dvm/parser.rs:13166` | `extract_window_exprs()` |
| Window diff operator | `src/dvm/operators/window.rs` | `diff_window()` |
| Aggregate diff operator | `src/dvm/operators/aggregate.rs` | `diff_aggregate()` |
| OR+sublink rewrite | `src/dvm/parser.rs:6242` | `rewrite_sublinks_in_or()` |
| AND+OR+sublink rewrite | `src/dvm/parser.rs:6395` | `rewrite_and_with_or_sublinks()` |
| OR+sublink rejection (stage 2) | `src/dvm/parser.rs:8527` | `extract_where_sublinks()` |
| AggFunc enum | `src/dvm/parser.rs:164` | `AggFunc` variants |
| Group-rescan flag | `src/dvm/parser.rs:342` | `AggFunc::is_group_rescan()` |

---

## 3. Gap Details & Implementation Plans

---

### G1: User-Defined Aggregates (UDAs)

#### Problem

Any aggregate function not in the hard-coded `is_known_aggregate()` list
(~45 built-in functions) is rejected from DIFFERENTIAL mode. This affects:

- **PostGIS**: `ST_Union()`, `ST_Collect()`, `ST_Extent()`, `ST_MakeLine()`
- **pgvector**: `avg()` on vector types (custom aggregate)
- **pg_trgm / hunspell**: custom text aggregates
- **User-created aggregates**: `CREATE AGGREGATE my_percentile(...)`

The current rejection is at `src/dvm/parser.rs` L13772:

```rust
} else if is_user_defined_aggregate(&name_lower) {
    return Err(PgTrickleError::UnsupportedOperator(format!(
        "User-defined aggregate function {func_name}() is not supported \
         in DIFFERENTIAL mode. ..."
    )));
```

#### Insight

The DVM's **group-rescan** strategy already handles many complex aggregates
(STRING_AGG, ARRAY_AGG, all statistical functions, etc.) by re-aggregating
entire groups from the base data when any row in the group changes. This exact
same strategy can safely support any UDA — it makes no assumptions about the
aggregate's internal semantics.

#### Design

**Approach**: Treat unknown aggregates as group-rescan aggregates. Rather than
rejecting them, classify them as a new `AggFunc::UserDefined { name, schema }`
variant that always uses the group-rescan strategy.

##### Step 1: Extend `AggFunc` Enum

File: `src/dvm/parser.rs`

```rust
pub enum AggFunc {
    // ... existing variants ...
    /// User-defined or unrecognized aggregate — always group-rescan.
    UserDefined {
        name: String,
        schema: Option<String>,
    },
}
```

Update `is_group_rescan()` to return `true` for `UserDefined`.

Update `to_sql_name()` (or equivalent) to emit `schema.name()` or just
`name()` depending on whether schema qualification is present.

##### Step 2: Modify Aggregate Extraction

File: `src/dvm/parser.rs`, function `extract_aggregates()` (~L13772)

Replace the UDA rejection with:

```rust
} else if is_user_defined_aggregate(&name_lower) {
    // Treat as group-rescan: re-aggregate the entire group on any change.
    AggFunc::UserDefined {
        name: name_lower.clone(),
        schema: extract_schema_from_funcname(funcname),
    }
}
```

##### Step 3: Update Aggregate Diff Operator

File: `src/dvm/operators/aggregate.rs`

The group-rescan code path in `diff_aggregate()` already handles group-rescan
aggregates generically — it emits:

```sql
SELECT group_cols, AGG_FUNC(args) FROM source WHERE group_key IN (changed_groups)
```

The `UserDefined` variant needs to:

1. Emit the function call with correct schema qualification.
2. Preserve any special syntax (ORDER BY within aggregate, FILTER clause,
   DISTINCT keyword).
3. Handle the case where the UDA uses ordered-set syntax
   (`WITHIN GROUP (ORDER BY ...)`).

Review `render_agg_call()` (or equivalent) to ensure it can emit arbitrary
function names rather than only hard-coded ones.

##### Step 4: Handle Edge Cases

- **Schema-qualified UDAs**: `myschema.my_agg(x)` — preserve schema in
  `AggFunc::UserDefined`.
- **Overloaded names**: A UDA with the same name as a built-in (unlikely but
  possible if schema-qualified) — prefer the built-in classification if
  unqualified, use `UserDefined` if schema-qualified to a non-pg_catalog
  schema.
- **Aggregate with FILTER**: `my_agg(x) FILTER (WHERE cond)` — the existing
  FILTER handling in the aggregate operator should work unchanged since
  group-rescan re-evaluates the full expression.
- **Aggregate with ORDER BY**: `string_agg(x, ',' ORDER BY y)` — ensure the
  ORDER BY clause is preserved in the rendered SQL. The existing group-rescan
  path already handles this for STRING_AGG; verify it generalizes.
- **Ordered-set aggregates**: `my_percentile(0.5) WITHIN GROUP (ORDER BY x)` —
  ensure `WITHIN GROUP` clause is preserved. The existing PERCENTILE_CONT
  handling is a reference implementation.

##### Step 5: Catalog Validation

Add a lightweight check at stream table creation time that verifies the UDA
exists in `pg_proc` with `prokind = 'a'`. This is already done by
`is_user_defined_aggregate()` — reuse it. If the UDA is dropped later, the
refresh will fail at execution time with a clear PostgreSQL error (function
does not exist), which is acceptable.

##### Step 6: Tests

| Test | Description |
|------|-------------|
| Unit: `test_uda_classified_as_group_rescan` | Verify `AggFunc::UserDefined` returns `is_group_rescan() == true` |
| Unit: `test_uda_sql_rendering` | Verify schema-qualified and unqualified UDA SQL emission |
| E2E: `test_uda_simple_differential` | CREATE AGGREGATE → use in stream table → verify DIFFERENTIAL mode |
| E2E: `test_uda_with_filter_clause` | UDA with FILTER (WHERE ...) |
| E2E: `test_uda_with_order_by` | UDA with ORDER BY inside aggregate |
| E2E: `test_uda_ordered_set` | UDA using WITHIN GROUP syntax |
| E2E: `test_uda_schema_qualified` | Schema-qualified UDA `myschema.my_agg()` |
| E2E: `test_uda_insert_delete_update` | Full CDC cycle with UDA in GROUP BY query |
| E2E: `test_uda_multiple_in_same_query` | Multiple UDAs in same SELECT |
| E2E: `test_uda_combined_with_builtin` | UDA + COUNT + SUM in same query |

##### Effort Estimate

- Parser changes: ~60 lines
- Aggregate operator changes: ~30 lines (mostly rendering)
- Tests: ~300 lines
- Risk: Low — group-rescan is a proven strategy; no new delta math required

---

### G2: Window Functions Nested in Expressions

#### Problem

Window functions that appear inside expressions are rejected:

```sql
-- All rejected:
SELECT RANK() OVER (...) + 1 AS adjusted_rank FROM t
SELECT CASE WHEN ROW_NUMBER() OVER (...) <= 10 THEN 'top' ELSE 'rest' END FROM t
SELECT COALESCE(LAG(value) OVER (...), 0) AS prev_value FROM t
SELECT CAST(ROW_NUMBER() OVER (...) AS text) FROM t
```

The rejection is at `src/dvm/parser.rs` L13166:

```rust
if unsafe { node_contains_window_func(rt.val) } {
    return Err(PgTrickleError::UnsupportedOperator(
        "Window functions nested inside expressions ..."
    ));
}
```

The DVM's window operator (`src/dvm/operators/window.rs`) already uses
**partition-based recomputation**: when any row in a partition changes, the
entire partition's window results are recomputed. This means the operator
doesn't need to understand the expression wrapping the window function — it
just needs to recompute the full SELECT expression for affected partitions.

#### Design

**Approach**: Auto-rewrite the defining query to extract nested window
functions into an inner subquery, then apply the outer expression in a wrapper.

**Strategy**: Two-layer CTE rewrite at parse time.

##### Step 1: Detect Window-in-Expression Patterns

File: `src/dvm/parser.rs`

The existing `node_contains_window_func()` already detects these patterns.
Add a companion function `extract_window_from_expression()` that:

1. Walks the expression tree.
2. For each window function found, replaces it with a synthetic column
   reference (`__pgt_win_N`).
3. Returns a list of `(synthetic_name, window_function_sql)` pairs plus the
   rewritten outer expression.

Example:

```sql
-- Input target:
RANK() OVER (PARTITION BY dept ORDER BY salary DESC) + 1 AS adjusted_rank

-- Extracted:
--   inner column: RANK() OVER (PARTITION BY dept ORDER BY salary DESC) AS __pgt_win_1
--   outer expression: __pgt_win_1 + 1 AS adjusted_rank
```

##### Step 2: Rewrite Query with Inner Subquery

File: `src/dvm/parser.rs`, new function `rewrite_nested_window_functions()`

Called from the query rewrite pipeline (`run_query_rewrite_pipeline()` in
`src/api.rs`), after existing rewrites but before DVM parsing.

Transform:

```sql
-- Input:
SELECT id,
       RANK() OVER (PARTITION BY dept ORDER BY salary DESC) + 1 AS adjusted_rank,
       COALESCE(LAG(salary) OVER (PARTITION BY dept ORDER BY hire_date), 0) AS prev_sal,
       name
FROM employees
WHERE active = true

-- Output:
SELECT "__pgt_winbase"."id",
       "__pgt_winbase"."__pgt_win_1" + 1 AS "adjusted_rank",
       COALESCE("__pgt_winbase"."__pgt_win_2", 0) AS "prev_sal",
       "__pgt_winbase"."name"
FROM (
    SELECT id, name,
           RANK() OVER (PARTITION BY dept ORDER BY salary DESC) AS "__pgt_win_1",
           LAG(salary) OVER (PARTITION BY dept ORDER BY hire_date) AS "__pgt_win_2"
    FROM employees
    WHERE active = true
) AS "__pgt_winbase"
```

The inner subquery becomes a standard window function query (already
supported), and the outer SELECT is a simple projection (also supported).

##### Step 3: Handle Edge Cases

- **Multiple window functions in one expression**:
  `RANK() OVER w1 - DENSE_RANK() OVER w2` → extract both as separate
  `__pgt_win_N` columns. The inner subquery can contain multiple window
  functions.

- **Window function in WHERE clause**: PostgreSQL itself rejects this
  (`ERROR: window functions are not allowed in WHERE`), so no handling needed.

- **Window function in GROUP BY / HAVING**: Also rejected by PostgreSQL.

- **Window function in ORDER BY**: Already handled by existing window support.

- **Nested expressions containing window + aggregate**:
  `SUM(x) + ROW_NUMBER() OVER (...)` — PostgreSQL allows this in SELECT.
  The rewrite must place the aggregate in the inner subquery alongside the
  window function, or more precisely, the inner subquery should preserve
  the original query's GROUP BY and aggregates while adding the window
  columns.

  However, PostgreSQL does not allow window functions and aggregates to
  coexist unless the window function operates on the aggregate result:
  `SUM(x) + ROW_NUMBER() OVER (ORDER BY SUM(x))` is valid. In this case,
  the inner subquery should include the aggregates and the window function,
  and the outer SELECT applies the wrapping expression.

- **DISTINCT + nested window**: The inner subquery preserves all rows (no
  DISTINCT); DISTINCT moves to the outer query if originally present.

- **ORDER BY + LIMIT (TopK)**: If the original query has ORDER BY + LIMIT,
  these stay on the outer query. The inner subquery produces all rows with
  window columns.

- **Aliased window definitions** (`WINDOW w AS (PARTITION BY ...)`): The
  inner subquery must carry the WINDOW clause.

##### Step 4: Parser Integration

The rewrite should happen in `run_query_rewrite_pipeline()` (in `src/api.rs`),
inserted after `rewrite_rows_from()` and before `parse_defining_query_full()`:

```
1. rewrite_scalar_subquery_in_where()
2. rewrite_correlated_scalar_in_select()
3. rewrite_sublinks_in_or()
4. rewrite_rows_from()
5. rewrite_grouping_sets()
6. rewrite_distinct_on()
7. rewrite_nested_window_functions()    ← NEW
8. parse_defining_query_full()
```

Remove the rejection in `extract_window_exprs()` — after the rewrite, window
functions will always appear at the top level of SELECT targets.

##### Step 5: Tests

| Test | Description |
|------|-------------|
| Unit: `test_extract_window_from_expression` | Pure logic: expression tree → synthetic columns + outer expr |
| Unit: `test_rewrite_nested_window_arithmetic` | `RANK() + 1` → inner/outer split |
| Unit: `test_rewrite_nested_window_case` | `CASE WHEN ROW_NUMBER() > 5 THEN ...` |
| Unit: `test_rewrite_nested_window_coalesce` | `COALESCE(LAG(x), 0)` |
| Unit: `test_rewrite_nested_window_cast` | `CAST(ROW_NUMBER() AS text)` |
| Unit: `test_rewrite_nested_window_multiple` | Two window funcs in one expression |
| Unit: `test_rewrite_preserves_plain_windows` | Non-nested windows pass through unchanged |
| E2E: `test_nested_window_rank_plus_one` | Full cycle: create, insert, refresh, verify |
| E2E: `test_nested_window_coalesce_lag` | COALESCE(LAG(...), default) |
| E2E: `test_nested_window_case_row_number` | CASE on ROW_NUMBER for top-N labeling |
| E2E: `test_nested_window_multiple_exprs` | Multiple nested windows in same query |
| E2E: `test_nested_window_with_group_by` | Nested window alongside aggregate |
| E2E: `test_nested_window_insert_delete` | CDC cycle with partition changes |

##### Effort Estimate

- Expression extractor: ~120 lines
- Query rewriter: ~200 lines
- Parser integration: ~20 lines (add call + remove rejection)
- Tests: ~400 lines
- Risk: Medium — expression tree walking and SQL reconstruction is complex; the
  inner/outer split must preserve all clauses correctly. Edge cases around
  aggregate + window interaction need careful handling.

---

### G3: Sublinks in Deeply Nested OR

#### Problem

The two-stage rewrite pipeline handles most OR + sublink patterns:

| Pattern | Status |
|---------|--------|
| `WHERE EXISTS(...) OR x > 5` | ✓ Stage 1 → UNION |
| `WHERE a = 1 AND (EXISTS(...) OR x > 5)` | ✓ Stage 1 → AND-prefix + UNION |
| `WHERE a AND (b AND (c OR EXISTS(...)))` | ✓ AND-flattening rescues this |
| `WHERE (a OR EXISTS(...)) AND (b OR NOT EXISTS(...))` | ✗ Only first OR rewritten |
| `WHERE NOT (a AND NOT EXISTS(...))` | ✗ De Morgan not applied |
| `WHERE a OR (b AND EXISTS(...))` | ✗ Mixed AND/OR under top-level OR |

The failing patterns reach `extract_where_sublinks()` (L8527) with residual
OR+sublink nodes and are rejected:

```rust
if inner_bool.boolop == pg_sys::BoolExprType::OR_EXPR
    && unsafe { node_tree_contains_sublink(arg_ptr) }
{
    return Err(PgTrickleError::UnsupportedOperator(
        "Subquery expressions (EXISTS, IN) inside OR conditions ..."
    ));
}
```

#### Design

**Approach**: Extend the OR+sublink rewrite pipeline to handle additional
nesting patterns through recursive normalization and multi-pass rewriting.

##### Step 1: Recursive OR+Sublink Normalization

File: `src/dvm/parser.rs`

Add a new function `normalize_or_sublinks()` that applies transformations
recursively until a fixed point (no more changes):

**Transform A — Multiple OR+sublink conjuncts**:

Currently `rewrite_and_with_or_sublinks()` only processes the **first**
OR+sublink conjunct it finds. Extend it to process all of them by iterating:

```
WHERE (a OR EXISTS(s1)) AND (b OR NOT EXISTS(s2))

Pass 1: Rewrite first OR → UNION branches, each still has second OR+sublink
Pass 2: Rewrite second OR inside each UNION branch
         (each branch is now a simple SELECT, so top-level OR rewrite applies)

Result:
  (SELECT ... WHERE a AND b) UNION
  (SELECT ... WHERE a AND NOT EXISTS(s2)) UNION
  (SELECT ... WHERE EXISTS(s1) AND b) UNION
  (SELECT ... WHERE EXISTS(s1) AND NOT EXISTS(s2))
```

Implementation: After the first rewrite pass produces a UNION, recursively
apply `rewrite_sublinks_in_or()` to each UNION branch. Cap recursion depth
at 3 to prevent pathological explosion.

**Transform B — Mixed AND/OR under top-level OR**:

```
WHERE a OR (b AND EXISTS(s1))

-- This is already a top-level OR. The issue is that one arm is an AND
-- containing a sublink. The current UNION rewrite handles this correctly
-- because each OR arm becomes a separate UNION branch:
  (SELECT ... WHERE a) UNION (SELECT ... WHERE b AND EXISTS(s1))

-- The second branch has EXISTS under AND, which extract_where_sublinks()
-- handles natively. This should already work.
```

Verify and add a test — this may already be supported but untested.

**Transform C — NOT wrapping OR+sublink (De Morgan)**:

```
WHERE NOT (a AND NOT EXISTS(s1))

-- Apply De Morgan's law:
WHERE NOT a OR EXISTS(s1)

-- Now it's a top-level OR with a sublink → UNION rewrite applies.
```

Add a De Morgan normalization pass that pushes NOT inward through AND/OR
before the UNION rewrite runs. Implementation:

```rust
fn push_not_inward(node: *mut pg_sys::Node) -> Result<*mut pg_sys::Node, PgTrickleError>
```

Rules:
- `NOT (a AND b)` → `(NOT a) OR (NOT b)`
- `NOT (a OR b)` → `(NOT a) AND (NOT b)`
- `NOT NOT a` → `a`
- `NOT EXISTS(s)` → `NOT EXISTS(s)` (leave as-is; already handled)

Only apply when the expression tree contains sublinks inside OR after NOT
push-down would expose them to the UNION rewrite.

##### Step 2: Multi-Pass Pipeline

File: `src/api.rs`, function `run_query_rewrite_pipeline()`

Modify the OR+sublink rewrite to run iteratively:

```rust
// Rewrite sublinks in OR (may need multiple passes for nested patterns)
let mut prev = query.clone();
for _ in 0..3 {
    query = rewrite_sublinks_in_or(&query)?;
    if query == prev {
        break;  // Fixed point reached
    }
    prev = query.clone();
}
```

The fixed-point loop ensures that UNION branches created in pass 1 get their
own OR+sublink patterns rewritten in pass 2.

##### Step 3: Guard Against Combinatorial Explosion

When an AND has N OR+sublink conjuncts, the UNION rewrite produces up to
$2^N$ branches. Add a configurable limit:

```rust
const MAX_UNION_BRANCHES: usize = 16;  // 4 OR+sublink conjuncts

if branch_count > MAX_UNION_BRANCHES {
    return Err(PgTrickleError::UnsupportedOperator(format!(
        "Query would produce {} UNION branches after OR+sublink rewriting \
         (limit: {}). Simplify the WHERE clause or use FULL refresh mode.",
        branch_count, MAX_UNION_BRANCHES,
    )));
}
```

This protects against queries like:
```sql
WHERE (a OR EXISTS(s1)) AND (b OR EXISTS(s2)) AND (c OR EXISTS(s3))
      AND (d OR EXISTS(s4)) AND (e OR EXISTS(s5))
-- Would produce 2^5 = 32 UNION branches
```

##### Step 4: Tests

| Test | Description |
|------|-------------|
| Unit: `test_normalize_multiple_or_sublink_conjuncts` | Two OR+sublink arms in AND |
| Unit: `test_normalize_not_demorgan` | NOT (a AND NOT EXISTS(...)) |
| Unit: `test_normalize_mixed_and_or_under_or` | `a OR (b AND EXISTS(...))` |
| Unit: `test_normalize_triple_or_sublink` | Three OR+sublink arms (8 branches) |
| Unit: `test_normalize_explosion_guard` | 5+ OR+sublink arms → error |
| Unit: `test_normalize_fixed_point` | Verify loop terminates |
| E2E: `test_nested_or_two_exists` | Two OR arms with EXISTS, differential |
| E2E: `test_nested_or_demorgan` | NOT(AND(NOT EXISTS)) pattern |
| E2E: `test_nested_or_mixed` | `a OR (b AND EXISTS(...))` |
| E2E: `test_nested_or_cdc_cycle` | Insert/delete with nested OR pattern |

##### Effort Estimate

- Multi-pass loop: ~15 lines
- Multiple OR+sublink conjunct handling: ~50 lines
- De Morgan normalization: ~80 lines
- Combinatorial guard: ~15 lines
- Tests: ~250 lines
- Risk: Low–Medium — each transform is well-defined; the main risk is
  SQL round-trip fidelity through deparse → rewrite → reparse cycles.

---

## 4. Implementation Order

Recommended order based on user impact and dependency:

| Phase | Gap | Rationale |
|-------|-----|-----------|
| **Phase 1** | G1: User-defined aggregates | Highest real-world impact (PostGIS, analytics). Lowest risk — reuses proven group-rescan. |
| **Phase 2** | G2: Window-in-expression | Common in ranking/pagination queries. Medium risk — requires careful expression extraction. |
| **Phase 3** | G3: Nested OR+sublinks | Lowest frequency. Some transforms (multi-pass) are simple; De Morgan is more involved. |

Each phase is independently shippable and testable.

---

## 5. Verification Criteria

After all three gaps are closed, the following should hold:

1. **No silent FULL fallback for supported patterns**: Every pattern listed in
   the "Status → ✓" column in each gap section should produce DIFFERENTIAL
   mode when `AUTO` is requested.

2. **`pgtrickle.describe_stream_table()`** reports `refresh_mode =
   'DIFFERENTIAL'` for queries that previously fell back to FULL.

3. **Zero regressions**: All existing E2E tests pass unchanged.

4. **Performance**: Group-rescan UDAs should have comparable performance to
   existing group-rescan aggregates (STRING_AGG, ARRAY_AGG). Window rewrite
   adds one subquery layer but no additional scans.

5. **Error messages**: When a UDA doesn't exist at creation time, or when
   OR+sublink expansion exceeds the branch limit, clear error messages guide
   the user.

---

## 6. Open Questions

1. **UDA FILTER/ORDER BY preservation**: The existing group-rescan SQL
   rendering for built-in aggregates preserves FILTER and ORDER BY clauses.
   Need to verify the rendering code path is generic enough for arbitrary
   function names, or whether it hard-codes built-in names.

2. **Window rewrite + DISTINCT**: If the original query has `SELECT DISTINCT`,
   should DISTINCT move to the outer query only, or should the inner subquery
   also deduplicate? (Answer: outer only — the inner subquery must preserve
   all rows for correct window computation.)

3. **De Morgan depth limit**: How deep should NOT push-down recurse? Suggest
   depth limit of 3, matching the UNION rewrite pass limit.

4. **UDA in HAVING clause**: `HAVING my_agg(x) > 10` — the group-rescan
   strategy recomputes the full GROUP BY expression. Verify that HAVING
   conditions are included in the recomputation query.

5. **UDA with DISTINCT inside**: `my_agg(DISTINCT x)` — verify the DISTINCT
   keyword is preserved in the rendered group-rescan SQL.
