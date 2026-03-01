# Codecov Integration Plan

## Status: Implementing

The `CODECOV_TOKEN` secret has been added to the GitHub repository.
The coverage workflow (`coverage.yml`) already collects `lcov.info` via
`cargo-llvm-cov` and uploads it to Codecov. This plan captures scope
decisions, configuration, and known limitations for the project record.

---

## What Codecov covers in this project

This is a pgrx extension. Coverage instrumentation runs against the Rust
test binary (`cargo test --lib`), not against the `.so` loaded by the
PostgreSQL server process. This shapes what Codecov can and cannot measure.

### Covered (meaningful signal)

| Module | Why it's well-covered |
|---|---|
| `src/dvm/parser.rs` | 910+ pure-Rust unit tests, no pgrx boundary |
| `src/dvm/operators/` | Exhaustive operator unit tests |
| `src/dvm/diff.rs` | Delta SQL generation, fully unit-tested |
| `src/dag.rs` | Graph logic, property tests |
| `src/hash.rs` | Deterministic hashing, unit-tested |
| `src/error.rs` | Enum variants, trivially covered |
| `src/refresh.rs` (pure logic only) | `classify_relkind`, `strip_view_definition_suffix` |

### Not covered (structural limitation, not a gap)

| Module | Why it can't be covered |
|---|---|
| `src/api.rs` | `#[pg_extern]` functions — execute inside postgres process |
| `src/cdc.rs` | Trigger-based CDC — runs as a PostgreSQL trigger callback |
| `src/wal_decoder.rs` | WAL polling — runs inside postgres backend |
| `src/scheduler.rs` | Background worker — separate postgres process |
| `src/hooks.rs` | DDL event trigger planner hook |
| `src/shmem.rs` | Shared memory init — postgres startup only |

These modules are covered by the E2E test suite (384 tests in Docker) but
that coverage is not instrumentable without a full PostgreSQL coverage build.

---

## Codecov configuration (`codecov.yml`)

Placed at the repository root. Key decisions:

1. **Never fail the build on overall project coverage** — the uninstrumentable
   pgrx modules structurally suppress the percentage below any reasonable gate.
2. **Enforce coverage on patch (PR diffs) for `src/dvm/`** — this is the
   pure-Rust differential engine where quality matters most and coverage is
   meaningful.
3. **Ignore uninstrumentable modules** from all reporting to avoid noise.

```yaml
# codecov.yml
coverage:
  status:
    project:
      default:
        informational: true        # never fail CI on overall %
    patch:
      dvm-engine:
        target: 70%
        paths:
          - "src/dvm/**"
          - "src/dag.rs"
          - "src/hash.rs"

ignore:
  - "src/api.rs"
  - "src/cdc.rs"
  - "src/wal_decoder.rs"
  - "src/scheduler.rs"
  - "src/hooks.rs"
  - "src/shmem.rs"
  - "src/bin/**"
  - "tests/**"
  - "benches/**"
```

---

## Workflow (`coverage.yml`)

### Current state

The workflow already:
- Installs `cargo-llvm-cov` via `taiki-e/install-action`
- Builds `libpg_stub.so` for `LD_PRELOAD` (provides NULL stubs for PG symbols)
- Runs `cargo llvm-cov --lib --features pg18` to produce `lcov.info`
- Uploads to Codecov via `codecov/codecov-action@v5`
- Uploads `lcov.info` as a workflow artifact

### Token placement

The `CODECOV_TOKEN` must be in `with:`, not `env:`, for `codecov-action@v5`:

```yaml
- name: Upload coverage to Codecov
  uses: codecov/codecov-action@v5
  with:
    token: ${{ secrets.CODECOV_TOKEN }}
    files: coverage/lcov.info
    flags: unittests
    fail_ci_if_error: false
```

### Triggers

Coverage runs on:
- Push to `main` when `src/**`, `Cargo.toml`, or `Cargo.lock` change
- Manual `workflow_dispatch`

It does NOT run on every PR (cost-saving decision — unit tests run on PR,
coverage upload is post-merge).

---

## Implementation checklist

- [x] `CODECOV_TOKEN` secret added to GitHub repository
- [x] `coverage.yml` workflow collects `lcov.info` via `cargo-llvm-cov`
- [x] `LD_PRELOAD` stub (`libpg_stub.so`) enables linking on Linux
- [ ] Move `CODECOV_TOKEN` from `env:` to `with:` in upload step
- [ ] Create `codecov.yml` at repository root
- [ ] Add Codecov badge to `README.md`
- [ ] Verify first upload shows coverage on Codecov dashboard

---

## Expected baseline numbers

Based on module analysis:

| Scope | Expected coverage |
|---|---|
| `src/dvm/**` | ~75–85% |
| `src/dag.rs` | ~80% |
| `src/hash.rs` | ~90% |
| Overall project | ~25–35% (structurally limited by pgrx) |

The overall number will look low to outside observers. The Codecov badge
should link to the `src/dvm/` coverage view, or the README should note the
structural limitation.

---

## Future: E2E coverage (stretch goal)

To get meaningful coverage for `api.rs`, `cdc.rs`, etc., a PostgreSQL
coverage build would be needed:

1. Compile PostgreSQL itself with coverage instrumentation
2. Build the extension `.so` with `RUSTFLAGS="-C instrument-coverage"`
3. Run E2E tests against that PostgreSQL
4. Merge LLVM profdata from the `.so` with the server's gcov data

This is a significant engineering investment and is not planned for v1.0.
Tracked as a potential post-1.0 improvement.
