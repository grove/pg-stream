# Slimming Down pg_trickle for 1.0

**Date:** 30 April 2026  
**Version reviewed:** 0.42.0  
**Status:** Draft — a prioritised list of what we could cut, move, or simplify

---

## Why this report exists

pg_trickle's core mission is simple: keep pre-computed query results (called
*stream tables*) up to date, quickly and correctly, whenever underlying data
changes.

Over 40+ releases, however, the project has picked up a lot of extra weight:
marketing articles, demo setups that nobody maintains, overlapping
configuration knobs, side-projects bundled inside the main codebase, leftover
scripts from one-off refactors, and duplicated documentation. None of this
extra weight makes the core product faster or more correct — it just makes the
project harder to navigate, slower to build, and more expensive to maintain.

This report is a **menu of things we could cut**. Nothing here is a final
decision. The goal is to give the team a clear picture of what is sitting in
the repo, why it is there, what value (if any) it still provides, and whether
we should keep it for 1.0.

### How to read the priority labels

- **P0** — Obvious junk. Safe to delete today with no user impact.
- **P1** — High-value cleanup. Removes significant clutter. Minor coordination needed.
- **P2** — Structural moves. Larger effort but big payoff for long-term maintenance.
- **P3** — Nice-to-have polish. Can wait until after 1.0 if time is tight.

### Risk levels

- 🟢 **Low risk** — Internal only. No user would ever notice.
- 🟡 **Medium risk** — Touches optional features or documentation. Might affect a small number of users.
- 🔴 **High risk** — Changes public interfaces or relocates something users depend on.

---

## Quick summary table

| Priority | What | Action | Risk | Effort |
|----------|------|--------|------|--------|
| P0 | Leftover scripts and log files | Delete | 🟢 | <30 min |
| P0 | Duplicate documentation folder (`doc/`) | Delete | 🟢 | <15 min |
| P0 | Three deprecated configuration settings | Remove from code | 🟡 | 1-2 h |
| P0 | Auto-generated test artefacts checked into git | Stop tracking them | 🟢 | <30 min |
| P1 | 78 blog posts in the main repo | Move to a separate blog site | 🟡 | 2-4 h |
| P1 | 109 historical roadmap files | Archive old versions, drop duplicates | 🟢 | 2 h |
| P1 | Six iterations of the same planning document | Keep only the latest | 🟢 | 1 h |
| P1 | Four nearly-identical Dockerfiles | Merge into one | 🟡 | 3-4 h |
| P1 | Duplicate "playground" demo | Delete (keep the real demo) | 🟢 | <30 min |
| P2 | The dbt integration package | Move to its own repo | 🔴 | 1 day |
| P2 | The message relay binary | Move to its own repo | 🔴 | 1 day |
| P2 | Pre-built monitoring dashboards | Move to a companion repo | 🟡 | 2 h |
| P2 | Kubernetes (CloudNativePG) examples | Fold into documentation | 🟡 | 2 h |
| P3 | Outbox & Inbox messaging APIs | Mark as experimental or move out | 🔴 | 1-2 days |
| P3 | Optional code modules (telemetry, diagnostics, Citus) | Make them opt-in at build time | 🟡 | 1 day |
| P3 | 23 automated test/build/deploy pipelines | Consolidate to ~17 | 🟡 | 1 day |

---

## 1. Leftover files that serve no purpose (🟢 P0)

Several files in the root of the project are leftovers from past development
sessions. They add nothing and confuse newcomers.

| File | What it is | Why remove it |
|------|-----------|---------------|
| `arch1b_split.py` | A small Python script written once to help split a large source file during a refactor. It did its job months ago. | It will never be run again. It just clutters the top-level directory. |
| `trace_rng.py` | A random-number utility someone wrote for generating test traces. No test or build step actually calls it. | Dead code. If needed again, it can be rewritten in minutes. |
| `staged_files.txt` | A list of files that was accidentally committed from a developer's local staging area. | Should never have been in version control at all. |
| `test_output.txt` | A captured log from someone's test run on their laptop. | Same — not meant for version control. |

**Recommendation:** Delete all four. Add the file names to `.gitignore` to prevent accidental re-commits.

