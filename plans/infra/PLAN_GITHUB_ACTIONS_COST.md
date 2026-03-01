# Plan: Reduce GitHub Actions Resource Consumption

## Current State

| Workflow | File | Trigger | Est. Duration | Frequency |
|----------|------|---------|--------------|-----------|
| **Build** | `build.yml` | push/PR on `main` | ~15–20 min (3-platform build + Docker) | Every push/PR |
| **Release** | `release.yml` | push `v*` tag | ~25–30 min (3-platform build + Docker + GHCR) | Infrequent |
| **CI** | `ci.yml` | `workflow_dispatch` (manual) | ~30+ min (unit × 3 OS + integration + E2E + bench + CNPG) | Manual |
| **Coverage** | `coverage.yml` | `workflow_dispatch` (manual) | ~10–15 min | Manual |
| **Benchmarks** | `benchmarks.yml` | `workflow_dispatch` (manual) | ~10–15 min | Manual |

### Already Done

- CI, Coverage, and Benchmarks workflows are **already manual-only** (`workflow_dispatch`).
- The `setup-pgrx` composite action already caches Rust artifacts, `cargo-pgrx` binary, and `~/.pgrx`.
- The Build workflow Docker step already uses GHA cache (`cache-from: type=gha`).
- All automatic workflows have `cancel-in-progress: true`.

### Primary Cost Driver

The **Build workflow** is the only remaining automatic workflow that runs on every push/PR. It runs:
1. **Lint** job (ubuntu) — ~3 min
2. **Build matrix** (Linux, macOS-arm64, Windows) — ~8–12 min each
3. **Docker image build** (ubuntu) — ~5–8 min

Total: **~4 billable jobs per push/PR**, with Windows being the most expensive (pgrx compiles PG from source).

---

## Prioritized Steps

### Step 1: Add path filters to Build workflow ⭐ HIGH IMPACT / LOW EFFORT

**Problem:** The Build workflow triggers on every push, including doc-only, plan, or config changes.

**Fix:** Add `paths-ignore` to skip builds when only non-code files change.

```yaml
on:
  push:
    branches: [main]
    paths-ignore:
      - '**/*.md'
      - 'docs/**'
      - 'LICENSE'
      - '.gitignore'
      - 'PLAN*.md'
      - 'REPORT*.md'
      - 'adrs/**'
      - 'coverage/**'
      - 'cnpg/**'
      - 'scripts/**'
  pull_request:
    branches: [main]
    paths-ignore:
      - '**/*.md'
      - 'docs/**'
      - 'LICENSE'
      - '.gitignore'
      - 'PLAN*.md'
      - 'REPORT*.md'
      - 'adrs/**'
      - 'coverage/**'
      - 'cnpg/**'
      - 'scripts/**'
```

**Estimated savings:** 30–50% fewer Build runs (this project has frequent doc/plan commits).

**Caveat:** If Build is a required status check for PR merge, skipped runs show as "pending" forever. Fix by adding a lightweight "pass-through" job that always succeeds, or use `paths-filter` action to set a condition.

---

### Step 2: Drop or conditionally skip Windows from the Build matrix ⭐ HIGH IMPACT / LOW EFFORT

**Problem:** The Windows build is `continue-on-error: true` (experimental) and is the slowest job — pgrx downloads and compiles PostgreSQL from source (~10–15 min). It's not a gating check, yet burns the most minutes.

**Fix — Option A (recommended):** Remove Windows from the Build workflow entirely. Keep it only in the manual CI workflow for periodic verification.

```yaml
matrix:
  include:
    - os: ubuntu-22.04
      artifact_suffix: linux-amd64
      archive_ext: tar.gz
    - os: macos-14
      artifact_suffix: macos-arm64
      archive_ext: tar.gz
    # Windows build available via manual CI workflow
```

**Fix — Option B:** Move Windows to a separate job gated on a label or `workflow_dispatch`:

```yaml
build-windows:
  if: github.event_name == 'workflow_dispatch' || contains(github.event.pull_request.labels.*.name, 'test-windows')
```

