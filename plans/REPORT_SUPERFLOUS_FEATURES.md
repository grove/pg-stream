# REPORT: Superfluous Features & Files for pg_trickle 1.0 Cleanup

**Date:** 30 April 2026
**Version analysed:** 0.42.0 (commit `aab1b36`)
**Status:** Draft for review — long, prioritised list of removal candidates

---

## Purpose

The project has accumulated significant weight on the road to 1.0: aspirational
marketing content, parallel sub-projects, deprecated knobs, duplicated demos,
historical planning artefacts, and a long tail of optional functionality.

This report catalogues **candidates for removal, archival, consolidation, or
relocation** before tagging 1.0. Each candidate has a recommended action, a
rough size/effort estimate, and a risk classification:

- 🟢 **Low risk** — safe deletion, no user-visible impact
- 🟡 **Medium risk** — touches docs/CI/optional features; coordinate with users
- 🔴 **High risk** — public surface change or sub-project relocation

> This is a **menu of candidates**, not a final cut list. The goal is to
> surface every plausible piece of weight so maintainers can debate and pick.

---

## Top recommendations at a glance

| Priority | Candidate | Action | Risk | Est. effort |
|----------|-----------|--------|------|-------------|
| P0 | Garbage top-level files (`staged_files.txt`, `test_output.txt`, `arch1b_split.py`, `trace_rng.py`) | Delete | 🟢 | <30 min |
| P0 | `doc/pg_trickle.md` (duplicate of `docs/`) | Delete | 🟢 | <15 min |
| P0 | Deprecated GUCs (`event_driven_wake`, `wake_debounce_ms`, `merge_planner_hints`) | Delete | 🟡 | 1-2 h |
| P0 | `proptest-regressions/` committed cases | `.gitignore` + delete tracked files | 🟢 | <30 min |
| P1 | `blog/` (78 markdown files) | Move to a separate site/repo | 🟡 | 2-4 h |
| P1 | `roadmap/` (109 historical version files, many `*-full.md` duplicates) | Archive old + drop `-full` variants | 🟢 | 2 h |
| P1 | `plans/PLAN_OVERALL_ASSESSMENT_{1,2,3,7,8,9}.md` | Keep latest only, archive rest | 🟢 | 1 h |
| P1 | Consolidate `Dockerfile.{demo,hub,ghcr}` into one parameterised image | Refactor | 🟡 | 3-4 h |
| P1 | `playground/` (duplicate of `demo/`) | Delete | 🟢 | <30 min |
| P2 | `dbt-pgtrickle/` | Move to standalone repo | 🔴 | 1 day |
| P2 | `pgtrickle-relay/` | Move to standalone repo | 🔴 | 1 day |
| P2 | `monitoring/` (Prom + Grafana stack) | Move to companion repo or `examples/` | 🟡 | 2 h |
| P2 | `cnpg/` | Collapse into `docs/integrations/cnpg/` | 🟡 | 2 h |
| P3 | Outbox / Inbox SQL APIs | Mark experimental or move to relay sub-project | 🔴 | 1-2 days |
| P3 | Feature-gate `otel`, `metrics_server`, `monitor`, `diagnostics` modules | Cargo features | 🟡 | 1 day |
| P3 | Consolidate CI workflows (`docker-hub` + `ghcr`, `docs` + `docs-drift`, benchmarks pair) | Refactor | 🟡 | 1 day |

---

## 1. Top-level files (🟢 P0)

| File | Size | Reason | Action |
|------|------|--------|--------|
| [arch1b_split.py](arch1b_split.py) | ~50 LOC | One-off refactor helper from the `src/refresh/mod.rs` split | **DELETE** |
| [trace_rng.py](trace_rng.py) | ~40 LOC | Stand-alone RNG trace utility, not invoked by any test | **DELETE** |
| [staged_files.txt](staged_files.txt) | small | Stale `git add` artefact accidentally committed | **DELETE**, add to `.gitignore` |
| [test_output.txt](test_output.txt) | large | Captured test log from a developer's laptop | **DELETE**, add to `.gitignore` |
| `Dockerfile.demo` / `Dockerfile.hub` / `Dockerfile.ghcr` / `Dockerfile.relay` | 4 files | Three of them produce overlapping Postgres + extension images differing only in metadata/labels | **CONSOLIDATE** to one parameterised `Dockerfile` driven by build args. Keep `Dockerfile.relay` only if the relay binary stays in-repo (see §7) |

