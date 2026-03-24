# Plan: Truly Mutually Recursive CTEs via Stream Table Decomposition

Date: 2026-03-24
Status: EXPLORATION
Last Updated: 2026-03-24

---

## 1. Problem Statement

### What Are Truly Mutually Recursive CTEs?

Standard SQL `WITH RECURSIVE` supports only single-CTE self-recursion: a CTE
can reference itself in its recursive term. Truly mutually recursive CTEs are
a pair (or set) of CTEs where each references the other:

```sql
-- Not valid SQL — illustrative only
WITH RECURSIVE
  A(cols) AS (base_A  UNION ALL  <term using B>),
  B(cols) AS (base_B  UNION ALL  <term using A>)
SELECT * FROM B;
```

PostgreSQL forbids this: the recursive term of a `WITH RECURSIVE` CTE may
reference only itself, not other recursive CTEs in the same `WITH` clause.
This means users needing mutual recursion patterns (bidirectional graph
reachability, alternating-turn game solving, multi-entity propagation) hit a
hard wall.

### Current pg_trickle State

pg_trickle already solves both halves of this problem independently:

1. **Single-CTE recursion** (within a query): Fully supported via
   `src/dvm/operators/recursive_cte.rs` with semi-naive, DRed, and
   recomputation strategies.

2. **Circular stream table dependencies** (across queries): Fully supported
   via SCC-based fixed-point iteration in `src/scheduler.rs`, with
   monotonicity validation, Tarjan's SCC decomposition, and convergence
   detection.

What is missing is the **bridge**: a mechanism to detect a mutually
recursive pattern in user intent and either (a) automatically decompose it
into circular stream tables, or (b) guide the user through the
decomposition with clear diagnostics and examples.

### Why This Matters

Mutual recursion arises naturally in many real-world patterns:

| Pattern | Description |
|---------|-------------|
| **Bidirectional graph reachability** | Nodes reachable via two edge types (red/blue edges) |
| **Alternating-turn games** | Positions winning for player A depend on B's losses, and vice versa |
| **Multi-entity propagation** | Risk scores propagate: companies affect officers, officers affect companies |
| **Type inference** | Constraint solving with mutually dependent type variables |
| **Supply chain analysis** | Supplier risk propagates through manufacturers and back |

Users attempting these patterns today must manually decompose into multiple
stream tables and configure circular dependencies. This plan designs the
tooling to make that decomposition either automatic or well-guided.

---

## 2. Theoretical Foundation

### 2.1. Equivalence of Mutual Recursion and Circular Dataflow

A system of mutually recursive equations:

$$A = f(A, B) \quad B = g(A, B)$$

is equivalent to a single fixed-point equation over the product lattice:

$$(A, B) = (f(A, B),\ g(A, B))$$

Both converge to the **least fixed point** when $f$ and $g$ are monotone
operators over a complete lattice (Knaster-Tarski theorem). For finite
relational data, bag-monotone queries over finite input always reach a fixed
point in a finite number of steps.

### 2.2. Semi-Naive Evaluation for Mutual Recursion

Classical Datalog semi-naive evaluation generalizes naturally to multiple
mutually recursive predicates. For predicates $A$ and $B$:

$$
\begin{aligned}
\Delta A^{i+1} &= f(A^i \cup \Delta A^i,\ B^i \cup \Delta B^i) - A^i \\
\Delta B^{i+1} &= g(A^i \cup \Delta A^i,\ B^i \cup \Delta B^i) - B^i
\end{aligned}
$$

Each iteration propagates only new tuples ($\Delta$) through the recursive
terms. Convergence is reached when $\Delta A^{i+1} = \emptyset$ and
$\Delta B^{i+1} = \emptyset$.

This is exactly what pg_trickle's SCC fixed-point loop implements: each
iteration refreshes all SCC members, and convergence is detected when no
member produces new rows.

### 2.3. Relationship to DBSP

In the DBSP (Dynamic Batch Stream Processing) framework (Budiu et al., 2023),
mutual recursion maps to a **nested fixed-point operator** containing multiple
lifting ($\delta_0$) and integration ($\int$) operators. The key insight is
that DBSP's incremental evaluation of nested circuits directly corresponds to
the SCC iteration loop:

- Each stream table is a lifted operator within the feedback loop
- The scheduler's iteration corresponds to one "clock tick" of the nested trace
- Convergence detection (zero changes) corresponds to the fixpoint terminator

### 2.4. Monotonicity Requirement

For guaranteed convergence, all queries in the mutual recursion must be
**monotone**: adding input rows can only add output rows, never remove them.

**Monotone-safe operators** (allowed in cycles):
- Scan, Filter, Project, InnerJoin, LeftJoin, FullJoin
- UNION ALL, UNION, INTERSECT, EXISTS, LATERAL
- CteScan, RecursiveCte, RecursiveSelfRef

**Non-monotone operators** (rejected in cycles):
- Aggregate (COUNT/SUM can decrease on DELETE)
- EXCEPT (right-side rows remove output)
- Window functions (rank changes)
- NOT EXISTS, AntiJoin (negation)
- DISTINCT without GROUP BY

