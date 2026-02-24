# PLAN: Non-Deterministic Function Handling

**Status:** Not started (future work)

## Problem Statement

pg_stream does not check the volatility of functions used in defining queries. This is a **correctness gap** for DIFFERENTIAL mode: volatile functions (e.g., `random()`, `gen_random_uuid()`, `clock_timestamp()`) produce different values on each evaluation, which breaks delta computation because the DVM engine assumes expressions are deterministic across refreshes.

### How it breaks

The DVM engine computes deltas by comparing "what changed in the source" against the stored state. When a volatile function is present:

1. A row is inserted into the source table → delta query evaluates `random()` → stores `0.42` in the stream table
2. On the next refresh, the merge CTE re-evaluates the same expression → gets `0.73`
3. The engine sees a phantom change (new value ≠ stored value) or misses real changes
4. The row hash (`__pgs_row_id`) may also differ, breaking row identity entirely

### PostgreSQL function volatility categories

| Category | Meaning | Safe for DIFFERENTIAL? |
|---|---|---|
| **IMMUTABLE** | Same result for same inputs, always | ✅ Yes |
| **STABLE** | Same result within a single SQL statement | ⚠️ Mostly safe (but value may shift between refreshes) |
| **VOLATILE** | Result can change at any time, even within a statement | ❌ No — breaks delta computation |

### Current state

- `node_to_expr()` in `parser.rs` parses `FuncCall` nodes and stores them as `Expr::FuncCall { func_name, args }`
- No volatility check is performed anywhere
- No warning or error is emitted for volatile functions
- Both FULL and DIFFERENTIAL modes accept any function silently

---

## Implementation Plan

### Part 1 — Volatility Checking Infrastructure

#### Step 1: Add a volatility lookup function

Create a utility function that resolves a function name to its volatility class using `pg_catalog.pg_proc`:

```rust
/// Look up the volatility category of a PostgreSQL function.
///
/// Returns 'i' (immutable), 's' (stable), or 'v' (volatile).
/// Returns 'v' (volatile) if the function cannot be found (safe default).
fn lookup_function_volatility(func_name: &str) -> Result<char, PgStreamError> {
    Spi::connect(|client| {
        let result = client.select(
            "SELECT provolatile::text FROM pg_catalog.pg_proc \
             WHERE proname = $1 LIMIT 1",
            None,
            &[&func_name],
        )?;
        if let Some(row) = result.first() {
            let vol: String = row.get_by_name("provolatile")?.unwrap_or("v".into());
            Ok(vol.chars().next().unwrap_or('v'))
        } else {
            Ok('v') // Unknown function → assume volatile
        }
    })
}
```

**Note on overloaded functions:** PostgreSQL allows multiple functions with the same name but different argument types. The lookup should ideally resolve using the full signature (function name + argument types). For the initial implementation, matching on `proname` alone is acceptable — if any overload is volatile, we treat the function as volatile. A future refinement can resolve argument types from the parse tree for exact matching.

#### Step 2: Add a recursive Expr volatility scanner

Walk an `Expr` tree and collect all `FuncCall` nodes, then check each one:

```rust
/// Scan an Expr tree and return the "worst" volatility found.
///
/// Returns 'i' if all functions are immutable,
/// 's' if the worst is stable,
/// 'v' if any function is volatile.
fn worst_volatility(expr: &Expr) -> Result<char, PgStreamError> {
    let mut worst = 'i'; // Start optimistic
    collect_volatilities(expr, &mut worst)?;
    Ok(worst)
}

fn collect_volatilities(expr: &Expr, worst: &mut char) -> Result<(), PgStreamError> {
    match expr {
        Expr::FuncCall { func_name, args } => {
            let vol = lookup_function_volatility(func_name)?;
            *worst = max_volatility(*worst, vol);
            for arg in args {
                collect_volatilities(arg, worst)?;
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_volatilities(left, worst)?;
            collect_volatilities(right, worst)?;
        }
        // ... recurse into all Expr variants that contain sub-expressions
        _ => {}
    }
    Ok(())
}

fn max_volatility(a: char, b: char) -> char {
    // v > s > i
    match (a, b) {
        ('v', _) | (_, 'v') => 'v',
        ('s', _) | (_, 's') => 's',
        _ => 'i',
    }
}
```

### Part 2 — Validation at Parse Time

#### Step 3: Scan the OpTree for volatility after parsing

After `parse_query()` produces an `OpTree`, walk the tree and collect the worst volatility from all expressions (target list, WHERE, JOIN conditions, HAVING, GROUP BY expressions, window function arguments):

