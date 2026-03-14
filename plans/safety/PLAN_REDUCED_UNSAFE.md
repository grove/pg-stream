# PLAN: Reduce Unsafe Code Surface

**Status:** Proposed
**Target milestone:** v0.7.0
**Last updated:** 2026-03-15

---

## Motivation

The project currently has **1,309 `unsafe {` blocks** across 6 source files.
98% of them are in `src/dvm/parser.rs` (1,286 blocks in 19 k lines) where
the DVM engine walks PostgreSQL parse-tree nodes via raw FFI pointers.

The remaining files contribute modestly:

| File | `unsafe {` blocks | Category |
|------|------------------:|----------|
| `src/dvm/parser.rs` | 1,286 | AST walking / deparsing |
| `src/api.rs` | 10 | Query parsing + walker callback |
| `src/scheduler.rs` | 5 | BGW entry points, sub-transactions |
| `src/wal_decoder.rs` | 4 | WAL reader setup |
| `src/shmem.rs` | 3 | Shared-memory initialization |
| `src/lib.rs` | 1 | PG global reads |

The project uses **Rust 2024 edition** with `#![deny(unsafe_op_in_unsafe_fn)]`
(see `src/lib.rs:18`), which means every unsafe operation inside an
`unsafe fn` still requires its own `unsafe {}` block. The count cannot be
lowered by relaxing lints — only by introducing safe abstractions or macros
that encapsulate the repeated patterns.

### Goals

1. **Reduce the unsafe block count by ≥ 60%** (~780+ blocks eliminated)
   without changing runtime behaviour.
2. **Consolidate safety reasoning** into a small number of well-documented
   helper functions and macros, each with a clear `// SAFETY:` contract.
3. **Make future parse-tree code easier to write and review** by providing
   idiomatic Rust patterns instead of raw pointer gymnastics.
4. **Zero functional regressions** — every existing test must pass unchanged.

### Non-goals

- Replacing pgrx's required `unsafe` APIs (`PgLwLock::new`, `PgAtomic::new`,
  `pg_shmem_init!`, `pg_sys::process_shared_preload_libraries_in_progress`,
  background worker `#[unsafe(no_mangle)]`). These are structurally required
  by the pgrx framework and cannot be abstracted away.
- Rewriting the parser to avoid FFI entirely (e.g. using a pure-Rust SQL
  parser). That would be a separate, much larger project.
- Changing the `unsafe_op_in_unsafe_fn` deny lint. The current setting is
  deliberately strict.

---

## Current Patterns — Root Cause Analysis

Five repetitive patterns account for ~95% of all unsafe blocks in `parser.rs`:

### P1 — Node type check + downcast (×239 sites → 478 unsafe blocks)

```rust
if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_FuncCall) } {
    let fcall = unsafe { &*(node as *const pg_sys::FuncCall) };
    // safe code using fcall
}
```

Every call-site needs two `unsafe` blocks: one for `is_a`, one for the cast.

### P2 — `PgList::from_pg` (×166 sites)

```rust
let list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(some_ptr.field) };
```

Every list extraction from a pg_sys struct field requires an unsafe block even
though `from_pg` handles null internally.

### P3 — `CStr::from_ptr` for C string reads (×69 sites)

```rust
let name = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
    .to_str()
    .map_err(|_| PgTrickleError::QueryParseError("..."))?;
```

### P4 — `raw_parser()` calls (×49 sites)

```rust
let c_sql = CString::new(sql)?;
let raw = unsafe {
    pg_sys::raw_parser(c_sql.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT)
};
```

Repeated boilerplate: CString conversion, null check, wrap in PgList.

### P5 — `node_to_expr` recursive descent (×200 call-sites)

Most calls to `node_to_expr` are already inside `unsafe fn` bodies, so each
call needs `unsafe { node_to_expr(ptr) }`. If `node_to_expr` were safe (or
called through a safe wrapper), these blocks would vanish.

---

## Implementation Steps

Six phases, each independently shippable. Every phase must leave the tree
green (`just lint && just test-unit && just test-integration`).

---

### Phase 1 — `pg_cstr_to_str()` helper

**Priority:** P1
**Effort:** Small (1 hour)
**Estimated reduction:** ~69 blocks

#### Description

Introduce a safe helper that encapsulates the null-check + `CStr::from_ptr` +
UTF-8 conversion pattern.

#### API