This validation is already implemented in `check_monotonicity()` and enforced
by `validate_cycle_allowed_inner()`.

---

## 3. Decomposition Strategy

### 3.1. The Rewrite Rule

Given a user-intended mutually recursive pair:

```
A(cols_a) = base_A  UNION ALL  rec_A(A, B)
B(cols_b) = base_B  UNION ALL  rec_B(A, B)
```

Decompose into two stream tables:

```sql
-- Stream table A (DIFFERENTIAL mode)
WITH RECURSIVE cte AS (
    <base_A>
    UNION ALL
    <rec_A with self-reference to "cte", cross-reference to stream_table_B>
)
SELECT * FROM cte;

-- Stream table B (DIFFERENTIAL mode)
WITH RECURSIVE cte AS (
    <base_B>
    UNION ALL
    <rec_B with self-reference to "cte", cross-reference to stream_table_A>
)
SELECT * FROM cte;
```

The critical transformation: each CTE's reference to the **other** CTE
becomes a plain table scan of the partner's **stream table storage**. Each
CTE's reference to **itself** remains a recursive self-reference within the
`WITH RECURSIVE`. This satisfies PostgreSQL's single-self-reference rule.

### 3.2. Two-Level Fixed-Point Nesting

The decomposed system has two levels of fixed-point iteration:

1. **Inner fixed-point** (per-query): Each `WITH RECURSIVE` CTE iterates its
   own recursive term to convergence within a single SQL execution. Handled
   by PostgreSQL's executor. Produce a complete snapshot of each stream
   table's result for the current iteration.

2. **Outer fixed-point** (cross-table): The scheduler's SCC loop iterates
   all members until no stream table changes. Handled by
   `execute_worker_cyclic_scc()` in `src/scheduler.rs`.

This nested structure converges correctly because:
- The inner fixed-point is exact (PostgreSQL runs each recursive CTE to
  completion).
- The outer fixed-point sees the full result of each inner computation and
  detects global convergence via row-count stability.
- Both are monotone under the required conditions.

### 3.3. Correctness Argument

**Claim**: The decomposed two-stream-table system computes the same least
fixed point as the (hypothetical) mutually recursive CTE.

**Proof sketch**:

Let $T_A^k$ and $T_B^k$ be the contents of stream tables A and B after outer
iteration $k$. Let $F_A(X, Y)$ denote the result of evaluating stream table
A's `WITH RECURSIVE` query when reading $Y$ from the partner. Then:

$$T_A^{k+1} = F_A(T_A^k, T_B^k) \quad T_B^{k+1} = F_B(T_A^{k+1}, T_B^k)$$

Since each $F$ is a complete recursive CTE evaluation (inner fixed-point),
and both $F_A$ and $F_B$ are monotone, the sequence
$(T_A^0, T_B^0), (T_A^1, T_B^1), \ldots$ is monotonically increasing. Since
the universe of possible tuples is finite, convergence follows.

Note: The Gauss-Seidel iteration order (B sees A's updated value within the
same outer iteration) converges at least as fast as Jacobi iteration (both
see previous values). The scheduler processes SCC members sequentially within
each iteration, naturally yielding Gauss-Seidel.

### 3.4. Worked Example: Bidirectional Reachability

**Problem**: Given a graph with red and blue edges, find all nodes reachable
from node 1 via alternating edge colors.

**Mutual recursion** (conceptual):
```
reach_red(target)  = {t : (1,t) in red_edges}
                   UNION {t : (r.target, t) in red_edges, r in reach_blue}

reach_blue(target) = {t : (1,t) in blue_edges}
                   UNION {t : (b.target, t) in blue_edges, b in reach_red}
```

**Decomposition** (actual SQL):
```sql
-- Source tables
CREATE TABLE red_edges  (src INT, dst INT);
CREATE TABLE blue_edges (src INT, dst INT);

INSERT INTO red_edges  VALUES (1,2), (3,4), (5,6);
INSERT INTO blue_edges VALUES (2,3), (4,5);

-- Enable circular dependencies
SET pg_trickle.allow_circular = true;

-- Stream table: reach_red
SELECT pgtrickle.create_stream_table(
    'reach_red',
    $$WITH RECURSIVE cte AS (
        SELECT dst AS target FROM red_edges WHERE src = 1
        UNION
        SELECT e.dst AS target
        FROM red_edges e
        INNER JOIN reach_blue rb ON e.src = rb.target
        UNION
        SELECT e.dst AS target
        FROM red_edges e
        INNER JOIN cte c ON e.src = c.target
    )
    SELECT DISTINCT target FROM cte$$,
    '1s', 'DIFFERENTIAL', false
);

-- Stream table: reach_blue
SELECT pgtrickle.create_stream_table(
    'reach_blue',
    $$WITH RECURSIVE cte AS (
        SELECT dst AS target FROM blue_edges WHERE src = 1
        UNION
        SELECT e.dst AS target
        FROM blue_edges e
        INNER JOIN reach_red rr ON e.src = rr.target
        UNION
        SELECT e.dst AS target
        FROM blue_edges e
        INNER JOIN cte c ON e.src = c.target
    )
    SELECT DISTINCT target FROM cte$$,
    '1s', 'DIFFERENTIAL', false
);
```

