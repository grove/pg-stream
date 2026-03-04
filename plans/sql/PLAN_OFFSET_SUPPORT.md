# PLAN: OFFSET Support (ORDER BY + LIMIT + OFFSET)

**Status:** In progress — Core implementation complete. Pending: clippy, E2E tests, upgrade migration SQL.
**Effort:** ~4–8 hours (leverages existing TopK infrastructure)

---

## Problem Statement

`OFFSET` is currently rejected in all defining-query contexts:

```sql
-- ❌ Rejected today
SELECT * FROM orders ORDER BY created_at DESC LIMIT 10 OFFSET 20

-- ❌ Also rejected
SELECT * FROM orders ORDER BY created_at DESC OFFSET 10
```

The original rationale (documented in the FAQ and PLAN_ORDER_BY_LIMIT_OFFSET)
was that OFFSET "destroys boundary stability" — every source change can shift
which rows fall inside the window. However, this argument applies to
**delta-based incremental maintenance**, not to **scoped recomputation**.

Since TopK already uses scoped recomputation (re-execute the full query and
MERGE the result), adding OFFSET to the query has **zero additional
complexity** in the refresh path.

---

## Analysis

### Why OFFSET was originally rejected

The FAQ states:

> TopK without OFFSET has a stable boundary: only rows that cross the N-th
> position threshold change membership. OFFSET destroys this stability.

This is true for delta-based IVM (Option D in PLAN_ORDER_BY_LIMIT_OFFSET —
the overflow-buffer approach). But TopK was implemented as Option B — scoped
recomputation — which doesn't reason about boundaries at all. It simply:

1. Re-executes `SELECT ... ORDER BY ... LIMIT N` against source tables
2. MERGEs the result into the stream table

The MERGE is agnostic to what the query returns. Whether it's `LIMIT 10` or
`LIMIT 10 OFFSET 20`, the flow is identical.

### Impact on future optimizations

PLAN_ORDER_BY_LIMIT_OFFSET describes two future optimizations:

