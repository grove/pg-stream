# PLAN_IGNORED_E2E_TESTS.md — Re-enable Ignored E2E Coverage

**Status:** Proposed  
**Date:** 2026-03-09  
**Branch:** `main`  
**Scope:** Reconcile the stale changelog note about 18 ignored E2E tests, re-enable the suites that are already supportable, and implement the remaining DVM fixes needed to bring the still-failing ignored tests back into normal E2E coverage.

---

## Motivation

The project currently has a stale mismatch between documentation and reality:

- `CHANGELOG.md` still says 18 E2E tests are ignored due to pre-existing DVM bugs.
- `tests/e2e_keyless_duplicate_tests.rs` no longer has ignored tests at all.
- The ignored `HAVING` transition suite currently appears supportable again.
- The ignored `FULL JOIN` and correlated `EXISTS ... HAVING` cases still fail.
- The correlated scalar-subquery bucket is still ignored, but needs a clean re-baseline before deciding whether it should be re-enabled or fixed.

This plan treats the ignored tests as a product-quality issue, not just a testing issue. The goal is to make the repository's claims, test annotations, DVM implementation, and CI behavior agree again.

---

## Ground Truth

### Changelog Claim

The stale claim currently lives in `CHANGELOG.md` and groups the ignored tests as:

- `e2e_full_join_tests` — 5
- `e2e_having_transition_tests` — 5
- `e2e_keyless_duplicate_tests` — 5
- `e2e_scalar_subquery_tests` — 2
- `e2e_sublink_or_tests` — 1

### Verified Current State

| Suite | Changelog Count | Current Ignore State | Current Reality | Plan Outcome |
|---|---:|---|---|---|
| `tests/e2e_keyless_duplicate_tests.rs` | 5 | No ignored tests remain | Suite already re-enabled | Documentation cleanup only |
| `tests/e2e_having_transition_tests.rs` | 5 | 5 ignored tests remain | Ignored-only suite passed in local verification | Re-enable now |
| `tests/e2e_scalar_subquery_tests.rs` | 2 | 2 ignored tests remain | Needs clean confirmation outside the noisy terminal wrapper | Verify, then either re-enable or fix |
| `tests/e2e_full_join_tests.rs` | 5 | 5 ignored tests remain | Ignored-only suite still fails 5/5 | Engine work required |
| `tests/e2e_sublink_or_tests.rs` | 1 | 1 ignored test remains | Ignored test still fails | Engine work required |

### Important Consequence

This is not one problem. It is three separate workstreams:

1. `Already fixed but not reflected everywhere`
2. `Likely fixed, needs clean verification`
3. `Still broken in DVM and requires implementation work`

The plan below keeps those buckets separate so the project can recover coverage incrementally instead of waiting for every remaining DVM edge case to be solved.

---

## Goals

1. Remove stale `#[ignore]` markers from suites that are already supportable.
2. Re-baseline the scalar-subquery bucket with clean evidence, not assumptions.
3. Fix the remaining DVM correctness bugs blocking the `FULL JOIN` and correlated `EXISTS ... HAVING` suites.
4. Bring docs and changelog back in sync with the actual test inventory.
5. Ensure default E2E coverage exercises every re-enabled test in CI.

## Non-Goals

1. Reworking unrelated E2E infrastructure.
2. Refactoring the DVM engine outside the failing operator paths.
3. Expanding SQL support beyond the currently ignored buckets.
4. Changing the meaning of `FULL`, `DIFFERENTIAL`, or `IMMEDIATE` mode beyond what is needed for correctness.

---

## Workstream A — Immediate Re-enable

This workstream covers cases that do not need new engine behavior.

### A1. Keyless Duplicate Suite

**Current state:** Already done in code. `tests/e2e_keyless_duplicate_tests.rs` has no ignored tests left.

**Required work:**

1. Remove the stale changelog claim that still counts these 5 tests as ignored.
2. Update user-facing docs that still warn as if exact-duplicate keyless rows are categorically unsupported in the same way they were before the counted-delete and non-unique row-id work landed.
3. Confirm CI includes this suite in normal E2E execution and that no dedicated `--ignored` path remains for it.

**Likely files:**