**Convergence trace**:

| Iter | reach_red | reach_blue | Changes |
|------|-----------|------------|---------|
| 0 (init) | {2} | {} | seed |
| 1 | {2, 4, 6} | {3, 5} | 4 |
| 2 | {2, 4, 6} | {3, 5} | 0 (converged) |

After 2 outer iterations, both stream tables hold the complete alternating
reachability from node 1.

---

## 4. Implementation Approaches

This plan considers three approaches, from least to most automated:

### Approach A: Documentation and Guidance (Low effort)

Provide comprehensive documentation, examples, and error messages that guide
users to manually decompose mutually recursive patterns into circular stream
tables. No code changes required beyond documentation.

### Approach B: Diagnostic Detection and Assisted Rewrite (Medium effort)

Detect when a user attempts to create a stream table with a mutually
recursive CTE structure (even though PostgreSQL would reject it) and provide
a clear error message with a concrete rewrite suggestion.

### Approach C: Automatic Decomposition (High effort)

Automatically decompose a `CREATE STREAM TABLE` with mutually recursive CTEs
into multiple stream tables with circular dependencies. The user writes the
conceptual mutual recursion; pg_trickle creates the internal topology.

**Recommendation**: Implement **Approach B** first (medium effort, high
user value), then evaluate demand for Approach C.

---

## 5. Approach A: Documentation and Guidance

### 5.1. New Documentation Section

Add a section to `docs/tutorials/MUTUAL_RECURSION.md`:

- Explanation of why PostgreSQL forbids mutual CTE recursion
- The decomposition pattern (with visual diagrams)
- Step-by-step walkthrough for the bidirectional reachability example
- Step-by-step walkthrough for the entity propagation example
- Prerequisites (`allow_circular = true`, DIFFERENTIAL mode, monotone queries)
- Convergence monitoring (`pgt_scc_status()`, `last_fixpoint_iterations`)
- Troubleshooting: what to do when convergence is slow or fails

### 5.2. Enhanced Error Messages

When `check_for_cycles()` rejects a cycle and `allow_circular = false`:

**Current message**:
```
ERROR: circular dependency detected among stream tables: [reach_a, reach_blue]
```

**Enhanced message**:
```
ERROR: circular dependency detected among stream tables: [reach_a, reach_blue]
HINT: Circular dependencies between stream tables are supported for monotone
queries. Set pg_trickle.allow_circular = true and ensure all cycle members
use DIFFERENTIAL refresh mode. See: docs/tutorials/MUTUAL_RECURSION.md
```

### 5.3. FAQ Entry

Add to `docs/FAQ.md`:

> **Q: Can pg_trickle handle mutually recursive queries?**
>
> A: PostgreSQL does not support mutually recursive CTEs (where CTE A
> references CTE B and vice versa). However, you can decompose the mutual
> recursion into separate stream tables with circular dependencies. Each
> stream table defines one recursive CTE that references the other stream
> table as a plain table. pg_trickle's SCC scheduler iterates all members
> to a fixed point, equivalent to solving the mutual recursion.
> See the [Mutual Recursion Tutorial](tutorials/MUTUAL_RECURSION.md).

### Files to Change

| File | Change |
|------|--------|
| `docs/tutorials/MUTUAL_RECURSION.md` | **New file** — tutorial with examples |
| `docs/FAQ.md` | New FAQ entry |
| `src/api.rs` | Enhanced HINT in `CycleDetected` error |

### Estimated Effort

**Low** — ~300 lines of documentation, ~10 lines of code.

---

## 6. Approach B: Diagnostic Detection and Assisted Rewrite

### 6.1. Overview

When a user submits a defining query containing `WITH RECURSIVE` with
multiple CTEs that reference each other, pg_trickle should:

1. Detect the mutual recursion pattern during query validation.
2. Reject the query with a clear, actionable error message.
3. Generate a concrete rewrite suggestion: the exact SQL for each stream
   table the user should create instead.

This happens at `create_stream_table()` time, before the query reaches
PostgreSQL's parser (which would reject it with a cryptic error).

### 6.2. Detection Algorithm

#### Step 1: Parse the WITH clause

In `validate_and_parse_query()`, after receiving the user's defining query,
pre-parse it to extract all CTE definitions and their internal references.

```
Input: WITH RECURSIVE
         A AS (base_A UNION ALL rec_A),
         B AS (base_B UNION ALL rec_B)
       SELECT * FROM B

Output: CTE graph:
  A references: {A (self), B (cross)}
  B references: {B (self), A (cross)}
  Main query references: {B}
```

#### Step 2: Build CTE dependency graph

Construct a directed graph where CTE X has an edge to CTE Y if X's body
references Y. Identify:
- **Self-edges**: X references X (standard recursive CTE)
- **Cross-edges**: X references Y where X != Y (mutual recursion)