| Optimization | Impact of OFFSET |
|---|---|
| **Option C — Boundary tracking** | With `LIMIT N` alone, you track one boundary (the N-th row's sort key). With `OFFSET M LIMIT N`, you need two boundaries (the M-th and M+N-th). This doubles the boundary-check logic but doesn't fundamentally break it. |
| **Option D — Overflow buffer** | More complex: the overflow buffer must cover positions before the OFFSET too. Would need rethinking. Unlikely to be implemented for OFFSET. |

Neither optimization is implemented or planned near-term. The scoped
recomputation path handles OFFSET correctly with no additional cost.

### Three OFFSET patterns to consider

| Pattern | Example | Bounded? | Useful? |
|---|---|---|---|
| **ORDER BY + LIMIT + OFFSET** | `ORDER BY score DESC LIMIT 10 OFFSET 20` | Yes (N rows) | Yes — "page" or "window" into ranked results |
| **ORDER BY + OFFSET** (no LIMIT) | `ORDER BY score DESC OFFSET 10` | No (all rows minus M) | Marginal — large, unbounded result set |
| **OFFSET without ORDER BY** | `OFFSET 10` | No | No — non-deterministic, already rejected |

### Recommendation

Support **`ORDER BY + LIMIT + OFFSET`** (the "Paged TopK" pattern). Reject
the other two:

- **`ORDER BY + OFFSET` without LIMIT** materializes a potentially huge result
  set (total rows minus M). This negates the TopK benefit of bounded storage
  and bounded refresh cost. Reject with a clear message suggesting adding
  LIMIT.

- **`OFFSET` without ORDER BY** remains non-deterministic and is already
  rejected.

---

## Use Cases

| Use Case | Query Pattern |
|---|---|
| Paginated dashboards | `ORDER BY created_at DESC LIMIT 20 OFFSET 40` — "page 3" |
| Excluding outliers | `ORDER BY score DESC LIMIT 50 OFFSET 5` — top 50, skipping top 5 |
| Windowed leaderboards | `ORDER BY points DESC LIMIT 10 OFFSET 10` — "second tier" |
| Cascading pages | Multiple stream tables with different OFFSETs for different pages |

### Caveat for users

When source data changes, the "page" shifts — a row on page 3 may move to
page 2 or 4. This is semantically correct (the stream table always reflects
the current state of the page) but may surprise users who expect row
stability. This should be documented clearly.

---

## Implementation Plan

### Step 1: Parser — accept ORDER BY + LIMIT + OFFSET

**File:** `src/dvm/parser.rs`

**Current behavior in `detect_topk_pattern()`:**
```rust
if !select.limitOffset.is_null() {
    return Err(PgTrickleError::UnsupportedOperator(
        "OFFSET is not supported with LIMIT in defining queries. ..."
    ));
}
```

**Change:** When ORDER BY + LIMIT + OFFSET are all present, extract the
OFFSET value (must be a constant non-negative integer, same as LIMIT):

```rust
let offset_value = if !select.limitOffset.is_null() {
    match unsafe { extract_const_int_from_node(select.limitOffset) } {
        Some(v) if v >= 0 => Some(v),
        Some(_) => return Err(PgTrickleError::UnsupportedOperator(
            "OFFSET must be a non-negative integer.".into()
        )),
        None => return Err(PgTrickleError::UnsupportedOperator(
            "OFFSET in defining queries must be a constant integer.".into()
        )),
    }
} else {
    None
};
```

Extend `TopKInfo` to include the offset:

```rust
pub struct TopKInfo {
    pub limit_value: i64,
    pub offset_value: Option<i64>,  // NEW
    pub full_query: String,
    pub base_query: String,
    pub order_by_sql: String,
}
```

**`reject_limit_offset()`** — currently rejects OFFSET unconditionally. Change
to only reject OFFSET when ORDER BY + LIMIT are not present (the non-TopK
path):

```rust
// OFFSET without ORDER BY + LIMIT is not supported.
// If ORDER BY + LIMIT + OFFSET → handled by detect_topk_pattern().
if !select.limitOffset.is_null() {
    // Only reject if this is not a TopK candidate.
    // TopK detection runs first and validates OFFSET there.
    return Err(PgTrickleError::UnsupportedOperator(
        "OFFSET is not supported in defining queries without ORDER BY + LIMIT. \
         Use ORDER BY + LIMIT + OFFSET for paged TopK stream tables."
            .into(),
    ));
}
```

Since `detect_topk_pattern()` runs before `reject_limit_offset()` in the API
flow, and TopK tables bypass the DVM pipeline (which has its own
reject_limit_offset in step 7), no additional changes are needed in the
rejection flow.

---

### Step 2: Catalog — store offset metadata

**File:** `src/catalog.rs`

Add a new nullable column to `pgtrickle.pgt_stream_tables`:

```sql
ALTER TABLE pgtrickle.pgt_stream_tables
  ADD COLUMN topk_offset INT;  -- NULL = no OFFSET (same as OFFSET 0)
```

Add field to `StreamTableMeta`:
```rust
pub topk_offset: Option<i32>,
```

Update `insert()`, `extract_meta()`, and all SELECT queries to include the
new column.

**Migration SQL** (for `pg_trickle--{current}--{next}.sql`):
```sql
ALTER TABLE pgtrickle.pgt_stream_tables ADD COLUMN topk_offset INT;
```

---

### Step 3: Refresh — include OFFSET in reconstructed query

**File:** `src/refresh.rs` — `execute_topk_refresh()`

Current query reconstruction:
```rust
let topk_query = format!(
    "{} ORDER BY {} LIMIT {}",
    st.defining_query, topk_order_by, topk_limit
);
```

Change to:
```rust
let topk_query = if let Some(offset) = st.topk_offset {
    format!(
        "{} ORDER BY {} LIMIT {} OFFSET {}",
        st.defining_query, topk_order_by, topk_limit, offset
    )
} else {
    format!(
        "{} ORDER BY {} LIMIT {}",
        st.defining_query, topk_order_by, topk_limit
    )
};
```

No other refresh logic changes needed — the MERGE is agnostic to the source
query shape.

---

### Step 4: API — pass offset through

**File:** `src/api.rs`

When calling `StreamTableMeta::insert()`, pass `topk_info.offset_value`:

```rust
StreamTableMeta::insert(
    // ... existing args ...
    topk_info.as_ref().map(|t| t.limit_value as i32),
    topk_info.as_ref().map(|t| t.order_by_sql.as_str()),
    topk_info.as_ref().and_then(|t| t.offset_value.map(|v| v as i32)),  // NEW
    // ...
);
```

---

### Step 5: `strip_order_by_and_limit` → also strip OFFSET

**File:** `src/dvm/parser.rs`

The existing `strip_order_by_and_limit()` function removes ORDER BY and LIMIT
from the query to produce the `base_query`. It must also strip OFFSET. Rename
to `strip_order_by_limit_offset()` or extend the existing implementation.

---

### Step 6: Tests

**E2E tests** (add to `tests/e2e_topk_tests.rs`):

| Test | Scenario |
|------|----------|
| `test_topk_with_offset_create` | `ORDER BY score DESC LIMIT 10 OFFSET 5` — verify 10 rows stored, starting from position 6 |
| `test_topk_with_offset_differential` | Insert/delete rows, verify page shifts correctly after refresh |
| `test_topk_with_offset_zero` | `OFFSET 0` — equivalent to no offset |
| `test_topk_offset_without_limit_rejected` | `ORDER BY x OFFSET 10` (no LIMIT) → error |
| `test_topk_offset_without_order_by_rejected` | `LIMIT 10 OFFSET 5` (no ORDER BY) → error |
| `test_topk_offset_non_constant_rejected` | `OFFSET (SELECT ...)` → error |
| `test_topk_offset_negative_rejected` | `OFFSET -1` → error |
| `test_topk_offset_with_aggregates` | `GROUP BY dept ORDER BY SUM(x) DESC LIMIT 5 OFFSET 2` |
| `test_topk_offset_catalog_metadata` | Verify `topk_offset` stored correctly in catalog |
| `test_topk_offset_fetch_syntax` | `FETCH NEXT 10 ROWS ONLY OFFSET 5` |

**Unit tests** in `parser.rs`:
- `detect_topk_pattern` with OFFSET returns correct `offset_value`
- `strip_order_by_limit_offset` removes all three clauses

---

### Step 7: Documentation updates

| File | Change |
|------|--------|
| `docs/FAQ.md` | Update "Why is OFFSET not supported with TopK?" → explain it's now supported. Add caveat about page shifting. |
| `docs/SQL_REFERENCE.md` | Update create_stream_table docs to mention OFFSET support with TopK. |
| `docs/DVM_OPERATORS.md` | Update line 12: `LIMIT / OFFSET` → clarify OFFSET is allowed with ORDER BY + LIMIT. |
| `README.md` | Update the SQL support table row for LIMIT/OFFSET. |
| `CHANGELOG.md` | Add entry for OFFSET support. |

---

## Task Checklist

- [x] Parser: accept OFFSET in `detect_topk_pattern()` (Step 1)
- [x] Parser: update `reject_limit_offset()` flow (Step 1)
- [x] Parser: extend `strip_order_by_and_limit()` to strip OFFSET (Step 5)
- [x] Parser: extend `TopKInfo` with `offset_value` field (Step 1)
- [x] Catalog: add `topk_offset` column to schema DDL in `lib.rs` (Step 2)
- [x] Catalog: update `StreamTableMeta` struct + CRUD (Step 2)
- [x] Refresh: include OFFSET in query reconstruction (Step 3)
- [x] API: pass offset through to catalog insert (Step 4)
- [x] E2E tests: 8 new tests + 2 updated rejection tests (Step 6)
- [x] Documentation: FAQ, SQL_REFERENCE, DVM_OPERATORS, README (Step 7)
- [x] `cargo fmt -- --check` clean
- [ ] `just lint` (clippy) — blocked by pgrx build env on dev machine; CI will validate
- [ ] `just test-e2e` — requires Docker E2E image build
- [ ] Upgrade migration SQL (`sql/pg_trickle--0.2.1--0.2.2.sql`) — add `topk_offset` column
- [ ] Unit tests: parser tests for OFFSET handling in `parser.rs` `#[cfg(test)]`
- [ ] CHANGELOG entry

---

## What Remains (Prioritized)

| # | Item | Priority | Notes |
|---|------|----------|-------|
| 1 | **clippy validation** | P0 | Blocked locally (no pgrx env); CI will catch issues |
| 2 | **E2E test run** | P0 | `just build-e2e-image && just test-e2e` to validate all 8 new OFFSET tests |
| 3 | **Upgrade migration SQL** | P1 | `ALTER TABLE pgtrickle.pgt_stream_tables ADD COLUMN topk_offset INT;` in the next version migration file |
| 4 | **CHANGELOG entry** | P1 | Add under next release version |
| 5 | **Unit tests** | P2 | Parser-level tests for `detect_topk_pattern` with OFFSET; nice-to-have since E2E covers the full path |

---

## Complexity Assessment

| Component | Effort | Risk |
|---|---|---|
| Parser changes | 1–2 hours | Low — extends existing TopK detection |
| Catalog schema + migration | 1 hour | Low — single nullable column |
| Refresh path | 30 min | Very low — one conditional format string |
| API plumbing | 30 min | Low — pass-through |
| Tests | 2–3 hours | Low — follows existing TopK test patterns |
| Documentation | 1 hour | Low |
| **Total** | **~4–8 hours** | **Low** |

The low effort is because this is purely an extension of the existing TopK
infrastructure. No new operators, no new refresh strategies, no query
rewriting — just one more clause in the reconstructed query.

---

## What We're NOT Doing

- **`ORDER BY + OFFSET` without LIMIT** — rejected because the result set is
  unbounded. Users should add LIMIT.
- **Delta-based OFFSET maintenance** — not needed. Scoped recomputation
  handles it correctly.
- **Boundary tracking optimization (Option C) for OFFSET** — deferred. Can be
  added later as a pure optimization. Would need to track two sort-key
  boundaries instead of one.
- **IMMEDIATE mode + OFFSET** — rejected (same as TopK without OFFSET).
  Scoped recomputation is incompatible with within-transaction IVM.

---

## Further Reading

- [PLAN_ORDER_BY_LIMIT_OFFSET.md](PLAN_ORDER_BY_LIMIT_OFFSET.md) — Original
  TopK plan and alternatives analysis
- [FAQ: TopK section](../../docs/FAQ.md#topk-order-by--limit) — Current user
  documentation