**Estimated savings:** ~10–15 min per Build run (the full Windows job duration).

---

### Step 3: Add timeout-minutes to all jobs ⭐ MEDIUM IMPACT / LOW EFFORT

**Problem:** A hung job (e.g., E2E Docker build, CNPG wait) can run for up to 6 hours (GitHub default), silently draining the budget.

**Fix:** Add `timeout-minutes` to every job:

| Job | Recommended timeout |
|-----|-------------------|
| Lint | 10 min |
| Build (per-platform) | 20 min |
| Docker image build | 15 min |
| Unit tests | 15 min |
| Integration tests | 15 min |
| E2E tests | 25 min |
| Benchmarks | 20 min |
| CNPG smoke test | 15 min |
| Coverage | 15 min |
| Release jobs | 30 min |

Example:
```yaml
jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
```

**Estimated savings:** Prevents runaway cost; caps worst-case at a known ceiling.

---

### Step 4: Reduce artifact retention days ⭐ MEDIUM IMPACT / LOW EFFORT

**Problem:** Stored artifacts count against GitHub storage quotas. Current settings:
- Build artifacts: 14 days
- Docker image artifact: 7 days
- Benchmark results: 14 days (manual CI) + 30 days (benchmarks)
- Coverage: 14 days

**Fix:** Reduce retention for non-release artifacts:

| Artifact | Current | Recommended |
|----------|---------|-------------|
| Build packages (`pkg-*`) | 14 days | 5 days |
| Docker image (`docker-image`) | 7 days | 3 days |
| Benchmark results | 14–30 days | 7 days |
| Coverage LCOV | 14 days | 7 days |
| Release artifacts | 30 days | 30 days (keep) |

```yaml
- uses: actions/upload-artifact@v4
  with:
    retention-days: 5  # was 14
```

**Estimated savings:** Reduces storage costs; no impact on workflow speed.

---

### Step 5: Split Build into fast-check gate + full build ⭐ MEDIUM IMPACT / MEDIUM EFFORT

**Problem:** All 3 platform builds start immediately. If there's a syntax error, all 3 fail after ~8 min each.

**Fix:** Add a fast `check` job that runs `cargo check` + `cargo clippy` + `cargo fmt` on Linux only (~2 min). The full build matrix depends on it via `needs: check`.

```yaml
jobs:
  check:
    name: Quick check
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v4
      - uses: ./.github/actions/setup-pgrx
      - run: cargo fmt -- --check
      - run: cargo check --all-targets --features pg18
      - run: cargo clippy --all-targets --features pg18 -- -D warnings

  build:
    name: Build (${{ matrix.artifact_suffix }})
    needs: check
    # ...existing matrix...
```

**Estimated savings:** On check failure, saves ~20+ min of wasted build time across the matrix. Typical pushes that pass add ~2 min of overhead.

**Note:** This replaces the current separate `lint` job — the quick check subsumes it.

---

### Step 6: Skip Docker image build on non-Docker changes ⭐ MEDIUM IMPACT / MEDIUM EFFORT