#### Step 3: Detect mutual recursion

If the CTE dependency graph contains a cycle involving cross-edges (not
just self-edges), the query contains mutual recursion. Extract the
strongly connected components of the CTE graph.

#### Step 4: Generate rewrite

For each SCC in the CTE dependency graph containing multiple CTEs:
1. Each CTE becomes a separate stream table definition.
2. Cross-references become stream table name references.
3. Self-references remain `WITH RECURSIVE` self-references.
4. The main query's CTE reference determines which stream table is the
   "primary" output.

### 6.3. Implementation Details

#### New module: `src/dvm/mutual_recursion.rs`

```rust
/// Result of mutual recursion analysis.
pub struct MutualRecursionAnalysis {
    /// The CTEs that form a mutually recursive group.
    pub cte_groups: Vec<MutualRecursionGroup>,
    /// Whether the query contains mutual recursion.
    pub has_mutual_recursion: bool,
}

/// A group of CTEs that are mutually recursive.
pub struct MutualRecursionGroup {
    /// CTE names in this group.
    pub cte_names: Vec<String>,
    /// For each CTE, the suggested stream table SQL.
    pub suggested_rewrites: Vec<SuggestedStreamTable>,
}

/// A suggested stream table to replace one CTE in a mutual recursion group.
pub struct SuggestedStreamTable {
    /// Suggested stream table name (derived from CTE alias).
    pub name: String,
    /// The rewritten defining query (WITH RECURSIVE, single self-ref).
    pub defining_query: String,
    /// Required refresh mode (always DIFFERENTIAL).
    pub refresh_mode: String,
    /// Which other stream tables this one depends on.
    pub depends_on: Vec<String>,
}
```

#### Detection function

```rust
/// Analyze a defining query for mutual recursion patterns.
///
/// Returns `None` if the query has no mutual recursion.
/// Returns analysis with rewrite suggestions if mutual recursion is detected.
pub fn detect_mutual_recursion(
    raw_query: &str,
) -> Result<Option<MutualRecursionAnalysis>, PgTrickleError> {
    // 1. Extract WITH clause CTE definitions
    // 2. Build CTE reference graph
    // 3. Run SCC on the CTE graph
    // 4. For multi-CTE SCCs, generate rewrites
}
```

#### SQL rewriting

For each CTE in a mutually recursive group:

```rust
/// Rewrite a mutually recursive CTE definition into a standalone
/// stream table defining query.
///
/// Transforms cross-CTE references into stream table references,
/// preserving self-references as WITH RECURSIVE self-references.
fn rewrite_cte_as_stream_table(
    cte_name: &str,
    cte_body: &str,
    cte_columns: &[String],
    cross_refs: &HashMap<String, String>,  // CTE name → stream table name
) -> Result<String, PgTrickleError> {
    // Build: WITH RECURSIVE <cte_name> AS (
    //            <base_case>
    //            UNION ALL
    //            <recursive_case with cross-refs replaced by ST names>
    //        )
    //        SELECT cols FROM <cte_name>
}
```

#### Integration with create_stream_table()

In `src/api.rs`, `create_stream_table()`, add a pre-validation step:

```rust
// Before sending query to PostgreSQL for parsing:
if let Some(analysis) = detect_mutual_recursion(&raw_query)? {
    // Generate helpful error with rewrite suggestions
    let mut suggestion = String::from(
        "This query contains mutually recursive CTEs, which PostgreSQL \
         does not support. You can decompose it into circular stream \
         tables:\n\n"
    );

    for group in &analysis.cte_groups {
        suggestion.push_str("SET pg_trickle.allow_circular = true;\n\n");

        for st in &group.suggested_rewrites {
            suggestion.push_str(&format!(
                "SELECT pgtrickle.create_stream_table(\n\
                 \x20   '{}',\n\
                 \x20   $${}\n\
                 \x20   $$,\n\
                 \x20   '1s', 'DIFFERENTIAL', false\n\
                 );\n\n",
                st.name, st.defining_query,
            ));
        }
    }

    return Err(PgTrickleError::MutualRecursionDetected {
        cte_names: analysis.cte_groups[0].cte_names.clone(),
        suggestion,
    });
}
```

### 6.4. Error Message Format

```
ERROR: query contains mutually recursive CTEs (A references B, B references A),
       which PostgreSQL does not support in a single WITH RECURSIVE clause

DETAIL: CTEs in mutual recursion group: A, B

HINT: Decompose into circular stream tables. The equivalent setup is:

  SET pg_trickle.allow_circular = true;

  SELECT pgtrickle.create_stream_table(
      'stream_a',
      $$WITH RECURSIVE cte AS (
          SELECT ... FROM base_table_a
          UNION ALL
          SELECT ... FROM some_table JOIN cte ON ...
                                     JOIN stream_b ON ...
      )
      SELECT * FROM cte$$,
      '1s', 'DIFFERENTIAL', false
  );

  SELECT pgtrickle.create_stream_table(
      'stream_b',
      $$WITH RECURSIVE cte AS (
          SELECT ... FROM base_table_b
          UNION ALL
          SELECT ... FROM some_table JOIN cte ON ...
                                     JOIN stream_a ON ...
      )
      SELECT * FROM cte$$,
      '1s', 'DIFFERENTIAL', false
  );

  -- Both stream tables will converge via fixed-point iteration.
  -- Monitor with: SELECT * FROM pgtrickle.pgt_scc_status();
```