```rust
/// Walk an OpTree and return the worst volatility found in any expression.
fn tree_worst_volatility(tree: &OpTree) -> Result<char, PgStreamError> {
    let mut worst = 'i';
    match tree {
        OpTree::Filter { predicate, child } => {
            collect_volatilities(predicate, &mut worst)?;
            let child_vol = tree_worst_volatility(child)?;
            worst = max_volatility(worst, child_vol);
        }
        OpTree::Project { expressions, child, .. } => {
            for expr in expressions {
                collect_volatilities(expr, &mut worst)?;
            }
            let child_vol = tree_worst_volatility(child)?;
            worst = max_volatility(worst, child_vol);
        }
        // ... all other OpTree variants
        _ => {}
    }
    Ok(worst)
}
```

#### Step 4: Enforce volatility rules

Apply the following policy in `create_stream_table()`:

| Mode | Volatile | Stable | Immutable |
|------|----------|--------|-----------|
| **DIFFERENTIAL** | ❌ Reject | ⚠️ Warn | ✅ Allow |
| **FULL** | ⚠️ Warn | ✅ Allow | ✅ Allow |

```rust
let vol = tree_worst_volatility(&op_tree)?;

match (refresh_mode, vol) {
    (RefreshMode::Differential, 'v') => {
        return Err(PgStreamError::UnsupportedOperator(
            "Defining query contains volatile functions (e.g., random(), \
             clock_timestamp()). Volatile functions are not supported in \
             DIFFERENTIAL mode because they produce different values on \
             each evaluation, breaking delta computation. \
             Use FULL refresh mode instead, or replace with a \
             deterministic alternative.".into(),
        ));
    }
    (RefreshMode::Differential, 's') => {
        pgrx::warning!(
            "Defining query contains stable functions (e.g., now(), \
             current_timestamp). These return the same value within a \
             single refresh but may shift between refreshes. \
             Delta computation is correct within each refresh cycle."
        );
    }
    (RefreshMode::Full, 'v') => {
        pgrx::warning!(
            "Defining query contains volatile functions (e.g., random()). \
             Each FULL refresh will re-evaluate them, potentially producing \
             different results. This is expected behavior but may be surprising."
        );
    }
    _ => {} // Immutable or stable-in-FULL: no action
}
```

### Part 3 — Handling Specific Built-in Functions

Some built-in functions deserve special treatment:

| Function | Volatility | Recommendation |
|---|---|---|
| `now()`, `current_timestamp` | STABLE | Allow with warning — value is consistent within a refresh |
| `clock_timestamp()` | VOLATILE | Reject in DIFFERENTIAL |
| `random()` | VOLATILE | Reject in DIFFERENTIAL |
| `gen_random_uuid()` | VOLATILE | Reject in DIFFERENTIAL |
| `nextval()` | VOLATILE | Reject in DIFFERENTIAL |
| `txid_current()` | STABLE | Allow with warning |
| `pg_backend_pid()` | STABLE | Allow with warning |
| `COALESCE`, `NULLIF`, `GREATEST`, `LEAST` | IMMUTABLE | Allow — these are safe |

No special-casing is needed beyond the general volatility lookup — PostgreSQL already classifies these correctly in `pg_proc.provolatile`.

### Part 4 — Operator-Level Volatility (Implicit Functions)

Some SQL operators implicitly call functions with specific volatility:

- **Type casts** — `Expr::Raw("CAST(x AS type)")`: Casts use the type's input/output functions, which are typically IMMUTABLE. No action needed.
- **Operators** (`=`, `<`, `+`, etc.) — These resolve to `pg_operator` → `pg_proc`. The underlying functions are almost always IMMUTABLE. Can be checked in a future refinement.
- **Aggregate functions** — These are already restricted to a known set (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`). All are IMMUTABLE in their accumulation. No action needed.

**Decision:** For the initial implementation, only check explicit `FuncCall` nodes. Extend to operators and casts in a future iteration if needed.

---

## Files to Change

| File | Change |
|---|---|
| `src/dvm/parser.rs` | Add `lookup_function_volatility()`, `worst_volatility()`, `tree_worst_volatility()` |
| `src/api.rs` | Add volatility check after `parse_query()` in `create_stream_table()` |
| `src/error.rs` | No changes needed — `UnsupportedOperator` already covers this |
| `docs/SQL_REFERENCE.md` | Document volatile function behavior |
| `docs/DVM_OPERATORS.md` | Add a "Function Volatility" section |
| `README.md` | Update limitations table |

## Unit Tests

```rust
#[test]
fn test_max_volatility_ordering() {
    assert_eq!(max_volatility('i', 'i'), 'i');
    assert_eq!(max_volatility('i', 's'), 's');
    assert_eq!(max_volatility('s', 'v'), 'v');
    assert_eq!(max_volatility('v', 'i'), 'v');
}

#[test]
fn test_worst_volatility_immutable_only() {
    let expr = Expr::FuncCall {
        func_name: "lower".to_string(),
        args: vec![Expr::ColumnRef { table_alias: None, column_name: "name".into() }],
    };
    // Would need pg backend or mock — see note below
}
```

**Note:** `lookup_function_volatility` requires SPI (live PostgreSQL). Unit tests for the volatility scanner should either:
- Test the `max_volatility()` and `collect_volatilities()` helpers with mock data
- Use e2e tests for the full integration path

## E2E Tests

```rust
#[tokio::test]
async fn test_volatile_function_rejected_in_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE vol_src (id INT PRIMARY KEY)").await;

    let result = db.try_execute(
        "SELECT pgstream.create_stream_table('vol_dt', \
         $$ SELECT id, random() AS r FROM vol_src $$, '1m', 'DIFFERENTIAL')"
    ).await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("volatile") || msg.contains("random"));
}