- `CHANGELOG.md`
- `docs/FAQ.md`
- `docs/SQL_REFERENCE.md`
- `plans/testing/STATUS_TESTING.md` if it enumerates ignored inventory

**Acceptance criteria:**

1. No docs claim these tests are still ignored.
2. The suite continues to pass in normal E2E runs.

### A2. HAVING Transition Suite

**Current state:** Five tests remain ignored in `tests/e2e_having_transition_tests.rs`, but the ignored-only suite passed during local verification.

**Implementation steps:**

1. Re-run the full ignored-only suite once in a clean shell and once in CI to eliminate false confidence from terminal-wrapper noise.
2. Remove all five `#[ignore = ...]` annotations from `tests/e2e_having_transition_tests.rs`.
3. Run the suite in normal mode and as part of the standard E2E test tier.
4. Update docs so `HAVING` support claims match the actual supported state.
5. Remove the suite from any residual “known limitations” or “future release” references.

**Why this is low risk:**

- The parser already has explicit `HAVING` rewrite logic.
- The DVM tree already models `HAVING` as `Aggregate` plus `Filter`.
- The behavior claimed in `docs/FAQ.md` already says threshold transitions are supported.

**Likely files:**

- `tests/e2e_having_transition_tests.rs`
- `CHANGELOG.md`
- `docs/FAQ.md`
- `docs/SQL_REFERENCE.md`

**Acceptance criteria:**

1. All five previously ignored `HAVING` tests run without `#[ignore]`.
2. The suite passes in normal E2E runs.
3. No documentation still lists `HAVING` threshold transitions as an open limitation.

---

## Workstream B — Verification Gate Before Re-enable

This workstream exists for cases that might already be fixed, but need clean proof before removing `#[ignore]`.

### B1. Correlated Scalar Subquery Suite

**Current state:** Two tests remain ignored in `tests/e2e_scalar_subquery_tests.rs`:

- `test_correlated_scalar_subquery_differential`
- `test_scalar_subquery_null_result_differential`

The earlier verification attempt did not surface a clean suite summary because the terminal wrapper was noisy. That is not enough evidence to remove the ignores safely.

**Primary hypothesis:** The correlated scalar-subquery path may now work after the parser decorrelation and scalar-subquery operator work, but this needs a clean pass/fail baseline.

**Relevant implementation areas:**

- `src/dvm/parser.rs`
  - correlated scalar-subquery extraction and decorrelation
  - scalar-subquery target parsing
  - scalar-subquery-in-WHERE rewrite and fallback behavior
- `src/dvm/operators/scalar_subquery.rs`
  - `diff_scalar_subquery`
- `src/dvm/diff.rs`
  - operator dispatch

**Implementation steps:**

1. Reproduce both ignored tests in a clean environment with output captured to files or CI artifacts.
2. If both pass repeatedly:
   - remove both `#[ignore]` annotations
   - add them to the normal E2E tier
   - update changelog/docs
3. If either fails:
   - capture the generated SQL and exact failure mode
   - determine whether the failure is in parser decorrelation, deparse, or the scalar-subquery diff operator
   - add unit coverage for the failing pattern before changing E2E annotations

**Decision rule:**

- `Passes cleanly twice` -> re-enable immediately
- `Fails once reproducibly` -> move into Workstream C-style bugfix work and keep ignored until fixed

**Acceptance criteria:**

1. There is clean evidence for one of two outcomes: re-enable or keep ignored for a specific bug.
2. The suite is no longer in an ambiguous state.

---

## Workstream C — Remaining DVM Fixes

This workstream covers the ignored tests that still represent real correctness bugs.

### C1. FULL JOIN Differential Correctness

**Current state:** All five ignored tests in `tests/e2e_full_join_tests.rs` still fail.

**Observed scenarios covered by the suite:**

1. Basic left-only and right-only boundary transitions
2. `NULL` join keys
3. Join-key migration between matched and unmatched states
4. Aggregate on top of `FULL JOIN`
5. Multi-column join keys

**Likely root cause area:** `src/dvm/operators/full_join.rs`

That module already has a dedicated `diff_full_join` implementation. The current failures indicate the operator still mishandles one or more of:

- unmatched row emission
- matched-to-unmatched transitions
- unmatched-to-matched transitions
- duplicate suppression or counter maintenance
- `NULL` semantics on the join boundary
- aggregation on top of the emitted delta

