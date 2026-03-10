# PLAN: Static Application Security Testing (SAST)

**Status:** Phase 2 + 3 complete  
**Date:** 2026-03-10  
**Branch:** `codeql-workflow` (merged), `sast-review-1` (Phase 1–3)  
**Scope:** Establish a practical SAST program for the `pg_trickle` PostgreSQL
extension that covers Rust application code, PostgreSQL extension attack
surfaces, dependency risk, and extension-specific SQL/security-context hazards.

---

## Table of Contents

1. [Security Newbie Checklist](#security-newbie-checklist)
2. [Motivation](#motivation)
3. [Threat Model and Security Surfaces](#threat-model-and-security-surfaces)
4. [Current State](#current-state)
5. [Goals and Non-Goals](#goals-and-non-goals)
6. [Proposed SAST Stack](#proposed-sast-stack)
7. [Phase Plan](#phase-plan)
8. [Semgrep Rules Roadmap](#semgrep-rules-roadmap)
9. [Unsafe / FFI Review Strategy](#unsafe--ffi-review-strategy)
10. [CI Integration Strategy](#ci-integration-strategy)
11. [Finding Triage and Policy](#finding-triage-and-policy)
12. [Validation Plan](#validation-plan)
13. [Success Criteria](#success-criteria)
14. [Implementation Checklist](#implementation-checklist)
15. [Open Questions](#open-questions)
16. [Triage Log](#triage-log)

---

## Security Newbie Checklist

> **Not a security expert? Start here.**  
> This section explains what this PR does and what you need to know as a
> reviewer or contributor, without requiring a deep background in static
> analysis or PostgreSQL security.

### What is SAST and why are we adding it?

**SAST** (Static Application Security Testing) means running automated tools
over source code to spot likely security problems before the code ever runs.
Think of it as a spell-checker, but for security patterns.

`pg_trickle` is a PostgreSQL extension that runs inside the Postgres server
process with elevated privileges. A bug here can affect the database itself,
not just some isolated application. That means the bar for security hygiene is
higher than a typical Rust CLI or web service.

The existing tooling (clippy, cargo audit, unit / E2E tests) is great for
correctness. SAST adds a complementary layer that looks specifically for
security-relevant patterns: unsafe pointer use, dynamic SQL that could be
abused, privilege escalation helpers, and risky dependency sources.

### What files does this PR actually add?

| File | Plain-English purpose |
|------|-----------------------|
| `.github/workflows/codeql.yml` | Runs GitHub's CodeQL scanner on Rust code. Findings appear in the repo's Security tab. |
| `.github/workflows/dependency-policy.yml` | Runs `cargo deny` to block known-bad or off-registry dependencies. |
| `deny.toml` | Configuration file that tells `cargo deny` what is and isn't allowed. |
| `.github/workflows/semgrep.yml` | Runs Semgrep to detect pg_trickle-specific patterns (see below). |
| `.semgrep/pg_trickle.yml` | Custom Semgrep rules written for this codebase. Currently advisory only. |

### Will any of these new checks block my PR?

**Right now:** CodeQL and `cargo deny` are blocking. Semgrep is advisory only
(it uploads findings but will not fail a PR).

| Check | Breaks the build? | What it catches |
|-------|-------------------|-----------------|
| CodeQL | Yes | Rust dataflow / security bug classes |
| `cargo deny` | Yes | Banned, unlicensed, or sketchy dependencies |
| `cargo audit` (existing) | Yes | Known CVE advisories |
| Semgrep | **No** (advisory) | Extension-specific patterns like dynamic SQL |

> **Note:** Semgrep warnings are meant to prompt a review conversation, not
> to require an immediate code change. If you see a Semgrep annotation on your
> PR, read the message, decide if it applies, and comment on your decision.
> You do not need to suppress it unless the pattern is a confirmed false
> positive.

### What does "advisory" mean in practice?

Advisory means the tool will report findings but will not make CI red. You
(or a reviewer) can choose to ignore them, leave a comment, or address them —
but the PR will not be blocked. The plan is to move from advisory to blocking
only after rules have been tuned and false-positive rates are acceptable.

### What should I do if CodeQL or cargo-deny flags something?

1. **Read the message.** It usually says what pattern was detected and why it
   can be risky.
2. **Check if it is a real issue.** Is dynamic SQL actually taking unchecked
   user input? Is a crate actually disallowed or from an untrusted source?
3. **Fix it if it is real.** Common fixes: use a quoting helper, replace
   `format!()` with bound parameters, or swap a dependency.
4. **Suppress with a justification if it is a false positive.** For CodeQL,
   use a `// lgtm` or `// codeql` inline suppression. For `cargo deny`, add
   an `allow` entry to `deny.toml`. Always include a comment explaining why
   the pattern is safe here.
5. **Never add a blanket ignore.** Suppressions should be as narrow as
   possible — scoped to the specific line or crate, not the whole file.

### The two things most likely to confuse reviewers

1. **Dynamic SPI SQL hits in Semgrep.**  
   The extension legitimately builds SQL strings to interact with PostgreSQL
   internals. Not all of these are bugs. The Semgrep rules are designed to
   surface them for review, not to declare them wrong. Ask yourself: "Is the
   interpolated value validated or quoted before it reaches the database?"
   If yes, leave a comment and move on.

2. **`unsafe` blocks.**  
   We use `unsafe` to cross the Rust / PostgreSQL FFI boundary. Every unsafe
   block should have a `// SAFETY:` comment explaining the invariant being
   upheld. If you add or touch an unsafe block and the comment is missing,
   add it. This is not optional.

### Quick review checklist for this PR

- [ ] The new workflow files trigger at sensible times (PR, push to main,
      weekly schedule) and do not run on every trivial commit.
- [ ] `deny.toml` does not accidentally ban anything already in `Cargo.toml`.
- [ ] Semgrep rules have clear, human-readable `message` fields.
- [ ] All new `unsafe` blocks have a `// SAFETY:` comment.
- [ ] Nothing in the Semgrep or CodeQL advisory output represents an obvious
      real vulnerability that should be fixed before merging.

---

## Motivation

`pg_trickle` is not a typical Rust CLI or web service. It is a PostgreSQL
extension loaded directly into the database server process, which changes the
security profile materially:

- Memory safety bugs can affect the PostgreSQL backend itself.
- Unsafe FFI boundaries matter more because they run in-process with Postgres.
- Dynamic SQL built via SPI can become an injection or privilege-context risk.
- Security mistakes around `SECURITY DEFINER`, `search_path`, RLS, and GUCs are
  extension-specific and will not be caught by generic Rust tooling.
- Dependency and supply-chain issues matter because the extension ships as a
  privileged shared object.

The repo already has strong correctness testing and standard linting, but SAST
coverage is still partial. The goal of this plan is to add security-focused
static analysis without creating a noisy, low-signal CI burden.

---

## Threat Model and Security Surfaces

This plan assumes the main in-scope risks described in [SECURITY.md](../../SECURITY.md)
remain the priority:

1. SQL injection or object-name injection via `pgtrickle.*` functions.
2. Privilege escalation through `SECURITY DEFINER`, `SET ROLE`, `row_security`,
   or unpinned `search_path`.
3. Memory safety bugs in Rust code that crosses into PostgreSQL FFI.
4. Low-privilege denial-of-service via expensive refreshes, unbounded work, or
   unsafe background-worker behavior.
5. Information disclosure through change buffers, monitoring views, or
   unexpected visibility semantics.

### Primary code surfaces

| Surface | Why it matters | Representative files |
|--------|----------------|----------------------|
| Dynamic SPI SQL | Injection, quoting, object-name construction, role context | `src/api.rs`, `src/refresh.rs`, `src/cdc.rs`, `src/monitor.rs`, `src/hooks.rs` |
| Unsafe / FFI | In-process memory safety, pointer lifetime, Postgres ABI | `src/lib.rs`, `src/shmem.rs`, `src/api.rs`, `src/dvm/parser.rs` |
| Background workers | Long-lived privileged code, scheduling, retry loops | `src/scheduler.rs`, `src/wal_decoder.rs`, `src/shmem.rs` |
| Trigger / function SQL | `SECURITY DEFINER`, `search_path`, runtime SQL generation | `src/api.rs`, `src/ivm.rs`, `sql/*.sql` |
| Dependencies | RustSec CVEs, banned sources, duplicate major versions | `Cargo.toml`, `Cargo.lock` |

### PostgreSQL-specific security surfaces

These are important enough that generic Rust SAST alone is insufficient:

- `Spi::run(&format!(...))` and `Spi::get_one(&format!(...))`
- `quote_identifier()` and other quoting helpers
- `NOTIFY` payload construction
- `ALTER TABLE`, `CREATE FUNCTION`, trigger generation, and `DEALLOCATE`
- `SET LOCAL row_security = off`
- `SECURITY DEFINER` and `SET search_path`
- background-worker bootstrap and shared-memory access

---

## Current State

### What already exists

| Control | Status | Notes |
|--------|--------|------|
| `clippy` | Present | Fast lint gate in `lint.yml` and `build.yml` |
| `cargo audit` | Present | Weekly + PR dependency advisory workflow |
| Security reporting policy | Present | `SECURITY.md` documents private disclosure path |
| Extensive E2E correctness tests | Present | Good base, but not SAST |

### What is missing or incomplete

| Gap | Why it matters |
|-----|----------------|
| CodeQL for Rust | No higher-level code scanning for Rust dataflow and security patterns |
| Dependency policy enforcement | `cargo audit` does not cover banned sources or dependency hygiene well |
| Repo-specific SPI SQL rules | Generic scanners will miss extension-specific SQL construction risks |
| Unsafe-code inventory | No explicit unsafe budget or automated visibility into unsafe growth |
| Security-context rule checks | No automated review hooks for `SECURITY DEFINER`, `search_path`, RLS toggles |
| Finding policy | No documented path from advisory findings to blocking CI |

### Initial implementation on this branch

The following baseline has already been added on `codeql-workflow`:

| File | Purpose |
|------|---------|
| `.github/workflows/codeql.yml` | CodeQL analysis for Rust |
| `.github/workflows/dependency-policy.yml` | `cargo deny` checks |
| `deny.toml` | Dependency-policy configuration |
| `.github/workflows/semgrep.yml` | Semgrep SARIF upload workflow |
| `.semgrep/pg_trickle.yml` | Initial repo-specific Semgrep rules |

This plan covers both that baseline and the next iterations needed to make it a
useful long-term SAST program.

---

## Goals and Non-Goals

### Goals

1. Catch obvious dependency and supply-chain issues automatically.
2. Make unsafe/FFI growth visible and reviewable.
3. Detect extension-specific SQL/security-context hazards early in PRs.
4. Keep the initial rollout low-noise enough that engineers will not disable it.
5. Evolve from advisory findings to blocking policies only after triage and rule tuning.

### Non-Goals

1. Replace runtime security testing or E2E role-based tests.
2. Prove absence of memory safety issues.
3. Fully model PostgreSQL execution semantics statically.
4. Block all dynamic SPI SQL. Dynamic SQL is legitimate in an extension; the
   objective is reviewability and safe construction, not blanket prohibition.

---

## Proposed SAST Stack

### Layer 1: CodeQL (Rust)

**Tool:** GitHub CodeQL  
**Purpose:** General Rust static analysis with control/dataflow analysis.  
**Why needed:** Catches classes of issues that clippy and grep-based tooling do
not, while integrating naturally with GitHub Security.

**Scope:**

- Rust source in `src/**`
- build configured via the repo's pgrx setup action
- SARIF findings uploaded to GitHub Security tab

**Expected value:**

- unsafe API misuse patterns
- suspicious string/data flow
- common Rust bug classes with security impact

### Layer 2: cargo-deny

**Tool:** `cargo deny`  
**Purpose:** Dependency policy enforcement beyond plain CVE scanning.  
**Why needed:** `cargo audit` is useful but narrow; `cargo deny` adds source,
duplicate-version, and registry hygiene checks.

**Checks to enforce initially:**

- advisories
- banned dependency patterns
- allowed registries / sources

**Checks to keep advisory initially:**

- multiple versions (`warn`)
- yanked crates (`warn`)

### Layer 3: Semgrep (extension-specific rules)

**Tool:** Semgrep  
**Purpose:** Repo-specific pattern checks that generic Rust scanners will not
model well.  
**Why needed:** The main security risk in this codebase is not generic “Rust
security”; it is the combination of SPI SQL, object-name quoting, PostgreSQL
privilege semantics, and generated SQL.

**Initial rule families:**

1. Dynamic SQL passed to `Spi::run`.
2. Dynamic SQL passed to `Spi::get_*` helpers.
3. `SECURITY DEFINER` occurrences in Rust-generated SQL or `sql/*.sql`.

**Initial operating mode:** advisory only (SARIF upload, no CI failure).

### Layer 4: Unsafe / FFI tracking

**Planned tools:**

- `cargo geiger` or equivalent unsafe inventory tooling
- stricter Rust/clippy lints around unsafe blocks where supported

**Purpose:**

- quantify unsafe usage growth
- make new unsafe entry points visible in PR review
- ensure every unsafe block remains documented and justified

### Layer 5: SQL / privilege-context review rules

This is the most important medium-term layer. The repo should gain static
checks that flag:

- `SECURITY DEFINER` without explicit `SET search_path`
- `SET LOCAL row_security = off`
- `SET ROLE` / `RESET ROLE`
- generated trigger functions
- dynamic `ALTER TABLE`, `DROP TABLE`, `TRUNCATE`, and `DEALLOCATE` SQL

These patterns are not automatically wrong, but they are security-sensitive and
deserve intentional review.

---

## Phase Plan

### Phase 0 — Baseline rollout (current branch)

**Status:** ✅ Complete — merged to `main` 2026-03-10  
**Goal:** Introduce low-friction SAST foundations with minimal CI disruption.

| Item | Status | Notes |
|------|--------|------|
| CodeQL workflow | ✅ Done | `build-mode: none`, upgraded to v4 action |
| `cargo deny` workflow + config | ✅ Done | License allow-list, advisory ignores, duplicate skips |
| Semgrep workflow + starter rules | ✅ Done | Advisory-only SARIF upload |

**Actual CI outcomes:**

- CodeQL scanned 115/115 Rust files; 1 extraction error (pgrx macro file, benign)
- CodeQL: **zero security findings** across all 16 security queries
- `cargo deny`: clean after adding `[licenses]` section and `skip` entries
- Semgrep: 47 initial alerts — all triaged and dismissed (see Triage Log below)

**Exit criteria:** all met.

### Phase 1 — Triage and noise reduction

**Priority:** P0  
**Status:** ✅ Complete — 2026-03-10 (branch `sast-review-1`)  

**Completed tasks:**

1. ✅ Ran all workflows on the merged `codeql-workflow` branch.
2. ✅ Reviewed all 47 initial findings via GitHub Security tab + `gh api`.
3. ✅ Classified all findings — see Triage Log section below.
4. ✅ Tuned `sql.security-definer.present` rule to exclude `plans/**`, `docs/**`,
   and `**/*.md` (root cause: `sql/**` include matched `plans/sql/` as substring).
5. ✅ Dismissed all 47 alerts via `gh api` with documented justifications.

**Deliverables:**

- ✅ Full triage pass documented in Triage Log section
- ✅ Reduced-noise Semgrep ruleset (security-definer scope fix)

### Phase 2 — Extension-specific rule expansion

**Priority:** P0  
**Status:** ✅ Complete — 2026-03-10 (branch `sast-review-1`)

**Completed tasks:**

1. ✅ Added `sql.row-security.disabled` Semgrep rule (detects `SET LOCAL row_security = off`).
2. ✅ Added `sql.set-role.present` Semgrep rule (detects `SET ROLE` / `RESET ROLE`).
3. ✅ Updated `sql.security-definer.present` message to include explicit `SET search_path` guidance.
4. ✅ All three privilege-context rules are advisory (WARNING/INFO), scoped to `src/**` and `sql/**`, excluding `*.md`, `plans/**`, `docs/**`.

**Notes:**
- No current occurrences in source — these are proactive forward-looking rules.
- The `SECURITY DEFINER` without `SET search_path` pairing check is aspirational:
  Semgrep generic mode cannot reliably span a multi-line `CREATE FUNCTION` body
  to assert both patterns are present. The updated message in `security-definer.present`
  makes the `search_path` requirement explicit for reviewers.
- A Semgrep rule for `unsafe {}` without `// SAFETY:` was deferred to Phase 3.

**Deliverables:**

- ✅ Two new Semgrep rules (`sql.row-security.disabled`, `sql.set-role.present`)
- ✅ Improved `sql.security-definer.present` message with `SET search_path` guidance

### Phase 3 — Unsafe growth controls

**Priority:** P1  
**Status:** ✅ Complete — 2026-03-10 (branch `sast-review-1`)

**Completed tasks:**

1. ✅ Added `scripts/unsafe_inventory.sh` — grep-based per-file `unsafe {` counter.
2. ✅ Added `.unsafe-baseline` — committed baseline (6 files, 1309 total blocks).
3. ✅ Added `.github/workflows/unsafe-inventory.yml` — CI workflow that runs the script
   on PRs targeting `src/**` and posts a per-file table to `GITHUB_STEP_SUMMARY`.
   Fails if any file exceeds its baseline count.
4. ✅ Added `just unsafe-inventory` recipe to `justfile`.

**Notes on `undocumented_unsafe_blocks` clippy lint:**
- The lint exists in stable clippy and would be valuable.
- `src/dvm/parser.rs` has 1286 `unsafe {` blocks (mechanical pgrx FFI node casts)
  but only 38 `// SAFETY:` comments — enabling the lint globally would produce
  ~1248 new warnings.
- **Policy decision:** add `#![allow(clippy::undocumented_unsafe_blocks)]` to
  `parser.rs` and enable the lint globally for all other files. This is tracked
  as a future improvement; it is not blocked on Phase 3.

**Baseline summary (2026-03-10):**

| File | `unsafe {` blocks |
|------|-------------------|
| `src/api.rs` | 10 |
| `src/dvm/parser.rs` | 1286 |
| `src/lib.rs` | 1 |
| `src/scheduler.rs` | 5 |
| `src/shmem.rs` | 3 |
| `src/wal_decoder.rs` | 4 |
| **Total** | **1309** |

**Deliverables:**

- ✅ `scripts/unsafe_inventory.sh` with `--report-only` and `--update` modes
- ✅ `.unsafe-baseline` committed baseline
- ✅ `unsafe-inventory.yml` CI workflow (advisory, fails on regression)
- ✅ `just unsafe-inventory` recipe

### Phase 4 — Move from advisory to blocking

**Priority:** P1  
**Estimated effort:** 2–3 hours after rules stabilize.

**Tasks:**

1. Keep CodeQL and cargo-deny blocking.
2. Change only high-confidence Semgrep rules to blocking.
3. Leave broader “review-required” rules as informational if they remain noisy.
4. Document which classes are blockers and why.

**Deliverables:**

- clear blocking policy in CI
- reduced chance of engineers papering over the toolchain

### Phase 5 — Link static findings to runtime security tests

**Priority:** P2  
**Estimated effort:** 4–8 hours.

**Tasks:**

1. Add E2E tests for security-sensitive paths highlighted by SAST:
   - RLS behavior
   - SECURITY DEFINER trigger behavior
   - low-privilege access to monitoring views / change buffers
   - SQL injection attempts through API inputs
2. Map static rules to runtime coverage so a suppressed SAST finding still has a
   compensating test where appropriate.

**Deliverables:**

- security-focused E2E coverage plan or implementation

---

## Semgrep Rules Roadmap

### Current rules

| Rule ID | Purpose | Mode |
|--------|---------|------|
| `rust.spi.run.dynamic-format` | Flag dynamic SQL passed to `Spi::run` | Advisory |
| `rust.spi.query.dynamic-format` | Flag dynamic SQL passed to `Spi::get_*` | Advisory |
| `sql.security-definer.present` | Surface `SECURITY DEFINER` for review (message includes `search_path` guidance) | Advisory |
| `sql.row-security.disabled` | Detect `SET LOCAL row_security = off` | Advisory |
| `sql.set-role.present` | Detect `SET ROLE` / `RESET ROLE` | Advisory |
| `rust.panic-in-sql-path` | Flag `.unwrap()`, `.expect()`, `panic!()` in `src/**` | Advisory |

### Planned next rules

| Category | Candidate rule | Value |
|----------|----------------|------|
| SQL quoting | Detect string interpolation into SPI SQL without quoting helper | High |
| Privilege context | `SECURITY DEFINER` without `SET search_path` | High |
| Role changes | `SET ROLE`, `RESET ROLE`, `row_security = off` | High — ✅ Added Phase 2 |
| Unsafe docs | `unsafe` without nearby `SAFETY:` comment | Medium |
| Panics in SQL path | `unwrap()` / `expect()` / `panic!()` in non-test SQL-reachable code | Medium — ✅ Added Phase 2 |
| GUC-sensitive SQL | direct `SET LOCAL work_mem`, `SET LOCAL row_security` review hooks | Medium |

### False-positive strategy

Do not attempt to make the first Semgrep rules perfectly precise. Instead:

1. Start broad.
2. Triage actual hits.
3. Narrow only where noise is unacceptable.
4. Keep “review-required” rules informational instead of blocking.

This is especially important because a PostgreSQL extension often needs dynamic
object names; the real question is whether they are safely quoted, not whether
dynamic SQL exists at all.

---

## Unsafe / FFI Review Strategy

Unsafe code in this repo is legitimate but high-impact. The policy should be:

1. Every unsafe block has a `SAFETY:` comment.
2. Unsafe growth is visible in PRs.
3. New unsafe blocks are reviewed as security-relevant changes, not merely
   implementation details.

### Review checklist for unsafe additions

- What Postgres invariant does this rely on?
- Is the pointer lifetime guaranteed by the surrounding callback/API?
- Can the same logic be expressed with a safe wrapper from `pgrx`?
- Does this code run in a background worker or user-triggered SQL path?
- What happens if Postgres returns null, stale, or unexpected node types?

### Candidate automation

- `cargo geiger` report in CI
- Semgrep rule for `unsafe {` without nearby `SAFETY:`
- code-review requirement for files containing new unsafe blocks

---

## CI Integration Strategy

The SAST system should respect the repo's existing fast/slow pyramid.

### Recommended posture

| Workflow | Trigger | Blocking? | Rationale |
|----------|---------|-----------|-----------|
| CodeQL | PR, push to main, weekly | Yes | High-value, low-maintenance |
| cargo-deny | PR, dependency changes, weekly | Yes | Good supply-chain hygiene |
| cargo-audit | Keep existing | Yes | Advisory DB coverage already in place |
| Semgrep | PR, push, weekly | No initially | Needs tuning first |

### Why Semgrep starts advisory

The repo has many legitimate dynamic SPI SQL callsites. If Semgrep is blocking
immediately, engineers will either disable it or add broad suppressions. An
advisory phase preserves signal while the rules are tuned.

---

## Finding Triage and Policy

### Severity mapping

| Finding type | Target policy |
|-------------|---------------|
| Known vulnerable dependency | Block merge |
| Unknown crate source / banned source | Block merge |
| High-confidence CodeQL issue | Block merge unless justified |
| `SECURITY DEFINER` without safe context | Block once rule is tuned |
| Generic dynamic SPI SQL | Advisory unless unsafe interpolation is proven |
| Unsafe block increase | Advisory first, then block if policy is accepted |

### Triage rules

1. Suppressions must be narrow and justified.
2. A suppression should explain why the pattern is safe in PostgreSQL terms,
   not only why the tool is noisy.
3. When a noisy pattern repeats, refine the rule rather than scattering ignores.

---

## Validation Plan

### Static validation

After changes to the SAST config itself:

```bash
just fmt
just lint
```

### Workflow validation

Manual dispatch after merge or on the PR branch:

1. Run `CodeQL` and confirm SARIF upload succeeds.
2. Run `Dependency policy` and confirm `cargo deny` resolves the graph cleanly.
3. Run `Semgrep` and capture the first findings set.

### Triage validation

For each Semgrep rule added:

1. Confirm it catches at least one real representative pattern.
2. Confirm it does not flood obviously safe code paths with unusable noise.
3. Confirm the remediation message is specific enough for reviewers.

---

## Success Criteria

This plan is successful when all of the following are true:

1. Dependency advisories and bad sources are enforced automatically.
2. CodeQL findings appear in GitHub Security for Rust changes.
3. Security-sensitive SPI SQL and privilege-context patterns are surfaced in PRs.
4. Unsafe growth is visible and reviewable.
5. Engineers trust the signal enough to keep the checks enabled.

### Phase completion criteria

| Phase | Done when |
|------|-----------|
| Phase 0 | ✅ Workflows merged and repo lint remains clean |
| Phase 1 | ✅ First findings triaged and Semgrep noise reduced |
| Phase 2 | ✅ High-value extension-specific rules added |
| Phase 3 | ✅ Unsafe inventory visible in CI |
| Phase 4 | High-confidence rules become blocking |
| Phase 5 | Static findings mapped to runtime security tests |

---

## Implementation Checklist

### Phase 0 — Done

- [x] Add CodeQL workflow for Rust (`build-mode: none`, v4 action)
- [x] Add `cargo deny` policy workflow
- [x] Add `deny.toml` with license allow-list and advisory ignores
- [x] Add initial Semgrep workflow
- [x] Add starter Semgrep ruleset
- [x] Validate the repo still passes `just lint`
- [x] Merge to `main`

### Phase 1 — Done

- [x] Run all new workflows and capture findings
- [x] Triage all 47 Semgrep and CodeQL results (see Triage Log)
- [x] Fix `security-definer` Semgrep rule to exclude `plans/**` / `docs/**` / `**/*.md`
- [x] Dismiss all 47 alerts via `gh api` with documented justifications

### Phase 2 — Done

- [x] Add Semgrep rule: `SET LOCAL row_security = off` (`sql.row-security.disabled`)
- [x] Add Semgrep rule: `SET ROLE` / `RESET ROLE` (`sql.set-role.present`)
- [x] Add Semgrep rule: `.unwrap()` / `.expect()` / `panic!()` in `src/**` (`rust.panic-in-sql-path`)
- [x] Update `sql.security-definer.present` message with explicit `SET search_path` guidance
- [x] Proactive rules scoped to `src/**` + `sql/**`, excluding `*.md`/`plans/**`/`docs/**`

Deferred to future: Semgrep rule for `unsafe {}` without `// SAFETY:` (clippy lint is  the better tool; see Phase 3 notes). `SECURITY DEFINER` + `SET search_path` pairing
check is aspirational with Semgrep generic mode; message guidance covers it for now.

### Phase 3 — Done

- [x] Add `scripts/unsafe_inventory.sh` — per-file `unsafe {` counter with baseline comparison
- [x] Commit `.unsafe-baseline` (baseline: 1309 blocks across 6 files)
- [x] Add `.github/workflows/unsafe-inventory.yml` — advisory CI workflow, fails on regression
- [x] Add `just unsafe-inventory` recipe for local use
- [x] Document `undocumented_unsafe_blocks` clippy lint policy (deferred until `parser.rs` gets `#![allow]`)

---

## Open Questions

1. Should Semgrep eventually fail PRs, or remain advisory with only select
   high-confidence rules blocking?
2. ~~Do we want unsafe growth to hard-fail CI, or just surface as a review signal?~~
   **Resolved Phase 3:** Inventory workflow hard-fails on regression; not a required
   status check yet. Add to branch protection once the team adopts the policy.
3. Should `cargo audit` remain separate from `cargo deny`, or be consolidated
   once confidence in `cargo deny` is established?
4. Do we want a dedicated security-review checklist for PRs that touch:
   - `Spi::run(&format!(...))`
   - `unsafe`
   - `SECURITY DEFINER`
   - `row_security`
   - background-worker logic?
5. Should a later phase add CodeQL custom queries for pgrx/PostgreSQL-specific
   APIs, or is Semgrep sufficient for that layer?

---

## Triage Log

### 2026-03-10 — First triage pass (Phase 1)

**Total alerts:** 47 (all WARNING or NOTE — zero errors)

#### CodeQL (2 alerts dismissed as `used in tests`)

| Alert # | Rule | File | Decision |
|---------|------|------|----------|
| 47 | CWE-312 Cleartext logging | `tests/e2e/light.rs:325` | False positive — `execute()` test helper; `salary` column is synthetic tutorial data, not PII |
| 46 | CWE-312 Cleartext storage in DB | `tests/e2e/light.rs:322` | False positive — same as above |

**Notes:** CodeQL traced a dataflow path from a test schema containing a `salary`
column through generic `execute()`/`try_execute()` helpers (in the e2e test harness)
and inferred that sensitive data was being stored unencrypted. This is test-only code;
the extension has no concept of PII sensitivity.

#### Semgrep `sql.security-definer.present` (13 alerts dismissed as `false positive`)

All 13 hits were in `plans/sql/PLAN_ROW_LEVEL_SECURITY.md` and
`plans/sql/PLAN_VIEW_INLINING.md` — design plan markdown files, not source code.

**Root cause:** The `paths.include: [sql/**]` pattern was matching `plans/sql/`
as a substring. Fixed by adding `exclude: ["**/*.md", "plans/**", "docs/**"]`
to the rule in `.semgrep/pg_trickle.yml`.

#### Semgrep `spi.run.dynamic-format` / `spi.query.dynamic-format` (32 alerts dismissed as `false positive`)

All 32 hits were in production code (`src/api.rs`, `src/refresh.rs`, `src/ivm.rs`,
`src/cdc.rs`, `src/monitor.rs`). Each was individually reviewed.

**Finding:** Every interpolated value is one of:
- A pre-quoted catalog identifier via `quote_identifier()` or double-quote escaping
  (`schema.replace('"', "\"\"")`)
- An internal storage-table name constructed from catalog OIDs
- A GUC-supplied integer (e.g. `mb` from `pg_trickle_merge_work_mem_mb()`)
- An internally generated prepared-statement name (`__pgt_merge_{pgt_id}`)
- A NOTIFY payload where string values are manually escaped
  (`name.replace('\'', "''")`) before interpolation

None of these are reachable from direct user input at the SQL API boundary.

**Policy decision:** Dismissed as false positives. The advisory rules are
retained so that *future* dynamic SPI callsites are surfaced for review.
Suppressions should not be added inline; instead each new callsite should be
triaged at the time it is introduced.

---

## Recommendation

~~Merge Phase 0 now.~~ Phase 0 and Phase 1 are complete. The next highest-value
move is Phase 2: adding Semgrep rules for `SECURITY DEFINER` + `search_path`
missing pairs, `row_security` toggles, and `SET ROLE` changes. These are the
extension-specific security hazards most likely to evade generic Rust tooling
and are not yet covered by any current rule.