### 6.5. Parsing Challenge: Pre-PostgreSQL Analysis

The main technical challenge is that the user's query contains syntax
PostgreSQL will reject. We cannot use `pg_sys::raw_parser()` to parse it.
Two options:

#### Option 1: Lightweight regex/text-based pre-parser

Scan the query text for `WITH RECURSIVE` followed by multiple CTE
definitions. Use regex to extract CTE names and detect cross-references.
This is fragile but handles the common patterns.

```rust
// Detect pattern: WITH RECURSIVE <name1> AS (...), <name2> AS (...)
// Then check if name1 appears in name2's body and vice versa
fn lightweight_mutual_recursion_scan(query: &str) -> Option<Vec<String>> {
    let re = Regex::new(
        r"(?is)WITH\s+RECURSIVE\s+(\w+)\s+(?:\([^)]*\)\s+)?AS\s*\("
    )?;
    // ... extract CTE names and bodies, check for cross-references
}
```

#### Option 2: Custom SQL pre-parser using `pg_query` crate

Use the `pg_query` Rust crate (wraps libpg_query) which can parse
PostgreSQL SQL into a protobuf AST without requiring a running database.
This handles edge cases (quoted identifiers, nested parentheses, comments)
correctly.

```rust
use pg_query::parse;

fn analyze_mutual_recursion(sql: &str) -> Result<Analysis, Error> {
    let result = parse(sql)?;
    // Walk the AST to find WITH RECURSIVE with cross-references
}
```

**Caveat**: `pg_query` uses PostgreSQL's own parser, which may reject
mutually recursive CTEs at parse time. If so, we need to preprocess the
query to temporarily make it parseable (e.g., replace cross-CTE references
with dummy table names).

#### Option 3: Intercept PostgreSQL parser error

Let PostgreSQL attempt to parse the query. If it fails with a specific
error related to recursive CTE reference ordering, catch the error and
apply the lightweight scan from Option 1 to provide the diagnostic.

**Recommendation**: Start with Option 3 (intercept error) combined with
Option 1 (lightweight scan) as the fallback. This minimizes new parsing
code while covering the common case.

### 6.6. New Error Variant

```rust
// In src/error.rs
pub enum PgTrickleError {
    // ... existing variants ...

    /// The defining query contains mutually recursive CTEs that must be
    /// decomposed into separate stream tables.
    MutualRecursionDetected {
        cte_names: Vec<String>,
        suggestion: String,
    },
}
```

### Files to Change

| File | Change |
|------|--------|
| `src/dvm/mutual_recursion.rs` | **New file** — detection and rewrite logic |
| `src/dvm/mod.rs` | Add `pub mod mutual_recursion;` |
| `src/api.rs` | Pre-validation call before query parse |
| `src/error.rs` | New `MutualRecursionDetected` variant |
| `tests/e2e_mutual_recursion_tests.rs` | **New file** — E2E tests |
| `docs/tutorials/MUTUAL_RECURSION.md` | **New file** — tutorial |
| `docs/FAQ.md` | New FAQ entry |

### Estimated Effort

**Medium** — ~400 lines for detection/rewrite, ~200 lines for tests,
~300 lines for documentation.

---

## 7. Approach C: Automatic Decomposition (Future)

### 7.1. Overview

Add a higher-level API that accepts a multi-CTE mutually recursive query
and automatically creates the necessary stream tables with circular
dependencies. The user writes the conceptual query; pg_trickle handles
the decomposition transparently.

### 7.2. Proposed API

```sql
-- Single-call creation of mutually recursive stream tables
SELECT pgtrickle.create_mutual_stream_tables(
    names   => ARRAY['reach_red', 'reach_blue'],
    query   => $$
        WITH RECURSIVE
          reach_red(target) AS (
              SELECT dst FROM red_edges WHERE src = 1
              UNION ALL
              SELECT e.dst FROM red_edges e
              INNER JOIN reach_blue rb ON e.src = rb.target
          ),
          reach_blue(target) AS (
              SELECT dst FROM blue_edges WHERE src = 1
              UNION ALL
              SELECT e.dst FROM blue_edges e
              INNER JOIN reach_red rr ON e.src = rr.target
          )
        SELECT * FROM reach_red, reach_blue
    $$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);
```

This function would:

1. Parse the query to extract mutually recursive CTE groups.
2. For each CTE, create a stream table with the rewritten query.
3. Establish circular dependencies between stream tables.
4. Handle ordering (create non-cyclic dependencies first, then form the cycle
   via ALTER QUERY if needed).
5. Return a summary of created stream tables and their SCC membership.

### 7.3. Creation Ordering Challenge

