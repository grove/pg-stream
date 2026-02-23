# pg_stream vs. DBSP: Similarities and Differences

## What They Share (Conceptual Foundation)

pg_stream explicitly cites DBSP as its theoretical foundation (see [PRIOR_ART.md](PRIOR_ART.md)). The key overlap:

| Concept | DBSP (paper) | pg_stream (implementation) |
|---|---|---|
| **Z-set / delta model** | Rows annotated with weights (+1/−1) in an abelian group | `__pgs_action = 'I'/'D'` column on every delta row — effectively Z-sets restricted to {+1, −1} |
| **Per-operator differentiation** | Recursive Algorithm 4.6: Q^Δ = D ∘ Q ∘ I, decomposed per-operator via the **chain rule** (Q₁ ∘ Q₂)^Δ = Q₁^Δ ∘ Q₂^Δ | `DiffContext::diff_node()` walks the OpTree and calls per-operator differentiators (scan, filter, project, join, aggregate, distinct, union, etc.) — same recursive structural decomposition |
| **Linear operators are self-incremental** | Theorem 3.3: for LTI operator Q, Q^Δ = Q | Filter and Project pass deltas through unchanged (just apply predicate/projection to the delta stream) |
| **Bilinear join rule** | Theorem 3.4: Δ(a × b) = Δa × Δb + a × Δb + Δa × b | `diff_inner_join` generates exactly 3 UNION ALL parts: (delta_left ⋈ current_right), (current_left ⋈ delta_right), and optionally (delta_left ⋈ delta_right) |
| **Aggregate auxiliary counters** | §4.2: counting algorithm for maintaining aggregates with deletions | `__pgs_count` auxiliary column, LEFT JOIN back to stream table to read old counts and compute new counts |
| **Recursive queries** | §6: fixed-point iteration with z⁻¹ delay operator, semi-naive evaluation | `diff_recursive_cte` uses recomputation-diff (DRed-style), not DBSP's native fixed-point circuit |

---

## Key Differences

### 1. Execution model — standalone engine vs. embedded in PostgreSQL

DBSP is a **standalone streaming runtime** (Rust library, now Feldera). It compiles query plans into **dataflow graphs** that maintain in-memory state and process continuous micro-batches. Operators are long-lived stateful actors with their own memory.

pg_stream is an **extension inside PostgreSQL**. It has no persistent dataflow graph. On each refresh, it generates a **single SQL query** (CTE chain) that PostgreSQL's own planner/executor evaluates. After execution, no operator state persists — auxiliary state lives in the stream table itself (`__pgs_count` columns) and change buffer tables.

### 2. Streams vs. periodic batches

DBSP operates on true **infinite streams** indexed by logical time t ∈ ℕ. Each "step" processes one micro-batch of changes, and operators carry **integration state** (I operator = running sum from t=0).

pg_stream operates in **discrete refresh cycles** triggered by a lag-based scheduler. There is no integration operator — the "current state" is just the stream table's contents, and changes are consumed from CDC buffer tables between LSN boundaries. Each refresh is a self-contained transaction.

### 3. Z-set weights vs. binary actions

DBSP uses **integer weights** in ℤ — rows can have weights > 1 (bags) or < −1 (multiple deletions). This enables correct multiset semantics and composable group algebra.

pg_stream uses **binary actions** (`'I'` insert, `'D'` delete, sometimes `'U'` update). It doesn't maintain true Z-set weights. For aggregates, the `__pgs_count` auxiliary column serves a similar purpose but is specific to the aggregate operator — it's not a general weight propagated through the operator tree.

### 4. Integration operator (I)

DBSP: The integration operator I(s)[t] = Σᵢ≤ₜ s[i] is an explicit first-class circuit element. It maintains running sums of changes and is the key mechanism for computing incremental joins (z⁻¹(I(a)) = "accumulated left side up to previous step").

pg_stream: No explicit integration. The equivalent of I is just "read the current contents of the source/stream table." Join differentiation directly reads the *current snapshot* of the non-delta side (`build_snapshot_sql()` generates `FROM "public"."orders" r`), which implicitly includes all historical changes.

### 5. Recursion

DBSP: Native fixed-point circuits with z⁻¹ delay. Can incrementally maintain recursive queries (e.g., transitive closure) by iterating only on new changes within each step — semi-naive evaluation generalized to arbitrary recursion.

pg_stream: Uses **recomputation-diff** for recursive CTEs — re-executes the full recursive query and anti-joins against current storage to compute the delta. This is correct but not truly incremental for the recursive part.

### 6. Correctness guarantees

DBSP: Proven correct in Lean. All theorems are machine-checked. The chain rule, cycle rule, and bilinear decomposition are formally verified.

pg_stream: Verified empirically via property-based tests (the `assert_invariant` checks that Contents(ST) = Q(DB) after each mutation cycle). No formal proof, but the per-operator rules are direct translations of DBSP's rules.

### 7. Scope

DBSP: A **general-purpose theory** and streaming engine. Handles nested relations, streaming aggregation over windows, arbitrary compositions. The Feldera implementation supports a full SQL frontend.

pg_stream: Focused on **materialized views inside PostgreSQL**. Supports a specific subset of SQL (scan, filter, project, inner/left/full join, aggregates, DISTINCT, UNION ALL, INTERSECT, EXCEPT, CTEs, window functions, lateral joins). It is not a general streaming engine — it leverages PostgreSQL's own query planner and executor.

---

## Summary

pg_stream applies DBSP's **differentiation rules** to generate delta queries, but it is not a DBSP implementation. It borrows the mathematical framework (per-operator differentiation, Z-set-like deltas, bilinear join decomposition) while making fundamentally different architectural choices: embedded in PostgreSQL, no persistent dataflow state, periodic batch execution, and PostgreSQL's planner as the optimizer. Think of it as "DBSP's differentiation algebra, compiled down to SQL CTEs and executed by PostgreSQL."