---

## 2. Documentation & marketing content

### 2.1 `blog/` — 78 markdown files (🟡 P1)

[blog/](blog/) contains 60+ long-form posts covering use-cases (RAG, Kafka,
PageRank, PostGIS, vector aggregates, etc.), most of which are **aspirational
marketing** rather than maintained reference material:

- Not linked from `README.md`, `docs/`, or any release notes.
- Not version-controlled against feature changes — risk of stale content
  drifting into the official docs path.
- Heavy SEO/marketing flavour rather than tightening technical accuracy.

**Recommendation:**

1. **Move all of `blog/` to a separate site or repo** (e.g.
   `grove/pg-trickle-blog` or a Hugo/Jekyll site) — this is the bulk win.
2. **Promote** the handful that are genuine reference material into `docs/`:
   - `differential-dataflow-explained.md` → `docs/THEORY.md`
   - `ivm-without-primary-keys.md` → `docs/LIMITATIONS.md`
   - `migrating-from-pg-ivm.md` → `docs/MIGRATION.md`
   - `medallion-architecture-postgresql.md` → `docs/PATTERNS.md`
3. **Delete** the rest from the repository.

**Impact:** ~78 files, ~150-300 KB; cleaner repo top-level; eliminates a
long-tail maintenance hazard.

### 2.2 `roadmap/` — 109 files (🟢 P1)

[roadmap/](roadmap/) contains per-version notes from `v0.1.0` through `v1.5.0`,
many split into `vX.Y.Z.md` + `vX.Y.Z.md-full.md` pairs.

Issues:

- Pre-v0.20 versions are obsolete — no users running them.
- The `-full.md` duplicates roughly double the file count.
- Content overlaps `CHANGELOG.md`.

**Recommendations (independent — pick any combination):**

1. **Archive** all `v0.1.x`–`v0.20.x` notes to a separate branch (e.g.
   `archive/old-roadmap`). Drop them from `main`.
2. **Drop `-full.md` variants**; keep one canonical file per version.
3. **Stop generating new `-full.md`** files going forward (see
   `scripts/split_roadmap.py` — likely also retire-able).
4. Forward-looking `v1.1.0`+ notes should live in [ROADMAP.md](ROADMAP.md) only.

**Impact:** Potentially -60 to -90 files.

### 2.3 `doc/` vs `docs/` (🟢 P0)

[doc/](doc/) contains exactly one file (`pg_trickle.md`) — a placeholder for
PGXN packaging. The canonical documentation lives in [docs/](docs/).

**Recommendation:** Delete `doc/`, make PGXN packaging derive its `doc` from
`README.md` or `docs/INDEX.md` directly.

### 2.4 `plans/` consolidation (🟢 P1)

[plans/](plans/) and its 12 subdirectories hold ~140 planning documents. The
worst offender:

- `PLAN_OVERALL_ASSESSMENT.md`, `PLAN_OVERALL_ASSESSMENT_2.md`, `_3`, `_7`,
  `_8`, `_9` — six historical iterations of the same assessment.

Other candidates:

| File | Status | Action |
|------|--------|--------|
| `plans/PLAN_OVERALL_ASSESSMENT{,_2,_3,_7,_8,_9}.md` | Iterations | **Keep `_9` only**; archive the rest |
| `plans/PLAN_PARTITIONING_SPIKE.md` | Spike completed | **Archive** |
| `plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md` | Now reflected in code/tests | **Archive** |
| `plans/REPORT_FUTURE_DIRECTIONS.md` | Predates current ROADMAP | Re-evaluate vs `ROADMAP.md` |
| `plans/dbt/PLAN_DBT_ADAPTER.md` | Superseded by `PLAN_DBT_MACRO.md` (Implemented) | **Archive** |
| `plans/relay/`, `plans/dbt/` | Likely move with sub-projects (see §7) | **Move out** if §7 happens |

**Impact:** Easily -15 to -25 plan files without losing institutional memory
(use a `plans/archive/` subdir if total deletion feels too risky).

---