Creating circular stream tables requires careful ordering because at creation
time, the referenced stream table may not exist yet. Two strategies:

#### Strategy 1: Placeholder-then-ALTER

1. Create all stream tables with placeholder (base-case-only) queries.
2. ALTER each stream table to add the full recursive query with
   cross-references.

```sql
-- Step 1: Create with base-case only
SELECT pgtrickle.create_stream_table('reach_red',
    'SELECT dst AS target FROM red_edges WHERE src = 1', ...);
SELECT pgtrickle.create_stream_table('reach_blue',
    'SELECT dst AS target FROM blue_edges WHERE src = 1', ...);

-- Step 2: ALTER to add mutual recursion
SELECT pgtrickle.alter_stream_table('reach_red',
    query => $$WITH RECURSIVE cte AS (
        SELECT dst AS target FROM red_edges WHERE src = 1
        UNION
        SELECT e.dst FROM red_edges e INNER JOIN reach_blue rb ON e.src = rb.target
        UNION
        SELECT e.dst FROM red_edges e INNER JOIN cte c ON e.src = c.target
    ) SELECT DISTINCT target FROM cte$$);

SELECT pgtrickle.alter_stream_table('reach_blue',
    query => $$WITH RECURSIVE cte AS (...) SELECT ...$$);
```

#### Strategy 2: Deferred validation

Allow `create_stream_table()` to reference not-yet-existing stream tables
when creating a mutual group. Validate all references at the end of the
transaction.

**Recommendation**: Strategy 1 is simpler and uses existing APIs. Strategy 2
requires transaction-level deferred constraint validation, which is
significantly more complex.

### 7.4. DROP and ALTER Semantics

When a user drops one member of an auto-decomposed mutual group:
- Option A: Cascade-drop all members of the group (with warning).
- Option B: Drop only the specified member; remaining members lose their
  circular dependency and may need query adjustment.

**Recommendation**: Option B (independent drop) — consistent with existing
circular dependency behavior where dropping a member simply breaks the cycle
and clears `scc_id`.

### 7.5. Group Metadata

Track auto-decomposed groups in a new catalog table:

```sql
CREATE TABLE pgtrickle.pgt_mutual_groups (
    group_id    SERIAL PRIMARY KEY,
    member_ids  INT[]       NOT NULL,  -- pgt_ids of member stream tables
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    original_query TEXT     NOT NULL   -- the user's original mutual CTE query
);
```

This allows:
- Reconstructing the original user intent for EXPLAIN / debugging.
- Providing a grouped DROP function:
  `pgtrickle.drop_mutual_group(group_id)`.
- Displaying group membership in monitoring views.

### Files to Change

| File | Change |
|------|--------|
| `src/api.rs` | New `create_mutual_stream_tables()` SQL function |
| `src/dvm/mutual_recursion.rs` | Rewrite logic (extends Approach B) |
| `src/catalog.rs` | `pgt_mutual_groups` table CRUD |
| `src/lib.rs` | DDL for `pgt_mutual_groups` catalog table |
| `src/error.rs` | Additional error variants |
| `tests/e2e_mutual_recursion_tests.rs` | Extended E2E tests |
| `docs/SQL_REFERENCE.md` | Document `create_mutual_stream_tables()` |

### Estimated Effort

**High** — ~800 lines for API + rewrite logic, ~400 lines for tests,
~200 lines for documentation.

---

## 8. Testing Strategy

### 8.1. Unit Tests (Approach B)

```rust
#[cfg(test)]
mod tests {
    // Detection tests
    #[test]
    fn test_detect_simple_mutual_recursion() {
        // WITH RECURSIVE A AS (...B...), B AS (...A...)
        // → detected as mutual recursion group {A, B}
    }

    #[test]
    fn test_detect_three_way_mutual_recursion() {
        // A→B, B→C, C→A → detected as mutual recursion group {A, B, C}
    }

    #[test]
    fn test_no_mutual_recursion_single_cte() {
        // WITH RECURSIVE A AS (...A...) → no mutual recursion
    }

    #[test]
    fn test_no_mutual_recursion_independent_ctes() {
        // WITH RECURSIVE A AS (...A...), B AS (...B...)
        // → no mutual recursion (independent self-recursions)
    }

    #[test]
    fn test_mixed_mutual_and_independent() {
        // WITH RECURSIVE A AS (...B...), B AS (...A...), C AS (...C...)
        // → group {A, B}, C is independent
    }

    // Rewrite tests
    #[test]
    fn test_rewrite_simple_pair() {
        // A references B, B references A
        // → two stream table definitions with correct cross-references
    }

    #[test]
    fn test_rewrite_preserves_base_case() {
        // Base case of each CTE is preserved unchanged
    }

    #[test]
    fn test_rewrite_preserves_self_reference() {
        // Self-references remain as WITH RECURSIVE self-refs
    }

    #[test]
    fn test_rewrite_replaces_cross_reference() {
        // Cross-references become stream table name references
    }

    #[test]
    fn test_rewrite_handles_quoted_identifiers() {
        // "My CTE" references "Other CTE" → properly quoted ST names
    }

    #[test]
    fn test_rewrite_handles_column_lists() {
        // A(x, y) AS (...) → preserves column definitions
    }
}
```