```rust
// In src/dvm/parser.rs (module-private)

/// Convert a PostgreSQL C string pointer to a Rust `&str`.
///
/// Returns `Err` if the pointer is null or the bytes are not valid UTF-8.
fn pg_cstr_to_str<'a>(ptr: *const std::ffi::c_char) -> Result<&'a str, PgTrickleError> {
    if ptr.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "NULL C string pointer".into(),
        ));
    }
    // SAFETY: Caller verified non-null. PostgreSQL identifiers and SQL
    // fragments stored in parse-tree nodes are always NUL-terminated C
    // strings allocated in a valid memory context.
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|_| PgTrickleError::QueryParseError("Invalid UTF-8 in C string".into()))
}
```

#### Migration pattern

Before:
```rust
let name = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
    .to_str()
    .map_err(|_| PgTrickleError::QueryParseError("Invalid relation name".into()))?;
```

After:
```rust
let name = pg_cstr_to_str(rv.relname)?;
```

#### Validation

- `just fmt && just lint` — zero warnings
- `just test-unit` — all pass
- `just test-integration` — all pass
- `scripts/unsafe_inventory.sh --report-only` — parser.rs count drops by ~69

---

### Phase 2 — `pg_list()` helper

**Priority:** P1
**Effort:** Small (1 hour)
**Estimated reduction:** ~166 blocks

#### Description

Introduce a safe wrapper around `PgList::from_pg` that handles null and
non-null pointers identically (pgrx already handles null, but the call is
marked unsafe).

#### API

```rust
/// Convert a PostgreSQL `List *` to a safe `PgList<T>`.
///
/// Null pointers yield an empty list (consistent with pgrx behaviour).
fn pg_list<T>(raw: *mut pg_sys::List) -> pgrx::PgList<T> {
    // SAFETY: `PgList::from_pg` is safe for both null and valid list pointers
    // returned from the PostgreSQL parser. Null produces an empty list.
    unsafe { pgrx::PgList::<T>::from_pg(raw) }
}
```

#### Migration pattern

Before:
```rust
let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(fcall.args) };
```

After:
```rust
let args_list = pg_list::<pg_sys::Node>(fcall.args);
```

#### Validation

Same as Phase 1.

---

### Phase 3 — `cast_node!` macro

**Priority:** P0 — largest single reduction
**Effort:** Medium (3–4 hours, mechanical refactor)
**Estimated reduction:** ~478 blocks

#### Description

The `is_a` + pointer-cast pattern appears 239 times. Each instance uses two
`unsafe` blocks. A macro eliminates both at the call-site.

#### API

```rust
/// Attempt to downcast a `*mut pg_sys::Node` to a concrete parse-tree type.
///
/// Returns `Some(&T)` if the node's tag matches, `None` if the pointer is
/// null or the tag does not match.
///
/// # Safety contract (inside the macro)
///
/// - `pgrx::is_a` reads the node's tag field, which is valid for any
///   non-null `Node*` allocated by the PostgreSQL parser.
/// - The pointer cast is sound because `is_a` has verified the concrete
///   type matches `$tag`.
macro_rules! cast_node {
    ($node:expr, $tag:ident, $ty:ty) => {{
        let __n = $node;
        if !__n.is_null() && unsafe { pgrx::is_a(__n, pg_sys::NodeTag::$tag) } {
            Some(unsafe { &*(__n as *const $ty) })
        } else {
            None
        }
    }};
}
```

#### Migration pattern

Before:
```rust
if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_FuncCall) } {
    let fcall = unsafe { &*(node as *const pg_sys::FuncCall) };
    // ...body using fcall...
}
```

After:
```rust
if let Some(fcall) = cast_node!(node, T_FuncCall, pg_sys::FuncCall) {
    // ...body using fcall...
}
```

For `else if` chains (very common in `node_to_expr`, `walk_node_for_volatility`):

Before:
```rust
if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_FuncCall) } {
    let fcall = unsafe { &*(node as *const pg_sys::FuncCall) };
    // ...
} else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
    let bexpr = unsafe { &*(node as *const pg_sys::BoolExpr) };
    // ...
}
```

After:
```rust
if let Some(fcall) = cast_node!(node, T_FuncCall, pg_sys::FuncCall) {
    // ...
} else if let Some(bexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr) {
    // ...
}
```

#### Migration order

Apply to functions in order of descending unsafe density:

1. `node_to_expr` (11177–…) — ~80 casts
2. `walk_node_for_volatility` (2327–…) — ~25 casts
3. `collect_view_subs_from_item` — ~10 casts
4. `from_item_to_sql` — ~12 casts
5. `deparse_select_stmt_with_view_subs` — ~8 casts
6. All remaining `unsafe fn` bodies — ~100+ casts

