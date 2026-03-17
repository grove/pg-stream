# PLAN_CARGO_NEXTEST.md — Accelerating and Stabilizing the pg_trickle Test Suite

**Status:** Completed
**Date:** 2026-03-17
**Driver:** Open
**Relates to:** Priority 0-1 of `PLAN_TEST_EVALS_FINAL_REPORT.md`

---

## 1. Motivation

During the recent deep evaluations of the `pg_trickle` test suites, we successfully prioritized correctness by introducing a massive number of property-based, multiset equality, and chaotic `proptest` assertions. 

The downside of mathematical rigor is wall-clock execution time. Our test pyramid has grown significantly "heavier." To maintain high developer velocity and keep PR feedback loops ideally under 5-10 minutes, we cannot rely on the default `cargo test` runner. 

Standard `cargo test` runs test binaries sequentially or with heavy thread contention, and a single flaky test can silently kill an entire 20-minute CI run. 

**Solution:** Migrate the test execution harness to [`cargo-nextest`](https://nexte.st/). 
`cargo-nextest` runs each test in its own isolated process, massively boosting parallelism, cleanly mitigating flaky E2E containers with automatic retries, and providing modern machine-readable CI outputs.

---

## 2. Goals & Expected ROI

*   **Decrease Unit / Integration Test Runtime:** Expect a 2x-5x speedup by eliminating thread-contention on heavily parameterized pure-Rust `proptests` and in-memory DVM dataflow builds.
*   **Isolate E2E Panics and Hangs:** Nextest supports granular timeouts. If a background worker or logical replication stream randomly hangs in Postgres, it will aggressively kill just that process and continue the suite rather than freezing Github Actions for 60 minutes.
*   **Quarantine Flakes:** Introduce native `--retries X` logic into the test scripts to prevent transient Docker spin-up failures from causing red CI pipelines.
*   **Better CI Tooling:** Generate JUnit XML directly for GitHub Actions test-summary annotation.

---

## 3. Implementation Steps

*Run these steps incrementally to ensure we don't break existing `pgrx` testing infrastructure.*

### Step 1: Install and Configure Nextest locally and in CI
1.  Add `cargo-nextest` to our GitHub Actions runner setup:
    ```yaml
    - name: Install cargo-nextest
      uses: taiki-e/install-action@nextest
    ```
2.  Define a repository-wide `.config/nextest.toml` profile. This config should explicitly outline our test tiers, set specific per-test timeouts, and establish retry counts for the heavily networked E2E bounds.

### Step 2: Configure Nextest Test Groups (The Critical Path)
Many of our `pgrx`-backed tests or Light E2E container tests *cannot* run infinitely parallel. Some integration tests dynamically claim Postgres ports, or modify global shared memory.
1.  Define **Test Groups** inside `.config/nextest.toml` to throttle parallelism for tests that need it. E.g., limit tests under `tests/e2e_partition_tests.rs` to run sequentially with each other to protect test database namespace pollution.
2.  Assign pure `src/dvm/` Rust logic to `default` (infinitely parallel).

### Step 3: Update `justfile` and Testing Scripts
We must swap `cargo test` out for `cargo nextest run` inside our primary execution scripts, while carefully managing `pgrx` specific bindings.

**Files to modify:**
*   `scripts/run_unit_tests.sh`
*   `scripts/run_dvm_integration_tests.sh`
*   `scripts/run_light_e2e_tests.sh`
*   `justfile` aliases (`test-unit`, `test-integration`, etc)

*Note:* `cargo pgrx test` does not natively wrap `nextest` yet. We might need to call `cargo nextest` manually while ensuring `pgrx` environment variables (like `PGRX_PG_SYS_STUB`) are correctly built and injected.

### Step 4: Add JUnit Output to GitHub Actions
Once running in CI, update `ci.yml` so nextest produces test reports.
```bash
cargo nextest run --profile ci --junit test-results.xml
```
Integrate the results utilizing a standard GitHub action like `mikepenz/action-junit-report` to generate visual flame-graphs of failing and slow `pg_trickle` tests.

---

## 4. Risks & Considerations

*   **PGRX Compatibility:** `cargo pgrx test` does specialized setup (compiling the extension dynamic library and starting an ephemeral `pg_test` instance) *before* invoking `cargo test`. `cargo nextest` will simply bypass this wrapper. We will need to manually ensure test database clusters are up before invoking `nextest` for integration tests.
*   **Port Collisions:** Heavy concurrency means multiple test containers spinning up concurrently. Our E2E tests must be strictly written to bind to `0` (random free ports) rather than hardcoded `5432` ports, or else `nextest` will just cause massive "Address already in use" panics. 

## 5. Definition of Done
- `just test-all` utilizes `cargo nextest` under the hood.
- CI pipelines use `cargo nextest` and output JUnit XML reports into GitHub.
- Flaky tests are documented as auto-retrying in CI.
- Overall CI runtime is reliably kept under 10 minutes.

## 6. Implementation Status (Updated 2026-03-17)

**What has been done:**
- Set global `NEXTEST_PROFILE: ci` environment variable inside `.github/workflows/ci.yml`.
- Transitioned `cargo test` execution workflows directly inside `ci.yml` over to `cargo nextest run`.
- Integrated JUnit tracking via `mikepenz/action-junit-report` for multiple test branches inside `ci.yml`.
- Transitioned standard testing aliases directly over to `cargo nextest run` inside local `justfile` configs.
- Mapped global namespace-heavy database integration binaries and `e2e` prefixes into proper `max-threads = 1` serialized bounds inside `.config/nextest.toml` configuration groups.
- Created initial `.config/nextest.toml` configuration with basic retry logic and group definitions.
- Updated `.github/actions/setup-pgrx/action.yml` to install `cargo-nextest` via `taiki-e/install-action@nextest`.
- Modified test execution scripts to conditionally use `cargo nextest run` if available. This fallback approach maintains backward compatibility for contributors who have not yet installed nextest. Scripts touched:
  - `scripts/run_unit_tests.sh`
  - `scripts/run_dvm_integration_tests.sh`
  - `scripts/run_light_e2e_tests.sh`

**What is left to do:**
- Wait for remaining parallel CI data and telemetry feedback loops to identify newly flaky tests to adjust retries explicitly in `.config/nextest.toml` configuration.

**Deferred / Will Not Do at this time:**
- Review `pgrx` wrapper integration to confirm whether custom environment variables are necessary for `nextest` running `cargo pgrx test`. Current integration using `cargo pgrx test` acts as an opaque outer wrapper, making exact test translation non-trivial at this time without large structural changes to how we spin up test structures. We will use Nextest exclusively for all non-pgrx-wrapped environments and rely on standard runs for the slim `pgrx` tests natively.