## 3. Examples, demos, and showcase directories

### 3.1 `playground/` (🟢 P1)

[playground/](playground/) contains `README.md`, `docker-compose.yml`,
`seed.sql` — a thin demo stack that overlaps almost entirely with
[demo/](demo/).

**Recommendation:** **DELETE** `playground/`. Keep `demo/` as the single
canonical quick-start.

### 3.2 `demo/` (🟡 P2)

[demo/](demo/) is a richer demo (Postgres + dashboard generator). It is
maintained but only nominally — see whether it is referenced from `INSTALL.md`
or `README.md` regularly.

**Recommendation:** Keep, but trim aggressively (single docker-compose + one
seed SQL). Move the Python `dashboard/` generator out unless it is wired into
nightly CI.

### 3.3 `examples/` (🟢 P2)

[examples/](examples/) holds `dbt_getting_started/` and `non-differential.sql`.

**Recommendation:** Move `dbt_getting_started/` with the dbt sub-project (§7).
Inline `non-differential.sql` into `docs/EXAMPLES.md`. **Delete** the
directory.

### 3.4 `monitoring/` (🟡 P2)

[monitoring/](monitoring/) ships an opinionated Prometheus + Grafana docker
stack with dashboards.

**Issues:**

- Dashboards drift any time a metric name changes — silent maintenance debt.
- Real users plug into their own observability stack.

**Recommendation:** Move to a companion repo (e.g.
`grove/pg-trickle-observability`) or strip down to a single example dashboard
JSON in `docs/observability/`.

### 3.5 `cnpg/` (🟡 P2)

[cnpg/](cnpg/) contains 2 Dockerfiles + 2 example YAMLs for CloudNativePG.

**Recommendation:** Collapse into `docs/integrations/cnpg/` with the YAMLs as
fenced code blocks. Leave the Dockerfile only if CNPG image-volume CI is
in scope for 1.0.

---

## 4. Sub-projects in the workspace

These are major candidates because they make up a non-trivial fraction of the
workspace, the build matrix, and the CI surface — but they are **not the core
extension**.

### 4.1 `dbt-pgtrickle/` (🔴 P2)

A standalone dbt package (~500+ files including its `integration_tests/`
fixtures, dbt project, and CI hooks).

**Why move out:**

- Independent versioning and release cadence already (`dbt_project.yml`
  version differs from extension).
- dbt users are a subset; non-dbt users carry the maintenance + CI cost.
- A separate repo (`grove/pg-trickle-dbt`) lets the dbt package depend on a
  pinned `pg_trickle` release without coupling its release timeline.

**Recommendation:** **Move to a separate repository** before 1.0. Leave a
1-line README in this repo pointing at it.

### 4.2 `pgtrickle-relay/` (🔴 P2)

A standalone Rust CLI binary bridging outbox/inbox to Kafka, NATS, RabbitMQ,
SQS, Redis, and webhooks (six optional Cargo features).

**Why move out:**

- It is not the extension — it is a sidecar.
- It pulls in many heavy optional dependencies (rdkafka, nats, etc.) that
  inflate the workspace `Cargo.lock` even when no one is building it.
- Tests, Dockerfile (`Dockerfile.relay`), and several CI jobs exist solely
  for the relay.

**Recommendation:** **Move to `grove/pg-trickle-relay`.** Delete
`Dockerfile.relay`, `pgtrickle-relay/`, `plans/relay/`, and the relay-specific
CI jobs from this repo. Coordinate with §6 (outbox/inbox API).

### 4.3 `fuzz/` (🟡 keep)

Six fuzz targets exist (`cdc`, `cron`, `dag`, `guc`, `parser`, `wal`). Already
gated behind `fuzz-smoke.yml` (weekly).

**Recommendation:** **Keep.** Document explicitly as a nightly/weekly tier in
`CONTRIBUTING.md`. Re-evaluate per-target value (e.g. drop `cron_fuzz` if no
findings).

---

## 5. Configuration surface — GUCs in `src/config.rs` (🟡 P0–P2)

`src/config.rs` is **4,509 lines** and exposes around 70 GUCs. A leaner 1.0
surface should classify every one of them as Core, Performance, Observability,
Experimental, or Deprecated.

### 5.1 Deprecated — delete in 1.0 (🟡 P0)