Each function can be migrated and tested individually.

#### Validation

Same as Phase 1, plus spot-check that `cast_node!` expansion matches
the original semantics by comparing unsafe baseline before/after.

---

### Phase 4 — `parse_query()` helper

**Priority:** P1
**Effort:** Small (1–2 hours)
**Estimated reduction:** ~50 blocks (×2 per call-site: `raw_parser` + `PgList::from_pg`)

#### Description

Wrap the CString → `raw_parser` → null-check → `PgList` boilerplate.

#### API

```rust
/// Parse a SQL string into a list of `RawStmt` nodes.
///
/// Must be called within a PostgreSQL backend with a valid memory context.
fn parse_query(sql: &str) -> Result<pgrx::PgList<pg_sys::RawStmt>, PgTrickleError> {
    let c_sql = std::ffi::CString::new(sql)
        .map_err(|_| PgTrickleError::QueryParseError("Query contains null bytes".into()))?;
    // SAFETY: raw_parser is a PostgreSQL C function that is safe to call
    // within a backend process with a valid memory context.
    let raw_list = unsafe {
        pg_sys::raw_parser(c_sql.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT)
    };
    if raw_list.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "raw_parser returned NULL".into(),
        ));
    }
    Ok(pg_list::<pg_sys::RawStmt>(raw_list))
}
```

Many callers also immediately extract the first `SelectStmt`. A companion:

```rust
/// Parse SQL and extract the first top-level SelectStmt.
///
/// Returns `None` if the query is empty or the first statement is not a SELECT.
fn parse_select_stmt(
    sql: &str,
) -> Result<Option<*const pg_sys::SelectStmt>, PgTrickleError> {
    let stmts = parse_query(sql)?;
    let raw_stmt = match stmts.head() {
        Some(rs) => rs,
        None => return Ok(None),
    };
    // SAFETY: raw_stmt is a valid pointer from the parser.
    let node = unsafe { (*raw_stmt).stmt };
    if let Some(select) = cast_node!(node, T_SelectStmt, pg_sys::SelectStmt) {
        Ok(Some(select as *const pg_sys::SelectStmt))
    } else {
        Ok(None)
    }
}
```

#### Migration pattern

Before (repeated ~25 times):
```rust
let c_query = CString::new(query)
    .map_err(|_| PgTrickleError::QueryParseError("Query contains null bytes".into()))?;
let raw_list =
    unsafe { pg_sys::raw_parser(c_query.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) };
if raw_list.is_null() {
    return Err(PgTrickleError::QueryParseError("raw_parser returned NULL".into()));
}
let list = unsafe { pgrx::PgList::<pg_sys::RawStmt>::from_pg(raw_list) };
let raw_stmt = match list.head() {
    Some(rs) => rs,
    None => return Ok(false),
};
let node = unsafe { (*raw_stmt).stmt };
if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SelectStmt) } {
    return Ok(false);
}
let select = unsafe { &*(node as *const pg_sys::SelectStmt) };
```

After:
```rust
let select = match parse_select_stmt(query)? {
    Some(s) => s,
    None => return Ok(false),
};
// SAFETY: pointer is valid for the duration of the current memory context.
let select = unsafe { &*select };
```

#### Validation

Same as Phase 1.

---

### Phase 5 — `SubTransaction` RAII guard

**Priority:** P2
**Effort:** Small (1 hour)
**Estimated reduction:** ~10 blocks from `scheduler.rs`
**File:** `src/scheduler.rs`

#### Description

The sub-transaction save-context / begin / rollback-or-commit pattern appears
3 times in `scheduler.rs`. A RAII wrapper consolidates the unsafe into one
place and adds automatic rollback on panic (Drop safety).

#### API