### 8.2. E2E Tests

```rust
// tests/e2e_mutual_recursion_tests.rs

#[tokio::test]
async fn test_mutual_recursion_detection_generates_helpful_error() {
    // Submit a query with mutual CTE recursion
    // Verify error message contains rewrite suggestion
    // Verify suggestion SQL is syntactically valid
}

#[tokio::test]
async fn test_mutual_recursion_manual_decomposition_converges() {
    // Follow the suggested rewrite manually
    // Verify both stream tables converge to correct result
    // Compare against ground truth (non-recursive SQL or known answer)
}

#[tokio::test]
async fn test_mutual_recursion_bidirectional_reachability() {
    // Red/blue edge example from Section 3.4
    // Verify complete alternating reachability after convergence
}

#[tokio::test]
async fn test_mutual_recursion_entity_propagation() {
    // Companies ↔ officers risk score propagation
    // Verify risk scores propagate correctly in both directions
}

#[tokio::test]
async fn test_mutual_recursion_three_way_cycle() {
    // A→B→C→A three-way mutual recursion
    // Verify convergence with three stream tables
}

#[tokio::test]
async fn test_mutual_recursion_incremental_after_base_insert() {
    // After initial convergence, insert new base rows
    // Verify re-convergence produces correct incremental result
}

#[tokio::test]
async fn test_mutual_recursion_incremental_after_base_delete() {
    // After initial convergence, delete base rows
    // Verify re-convergence correctly removes derived rows
}

#[tokio::test]
async fn test_mutual_recursion_nonmonotone_rejected() {
    // Attempt mutual recursion with EXCEPT → rejected
    // Verify error mentions monotonicity requirement
}

#[tokio::test]
async fn test_mutual_recursion_convergence_monitoring() {
    // Decompose and converge
    // Verify pgt_scc_status() shows correct SCC membership
    // Verify last_fixpoint_iterations is recorded
}
```

### 8.3. Property-Based Tests

```rust
// tests/e2e_property_mutual_recursion_tests.rs

#[tokio::test]
async fn test_mutual_recursion_matches_ground_truth() {
    // Generate random graph with two edge types
    // Decompose mutual reachability into two stream tables
    // Verify result matches WITH RECURSIVE ground truth
    //   (computed as a single self-recursive CTE over combined edges)
}
```

---

## 9. Execution Plan

### Phase 1: Documentation and Enhanced Errors (Approach A)

| Step | Description | Depends On | Est. Lines |
|------|-------------|-----------|------------|
| A-1 | Write `docs/tutorials/MUTUAL_RECURSION.md` | — | 200 |
| A-2 | Add FAQ entry to `docs/FAQ.md` | — | 30 |
| A-3 | Enhance `CycleDetected` error with HINT | — | 15 |
| A-4 | Add example to `docs/GETTING_STARTED.md` | A-1 | 50 |

### Phase 2: Diagnostic Detection (Approach B)

| Step | Description | Depends On | Est. Lines |
|------|-------------|-----------|------------|
| B-1 | Add `MutualRecursionDetected` error variant | — | 20 |
| B-2 | Implement lightweight CTE cross-reference scanner | — | 200 |
| B-3 | Implement rewrite suggestion generator | B-2 | 200 |
| B-4 | Integrate detection into `create_stream_table()` | B-1, B-3 | 30 |
| B-5 | Unit tests for detection and rewrite | B-3 | 150 |
| B-6 | E2E tests for error messages and manual decomposition | B-4 | 200 |

### Phase 3: Automatic Decomposition (Approach C — Future)

| Step | Description | Depends On | Est. Lines |
|------|-------------|-----------|------------|
| C-1 | Design `create_mutual_stream_tables()` API | B-3 | 50 |
| C-2 | Implement placeholder-then-ALTER creation flow | C-1 | 300 |
| C-3 | Add `pgt_mutual_groups` catalog table | C-2 | 100 |
| C-4 | Implement `drop_mutual_group()` | C-3 | 80 |
| C-5 | E2E tests for automatic decomposition | C-4 | 300 |
| C-6 | Documentation updates | C-5 | 100 |

### Recommended Execution Order

1. **Phase 1** first — immediate user value, low risk, sets up documentation.
2. **Phase 2** — the core deliverable of this plan.
3. **Phase 3** — only if user demand warrants the added complexity.

---