#[tokio::test]
async fn test_volatile_function_allowed_in_full_mode() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE vol_full_src (id INT PRIMARY KEY)").await;
    db.execute("INSERT INTO vol_full_src VALUES (1)").await;

    // Should succeed (with warning) in FULL mode
    db.create_dt("vol_full_dt", "SELECT id, random() AS r FROM vol_full_src", "1m", "FULL").await;
    assert_eq!(db.count("public.vol_full_dt").await, 1);
}

#[tokio::test]
async fn test_stable_function_allowed_in_differential_with_warning() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE stable_src (id INT PRIMARY KEY, ts TIMESTAMPTZ DEFAULT now())").await;
    db.execute("INSERT INTO stable_src VALUES (1)").await;

    // now() is STABLE — allowed in DIFFERENTIAL with warning
    db.create_dt("stable_dt", "SELECT id, now() AS refresh_time FROM stable_src", "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.stable_dt").await, 1);
}

#[tokio::test]
async fn test_immutable_function_fully_allowed() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE imm_src (id INT PRIMARY KEY, name TEXT)").await;
    db.execute("INSERT INTO imm_src VALUES (1, 'HELLO')").await;

    db.create_dt("imm_dt", "SELECT id, lower(name) AS lname FROM imm_src", "1m", "DIFFERENTIAL").await;
    assert_eq!(db.count("public.imm_dt").await, 1);
}