```rust
/// RAII guard for a PostgreSQL internal sub-transaction.
///
/// Automatically rolls back on drop if neither `commit()` nor `rollback()`
/// was called (panic safety).
struct SubTransaction {
    old_cxt: pg_sys::MemoryContext,
    old_owner: pg_sys::ResourceOwner,
    finished: bool,
}

impl SubTransaction {
    /// Begin a new sub-transaction within the current worker transaction.
    fn begin() -> Self {
        // SAFETY: Called within a PostgreSQL worker transaction. The current
        // memory context and resource owner are valid.
        let old_cxt = unsafe { pg_sys::CurrentMemoryContext };
        let old_owner = unsafe { pg_sys::CurrentResourceOwner };
        // SAFETY: BeginInternalSubTransaction sets up a sub-transaction
        // using PostgreSQL's resource-owner mechanism.
        unsafe { pg_sys::BeginInternalSubTransaction(std::ptr::null()) };
        Self {
            old_cxt,
            old_owner,
            finished: false,
        }
    }

    /// Commit the sub-transaction and restore the outer context.
    fn commit(mut self) {
        // SAFETY: Commits the sub-transaction, restores context. Called
        // only once before drop.
        unsafe {
            pg_sys::ReleaseCurrentSubTransaction();
            pg_sys::MemoryContextSwitchTo(self.old_cxt);
            pg_sys::CurrentResourceOwner = self.old_owner;
        }
        self.finished = true;
    }

    /// Roll back the sub-transaction and restore the outer context.
    fn rollback(mut self) {
        // SAFETY: Rolls back the sub-transaction, restores context. Called
        // only once before drop.
        unsafe {
            pg_sys::RollbackAndReleaseCurrentSubTransaction();
            pg_sys::MemoryContextSwitchTo(self.old_cxt);
            pg_sys::CurrentResourceOwner = self.old_owner;
        }
        self.finished = true;
    }
}

impl Drop for SubTransaction {
    fn drop(&mut self) {
        if !self.finished {
            // Auto-rollback on drop for panic safety.
            // SAFETY: Same invariants as rollback().
            unsafe {
                pg_sys::RollbackAndReleaseCurrentSubTransaction();
                pg_sys::MemoryContextSwitchTo(self.old_cxt);
                pg_sys::CurrentResourceOwner = self.old_owner;
            }
        }
    }
}
```

#### Migration pattern

Before:
```rust
let old_cxt = unsafe { pg_sys::CurrentMemoryContext };
let old_owner = unsafe { pg_sys::CurrentResourceOwner };
unsafe { pg_sys::BeginInternalSubTransaction(std::ptr::null()) };

// ... do work ...

// On failure:
unsafe {
    pg_sys::RollbackAndReleaseCurrentSubTransaction();
    pg_sys::MemoryContextSwitchTo(old_cxt);
    pg_sys::CurrentResourceOwner = old_owner;
}
return result;

// On success:
unsafe {
    pg_sys::ReleaseCurrentSubTransaction();
    pg_sys::MemoryContextSwitchTo(old_cxt);
    pg_sys::CurrentResourceOwner = old_owner;
}
```

After:
```rust
let subtxn = SubTransaction::begin();

// ... do work ...

// On failure:
subtxn.rollback();
return result;

// On success:
subtxn.commit();
```

#### Validation

Same as Phase 1, plus `just test-e2e` for scheduler-related tests.

---

### Phase 6 — Convert `unsafe fn` to safe fn where possible

**Priority:** P2
**Effort:** Large (4–6 hours)
**Estimated reduction:** ~100–200 blocks (depends on how many functions qualify)

#### Description

After Phases 1–4 introduce safe helpers, many functions currently declared
`unsafe fn` may no longer contain any direct unsafe operations — all their
unsafe code has been pushed into the helpers. These functions can be converted
from `unsafe fn` to safe `fn`, which eliminates the need for `unsafe { }` at
every call-site.

#### Approach

1. After applying Phases 1–4, audit each `unsafe fn` in `parser.rs`:
   - If the function body contains **zero** remaining `unsafe` blocks, convert
     to a safe `fn`.
   - If the function body's only remaining `unsafe` is calls to other
     `unsafe fn`s that themselves qualify for conversion, convert bottom-up.
2. For each converted function, remove `unsafe { }` wrappers at all call-sites.

#### Candidate functions (to be confirmed after Phases 1–4)

These functions are likely convertible because their bodies are dominated by
patterns P1–P4:

- `query_has_cte_inner` / `query_has_recursive_cte_inner` — just
  `raw_parser` + `is_a` + cast (all wrapped by Phase 3–4 helpers)
- `collect_view_substitutions` — mainly `is_a` + cast + `from_pg`
- `collect_view_subs_from_item` — mainly `is_a` + cast + `CStr::from_ptr`
- `check_for_matviews_or_foreign` / `check_from_item_for_matview_or_foreign`
- `collect_from_clause_table_names` / `collect_table_names_from_node`
- `check_select_unsupported` / `check_from_item_unsupported`
- `walk_from_for_limit_warning` / `check_from_item_limit_warning`
- `extract_const_int_from_node` / `is_limit_all_node`

Each of the 84 `unsafe fn` declarations in `parser.rs` should be reviewed.
Functions that still require raw pointer dereferences beyond the helpers
(e.g. `deparse_select_stmt_with_view_subs` which accesses many struct fields
directly) will remain `unsafe fn`.