---

## 2. Four overlapping Dockerfiles (🟡 P1)

### What they are

Dockerfiles are recipes for building container images — self-contained packages
that let users run pg_trickle without installing it manually. We have four:

| File | Purpose |
|------|---------|
| `Dockerfile.hub` | Publishes the official image to Docker Hub |
| `Dockerfile.ghcr` | Publishes essentially the same image to GitHub's container registry |
| `Dockerfile.demo` | Builds a local demo image for trying things out |
| `Dockerfile.relay` | Builds the separate message-relay binary (see §7) |

### Why consider merging them

The first three produce almost identical images — they differ only in a few
metadata labels. Maintaining three separate files means any change to how we
build the extension must be repeated in three places, which is error-prone.

**Recommendation:** Merge `.hub`, `.ghcr`, and `.demo` into a single
Dockerfile that accepts a parameter for the target registry. Keep
`.relay` only as long as the relay binary lives in this repo (see §7 below).

---

## 3. The blog directory — 78 marketing articles (🟡 P1)

### What it is

The `blog/` folder contains 78 long-form articles exploring use-cases,
comparisons with other tools, and deep-dive explanations. Topics range from
"Deploying RAG at scale" to "Streaming to Kafka without Kafka" to "PostGIS
incremental geospatial."

### What value it provides

These articles can help potential users understand what pg_trickle can do and
how it compares to alternatives. They are content marketing.

### Why consider removing it

- **Not maintained:** When features change, nobody updates the blog posts.
  Stale technical advice is worse than no advice.
- **Not linked anywhere:** The README, installation guide, and official docs
  do not point to these posts. Most users never see them.
- **Makes the repo enormous:** 78 markdown files add hundreds of kilobytes and
  make it harder to find what matters.
- **Wrong home:** Blog content belongs on a website, not inside a source code
  repository that developers clone to build the extension.

**Recommendation:**

1. Move the entire `blog/` folder to a dedicated blog repository or website
   (e.g. a Hugo site published to GitHub Pages).