**Secondary supporting modules:**

- `src/dvm/operators/join_common.rs`
- `src/dvm/parser.rs`
- `src/dvm/diff.rs`
- `src/api.rs` for storage-table metadata such as `__pgt_count_l` / `__pgt_count_r`

**Implementation steps:**

1. Reproduce each of the five failing tests individually and capture the actual stream table contents vs. the defining query results after each mutation step.
2. Add focused operator-level unit tests in `src/dvm/operators/full_join.rs` for the exact failing transitions:
   - left-only insert becoming matched
   - matched row becoming left-only or right-only after delete
   - key migration across join buckets
   - `NULL` join-key preservation
   - multi-column equality handling
3. Inspect whether `diff_full_join` emits all required delete and insert rows for the boundary state transition, not only the net current-state rows.
4. Validate hidden counter behavior for matched/unmatched rows when a row flips sides.
5. Re-run the aggregate-on-top case after the base operator is fixed to ensure aggregate delta logic receives a complete delta stream.
6. Only after the unit tests and all five E2E tests pass, remove the five ignore annotations.

**Key engineering rule:**

Do not remove the `#[ignore]`s incrementally inside this suite unless the operator semantics are clearly fixed across all five scenarios. These failures are likely coupled.

**Acceptance criteria:**

1. All five `FULL JOIN` ignored tests pass.
2. New unit tests exist for the exact transition patterns that were previously wrong.
3. Docs no longer claim `FULL OUTER JOIN` is supported if the suite is still ignored.

### C2. Correlated `EXISTS` with `HAVING` in OR-Rewritten Sublink Path

**Current state:** The single ignored test in `tests/e2e_sublink_or_tests.rs` still fails.

**Failure description from the annotation:**

Correlated `EXISTS` with `HAVING` uses a recomputation diff path that loses aggregate state.

**Most likely implementation touchpoints:**

- `src/api.rs`
  - `rewrite_sublinks_in_or()` call site
- `src/dvm/parser.rs`
  - `rewrite_sublinks_in_or`
  - semijoin wrapping for grouped/HAVING subqueries
  - aggregate extraction from `HAVING`
  - `rewrite_having_expr`
- `src/dvm/operators/aggregate.rs`
  - aggregate delta and rescan behavior when fed by filter/semijoin wrappers

**Primary hypothesis:**

The OR-to-UNION rewrite preserves logical equivalence for plain `EXISTS`, but drops or mis-rebinds the pre-HAVING aggregate columns needed by the correlated grouped subquery branch. The branch still answers the right question on a full recomputation, but the differential path does not retain enough state to emit the right delete/insert sequence when the correlated aggregate crosses the threshold.

**Implementation steps:**

1. Reproduce the failing test with generated SQL logging enabled for the rewritten OR branches.
2. Confirm which branch loses the grouped aggregate state and whether the failure occurs in rewrite, parse, or diff generation.
3. Add parser-level tests that lock down the expected transformed tree for:
   - `EXISTS (SELECT 1 ... GROUP BY ... HAVING ...) OR ...`
   - correlated aggregate aliases extracted only from `HAVING`
4. Ensure the rewritten branch preserves every aggregate output needed by the `HAVING` filter, even when those aggregates are not in the original SELECT list.
5. If the problem is downstream of the parser, update aggregate/filter diff logic so threshold transitions in the correlated semijoin branch are emitted correctly.
6. Add at least one unit test and one E2E regression test before removing the ignore.

**Acceptance criteria:**

1. The ignored sublink test passes without `#[ignore]`.
2. There is parser or operator unit coverage for the exact grouped correlated sublink shape.

---

## Cross-Cutting Cleanup

These tasks should happen alongside the workstreams above.

### D1. Re-baseline the Changelog and Docs

The current public narrative is internally inconsistent. `CHANGELOG.md` and parts of the docs still present the ignored-test inventory as if it were current, while other docs already claim support for some of those features.

**Required work:**

1. Replace the stale “18 ignored tests” note with a current statement.
2. Move the remaining limitations into a smaller, precise list based on actual failing suites.
3. Ensure docs only claim support for features that are exercised by non-ignored tests.

**Files likely affected:**