#### Validation

Same as Phase 1. The compiler enforces soundness — if a safe fn tries to do
something unsafe without a block, it won't compile.

---

## Summary — Projected Impact

| Phase | Helper / Change | Blocks eliminated | Cumulative |
|-------|----------------|------------------:|-----------:|
| 1 | `pg_cstr_to_str()` | ~69 | ~69 |
| 2 | `pg_list()` | ~166 | ~235 |
| 3 | `cast_node!` macro | ~478 | ~713 |
| 4 | `parse_query()` + `parse_select_stmt()` | ~50 | ~763 |
| 5 | `SubTransaction` RAII | ~10 | ~773 |
| 6 | `unsafe fn` → safe `fn` conversion | ~100–200 | **~873–973** |

**Projected final count:** ~336–436 `unsafe {` blocks (down from 1,309).
**Projected reduction:** 67–74%.

The remaining ~336–436 blocks will be:

- Inside the helper functions themselves (correctly concentrated)
- In `node_to_expr` recursive calls that still need `unsafe` for ptr derefs
  of struct fields not covered by the helpers
- In `deparse_*` functions that build SQL from struct field access
- Framework-required unsafe in `shmem.rs`, `lib.rs`, `wal_decoder.rs`
- Background worker entry points (`#[unsafe(no_mangle)]`)

---

## Implementation Order & Dependencies

```
Phase 1 (pg_cstr_to_str)  ─────────────────╮
Phase 2 (pg_list)          ─────────────────┤
Phase 5 (SubTransaction)   ─────────────────┤ (all independent)
                                            │
Phase 3 (cast_node!)       ─────────────────┤ (independent but largest)
                                            │
Phase 4 (parse_query)      ─── depends on → Phase 2 + Phase 3
                                            │
Phase 6 (unsafe fn→fn)     ─── depends on → Phases 1–4
```

Phases 1, 2, 3, and 5 are fully independent and could be done in parallel.
Phase 4 composes `pg_list` (Phase 2) and `cast_node!` (Phase 3).
Phase 6 requires all helpers to be in place to determine which functions
qualify for conversion.

---

## Risks & Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Mechanical refactor introduces a bug | Low | All existing tests cover the same code paths. Run full `just test-all` after each phase. |
| `cast_node!` changes control flow subtly | Low | `if unsafe { is_a(…) } { let x = … }` → `if let Some(x) = cast_node!(…)` is semantically identical. The early-return/fallthrough semantics are preserved by `else if let`. |
| `pg_cstr_to_str` null check changes error path | Low | Current code either doesn't check null or checks it inconsistently. The helper adds a consistent null check, which is strictly safer. Review each site for cases where null was intentionally tolerated. |
| Performance impact from helper function call overhead | Negligible | All helpers are small enough for the compiler to inline. Can add `#[inline]` if profiling shows impact. |
| `SubTransaction::drop` auto-rollback masks bugs | Low | Existing code has no drop safety — a panic during sub-transaction work would already leave PG in a bad state. The guard strictly improves the situation. |
| Baseline drift during implementation | Low | Update `.unsafe-baseline` after each phase via `scripts/unsafe_inventory.sh --update`. |

---

## Verification Checklist (per phase)

- [ ] `just fmt` passes
- [ ] `just lint` passes with zero warnings
- [ ] `just test-unit` passes
- [ ] `just test-integration` passes
- [ ] `scripts/unsafe_inventory.sh --report-only` shows expected reduction
- [ ] `scripts/unsafe_inventory.sh --update` committed with new baseline
- [ ] No new `unwrap()` or `panic!()` introduced in non-test code
- [ ] All new helpers have `// SAFETY:` documentation on every `unsafe` block
- [ ] Git commit per phase with descriptive message

---

## Future Work (out of scope)

- **Typed node enum:** A Rust enum mirroring PostgreSQL's `NodeTag` would
  eliminate raw pointer casts entirely, but would be a large API surface to
  maintain across PG versions. Worth revisiting if pgrx adds this upstream.
- **Pure-Rust SQL parser:** Libraries like `sqlparser-rs` could eliminate
  FFI entirely for the analysis pass, but would diverge from PostgreSQL's
  exact parse semantics. Evaluated and deferred — see
  [REPORT_ENGINE_COMPOSABILITY.md](../infra/REPORT_ENGINE_COMPOSABILITY.md).
- **`unsafe_inventory.sh` CI enforcement:** Already in CI. After this plan
  lands, the baseline will reflect the lower counts and regressions will be
  caught automatically.