| GUC | Notes |
|-----|-------|
| `pg_trickle.event_driven_wake` | No-op since v0.39; marked for removal in v1.0 in docs |
| `pg_trickle.wake_debounce_ms` | Paired with above; obsolete |
| `pg_trickle.merge_planner_hints` | Replaced by `planner_aggressive` long ago |

**Action:** Remove the GUCs, the related plumbing, and references from docs.

### 5.2 Experimental / rarely used — review (🟡 P2)

Candidates to either remove, hide behind a single `experimental_features` GUC,
or push to v1.1:

- `pg_trickle.foreign_table_polling`
- `pg_trickle.matview_polling`
- `pg_trickle.buffer_partitioning`
- `pg_trickle.online_schema_evolution`
- `pg_trickle.ivm_topk_max_limit`
- `pg_trickle.ivm_recursive_max_depth`
- `pg_trickle.max_grouping_set_branches`

Each one that survives should have an integration test that *fails* if the GUC
is removed — otherwise it is dead surface.

### 5.3 General principle

Any GUC without (a) a user-facing doc entry in `docs/CONFIGURATION.md` and
(b) a regression test should be a removal candidate. A pre-1.0 audit pass
across all 70 will likely cull 5–15.

---

## 6. SQL-callable API surface — `src/api/mod.rs` (🟡 P3)

`src/api/mod.rs` is **7,387 lines** with **23 `#[pg_extern]` functions**.

### 6.1 Core (must keep)

`create_stream_table`, `alter_stream_table`, `drop_stream_table`,
`refresh_stream_table`, `pgt_status`, `health_check`, `explain_stream_table`.

### 6.2 Diagnostics — review (🟡 P3)

`diagnose_errors`, `list_auxiliary_columns`, `validate_query`,
`check_cdc_health`, `recommend_schedule`, `repair_stream_table`.

These are valuable but should arguably live in a `pgtrickle_diag` submodule or
behind a `diagnostics` Cargo feature so they can be excluded from minimal
builds.

### 6.3 Outbox / Inbox / Drain — move out or mark experimental (🔴 P3)

- `pgtrickle.enable_outbox()`
- `pgtrickle.enable_inbox()`
- `pgtrickle.drain()`
- `pgtrickle.rebuild_cdc_triggers()`

Outbox and Inbox are tightly coupled to `pgtrickle-relay/`. If §4.2 moves the
relay out, these should follow — either into a companion extension
(`pg_trickle_outbox`) or into the relay repo with its own SQL bootstrap.

### 6.4 General principle

Every `#[pg_extern]` is a public contract for 1.0. Any function that is not
explicitly documented in `docs/SQL_REFERENCE.md` should be either documented or
removed before tagging.

---

## 7. Source modules — feature-gating (🟡 P3)

[src/lib.rs](src/lib.rs) wires in roughly 20 modules. The core (catalog, cdc,
dag, dvm, refresh, scheduler, hooks, shmem, hash, version, logging) is
non-negotiable. The optional layers below are candidates for **Cargo feature
gating** so packagers can build a slimmer binary:

| Module | Purpose | Feature flag candidate |
|--------|---------|------------------------|
| `otel` | OpenTelemetry exporter | `otel` |
| `metrics_server` | Prometheus HTTP endpoint | `metrics-server` |
| `monitor` | Background monitor worker | `monitoring` |
| `diagnostics` | `diagnose_errors`, `validate_query`, etc. | `diagnostics` |
| `citus` | Citus distributed-table compatibility | `citus` |

Default profile would include all of them; downstream packagers / minimal
builds get a smaller surface. None of these are actively required by the core
refresh / DVM path.

---

## 8. CI workflows — `.github/workflows/` (🟡 P3)

23 workflows is a lot. Consolidation candidates:

| Pair / group | Today | Proposed |
|--------------|-------|----------|
| `docker-hub.yml` + `ghcr.yml` | Two near-identical pipelines | Single workflow, two registry-push steps |
| `benchmarks.yml` + `e2e-benchmarks.yml` | Similar matrix split by trigger | One workflow with `on:` filter |
| `docs.yml` + `docs-drift.yml` | Two passes over the same docs tree | Single workflow with two jobs |
| `tpch-explain-artifacts.yml` | Manual-only artefact dump | Move to `workflow_dispatch`-only release script outside CI |
| `unsafe-inventory.yml` | Weekly | Drop to monthly; or roll into `security.yml` |