- `CHANGELOG.md`
- `docs/FAQ.md`
- `docs/SQL_REFERENCE.md`

### D2. CI Coverage Alignment

Every re-enabled test must run in the same CI tier that the project expects for E2E correctness.

**Required work:**

1. Verify `just test-e2e` and the PR-time light/full E2E jobs include the re-enabled suites.
2. Ensure there is no separate `--ignored` invocation required for the recovered tests.
3. If scalar-subquery verification is deferred, add an explicit note so that future contributors know that bucket is intentionally pending, not forgotten.

**Files likely affected:**

- `justfile`
- `.github/workflows/ci.yml` if suite selection is explicit there

### D3. Testing Status Tracking

Once the re-enable work starts, the testing status documents should reflect it.

**Files likely affected:**

- `plans/testing/STATUS_TESTING.md`
- `plans/testing/PLAN_TESTING_GAPS.md` if the remaining failures become tracked implementation gaps

---

## Recommended Execution Order

### Phase 0 — Re-baseline

1. Re-run the ignored suites with clean output capture.
2. Produce a one-page matrix of `ignored -> passes -> re-enable` vs. `ignored -> still fails -> fix`.
3. Use that matrix as the only source of truth for the rest of the work.

### Phase 1 — Recover Fast Wins

1. Land changelog cleanup for keyless duplicates.
2. Remove ignores from the five `HAVING` transition tests.
3. Run `just fmt`, `just lint`, and the affected E2E suites.

### Phase 2 — Resolve Scalar Ambiguity

1. Verify the scalar-subquery suite cleanly.
2. Re-enable it if green.
3. Otherwise convert it into a targeted bugfix task with exact captured failure output.

### Phase 3 — Fix FULL JOIN

1. Add failing unit tests that mirror the five E2E cases.
2. Fix `diff_full_join` and any supporting join-common behavior.
3. Re-enable the suite once both unit and E2E tests pass.

### Phase 4 — Fix Correlated `EXISTS ... HAVING`

1. Stabilize parser rewrite and aggregate-state preservation.
2. Add regression coverage.
3. Re-enable the final ignored test.

### Phase 5 — Final Consistency Pass

1. Update docs and status files.
2. Re-run the full validation stack.
3. Confirm the repository no longer has stale references to the original ignored-test inventory.

---

## Validation Matrix

After each phase, run the narrowest relevant validation first, then the required repo-wide checks.

### Narrow Validation

| Change | Minimum validation |
|---|---|
| Remove ignores from `HAVING` suite | `cargo test --test e2e_having_transition_tests -- --test-threads=1 --nocapture` |
| Remove ignores from scalar suite | `cargo test --test e2e_scalar_subquery_tests -- --test-threads=1 --nocapture` |
| Fix `FULL JOIN` operator | `cargo test --test e2e_full_join_tests -- --test-threads=1 --nocapture` plus new unit tests |
| Fix sublink OR aggregate case | `cargo test --test e2e_sublink_or_tests -- --test-threads=1 --nocapture` plus new parser/operator unit tests |

### Required Repository Validation After Code Changes

1. `just fmt`
2. `just lint`
3. The affected E2E suites
4. `just test-e2e` before closing the work if multiple ignored buckets were touched

---

## Risks

1. `HAVING` may pass in the current environment but still hide order-dependent or timing-dependent failures in CI.
2. The scalar-subquery bucket may be partially fixed, creating pressure to remove ignores before the root cause is fully understood.
3. `FULL JOIN` failures may expose coupled bugs in both the dedicated full-join operator and aggregate-on-top behavior.
4. The OR-to-UNION sublink rewrite may need a more principled representation of grouped correlated subqueries than the current branch rewrite preserves.

---

## Definition of Done

This plan is complete when all of the following are true:

1. The stale “18 ignored E2E tests” statement is removed or replaced with current facts.
2. Every test that is already supportable runs without `#[ignore]`.
3. Every still-ignored test has a concrete tracked implementation reason, not a stale inherited annotation.
4. `FULL JOIN` and correlated `EXISTS ... HAVING` have unit-level regression coverage in addition to E2E coverage.
5. Docs, changelog, and CI all reflect the same supported-state story.

Until then, the project should treat ignored-test inventory drift as a correctness and release-readiness issue, not just a documentation issue.