## 10. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| Lightweight parser misidentifies mutual recursion | Medium | Conservative detection: only flag clear WITH RECURSIVE multi-CTE patterns; false negatives are acceptable (user gets PostgreSQL's native error) |
| Rewrite suggestion produces invalid SQL | Medium | Validate generated SQL in tests; include caveat in error message ("suggested rewrite may need adjustment") |
| Decomposed stream tables don't converge for complex patterns | Medium | Already mitigated by `max_fixpoint_iterations` GUC and monotonicity checks |
| Performance: extra SCC iteration overhead vs. single recursive CTE | Low | Inner recursive CTE runs to completion per iteration; overhead is the outer iteration count (typically 2-5 for common patterns) |
| User confusion: "why do I need two stream tables?" | Medium | Clear documentation (Phase 1) addresses this; error messages (Phase 2) provide actionable guidance |
| Three-or-more-way mutual recursion is hard to detect/rewrite | Low | Start with two-way detection; three-way follows the same pattern |
| Interaction with IMMEDIATE refresh mode | Low | IMMEDIATE mode is incompatible with cycles; already enforced by `validate_cycle_allowed_inner()` |

---

## 11. Alternatives Considered

### Alternative 1: Extend PostgreSQL's Parser to Allow Mutual CTEs

Modify PostgreSQL's grammar/parser via the extension hook to accept mutual
CTE references, then handle them in pg_trickle's query rewriter.

**Rejected**: PostgreSQL's parser doesn't offer extensibility for grammar
rules. This would require a fork or a custom parser, both of which are
impractical for an extension.

### Alternative 2: Rewrite Mutual Recursion as a Single Self-Recursive CTE

Automatically merge mutually recursive CTEs into a single CTE with a
discriminator column:

```sql
WITH RECURSIVE combined(kind, target) AS (
    SELECT 'A', ... FROM base_A
    UNION ALL SELECT 'B', ... FROM base_B
    UNION ALL
    SELECT 'A', ... FROM rec_A_body WHERE kind = 'B' -- A reads B's output
    UNION ALL
    SELECT 'B', ... FROM rec_B_body WHERE kind = 'A' -- B reads A's output
)
SELECT * FROM combined WHERE kind = 'B';
```

**Rejected for default approach**: While theoretically possible for simple
cases, this rewrite:
- Changes the column structure (adds discriminator)
- Requires complex merging of recursive terms with different schemas
- Breaks down for CTEs with different column counts/types
- Loses the operational benefits of separate stream tables (independent
  scheduling, permissions, monitoring)
- Cannot handle cases where A and B have different base tables

However, this technique may be valuable as a **user-facing recommendation**
for simple cases. Document it in the tutorial as an alternative for users who
prefer a single stream table.

### Alternative 3: Build a Custom SQL Execution Engine for Mutual CTEs

Implement a custom executor that handles mutual recursion natively within
a single query execution, bypassing PostgreSQL's restriction.

**Rejected**: Far too complex. Would require reimplementing significant parts
of PostgreSQL's executor. The stream table decomposition achieves the same
result with existing infrastructure.

---

## 12. Open Questions

1. **Should `create_mutual_stream_tables()` (Approach C) support
   non-recursive mutual dependencies?** For example, two views that
   reference each other without any `WITH RECURSIVE`. These are simpler
   (no inner fixed point needed) but the circular dependency mechanism
   still applies.

2. **How should the rewrite handle CTEs with different output schemas?**
   If CTE A produces `(id, name)` and CTE B produces `(id, score)`, the
   decomposed stream tables naturally have different schemas. This works
   fine but the mutual recursion detector needs to handle it.

3. **Should there be a `pgtrickle.explain_mutual_recursion()` function?**
   A diagnostic function that takes a query string and returns the
   decomposition plan without creating anything. Useful for exploring
   before committing.

4. **What is the interaction with `pg_trickle.ivm_recursive_max_depth`?**
   Each inner recursive CTE respects the depth limit independently. The
   outer SCC iteration is bounded by `max_fixpoint_iterations`. Both
   limits apply. Should there be guidance on how to set them for mutual
   recursion patterns?

5. **Should the documentation recommend UNION vs. UNION ALL for the
   decomposed queries?** UNION (with dedup) prevents unbounded growth in
   the inner CTE but has higher per-iteration cost. UNION ALL is cheaper
   but may grow without bound if the recursion is not naturally bounded.
   Recommendation: UNION (with dedup) for safety, UNION ALL only when the
   user can prove termination.

---

## 13. References

- Knaster-Tarski Fixed-Point Theorem: guarantees least fixed point for
  monotone functions over complete lattices.
- Tarjan, R. (1972). Depth-first search and linear graph algorithms.
  SIAM Journal on Computing 1(2).
- Gupta, A., Mumick, I.S., Subrahmanian, V.S. (1993). Maintaining views
  incrementally (DRed algorithm).
- Budiu, M. et al. (2023). DBSP: Automatic Incremental View Maintenance.
  VLDB 2023.
- Bancilhon, F. & Ramakrishnan, R. (1986). An amateur's introduction to
  recursive query processing strategies.
- Abiteboul, S., Hull, R., Vianu, V. (1995). Foundations of Databases —
  Chapter 12: Datalog and recursion.
- PLAN_CIRCULAR_REFERENCES.md — existing pg_trickle plan for circular
  stream table dependencies.

---

## Implementation Status

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 1 (A) | Documentation and enhanced errors | Not started |
| Phase 2 (B) | Diagnostic detection and assisted rewrite | Not started |
| Phase 3 (C) | Automatic decomposition | Not started |