**Problem:** The Build workflow always builds the Docker E2E image (~5–8 min) even when only Rust source changed (and the Dockerfile didn't).

**Fix:** Use `dorny/paths-filter` to conditionally run the Docker build:

```yaml
jobs:
  detect-changes:
    runs-on: ubuntu-latest
    outputs:
      docker: ${{ steps.filter.outputs.docker }}
      rust: ${{ steps.filter.outputs.rust }}
    steps:
      - uses: actions/checkout@v4
      - uses: dorny/paths-filter@v3
        id: filter
        with:
          filters: |
            docker:
              - 'tests/Dockerfile.e2e'
              - 'Cargo.toml'
              - 'Cargo.lock'
              - 'src/**'
              - 'sql/**'
              - 'pg_trickle.control'
            rust:
              - 'src/**'
              - 'Cargo.toml'
              - 'Cargo.lock'

  build-docker:
    needs: detect-changes
    if: needs.detect-changes.outputs.docker == 'true'
    # ...existing docker job...
```

**Estimated savings:** Skips Docker build on ~20–30% of pushes (e.g., test-only, bench-only changes).

---

### Step 7: Consider making Build also manual, keep only Lint automatic ⭐ HIGH IMPACT / LOW EFFORT

**Problem:** If the project is in active solo development (not team/PR workflow), even the Build workflow fires too often.

**Fix:** Create a minimal `lint.yml` that runs only `fmt + clippy` (~2–3 min) on push/PR, and make the full Build manual:

```yaml
# .github/workflows/lint.yml
name: Lint
on:
  push:
    branches: [main]
    paths-ignore: ['**/*.md', 'docs/**', 'LICENSE', 'PLAN*.md', 'adrs/**']
  pull_request:
    branches: [main]

jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v4
      - uses: ./.github/actions/setup-pgrx
      - run: cargo fmt -- --check
      - run: cargo clippy --all-targets --features pg18 -- -D warnings
```

Then change `build.yml` to `workflow_dispatch`.

**Estimated savings:** ~80% reduction — from ~20 min per push down to ~3 min (lint only).

**Trade-off:** Build artifacts won't be automatically produced. Run Build manually before releases or when you want cross-platform verification.

---

### Step 8: Optimize the setup-pgrx composite action ⭐ LOW IMPACT / LOW EFFORT

The `setup-pgrx` action is already well-cached. Minor improvements:

1. **Pin `actions/cache` to v4 consistently** — already done
2. **Add `save-always: true`** to Rust cache so partial builds are also saved on failure:
   ```yaml
   - uses: Swatinem/rust-cache@v2
     with:
       cache-on-failure: true
       save-always: true
   ```
3. **Consider `sccache`** for cross-job compilation cache if multiple jobs compile the same crate (diminishing returns given Swatinem already handles this).

**Estimated savings:** Marginal (~1–2 min improvement on cache misses).

---

## Implementation Order

| Priority | Step | Effort | Savings | Risk | Status |
|----------|------|--------|---------|------|--------|
| **P1** | 1. Path filters on Build | 5 min | 30–50% fewer runs | Low — may need pass-through job | ✅ Done |
| **P1** | 2. Drop Windows from Build | 5 min | ~10–15 min/run | Low — Windows stays in manual CI | ✅ Done |
| **P1** | 3. Add timeout-minutes | 10 min | Prevents runaway cost | None | ✅ Done |
| **P2** | 4. Reduce artifact retention | 5 min | Storage savings | None | ✅ Done |
| **P2** | 5. Fast check gate | 15 min | ~20 min on failures | Low | ✅ Done |
| **P2** | 6. Skip Docker conditionally | 15 min | ~5–8 min/run when skipped | Low | ✅ Done |
| **P3** | 7. Lint-only automatic | 10 min | ~80% reduction | Higher — no auto build artifacts | ✅ Done |
| **P3** | 8. Optimize setup-pgrx | 5 min | Marginal | None | ✅ Done |

---

## Expected Total Savings

**Before (current):** Every push to main or PR triggers Build with 4 jobs (lint + 3 platforms + Docker) ≈ **40–50 billable minutes**.

**After P1–P3:** Only Lint runs automatically (~3 min). Doc-only commits are skipped entirely. Full build is manual.

| Scenario | Before | After (P1+P2) | After (P1–P3) |
|----------|--------|---------------|----------------|
| Code push | ~45 min | ~20 min | ~3 min |
| Doc-only push | ~45 min | **0 min** (skipped) | **0 min** |
| Manual CI run | ~35 min | ~35 min | ~35 min |
| Release (v* tag) | ~30 min | ~30 min | ~30 min |

**Monthly estimate (assuming ~100 pushes/month):**
- Before: ~4,500 min
- After P1+P2: ~2,000 min (55% reduction)
- After P1–P3: ~300 min + manual runs (93% reduction)