2. Identify the 3-4 articles that explain core concepts (like "Differential
   Dataflow Explained" or "Migrating from pg_ivm") and absorb their content
   into the official `docs/` folder.
3. Delete the rest from this repo.

---

## 4. Historical roadmap files — 109 version notes (🟢 P1)

### What they are

The `roadmap/` folder contains release notes for every version ever shipped,
from `v0.1.0` all the way to planned future versions. Many versions have two
files: a short summary and a `-full.md` expanded version.

### What value they provide

Release notes tell users what changed in each version. Useful when upgrading or
troubleshooting regressions.

### Why consider trimming

- **Ancient history:** Nobody runs v0.1 through v0.20 anymore. Keeping 60+
  files for long-dead versions is pure noise.
- **Duplication:** The `-full.md` variants repeat the same content in longer
  form. Two files per version is one too many.
- **Overlap with CHANGELOG.md:** We already maintain a single changelog file
  that covers the same ground.

**Recommendation:**

1. Archive all files for versions older than v0.20 (move to a separate branch
   or delete — the git history preserves them forever anyway).
2. Stop producing the `-full.md` variant; one file per version is enough.
3. For versions v1.0 and beyond, use `CHANGELOG.md` as the single source of
   truth and stop creating individual roadmap files.

---

## 5. Duplicate and outdated planning documents (🟢 P1)

### What they are

The `plans/` folder is where design documents, architecture decision records,
spike reports, and project assessments live. It has grown to ~140 files across
12 subdirectories.

### The problem

The biggest offender is six numbered iterations of the same "Overall
Assessment" document: versions 1, 2, 3, 7, 8, and 9. Each was written to
capture the project's state at a point in time, but only the latest one
reflects reality.

Other dead weight includes:

- **Completed spike reports** (the investigation is done, the conclusion is
  now in the code).
- **Superseded plans** (e.g. the "dbt adapter" plan was abandoned in favour of
  the "dbt macro" approach that shipped).
- **Future-direction reports** that predate the current roadmap.

### Why trim

Planning documents that no longer reflect reality are misleading. A newcomer
reading an old assessment will form wrong conclusions about the project's state.

**Recommendation:**

1. Keep only `PLAN_OVERALL_ASSESSMENT_9.md` (the latest). Archive the others.
2. Archive completed spikes and superseded plans into a `plans/archive/`
   subfolder so they are still findable but clearly marked as historical.
3. If the dbt and relay sub-projects move out (see §7), their planning
   documents should move with them.

---

## 6. Three overlapping "try it out" directories (🟢–🟡 P1–P2)

### What they are

The repo contains three separate places for showing people how to get started:

| Directory | Contents | Status |
|-----------|----------|--------|
| `playground/` | A minimal docker-compose setup with a seed SQL file | Barely maintained; duplicates `demo/` |
| `demo/` | A richer setup with Postgres, a Python dashboard generator, and Grafana | Semi-maintained |
| `examples/` | Two files: a dbt starter and a "non-differential mode" SQL example | Minimal content |

### Why consolidate

Having three directories that all say "try it out" confuses newcomers who don't
know which one to use. Worse, none of them are regularly tested in CI, so they
quietly break.

**Recommendation:**

1. **Delete `playground/`** — it is a strict subset of `demo/`.
2. **Trim `demo/`** to the essentials: one docker-compose file and one seed
   SQL script. Remove the Python dashboard generator unless it is actively
   tested.
3. **Delete `examples/`** — move the dbt example into the dbt sub-project
   (see §7) and fold the SQL example into the documentation.

---

## 7. Sub-projects that should live on their own (🔴 P2)

These are the two highest-impact candidates on this list. Both are legitimate
products, but they are not the core extension and they add significant weight.

### 7.1 The dbt integration package (`dbt-pgtrickle/`)

**What it is:** [dbt](https://www.getdbt.com/) is a popular tool for managing
data transformations. Our dbt package lets dbt users define stream tables using
dbt's workflow instead of raw SQL.

**What value it provides:** Makes pg_trickle accessible to teams that already
use dbt. Gives them familiar commands (`dbt run`, `dbt test`) instead of raw
SQL.

**Why move it out:**

- It already has its own version number (independent from the extension).
- It includes 500+ files (test fixtures, dbt configurations, integration
  tests) that have nothing to do with the core Postgres extension.
- Users who do not use dbt still pay the cost: longer builds, more CI time,
  a larger download.
- A separate repository (`grove/pg-trickle-dbt`) would let it evolve on its
  own schedule without blocking core releases.

### 7.2 The message relay binary (`pgtrickle-relay/`)

**What it is:** A standalone command-line program that watches for outbox/inbox
events produced by pg_trickle and forwards them to external messaging systems
like Kafka, NATS, RabbitMQ, Amazon SQS, Redis, or plain HTTP webhooks.

**What value it provides:** Lets pg_trickle push change notifications to the
rest of your infrastructure — useful for event-driven architectures where
downstream services need to react when data changes.

**Why move it out:**

- It is a separate binary, not a Postgres extension. It has its own
  dependencies (networking libraries for each messaging system) that are
  irrelevant to users who just want the extension.
- Those extra dependencies make the project's build heavier and its lock file
  larger.
- It already has a separate Dockerfile and several CI pipelines dedicated to
  building and testing it alone.
- A separate repository (`grove/pg-trickle-relay`) would make both projects
  simpler to build and release.

**Impact of moving both:** The core repo loses ~800+ files, the build becomes
faster, and the release process becomes cleaner. Existing users of these
sub-projects are unaffected — they would simply clone from a different URL.

---

## 8. The monitoring stack (`monitoring/`) (🟡 P2)

### What it is

A ready-made Docker setup containing Prometheus (for collecting metrics) and
Grafana (for displaying dashboards), pre-configured to show pg_trickle's
internal metrics.

### What value it provides

Makes it easy for someone evaluating the extension to see graphs of refresh
latency, queue depth, and error counts without configuring their own monitoring
tools.

### Why consider removing it

- Real production users already have their own monitoring infrastructure
  (Datadog, Grafana Cloud, New Relic, etc.). They will not use our bundled
  stack.
- Every time we rename or add a metric in the extension, the bundled dashboard
  JSON needs updating. If we forget, the dashboards silently break — creating
  a support burden.
- It is ~20 files of configuration that rarely gets tested.

**Recommendation:** Move to a companion repository (e.g.
`grove/pg-trickle-observability`) or reduce to a single example dashboard JSON
file in the documentation.

---

## 9. CloudNativePG examples (`cnpg/`) (🟡 P2)

### What it is

[CloudNativePG](https://cloudnative-pg.io/) is an operator for running
PostgreSQL on Kubernetes. The `cnpg/` folder provides Dockerfiles and YAML
templates for deploying pg_trickle in a CNPG-managed cluster.

### What value it provides

Kubernetes users get a working starting point for their deployment instead of
figuring it out from scratch.

### Why consider simplifying

- This is documentation, not code. The Dockerfiles and YAML templates are
  better served as code blocks inside an integration guide in `docs/`.
- Four standalone files in a top-level directory overstate the importance of
  this one deployment method.

**Recommendation:** Collapse into a page under `docs/integrations/cnpg/` with
the examples shown inline.

---

## 10. Deprecated configuration settings (🟡 P0)

### What they are

pg_trickle exposes about 70 configuration settings (called GUCs in PostgreSQL
terminology) that let administrators tune its behaviour. Three of these were
deprecated in earlier releases and are now completely non-functional:

| Setting | What it used to do | Why remove it |
|---------|-------------------|---------------|
| `pg_trickle.event_driven_wake` | Controlled whether the scheduler should wake immediately when data changes, rather than waiting for its next scheduled cycle. | This behaviour is now always on — the setting does nothing since v0.39. Our own docs already say "will be removed in v1.0." |
| `pg_trickle.wake_debounce_ms` | Set how long to wait before waking the scheduler after a burst of changes, to avoid excessive CPU use. | Paired with `event_driven_wake` — equally dead. |
| `pg_trickle.merge_planner_hints` | Gave the query planner extra hints about how to merge changes. | Replaced by a better mechanism (`planner_aggressive`) many versions ago. |

### Why remove them now

Keeping dead settings around confuses users who try to tune them. It also means
we carry code that processes them (parsing, validation, documentation) even
though it does nothing useful. Removing them simplifies the codebase and sends
a clear signal that the 1.0 surface is intentional, not accidental.

### Additional experimental settings to review

Seven more settings exist for features that are incomplete or rarely used. They
should be individually evaluated before 1.0 — either finish the feature, test
it properly, or remove the setting:

| Setting | What it controls |
|---------|------------------|
| `pg_trickle.foreign_table_polling` | Support for polling changes from foreign (remote) tables |
| `pg_trickle.matview_polling` | Support for watching materialized views for changes |
| `pg_trickle.buffer_partitioning` | An optimisation that splits change buffers into partitions for throughput |
| `pg_trickle.online_schema_evolution` | Ability to change a stream table's query without downtime |
| `pg_trickle.ivm_topk_max_limit` | Maximum number of rows for "top-K" immediate-mode queries |
| `pg_trickle.ivm_recursive_max_depth` | How deep recursive queries are allowed to go in immediate mode |
| `pg_trickle.max_grouping_set_branches` | Parser limit for `GROUP BY ROLLUP/CUBE` complexity |

The general rule: any setting that lacks both documentation and a test is a
removal candidate. A sweep across all 70 settings will likely identify 5-15
that can be cut.

---

## 11. SQL functions that might not belong in the core (🟡–🔴 P3)

### What they are

pg_trickle exposes 23 SQL functions that users can call. These are the
extension's public interface — the "buttons" that users press.

### The core — definitely keeping

These are the reason the extension exists:

- `create_stream_table()` — Define a new stream table from a SQL query
- `alter_stream_table()` — Change its configuration
- `drop_stream_table()` — Remove it
- `refresh_stream_table()` — Manually trigger an update
- `pgt_status()` — See the current state of all stream tables
- `health_check()` — Verify the extension is working properly
- `explain_stream_table()` — Show how a refresh will be executed

### Diagnostic helpers — useful but optional

- `diagnose_errors()` — Show what went wrong with the last refresh
- `validate_query()` — Check if a SQL query can be used as a stream table
- `check_cdc_health()` — Verify that change tracking is healthy
- `recommend_schedule()` — Suggest a good refresh interval
- `repair_stream_table()` — Fix a stream table that got into a bad state

These are genuinely helpful for troubleshooting but could be made optional
(only included in "full" builds) to keep the minimal surface small.

### Outbox, Inbox, and Drain — should probably move out

- `enable_outbox()` — Start publishing change events to an outbox table for
  external consumers
- `enable_inbox()` — Accept incoming events from external systems
- `drain()` — Gracefully stop all stream table processing (for shutdowns)
- `rebuild_cdc_triggers()` — Recreate the internal change-tracking triggers

The outbox and inbox functions are designed to work with the relay binary (§7.2
above). If the relay moves to its own repository, these functions should
probably go with it — either as a separate small extension or as part of the
relay's setup scripts. Keeping them in the core extension adds API surface that
only a subset of users need.

**General rule:** Every SQL function we ship in 1.0 becomes a contract we must
maintain. Any function not documented in the SQL reference should either be
documented (meaning we commit to supporting it) or removed.

---

## 12. Optional code modules that could be made opt-in (🟡 P3)

### What this means

The extension is built from about 20 internal modules. Some of them provide
functionality that not every deployment needs. By making these optional at build
time, packagers (and cloud providers hosting the extension) could produce
smaller, faster binaries.

| Module | What it does | Who needs it |
|--------|-------------|--------------|
| **OpenTelemetry exporter** | Sends traces and spans to observability platforms (Jaeger, Honeycomb, etc.) | Only teams with distributed tracing infrastructure |
| **Prometheus metrics server** | Runs a tiny HTTP server inside Postgres to expose metrics for Prometheus to scrape | Only teams using Prometheus |
| **Background monitor** | Periodically checks stream table health and logs warnings | Useful but not critical to operation |
| **Diagnostic functions** | The `diagnose_errors()`, `validate_query()` etc. from §11 | Debugging only |
| **Citus compatibility** | Special handling for Citus distributed tables | Only Citus users (a small minority) |

None of these are needed for the core job of creating, refreshing, and
maintaining stream tables. Making them opt-in would not remove them — it would
just let people choose whether to include them.

---

## 13. Too many CI pipelines (🟡 P3)

### What they are

We have 23 automated workflows (GitHub Actions) that run on every commit,
nightly, weekly, or on releases. They build the code, run tests, check
security, publish images, and report coverage.

### The problem

Several workflows do nearly the same thing but are split into separate files
for historical reasons:

| Current (2 pipelines) | Could become (1 pipeline) | Why |
|----------------------|--------------------------|-----|
| `docker-hub.yml` + `ghcr.yml` | One pipeline that pushes to both registries | The only difference is the registry URL |
| `benchmarks.yml` + `e2e-benchmarks.yml` | One pipeline with different triggers | Same benchmarking logic, just different scope |
| `docs.yml` + `docs-drift.yml` | One pipeline with two checks | Both look at the same documentation tree |

Additionally, `tpch-explain-artifacts.yml` only runs manually and produces
artefacts for analysis — it is more of a developer tool than a CI check.

**Recommendation:** Consolidate from 23 to about 17-18 workflows. Fewer files
means less duplication and a smaller maintenance surface. If the dbt and relay
sub-projects move out, their dedicated workflows go with them automatically.

---

## 14. Scripts that have outlived their purpose (🟢 P2)

The `scripts/` directory contains ~20 utility scripts. Most are actively used
by CI or developers, but a few are one-off helpers that did their job and are
now just sitting around:

| Script | What it did | Why consider removing |
|--------|-----------|---------------------|
| `add_roadmap_crosslinks.py` | Added cross-reference links between roadmap documents | A one-time documentation fixup; never needs to run again |
| `split_roadmap.py` | Splits a roadmap file into short and `-full.md` variants | Only needed if we keep the dual-file convention (§4 recommends dropping it) |
| `convert_matviews_to_pgtrickle.py` | Helps users migrate from PostgreSQL materialized views to stream tables | Better as documentation (a page in the migration guide) than a standalone script |
| `gen_catalogs.py` | Generates internal catalog SQL files | Verify if the build still uses it; if not, it is dead |

---

## 15. Auto-generated test files in version control (🟢 P0)

### What they are

The `proptest-regressions/` directory contains automatically generated test
inputs that a testing library (proptest) creates when it finds a bug. These
files are meant to help developers reproduce issues on their own machine.

### Why remove them from version control

- They are machine-generated noise — not human-written code.
- Standard practice is to list this directory in `.gitignore` and let each
  developer's local copy accumulate its own regression seeds.
- If a particular test input is important, it should be turned into a proper
  named test case in the source code, not left as a binary seed file.

**Recommendation:** Add `proptest-regressions/` to `.gitignore` and delete the
currently tracked files.

---

## 16. Performance benchmarks (🟢 — keep)

Three benchmark files (`diff_operators.rs`, `refresh_bench.rs`,
`scheduler_bench.rs`) are all actively maintained and integrated into our
regression-detection pipeline. **Keep all three.** They are small, valuable,
and not adding unnecessary weight.

---

## 17. Third-party library audit (🟡 P3)

Not covered in detail here, but worth a dedicated pass before 1.0. Tools like
`cargo machete` can detect libraries listed as dependencies that are never
actually used in the code. The relay sub-project (if still bundled) is the
biggest offender — it pulls in networking libraries for six different messaging
systems, all of which inflate the project's dependency tree even when nobody
is building the relay.

---

## What 1.0 looks like after the cleanup

If we do the P0 and P1 items alone:

- **5 junk files gone** from the top-level
- **78 blog posts** moved to their own home
- **~60 outdated roadmap files** archived
- **~12 duplicate planning docs** consolidated
- **`playground/` deleted** (one less confusing directory)
- **3 dead settings** removed from the configuration surface
- **3 Dockerfiles merged** into 1
- **Auto-generated test files** stopped cluttering git history

If we also do P2:

- **The dbt package** and **the relay binary** live in their own repositories,
  each free to release on their own schedule
- **The monitoring stack** lives in a companion repo where dashboard drift
  doesn't slow down core development
- **The Kubernetes examples** are folded into the docs where they belong

The result is a core repository that is clearly focused on its stated mission:
*streaming tables with incremental view maintenance for PostgreSQL*. Nothing
more, nothing less.

**Importantly, none of these changes affect correctness or data safety.** Every
candidate on this list is in the territory of marketing content, demo
scaffolding, optional integrations, or dead code. The differential refresh
engine, the change-data-capture pipeline, the dependency graph, and the
scheduler are all untouched.

---

## Suggested order of work

1. **Day 1 — Quick wins (P0):** Delete junk files, remove `doc/`, gitignore
   proptest regressions, remove three deprecated settings.
2. **Days 2-4 — Content cleanup (P1):** Move blog posts out, archive old
   roadmap files, consolidate planning docs, merge Dockerfiles, delete
   playground.
3. **Week 2 — Structural moves (P2):** Split dbt and relay into their own
   repos, relocate monitoring and CNPG.
4. **Post-1.0 — Polish (P3):** Feature-gate optional modules, consolidate CI
   pipelines, audit dependencies, review remaining settings.

---

## Open questions

1. **Blog ownership:** Are the blog posts maintained elsewhere (a CMS, a
   separate writing tool) and mirrored here? Or is this repo the only copy?
   The answer determines whether "move out" means "copy to a new home" or
   "just delete."

2. **Relay users:** Do any customers or partners already depend on
   `pgtrickle-relay`? If so, moving it requires coordination (new download
   URL, new release announcements). If not, it can be moved silently.

3. **dbt Hub listing:** Does the dbt package registry point to a path inside
   this repository? If so, moving it means updating that listing.

4. **Minimal builds:** Do we anticipate cloud providers or embedded deployments
   wanting a stripped-down build without telemetry and diagnostics? If yes,
   the feature-gating work in §12 is worth prioritising.

5. **Experimental settings:** Which of the seven experimental settings in §10
   are actually slated for completion in the v1.x timeline? Those should stay
   (and get proper tests). The rest should go.