#[tokio::test]
async fn test_nested_volatile_in_where_clause_rejected() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE nest_vol_src (id INT PRIMARY KEY, val FLOAT)").await;

    let result = db.try_execute(
        "SELECT pgstream.create_stream_table('nest_vol_dt', \
         $$ SELECT id, val FROM nest_vol_src WHERE val > random() $$, '1m', 'DIFFERENTIAL')"
    ).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_gen_random_uuid_rejected_in_differential() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE uuid_src (id INT PRIMARY KEY)").await;

    let result = db.try_execute(
        "SELECT pgstream.create_stream_table('uuid_dt', \
         $$ SELECT id, gen_random_uuid() AS uid FROM uuid_src $$, '1m', 'DIFFERENTIAL')"
    ).await;
    assert!(result.is_err());
}
```

---

## Estimated Effort

| Part | Effort | Description |
|------|--------|-------------|
| 1 — Volatility infrastructure | Medium | ~80 lines: SPI lookup + Expr walker |
| 2 — Validation at parse time | Low | ~30 lines: OpTree walker + policy enforcement |
| 3 — Built-in functions | None | Covered by general `pg_proc` lookup |
| 4 — Operators/casts | Low (future) | Optional refinement |
| Tests | Medium | 3-4 unit tests + 6 e2e tests |
| Docs | Low | ~20 lines across 3 files |

**Total:** ~1-2 hours of implementation.

---

## Edge Cases

1. **User-defined functions** — Handled automatically by `pg_proc` lookup. Users marking their functions with wrong volatility is their problem (same as PostgreSQL's own stance).

2. **Schema-qualified function names** — `myschema.my_func()` needs the lookup to handle schema qualification. Join against `pg_namespace` or use `regprocedure` resolution.

3. **Overloaded functions** — Multiple `pg_proc` rows for the same `proname`. Conservative approach: if any overload is volatile, treat as volatile. Better approach: resolve argument types for exact match.

4. **Functions in DEFAULT expressions** — Not relevant; DEFAULT is not part of the defining query.

5. **Functions in CTE bodies** — Already traversed because CTE bodies are parsed into OpTree nodes.

6. **Recursive CTEs** — In DIFFERENTIAL mode, recursive CTEs use recomputation diff (re-execute + anti-join). Volatile functions would produce different results on each re-execution. The volatility check should apply to recursive CTE bodies too.

7. **`Expr::Raw` passthrough** — Some expressions are stored as raw SQL strings (`Expr::Raw`). These cannot be introspected for function calls. Consider either: (a) parsing them more thoroughly, or (b) flagging `Expr::Raw` containing parentheses as potentially volatile (conservative warning).

---

## Prior Art — How Other Systems Handle Volatile Functions

The challenge of non-deterministic functions in incremental view maintenance is well-known. Different systems address it based on their underlying architecture.

### Compiled Dataflow Systems

**Feldera**, **Flink SQL**, and similar systems use a _compiled dataflow_ model: the defining query is compiled into a dataflow graph, and expressions (including function calls) are evaluated **once** as data enters the pipeline. The result becomes part of the record flowing through operators. This means volatile functions are naturally "captured" at ingestion — `random()` would produce a value once and that value propagates deterministically through the rest of the graph. Feldera's UDFs are compiled into the dataflow and are deterministic by construction.

**Flink SQL** distinguishes explicitly between deterministic and non-deterministic functions. Non-deterministic results are _pinned_ — evaluated once and materialized into the changelog stream. It also provides dedicated temporal primitives (`PROCTIME()` for processing time, `ROWTIME()` for event time) rather than relying on `now()`.

### Materialize

Materialize takes a hybrid approach. Truly volatile functions like `random()` are **rejected** in materialized views. For temporal semantics, Materialize introduces `mz_now()`, a special temporal filter that has well-defined semantics for incremental maintenance. PostgreSQL's `now()` is rewritten to `mz_now()` which can be efficiently maintained as a "temporal filter" (the system knows how to advance it). Stable functions are evaluated once per logical timestamp.

### ksqlDB

ksqlDB **rejects** non-deterministic functions in persistent (continuously maintained) queries. Pull queries (point-in-time, equivalent to FULL refresh) allow them since they re-execute from scratch each time.

### PipelineDB (discontinued)

PipelineDB evaluated expressions at trigger/insertion time, capturing volatile results once into the continuous view's storage. This is conceptually similar to Feldera's approach but implemented at the PostgreSQL trigger layer.

### Architectural Insight

The fundamental distinction is between:

| Architecture | Volatility Handling | Examples |
|---|---|---|
| **Compiled dataflow** | Expressions evaluated once at ingestion; results flow deterministically | Feldera, Flink SQL |
| **Temporal special-casing** | Reject volatile, rewrite temporal functions to special operators | Materialize |
| **Reject volatile** | Block non-deterministic functions in incremental modes | ksqlDB |
| **Capture at trigger** | Evaluate volatile functions once in the CDC trigger | PipelineDB |
| **SQL re-execution** | Re-executes the defining query; volatile functions produce different results each time | pg_stream (current) |

pg_stream uses SQL re-execution — the delta and merge CTEs re-evaluate expressions on each refresh. This means pg_stream **cannot** adopt the compiled-dataflow approach without a fundamental architecture change. The two viable options are:

1. **Reject volatile** (chosen approach) — simplest, safest, matches ksqlDB and Materialize's strategy
2. **Capture at trigger** — evaluate volatile expressions inside the CDC trigger and store the results; technically possible but extremely complex (requires partial evaluation of the defining query inside PL/pgSQL triggers)

The reject-volatile approach is the right choice for pg_stream's current architecture. If a future version introduces a compiled dataflow engine (per DBSP's formal model), volatile functions could be re-evaluated.

---

## Alternative Approaches Considered

### A: Blocklist of known volatile functions
Reject a hardcoded list (`random`, `gen_random_uuid`, `clock_timestamp`, `nextval`, etc.). **Rejected** because it wouldn't catch user-defined volatile functions and requires ongoing maintenance.

### B: Allow volatile everywhere, document the caveat
Simply document that volatile functions may produce incorrect results in DIFFERENTIAL mode. **Rejected** because silent data corruption is worse than a clear error.

### C: Snapshot volatile values at CDC time
Capture the volatile value once (at insert/update time in the trigger) and store it in the change buffer. This would make `random()` deterministic per-row. **Rejected** because it requires evaluating the defining query's expressions inside the trigger function, which is far too complex and brittle.

### D: Use `pg_stat_user_functions` or expression analysis
Use PostgreSQL's built-in expression analysis (`contain_volatile_functions()` in `src/backend/optimizer/util/clauses.c`). This is the most accurate approach but requires calling a C function via pgrx FFI. **Considered for future refinement** — the SPI-based approach is simpler and sufficient for the initial implementation.