Conservative target: **23 → ~17–18 workflows.**

If the relay/dbt sub-projects move out (§4), several workflows go with them.

---

## 9. Scripts (`scripts/`) (🟢 P2)

`scripts/` contains ~20 shell + Python utilities. Most are useful, but the
following are likely safe to retire:

- `add_roadmap_crosslinks.py` — one-shot doc helper.
- `split_roadmap.py` — only useful if we keep the `-full.md` convention; if
  §2.2 lands, retire.
- `convert_matviews_to_pgtrickle.py` — one-off migration helper. Move to
  `docs/MIGRATION.md` as documentation, delete the script if unmaintained.
- `gen_catalogs.py` — verify it is still triggered by the build; otherwise
  drop.

---

## 10. Tests, regression artefacts, and benches

### 10.1 `proptest-regressions/` (🟢 P0)

These files are **machine-generated regression seeds** committed verbatim.
Standard practice is to gitignore them and rely on developers to share
interesting cases via test fixtures.

**Recommendation:** Add `proptest-regressions/` to `.gitignore`, delete tracked
files. If a specific failing case needs to be preserved, promote it to a named
proptest seed in source.

### 10.2 `benches/` (🟢 keep)

Three benches (`diff_operators.rs`, `refresh_bench.rs`, `scheduler_bench.rs`)
all show recent activity and are wired into the regression check. **Keep all
three.**

### 10.3 `tests/`

A test inventory pass would likely surface a few overlapping E2E test files
(particularly older `e2e_*` files that predate the Light-E2E refactor).
Recommend a follow-up audit, not in scope for this report.

---

## 11. Cargo dependency audit (🟡 P3)

Out of scope for this textual report, but worth a `cargo machete` /
`cargo +nightly udeps` pass before 1.0. Likely candidates for trimming:

- Optional relay-only dependencies (rdkafka, nats, lapin, aws-sdk-sqs, etc.)
  — auto-resolved if §4.2 lands.
- Heavy serde alternatives if any are unused.

---

## Summary — what 1.0 looks like after the cuts

If even the P0 and P1 items land:

- **~5 garbage top-level files removed.**
- **78 blog posts** → external site.
- **~60 roadmap files** → archived.
- **~12 plan files** → consolidated.
- **`playground/` deleted, `monitoring/` + `cnpg/` relocated.**
- **3 deprecated GUCs gone.**
- **3 Dockerfiles → 1.**

P2/P3 (sub-projects + feature-gating + CI consolidation) take longer but
deliver a much leaner core extension that maps tightly to the README's
"streaming tables with incremental view maintenance" pitch.

**None of this should compromise correctness, durability, or the differential
refresh path** — the explicit non-negotiables in `AGENTS.md`. Every cut above
is in marketing, demo, optional integration, or deprecated-knob territory.

---

## Suggested phasing

1. **Phase 1 (½ day):** Section 1, §2.3, §3.1, §5.1, §10.1, §2.4 partial.
2. **Phase 2 (2-3 days):** §2.1 (blog), §2.2 (roadmap), §3.4, §3.5, §9.
3. **Phase 3 (1 week):** §4.1 (dbt repo split), §4.2 (relay repo split),
   §6.3 (outbox/inbox follow), §8 (CI consolidation), §11 (deps).
4. **Phase 4 (post-1.0):** §5.2, §6.2, §7 feature-gating, broader test
   inventory.

---

## Open questions for maintainers

1. Are the `blog/` posts authored elsewhere and mirrored here, or is this the
   source of truth? That changes whether we can simply delete them.
2. Is `pgtrickle-relay/` already a public surface customers depend on? Moving
   it out is much cheaper if not.
3. Same question for `dbt-pgtrickle/` — does the dbt Hub package point at this
   path?
4. Do we want a `pg_trickle_minimal` Cargo profile for embedded/cloud
   deployments, justifying §7's feature-gating?
5. Which experimental GUCs in §5.2 are slated for v1.x feature work? Those
   should be kept (and tested); the rest can be culled.
