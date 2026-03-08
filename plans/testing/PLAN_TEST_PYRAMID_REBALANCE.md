# PLAN: Test Pyramid Rebalance

**Status:** Complete  
**Date:** 2026-03-05  
**Branch:** `plan-test-pyramid-rebalance`  
**Scope:** Shift test coverage down the pyramid — extract pure-logic unit tests
from SPI-heavy modules, introduce a light-E2E tier to eliminate the 20-minute
Docker build for most tests, and promote validation/error-path tests to unit
level.

---

## Progress Summary

| Phase | Status | New Unit Tests | Notes |
|-------|--------|:--------------:|-------|
| P1 — Test already-pure functions | **Done** | 27 | ivm, cdc |
| P2-A — IVM SQL builders | **Done** | 10 | Extracted `build_ivm_delete_sql`, `build_ivm_insert_sql`, `build_column_lists` |
| P2-B — Monitor tree renderer | **Done** | 8 | Extracted `render_dependency_tree` + `dfs` |
| P2-C — DDL event classification | **Done** | 12 | Extracted `classify_ddl_event` enum + `compare_snapshot_with_current` |
| P2-D — Scheduler decisions | **Done** | 11 | Extracted `is_group_due_pure` + `is_falling_behind` |
| P2-E — Alert payloads | **Done** | 4 | Extracted `build_alert_payload` |
| P2-F — CDC column lists | **Done** | 4 | Extracted `build_typed_col_defs` |
| P2-A4 — IVM trigger names | **Done** | 9 | Extracted `IvmTriggerNames` struct |
| P3 — Light-E2E tier | **Done** | — | Harness + CI + justfile targets |
| P4 — Validation tests to unit | **Done** | 55 | Revised scope (see below) |

**Total new unit tests: 133** (1,040 → 1,173)

### P3 Implementation

The light-E2E tier uses a **bind-mount + exec** approach:

1. `cargo pgrx package` produces compiled extension artifacts in
   `target/release/pg_trickle-pg18/`.
2. A stock `postgres:18.1` container starts with the artifacts bind-mounted
   to `/tmp/pg_ext`.
3. An `exec` copies the files to the PostgreSQL extension directories.
4. `CREATE EXTENSION pg_trickle` loads the extension on-demand.

**No custom Docker image, no `shared_preload_libraries`, no background worker.**

Files delivered:
- `tests/e2e/light.rs` — `LightE2eDb` harness (exported as `E2eDb` via feature gate)
- `tests/e2e/mod.rs` — Conditional compilation: `#[cfg(feature = "light-e2e")]`
- `Cargo.toml` — `light-e2e = []` feature
- `justfile` — `package-extension`, `test-light-e2e`, `test-light-e2e-fast`
- `.github/workflows/ci.yml` — `light-e2e-tests` job (runs on every PR)

42 test files (~570 tests) are light-eligible. 10 files (~90 tests) require
full E2E (bgworker, scheduler, bench tuning, upgrade, GUC variation).

### What Remains

All phases are complete. No remaining work items.

### P4 Scope Revision

The original P4 plan assumed LIMIT/OFFSET, FOR UPDATE, TABLESAMPLE, and
self-reference validation could be factored into pure `validate_*()` functions.
Investigation revealed these checks all call `raw_parser()` (PostgreSQL's C
parser via FFI) and **cannot be unit-tested without a DB backend**. They belong
in the Light-E2E tier (P3), not P4.

Instead, P4 was redirected to test **existing pure functions** that lacked
coverage — parser helpers, OpTree methods, and expression utilities:

| Sub-phase | Function | Tests |
|-----------|----------|------:|
| P4-A | `strip_order_by_and_limit()` | 7 |
| P4-B | `Expr::output_name()` | 5 |
| P4-C | `unwrap_transparent()` | 5 |
| P4-D | `OpTree::output_columns()` (8 variants) | 8 |
| P4-E | `OpTree::source_oids()` (6 variants) | 6 |
| P4-F | `split_and_predicates()` / `join_and_predicates()` | 7 |
| P4-G | `AggFunc::is_group_rescan()` | 10 |
| P4-H | `collect_volatilities()` expanded (COALESCE et al.) | 7 |

---

## Table of Contents

1. [Motivation](#motivation)
2. [Current State](#current-state)
3. [Phase 1 — Unit-Test Already-Pure Functions](#phase-1--unit-test-already-pure-functions)
4. [Phase 2 — Extract & Unit-Test Embedded Logic](#phase-2--extract--unit-test-embedded-logic)
5. [Phase 3 — Light-E2E Tier](#phase-3--light-e2e-tier)
6. [Phase 4 — Promote Validation Tests to Unit Level](#phase-4--promote-validation-tests-to-unit-level)
7. [Implementation Order](#implementation-order)
8. [Verification Criteria](#verification-criteria)

---

## Motivation

The test pyramid is bottom-heavy by raw count but structurally top-heavy in
two ways:

1. **660 E2E tests require a custom Docker image** that takes ~20 minutes to
   build, so they're **skipped on every PR**. Only 51 of those tests actually
   need `shared_preload_libraries`. The other 580+ tests run against the
   extension API but don't need the background worker — they could use a stock
   PG image with the compiled `.so` mounted in.

2. **Several large modules with complex logic have minimal unit tests.** Their
   correctness is validated exclusively by E2E tests, meaning feedback is slow
   and failures are hard to localize.

| Module | Lines | Unit Tests | Lines/Test |
|--------|------:|----------:|------------|
| `src/ivm.rs` | 922 | **0** | ∞ |
| `src/scheduler.rs` | 1,582 | 4 | 395 |
| `src/monitor.rs` | 1,836 | 5 | 367 |
| `src/cdc.rs` | 1,594 | 8 | 199 |
| `src/hooks.rs` | 1,583 | 8 | 198 |

By contrast the DVM engine is well-covered: `parser.rs` (279 tests),
`aggregate.rs` (110), `api.rs` (85), `dag.rs` (48).

---

## Current State

| Tier | Tests | Share | Build Overhead | Runs on PR? |
|------|------:|------:|----------------|:-----------:|
| Unit | 1,040 | 57% | None | Yes |
| Property | 27 | 2% | None | Yes |
| Integration (testcontainers, stock PG) | 81 | 4% | ~seconds | Yes |
| E2E (custom Docker image) | 660 | 37% | ~20 min | **No** |

---

## Phase 1 — Unit-Test Already-Pure Functions

These functions are already extracted and side-effect-free. They just need
test coverage. **Zero refactoring required.**

| ID | Module | Function | Lines | Est. Tests |
|----|--------|----------|-------|-----------|
| P1-1 | `src/ivm.rs` | `IvmLockMode::for_query()` | L71-82 | 4–6 |
| P1-2 | `src/ivm.rs` | `IvmLockMode::is_simple_scan_chain()` | L86-95 | 4–6 |
| P1-3 | `src/ivm.rs` | `hash_str()` | L130-134 | 2 |
| P1-4 | `src/cdc.rs` | `build_changed_cols_bitmask_expr()` | L311-339 | 5–8 |
| P1-5 | `src/cdc.rs` | `parse_partition_upper_bound()` | L605-613 | 4–6 |

**Effort:** ~30 min  
**Yield:** ~20 new unit tests

### P1-1 / P1-2 — IvmLockMode tests

`IvmLockMode::for_query()` delegates to `dvm::parse_defining_query()` (pure
parser) and `is_simple_scan_chain()` (pattern match on `OpTree`). Both are
fully testable without SPI.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_select_gets_row_exclusive() {
        assert_eq!(
            IvmLockMode::for_query("SELECT id, val FROM t"),
            IvmLockMode::RowExclusive,
        );
    }

    #[test]
    fn test_aggregate_gets_exclusive() {
        assert_eq!(
            IvmLockMode::for_query("SELECT dept, COUNT(*) FROM t GROUP BY dept"),
            IvmLockMode::Exclusive,
        );
    }

    #[test]
    fn test_join_gets_exclusive() {
        assert_eq!(
            IvmLockMode::for_query("SELECT a.id FROM a JOIN b ON a.id = b.id"),
            IvmLockMode::Exclusive,
        );
    }

    #[test]
    fn test_unparseable_query_defaults_to_exclusive() {
        assert_eq!(
            IvmLockMode::for_query("NOT VALID SQL {{{{"),
            IvmLockMode::Exclusive,
        );
    }
}
```

### P1-4 — build_changed_cols_bitmask_expr

Already a pure `fn(&[String], &[(String, String)]) -> Option<String>`. Test
the bitmask algebra:

```rust
#[test]
fn test_bitmask_single_non_pk_col() {
    let pk = vec!["id".to_string()];
    let cols = vec![
        ("id".to_string(), "integer".to_string()),
        ("val".to_string(), "text".to_string()),
    ];
    let expr = build_changed_cols_bitmask_expr(&pk, &cols);
    assert!(expr.is_some());
    // bit 1 set when "val" differs between NEW and OLD
}

#[test]
fn test_bitmask_none_when_over_63_cols() {
    let pk = vec!["id".to_string()];
    let cols: Vec<_> = (0..64)
        .map(|i| (format!("c{i}"), "int".to_string()))
        .collect();
    assert!(build_changed_cols_bitmask_expr(&pk, &cols).is_none());
}

#[test]
fn test_bitmask_none_when_no_pk() {
    let cols = vec![("a".to_string(), "int".to_string())];
    assert!(build_changed_cols_bitmask_expr(&[], &cols).is_none());
}
```

### P1-5 — parse_partition_upper_bound

```rust
#[test]
fn test_parse_valid_range() {
    assert_eq!(
        parse_partition_upper_bound("FOR VALUES FROM ('0/0') TO ('1/A3F')"),
        Some("1/A3F".to_string()),
    );
}

#[test]
fn test_parse_no_match() {
    assert_eq!(parse_partition_upper_bound("LIST (1, 2, 3)"), None);
}
```

---

## Phase 2 — Extract & Unit-Test Embedded Logic

These are blocks of pure logic currently inlined inside SPI-calling functions.
Each requires a small refactor: move the logic into a standalone `fn`, call
it from the original function, and add unit tests.

### P2-A — IVM SQL Builders (`src/ivm.rs`)

| ID | Function to Extract | From | Input → Output |
|----|---------------------|------|----------------|
| P2-A1 | `build_ivm_delete_sql(st, delta, keyless) → String` | `pgt_ivm_apply_delta` L555-600 | 2 table names + bool → SQL |
| P2-A2 | `build_ivm_insert_sql(st, delta, cols, keyless) → String` | `pgt_ivm_apply_delta` L604-643 | 2 table names + col list + bool → SQL |
| P2-A3 | `build_column_lists(cols) → (col_list, d_col_list, update_set)` | `pgt_ivm_apply_delta` + `apply_topk_micro_refresh` | `&[String]` → 3 SQL fragments |
| P2-A4 | `ivm_trigger_names(pgt_id, oid) → IvmTriggerNames` | `setup_ivm_triggers` (repeated 8×) | 2 ints → struct of 8 name strings |

**Effort:** ~60 min  
**Yield:** ~15-20 new unit tests

Example for P2-A1:

```rust
/// Build the DELETE SQL for IVM delta application.
///
/// Keyless sources use counted-DELETE (ROW_NUMBER matching) to avoid
/// deleting ALL duplicates when only a subset should be removed.
fn build_ivm_delete_sql(
    st_qualified: &str,
    delta_table: &str,
    has_keyless_source: bool,
) -> String { /* extracted from pgt_ivm_apply_delta */ }

#[cfg(test)]
mod tests {
    #[test]
    fn test_keyed_delete_uses_simple_join() {
        let sql = build_ivm_delete_sql(r#""public"."my_st""#, "__delta_1", false);
        assert!(sql.contains("USING"));
        assert!(sql.contains("__pgt_action = 'D'"));
        assert!(!sql.contains("ROW_NUMBER"));
    }

    #[test]
    fn test_keyless_delete_uses_row_number() {
        let sql = build_ivm_delete_sql(r#""public"."my_st""#, "__delta_1", true);
        assert!(sql.contains("ROW_NUMBER"));
        assert!(sql.contains("st_rn <= dc.del_count"));
    }
}
```

### P2-B — Monitor Tree Renderer (`src/monitor.rs`)

The `dependency_tree()` SQL-returning function contains a ~120-line inner
`dfs()` function that takes `HashMap<String, Vec<String>>` data structures
and produces ASCII tree rows using box-drawing characters. It has zero SPI
dependency.

| ID | Function to Extract | Lines | Input → Output |
|----|---------------------|-------|----------------|
| P2-B1 | `render_dependency_tree(st_info, children, sources) → Vec<TreeRow>` | L1260-1380 | 3 HashMaps → vec of formatted rows |

**Effort:** ~45 min  
**Yield:** ~8-10 tests (leaf, single-chain, diamond, forest topologies)

### P2-C — DDL Event Classification (`src/hooks.rs`)

| ID | Function to Extract | Lines | Input → Output |
|----|---------------------|-------|----------------|
| P2-C1 | `classify_ddl_event(object_type, command_tag) → DdlEventKind` | L131-178 | 2 strings → enum |
| P2-C2 | `detect_schema_change_pure(stored_entries, current_cols) → SchemaChangeKind` | L1430-1527 | JSON + HashMap → enum |

**Effort:** ~45 min  
**Yield:** ~10-12 tests

### P2-D — Scheduler Decision Logic (`src/scheduler.rs`)

| ID | Function to Extract | Lines | Input → Output |
|----|---------------------|-------|----------------|
| P2-D1 | `should_retry_db(had_scheduler, elapsed, skip_ttl, retry_ttl) → bool` | L191-201 | bools + durations → bool |
| P2-D2 | `is_group_due_pure(member_due, policy) → bool` | L812-833 | `&[bool]` + enum → bool |
| P2-D3 | `is_falling_behind(elapsed_ms, schedule_ms) → Option<f64>` | L1428-1448 | 2 ints → optional ratio |

**Effort:** ~30 min  
**Yield:** ~8-10 tests

### P2-E — Alert Payload Construction (`src/monitor.rs`)

| ID | Function to Extract | Lines | Input → Output |
|----|---------------------|-------|----------------|
| P2-E1 | `build_alert_payload(event, schema, name, extra) → String` | L71-93 | 4 strings → JSON |
| P2-E2 | `split_qualified_name(name) → (&str, &str)` | ~5 call sites | string → 2 strings |

**Effort:** ~20 min  
**Yield:** ~6-8 tests (escaping, truncation at 7900 chars, default schema)

### P2-F — CDC Column-List Generation (`src/cdc.rs`)

| ID | Function to Extract | Lines | Input → Output |
|----|---------------------|-------|----------------|
| P2-F1 | `build_trigger_column_lists(columns) → TriggerColumnLists` | L100-117 | column defs → 4 SQL fragments |
| P2-F2 | `build_typed_col_defs(columns) → String` | L357-367 | column defs → DDL fragment |

**Effort:** ~20 min  
**Yield:** ~6 tests

---

## Phase 3 — Light-E2E Tier

### Problem

88% of E2E tests (580 of 660) don't need `shared_preload_libraries`. They
call the extension API but never exercise the background worker/scheduler.
Yet they all require a custom Docker image that takes ~20 min to build,
meaning they are **skipped on every PR**.

### Solution — Bind-Mount + Exec (Implemented)

Instead of building a custom Docker image, the light-E2E harness:

1. Runs `cargo pgrx package` to produce compiled extension artifacts.
2. Starts a stock `postgres:18.1` container with the artifacts bind-mounted
   to `/tmp/pg_ext`.
3. Uses `container.exec()` to copy files to the PostgreSQL extension dirs.
4. Runs `CREATE EXTENSION pg_trickle` which loads the `.so` on-demand.

```
┌──────────────────────────────────────────────────────┐
│                   Before                             │
│                                                      │
│   Unit (1,164)  ──────► runs on PR ✓                 │
│   Integration (81)  ──► runs on PR ✓                 │
│   E2E (660)  ────────► skipped on PR ✗               │
│                        (20 min Docker build)         │
└──────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────┐
│                   After                              │
│                                                      │
│   Unit (1,164)  ──────► runs on PR ✓                 │
│   Integration (81)  ──► runs on PR ✓                 │
│   Light E2E (~570) ──► runs on PR ✓  ← NEW          │
│                        (stock PG + bind-mount)       │
│   Full E2E (~90)  ───► push-to-main + daily only     │
│                        (bgworker, bench, upgrade)    │
└──────────────────────────────────────────────────────┘
```

### Architecture

```rust
// tests/e2e/light.rs (exported as E2eDb via #[cfg(feature = "light-e2e")])

pub struct E2eDb {
    pub pool: PgPool,
    _container: ContainerAsync<GenericImage>,
}

impl E2eDb {
    pub async fn new() -> Self {
        let ext_dir = find_extension_dir(); // PGT_EXTENSION_DIR or default

        let container = GenericImage::new("postgres", "18.1")
            .with_mount(Mount::bind_mount(ext_dir, "/tmp/pg_ext"))
            .start().await;

        // Copy extension files from staging to system dirs
        container.exec(ExecCommand::new(vec!["sh", "-c",
            "cp /tmp/pg_ext/usr/share/postgresql/18/extension/pg_trickle* \
                /usr/share/postgresql/18/extension/ && \
             cp /tmp/pg_ext/usr/lib/postgresql/18/lib/pg_trickle* \
                /usr/lib/postgresql/18/lib/"
        ])).await;

        // Connect + CREATE EXTENSION
        // ...
    }
}
```

### Files That Must Stay in Full E2E

These 10 files (~90 tests) require `shared_preload_libraries`, the background
worker, `SET pg_trickle.*` GUCs, or custom image layering:

| File | Tests | Reason |
|------|------:|--------|
| `e2e_bench_tests.rs` | 18 | Uses `new_bench()` with shmem tuning |
| `e2e_bgworker_tests.rs` | 9 | Tests scheduler/bgworker lifecycle |
| `e2e_cascade_regression_tests.rs` | 8 | Uses `SET pg_trickle.*` GUCs |
| `e2e_dag_autorefresh_tests.rs` | 5 | `wait_for_auto_refresh` requires scheduler |
| `e2e_ddl_event_tests.rs` | 14 | Uses `SET pg_trickle.*` GUCs |
| `e2e_guc_variation_tests.rs` | 7 | Entirely about GUC variations |
| `e2e_multi_cycle_tests.rs` | 5 | Uses `wait_for_auto_refresh` |
| `e2e_tpch_tests.rs` | 6 | Heavy benchmarks, custom setup |
| `e2e_upgrade_tests.rs` | 13 | Tests version upgrade path, image layering |
| `e2e_user_trigger_tests.rs` | 10 | Uses `SET pg_trickle.*` for trigger control |

### Limitations

- **No background worker / scheduler** — `shared_preload_libraries` is not set.
- **No auto-refresh** — `wait_for_auto_refresh()` always returns `false`.
- **GUCs may be unavailable** — `SET pg_trickle.*` only works after the `.so`
  is loaded, and only in the same session.
- **macOS only works in CI** — `cargo pgrx package` on macOS produces `.dylib`
  files that can't run inside a Linux container. Local macOS development uses
  `just test-e2e-fast` instead.

### CI Impact

| Job | Before | After |
|-----|--------|-------|
| PR | Unit + Integration (1,164 + 81 tests) | Unit + Integration + Light E2E (~1,815 tests) |
| Push to main | Unit + Integration + E2E | Unit + Integration + Light E2E + Full E2E |
| Daily | All | All (unchanged) |

---

## Phase 4 — Promote Validation Tests to Unit Level

~70 E2E tests in `e2e_error_tests.rs` and `e2e_coverage_error_tests.rs`
test input validation: rejecting `LIMIT`, `FOR UPDATE`, self-references,
TABLESAMPLE, volatile functions, etc. The validation logic lives in the Rust
parser/API layer.

### Approach

For each validation check currently tested only by E2E:

1. Factor the check into a pure `fn validate_*(input) -> Result<(), PgTrickleError>`.
2. Add unit tests directly in the source module.
3. Keep the E2E test as a thin integration smoke test (or remove if redundant).

### Candidates

| Validation | Current Location | E2E Tests | Unit-Testable? |
|------------|-----------------|-----------|:--------------:|
| LIMIT/OFFSET rejection | `api.rs` → `validate_defining_query` | 3 | Yes — string inspection |
| FOR UPDATE rejection | `api.rs` | 1 | Yes |
| Self-reference detection | `api.rs` | 2 | Yes — name matching |
| TABLESAMPLE rejection | `dvm/parser.rs` | 1 | Yes — OpTree check |
| Volatile function detection | `dvm/parser.rs` | 2 | Yes — already parsed |
| Recursive CTE depth guard | `dvm/operators/recursive_cte.rs` | 2 | Yes — tree depth |
| Cycle detection in DAG | `dag.rs` | 3 | **Already unit-tested** |

**Effort:** ~2-3 hours (refactoring + tests)  
**Yield:** ~25-30 new unit tests; the E2E tests become thin smoke checks

---

## Implementation Order

| Priority | Phase | Effort | New Unit Tests | Impact |
|:--------:|-------|-------:|:---------:|--------|
| 1 | **P1 — Test already-pure functions** | 30 min | ~20 | Quick wins, zero refactoring |
| 2 | **P2-A — IVM SQL builders** | 60 min | ~18 | Highest-risk untested module |
| 3 | **P2-B — Monitor tree renderer** | 45 min | ~10 | 120 lines of pure logic, 0 tests |
| 4 | **P2-C — DDL event classification** | 45 min | ~12 | Correctness-critical for auto-reinit |
| 5 | **P2-D — Scheduler decisions** | 30 min | ~10 | Core scheduling correctness |
| 6 | **P2-E — Alert payloads** | 20 min | ~8 | Subtle escaping/truncation bugs |
| 7 | **P2-F — CDC column lists** | 20 min | ~6 | DRY + coverage |
| 8 | **P3 — Light-E2E harness** | 6-8 hrs | — | 580 tests gain PR feedback |
| 9 | **P4 — Promote validation tests** | 2-3 hrs | ~28 | Move error-path tests to unit tier |

### Projected State After All Phases

| Tier | Tests | Share | Runs on PR? |
|------|------:|------:|:-----------:|
| Unit | ~1,164 | 63% | Yes |
| Property | 27 | 1% | Yes |
| Integration | 81 | 4% | Yes |
| Light E2E | ~580 | 31% | **Yes** |
| Full E2E | ~51 | 3% | No (main + daily) |

All tests except 51 (3%) will run on every PR.

---

## Verification Criteria

- [x] `just test-unit` passes with the new unit tests (Phases 1-2, 4)
- [x] `just lint` passes with zero warnings after all extractions
- [ ] No E2E test is deleted — only duplicated to unit level or migrated to
      light-E2E tier
- [ ] Light-E2E harness works with `cargo pgrx package` artifacts
- [ ] CI job for light-E2E completes in < 10 min on PR
- [ ] Full E2E continues to pass on push-to-main
- [ ] Lines/test ratio for `ivm.rs`, `scheduler.rs`, `monitor.rs`, `cdc.rs`,
      `hooks.rs` drops below 100
