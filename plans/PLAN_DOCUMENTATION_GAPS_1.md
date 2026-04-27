# Plan: Documentation Gap Analysis (Round 1)

**Date:** 2026-04-27
**Status:** PROPOSED — review and prioritise before implementing
**Scope:** Full review of `docs/`, `README.md`, `ESSENCE.md`, `INSTALL.md`,
`ROADMAP.md`, the integrations/tutorials/research subtrees, and the
ancillary READMEs for `playground/`, `demo/`, `pgtrickle-tui`, and
`pgtrickle-relay/`.
**Author audience for this report:** maintainers and documentation owners.
**Audience target the recommendations are written for:** newcomers ranging
from SQL-literate analysts to senior DBAs and SREs, with an explicit
emphasis on lowering the technical barrier of entry.

---

## 1. Executive Summary

pg_trickle ships an unusually large body of documentation — **~32,000 lines
across 70+ files**, with strong reference material (SQL_REFERENCE.md,
CONFIGURATION.md, FAQ.md, TROUBLESHOOTING.md) and a thorough hands-on
tutorial (GETTING_STARTED.md). The technical accuracy is high and the
breadth is impressive.

However, the documentation has grown by accretion rather than by design,
and a non-technical reader landing on the project today will struggle.
The most important problems are:

1. **No truly approachable entry point.** README, introduction.md, and
   ESSENCE.md all assume the reader already understands materialized
   views, IVM, DBSP, CDC, MERGE, and PostgreSQL internals.
2. **No glossary.** Terms like *frontier*, *DVM*, *DRed*, *fuse*, *tier*,
   *watermark*, *SCC*, *bilinear expansion*, *semi-naive evaluation*, and
   *delta query* are used freely without definition.
3. **Several headline features have no user-facing doc.** Snapshots,
   self-monitoring, the predictive cost model, the multi-database
   launcher, and the parallel refresh worker pool are referenced in code
   and the changelog but not surfaced as readable pages.
4. **Citus support is buried** under `integrations/` even though the
   README treats it as a marquee v0.32+ capability.
5. **Nine documentation files are mdBook stubs** (`{{#include …}}`),
   which render as empty or broken pages on GitHub.
6. **The book's table of contents (`SUMMARY.md`) is incomplete** —
   roughly a dozen real documents are not listed at all.
7. **Persona-based reading paths are missing.** A product manager, an
   analytics engineer, a backend developer, a DBA, and an SRE all need
   different journeys; today they share a single dense menu of 50+
   entries with no progression hints.
8. **Visual material is sparse.** A handful of ASCII diagrams,
   no screenshots (TUI, Grafana, dashboard), no animation or worked
   "before/after" image of differential refresh.
9. **Numbers and version markers drift.** ESSENCE claims "1,200+ unit
   tests / 570+ E2E"; README claims "~1,670 unit / ~1,340 E2E / ~140
   TUI"; PATTERNS.md is pinned at v0.14.0+ while the latest release is
   v0.34.0.

The recommendation is a **major reorganisation in two phases**:

- **Phase A (low-risk, high-value):** fix the stubs, fill SUMMARY.md,
  add a glossary, write a one-page plain-language overview, add a
  use-case gallery, and add persona-routing on the landing page.
- **Phase B (deeper rework):** restructure the information architecture
  around four reading paths (Discover → Use → Operate → Build), promote
  Citus and Snapshots to top-level chapters, and add the missing
  feature pages.

Estimated total effort: roughly 30–45 documentation tasks, of which
~12 fit cleanly in a single milestone (see §15 for the sequenced
action plan).

---

## 2. Methodology

The review was carried out by:

1. Enumerating every Markdown file under `docs/`, plus the top-level
   READMEs, ESSENCE.md, INSTALL.md, ROADMAP.md, and CHANGELOG.md.
2. Reading the headline files in full (README.md, introduction.md,
   ESSENCE.md, GETTING_STARTED.md, ARCHITECTURE.md, SQL_REFERENCE.md,
   CONFIGURATION.md, FAQ.md, TROUBLESHOOTING.md).
3. Sampling representative chapters of larger docs and the first 60–120
   lines of every other doc to assess tone, depth, and accuracy.
4. Cross-referencing the documented surface area against the source
   tree (`src/api/*.rs`, `src/*.rs`, `pgtrickle-relay/`, `pgtrickle-tui/`)
   and against the README's own feature list.
5. Comparing the SUMMARY.md (mdBook navigation) against the actual
   contents of `docs/`.
6. Searching for plan documents that already address documentation
   (`plans/docs/PLAN_FAQ.md` is the only one found).

This is a **reading-and-mapping** review; it does not attempt to
fact-check every claim against the code, nor does it lint links.

---

## 3. Audience Analysis

The user request highlighted "a fairly non-technical audience". Today
the docs implicitly assume the following knowledge:

| Concept | Where it appears unexplained | Likely confusion for a non-technical reader |
|---|---|---|
| Materialized view & `REFRESH MATERIALIZED VIEW` | README §1, introduction.md, ESSENCE.md | Needs a one-paragraph plain-English primer. |
| Incremental View Maintenance (IVM) | Used as a term-of-art everywhere | No definition outside FAQ §GS-01. |
| DBSP / differential dataflow | README §History, introduction, ESSENCE | Unhelpful link to an academic paper. |
| Delta query / ΔQ | README, GETTING_STARTED, ARCHITECTURE | Symbol "ΔQ" never decoded. |
| Frontier / LSN | ARCHITECTURE, CONFIGURATION, refresh notes | Reader has no idea what either is. |
| `MERGE` execution | Performance section, BENCHMARK | Assumes deep PostgreSQL fluency. |
| pgrx, advisory locks, `pg_sys`, BGW | ARCHITECTURE | OK for contributors, not for users. |
| WAL, `wal_level=logical`, replication slots | CDC_MODES, INSTALL | Defined briefly but assumed elsewhere. |
| MERGE/CTE/LATERAL/SRF | SQL Reference | Reasonable to assume *some* SQL fluency, but worth a one-line gloss. |
| Bilinear expansion / DRed / semi-naive | introduction.md, README, DVM_OPERATORS | Pure jargon for non-PhDs. |

**Recommendation:** Add a **Glossary** page and link every first
appearance of these terms to it (mdBook supports this trivially via
relative links). See §11 for the proposed structure.

There is also a missing **"Why should I care?"** layer. The current
homepage tells you *what* pg_trickle does (`materialized views that
refresh themselves`) but not *who* it is for or *what they would build
with it*. A use-case gallery (§13) is the highest-leverage single
addition.

---

## 4. Inventory & Coverage Map

### 4.1 Existing documentation (by category)

| Category | Files | Largest | Smallest | Notes |
|---|---|---|---|---|
| Landing / overview | README, introduction.md, ESSENCE.md | 639 lines | 25 lines | Three different entry points; no single canonical one. |
| Tutorials | docs/tutorials/* (15 files) | 615 lines | 155 lines | Strong "What happens on …" series; uneven coverage of features. |
| Reference | SQL_REFERENCE, CONFIGURATION, ERRORS, FAQ | 4,897 lines | 469 lines | Excellent; very long and search-driven. |
| Operations | TROUBLESHOOTING, PRE_DEPLOYMENT, SCALING, BACKUP_AND_RESTORE, UPGRADING, RELEASE | 983 lines | 83 lines | BACKUP_AND_RESTORE is critically thin (83 lines) for a production topic. |
| Architecture | ARCHITECTURE, DVM_OPERATORS, DVM_REWRITE_RULES | 1,129 lines | 127 lines | Assumes contributor-level familiarity. |
| Patterns / Performance | PATTERNS, PERFORMANCE_COOKBOOK | 838 lines | 553 lines | Excellent recipes; need to be discoverable from a "Use Cases" page. |
| Integrations | docs/integrations/* (9 files) | 352 lines | 1 line (stub) | Citus is here but should be promoted. |
| Research / theory | docs/research/* (6 files) | 177 lines | 1 line (stubs ×3) | Three are mdBook stubs. |
| Other | DEMO, PLAYGROUND, TUI, RELAY, RELAY_GUIDE, INBOX, OUTBOX, PUBLICATIONS, SLA_SCHEDULING, CDC_MODES | varies | — | Several are excellent stand-alone but invisible from the SUMMARY (see §4.4). |

### 4.2 Stub files (mdBook `{{#include …}}` only)

These files render as a single line on GitHub, breaking the in-repo
reading experience:

| File | Source | Fix |
|---|---|---|
| `docs/changelog.md` | `../CHANGELOG.md` | Either keep as mdBook-only (and add a banner), or replace with a curated "What's new" page that links to CHANGELOG.md. |
| `docs/contributing.md` | `../CONTRIBUTING.md` | Same. |
| `docs/installation.md` | `../INSTALL.md` | Same; INSTALL.md should remain canonical. |
| `docs/roadmap.md` | `../ROADMAP.md` | Same. |
| `docs/security.md` | `../SECURITY.md` | SECURITY.md is a vulnerability-reporting policy, not user security guidance — see §6.7. |
| `docs/integrations/dbt.md` | `../../dbt-pgtrickle/README.md` | Inline a brief intro and link to the full README. |
| `docs/research/CUSTOM_SQL_SYNTAX.md` | `plans/sql/REPORT_CUSTOM_SQL_SYNTAX.md` | Plans should not be exposed as docs verbatim. Either curate or remove. |
| `docs/research/PG_IVM_COMPARISON.md` | `plans/ecosystem/GAP_PG_IVM_COMPARISON.md` | Same. |
| `docs/research/TRIGGERS_VS_REPLICATION.md` | `plans/sql/REPORT_TRIGGERS_VS_REPLICATION.md` | Same. |

Recommendation: **avoid `{{#include}}` for any file that ships in the
repo and is meant to be read on GitHub**. Either pick mdBook-only or
GitHub-only as the source of truth; do not split.

### 4.3 Missing from SUMMARY.md (mdBook navigation)

These files exist in `docs/` but are not in `docs/SUMMARY.md` and so
do not appear in the published book navigation:

- `docs/PERFORMANCE_COOKBOOK.md` (553 lines — major omission)
- `docs/ERRORS.md` (469 lines — major omission)
- `docs/INBOX.md`
- `docs/OUTBOX.md`
- `docs/PUBLICATIONS.md`
- `docs/CDC_MODES.md`
- `docs/SLA_SCHEDULING.md`
- `docs/DEMO.md`
- `docs/RELAY.md` (RELAY_GUIDE is listed; RELAY is not)
- `docs/DVM_REWRITE_RULES.md`
- `docs/integrations/citus.md`
- `docs/integrations/multi-tenant.md`
- `docs/integrations/orm.md`
- `docs/integrations/flyway-liquibase.md`
- `docs/integrations/dbt-hub-submission.md`
- `docs/research/multi_db_refresh_broker.md`
- `docs/research/PRIOR_ART.md`

This is the single most damaging structural gap: a reader using the
mdBook site cannot discover roughly a third of the documentation.

### 4.4 Documented in code but missing in docs

A scan of `src/api/*.rs` against the docs surfaces these
under-documented features:

| Source module | Feature | Documentation status |
|---|---|---|
| `src/api/snapshot.rs` | Stream-table snapshot/restore (v0.27.0) | Mentioned in PATTERNS §"Replica Bootstrap & PITR Alignment"; no dedicated page; not in SUMMARY. |
| `src/api/self_monitoring.rs` | Built-in self-monitoring stream tables | No user-facing doc; only mentioned in ARCHITECTURE table. |
| `src/api/cluster.rs` | Multi-database cluster overview (`cluster_worker_summary`) | No dedicated doc; one paragraph in SCALING. |
| `src/api/publication.rs` | Predictive cost model | Cost model concepts surface in CONFIGURATION GUCs but no narrative doc. |
| `src/api/planner.rs` | `recommend_schedule` and other planner API | Brief mention in SLA_SCHEDULING; not in SQL_REFERENCE TOC visibly. |
| `src/api/metrics_ext.rs` | Extended Prometheus metrics | Documented partially in `integrations/prometheus.md`; full metric catalogue absent. |
| Citus distributed support | Major v0.32+ feature | Only `integrations/citus.md` (338 lines, not in SUMMARY). |
| `pgtrickle-relay/` (workspace) | Standalone relay binary | RELAY.md focuses on architecture; no "what is this and why would I run it" intro for non-Rust users. |
| `pgtrickle-tui/` | TUI binary, 18 subcommands, 13 views | TUI.md exists (536 lines); no per-subcommand reference akin to a man page. |

---

## 5. Critical Gaps (Phase A — high priority)

### 5.1 No plain-language overview

**Problem.** README.md (639 lines) is feature-complete but reads as a
spec. ESSENCE.md (25 lines) is closer in spirit to what a non-technical
reader needs but is undersized and buried. introduction.md (106 lines)
sits in the docs site only.

**Recommendation.** Promote a single canonical **"What is pg_trickle?"**
page (~150–250 lines) that:

- Opens with a one-sentence description ("Materialized views that keep
  themselves up to date — without external infrastructure.").
- Uses one **before/after diagram** (manual refresh pipelines vs.
  declarative stream tables).
- Names three concrete things you can build with it (dashboards,
  read-models, fraud/alert pipelines).
- Defers all internals (DBSP, frontier, DVM) to a "How it works" link.
- Ends with two CTAs: "Try the playground (30 s)" and "Tutorial (15
  min)".

### 5.2 No glossary

**Problem.** A non-technical reader meets opaque jargon within the
first 200 words of nearly every page.

**Recommendation.** Add `docs/GLOSSARY.md` with ~30 entries covering at
least: stream table, base/source table, defining query, schedule, tier,
refresh mode (FULL / DIFFERENTIAL / IMMEDIATE / AUTO), CDC, change
buffer, frontier, LSN, watermark, fuse, DAG, SCC, diamond, delta query
(ΔQ), DVM, DBSP, semi-naive, DRed, IVM, MERGE, advisory lock, GUC, BGW
(background worker), pgrx, hybrid CDC, columnar tracking, predicate
pushdown, scoped recomputation, group-rescan, semi-/anti-join,
algebraic / semi-algebraic / holistic aggregate, snapshot, outbox,
inbox, publication, relay.

Link first uses from headline pages.

### 5.3 No use-case gallery

**Problem.** "Materialized views that refresh themselves" is technically
correct but does not motivate the product. PATTERNS.md is excellent
content but is structured as recipes for users who already know they
want pg_trickle.

**Recommendation.** Add `docs/USE_CASES.md`:

1. **Real-time dashboards** — KPI tiles, daily/hourly aggregates.
2. **Operational read-models / CQRS** — denormalised projections.
3. **Fraud, alerting, anomaly detection** — incremental aggregates +
   thresholds (point at the demo).
4. **Leaderboards & TopK** — `ORDER BY … LIMIT N`.
5. **Bronze / Silver / Gold pipelines** — point at PATTERNS §1.
6. **Outbox/inbox event distribution** — point at OUTBOX/INBOX.
7. **Cross-system replication** — publications + relay.
8. **Bitemporal & SCD** — point at PATTERNS §3.
9. **Multi-tenant analytics** — point at integrations/multi-tenant.
10. **Citus-distributed analytics** — point at integrations/citus.

Each entry: 1 paragraph, 1 minimal SQL block, 1 link.

### 5.4 No persona-based reading paths

**Problem.** SUMMARY.md presents a flat list of 50+ entries.

**Recommendation.** On the landing page, add a "Choose your path"
section with four tiles:

| Persona | First doc | Then | Then |
|---|---|---|---|
| **Curious / evaluator** | What is pg_trickle? | Use Cases | Playground |
| **Application developer** | Getting Started | SQL Reference | PATTERNS |
| **DBA / SRE** | Pre-Deployment | Configuration | Troubleshooting + Scaling |
| **Data / analytics engineer** | Use Cases | dbt integration | Migration from materialized views |

### 5.5 Critical features missing dedicated pages

| Feature | Severity | Recommended page |
|---|---|---|
| **Snapshots** (`pgtrickle.snapshot_stream_table`) | HIGH — production-critical, v0.27+ | `docs/SNAPSHOTS.md` |
| **Citus integration** (currently under integrations/) | HIGH — promoted in README as headline | Promote to top-level `docs/CITUS.md` and keep integrations/citus.md as a redirect. |
| **Multi-database launcher** | MEDIUM | Section in SCALING.md or a new `docs/MULTI_DATABASE.md`. |
| **Predictive cost model** (`recommend_refresh_mode`, `recommend_schedule`) | MEDIUM | Section in PERFORMANCE_COOKBOOK or new `docs/COST_MODEL.md`. |
| **Self-monitoring** | MEDIUM | Section in MONITORING_AND_ALERTING tutorial. |
| **Watermark gating** (sources / external loaders) | MEDIUM | Promote SLA_SCHEDULING §watermarks or new `docs/WATERMARKS.md`. |
| **Parallel refresh worker pool** (`max_dynamic_refresh_workers`, `parallel_job_status`) | LOW | Section in SCALING.md (touched on; needs walk-through). |
| **Online ALTER QUERY** | LOW | Section in SQL_REFERENCE plus a short tutorial. |

### 5.6 Stub files

Resolve as listed in §4.2.

### 5.7 SUMMARY.md is incomplete

Add the 17 entries listed in §4.3.

---

## 6. Medium-Priority Gaps

### 6.1 BACKUP_AND_RESTORE is too thin

83 lines for a topic that production users care about deeply. Should
cover: `pg_dump`/`pg_restore` interaction with stream tables and CDC
slots, point-in-time recovery alignment with frontiers, snapshot-based
restore (v0.27+), how to rebuild after restore, and CNPG-specific
guidance.

### 6.2 No HA / replication story

There is no dedicated page on running pg_trickle alongside streaming
or logical replication, failover behaviour, or how stream tables
behave on a hot standby. This is a recurring question for any
production reader and is currently scattered across the FAQ.

### 6.3 No cost / capacity-planning guide

SCALING.md is good for worker sizing but not for "how much disk will
my change buffers consume?", "how big can my DAG get before scheduling
overhead matters?", or "how do I budget WAL retention for the WAL CDC
mode?". A `docs/CAPACITY_PLANNING.md` would consolidate this.

### 6.4 No comparison matrix on a discoverable page

`docs/research/PG_IVM_COMPARISON.md` is a stub and `DBSP_COMPARISON.md`
is short. There is no comparison with Materialize, RisingWave, Flink,
Debezium, ksqlDB, or Snowflake Dynamic Tables. A
`docs/COMPARISONS.md` listing strengths/weaknesses honestly would
substantially help evaluators (the persona most affected by missing
docs today).

### 6.5 CLI man-pages

The TUI has 18 subcommands. TUI.md is a tour, not a reference. A
`docs/CLI_REFERENCE.md` with one section per subcommand (synopsis,
arguments, exit codes, examples) is a low-cost improvement.

### 6.6 Relay does not have an intro

`docs/RELAY.md` and `docs/RELAY_GUIDE.md` exist but jump straight to
modules and operations. There is no "What is the relay? Do I need
it?" page. For a non-Rust user this is the main barrier.

### 6.7 Security guidance

`docs/security.md` is a stub including the vulnerability-reporting
policy. There is **no user-facing security page** covering: roles &
grants for `pgtrickle.*`, SECURITY DEFINER vs INVOKER semantics in
generated triggers, how RLS interacts with stream tables (only a
tutorial today), secrets handling in the relay, audit trail, and how
to lock down `pg_trickle.allow_circular`. Recommend a new
`docs/SECURITY_GUIDE.md`.

### 6.8 Observability metrics catalogue

`integrations/prometheus.md` shows how to scrape, but does not list
every metric, its labels, units, and recommended SLO/alerts. Add a
"Metrics reference" appendix.

### 6.9 Migration stories

Tutorials cover migrating from materialized views and from `pg_ivm`.
Common requests left unanswered: migrating from a homemade refresh
cron, from Debezium + Materialize, from periodic ETL, from Looker
PDTs, from Snowflake Dynamic Tables. A short table at the top of
`MIGRATING_FROM_MATERIALIZED_VIEWS.md` linking each path would help.

### 6.10 "Available since v…" badges

Inconsistent. PATTERNS.md uses `> **Version:** v0.14.0+`, INBOX.md uses
`> **Available since v0.28.0**`, RELAY refers to v0.29, README pins to
v0.34.0. Recommend a single convention (a small `<sup>since v0.28</sup>`
inline tag) and apply it uniformly.

### 6.11 Numbers drift

| Source | Unit-test count | E2E count | TUI tests | Releases mentioned |
|---|---|---|---|---|
| README.md (line ~620) | ~1,670 | ~1,340 | ~140 | "v0.34.0" |
| ESSENCE.md | "1,200+" | "570+" | — | "eleven releases" |
| FAQ.md | (varies) | — | — | — |

Pick a single source of truth (e.g. a `docs/_data/stats.md` snippet)
and include it everywhere.

### 6.12 GETTING_STARTED is one shape only

GETTING_STARTED.md (1,490 lines) is a single multi-chapter narrative
ending in recursive CTEs and circular DAGs. That is excellent for one
audience but excessive for someone who wants a 5-minute introduction
before the playground. Recommend splitting:

- **5-minute Quickstart** (single SQL query, no chains).
- **15-minute Tutorial** (multi-table joins, one chain).
- **In-depth Tour** (current GETTING_STARTED, renamed).

---

## 7. Tone & Language Audit

### 7.1 Sample readability findings

Spot-checking the first paragraph of each headline doc with the
"non-technical reader" lens:

| Doc | First-paragraph pain points |
|---|---|
| README §intro | "DBSP differential dataflow framework", arXiv link, Wikipedia-level density. |
| ESSENCE | Better — but ends with "the bottleneck is PostgreSQL's own MERGE, not pg_trickle's pipeline" which restates jargon. |
| introduction.md | Uses `CREATE EXTENSION pg_trickle` in the second line; says "self-maintaining stream tables" without defining "stream table". |
| ARCHITECTURE | Heavy ASCII diagram in lines 8–48; no narrative warm-up. |
| GETTING_STARTED | Strong; uses analogies. **Best in class.** |
| FAQ | Strong; structured and quotable. |
| PATTERNS | Pleasant; assumes you already know what bronze/silver/gold means. |
| TROUBLESHOOTING | Operator-targeted; tone is fine for that audience. |
| RELAY | Module table on line 18 — too eager. |

### 7.2 Style suggestions

1. **Lead with intent, not mechanism.** Open every page with "What
   this is for" and "When to read this", then mechanism.
2. **Decode every symbol on first use.** ΔQ, LSN, OID, BGW, GUC.
3. **Replace "trivially", "simply", "obviously"** — they signal
   condescension to readers who do not find the topic trivial.
4. **Prefer active voice and short sentences** in landing pages; the
   current README has many ~40-word sentences.
5. **Use callouts consistently.** mdBook supports admonitions; today
   the codebase uses `> **Note:**`, `> **Tip:**`, `> **Available
   since**`, and `> ` blockquotes interchangeably. Pick three (Note,
   Tip, Warning) and apply.
6. **Avoid embedding feature work in narrative chapters.** PATTERNS.md
   ends with three "Pattern N (vX.Y.0)" entries that read like release
   notes — they would be more discoverable as their own pages.

### 7.3 Diagrams

Today's diagrams are ASCII art. They render well in GitHub but are
inaccessible (no alt text), do not scale on mobile, and look dated on
the docs site. Recommend:

- Convert the four "core" diagrams (architecture, hybrid CDC
  transition, DAG fan-out, refresh lifecycle) to **Mermaid** (mdBook
  supports it via plug-in). One Mermaid source serves both GitHub and
  the book.
- Add **two screenshots**: TUI dashboard, Grafana dashboard. The
  project has both and shows neither.
- Add **one animated GIF** of the demo dashboard updating in real
  time on the landing page — a single 5-second clip would change the
  "what is this?" experience completely.

---

## 8. Structural & Navigation Issues

### 8.1 Three competing landing pages

README.md (GitHub), introduction.md (mdBook), ESSENCE.md (linked from
both, used as PR-blurb material). These overlap heavily, are out of
sync, and no single one is canonical for non-technical readers.

**Recommendation.**

- Make ESSENCE.md the **single canonical "what is this"** narrative
  (~200–300 lines, plain language).
- Keep README.md as the GitHub landing — open with the same hero
  paragraph as ESSENCE, then jump to features/install.
- Delete or shrink introduction.md to a one-paragraph welcome that
  links to ESSENCE.

### 8.2 SUMMARY.md is too flat

The current SUMMARY has six unranked sections. Suggested structure (also
solves §4.3):

```
# Summary

# Discover
- What is pg_trickle?      (was ESSENCE/introduction)
- Use Cases & Examples     (NEW)
- Comparisons              (NEW, replaces stub research pages)
- Glossary                 (NEW)

# Get Started
- 5-Minute Quickstart      (NEW)
- 15-Minute Tutorial       (split of GETTING_STARTED)
- In-Depth Tour            (current GETTING_STARTED)
- Playground (Docker)
- Real-time Demo
- Installation

# Build with Stream Tables
- Best-Practice Patterns
- Performance Cookbook
- SQL Reference
- Configuration
- Migrating from Materialized Views
- Migrating from pg_ivm
- ORM Integration
- dbt Integration

# Operate
- Pre-Deployment Checklist
- Scaling Guide
- Backup & Restore
- Upgrading
- Snapshots                (NEW)
- Multi-Database           (NEW)
- Capacity Planning        (NEW)
- Monitoring & Alerting
- Troubleshooting Runbook
- Error Reference
- Security Guide           (NEW)
- CLI Reference            (NEW)
- TUI Tool

# Distributed & Streaming
- Citus                    (PROMOTED)
- Hybrid CDC Modes
- Watermarks               (NEW or section)
- SLA-based Smart Scheduling
- Downstream Publications
- Transactional Outbox
- Transactional Inbox
- Relay Service
- Relay Architecture & Operations

# Tutorials (deep dives)
- What Happens on INSERT / UPDATE / DELETE / TRUNCATE
- Row-Level Security
- Partitioned Tables
- Foreign Table Sources
- Tiered Scheduling
- Fuse Circuit Breaker
- Circular Dependencies
- ETL & Bulk Load
- Tuning Refresh Mode

# Reference
- FAQ
- Changelog
- Roadmap
- Release Process
- Contributing

# Internals (for contributors)
- Architecture
- DVM Operators
- DVM Rewrite Rules
- Benchmarks
- Research: DBSP / pg_ivm / triggers vs replication / prior art / multi-DB broker
```

### 8.3 Cross-link density is low

Many docs do not link to related docs. For example, PATTERNS §"Real-Time
Dashboards" never links to MONITORING_AND_ALERTING; CDC_MODES never
links to CONFIGURATION's `cdc_mode` GUC; SQL_REFERENCE function entries
do not link to the matching section in PATTERNS or PERFORMANCE_COOKBOOK.
A pass adding `See also:` footers on every page would substantially
improve discoverability.

### 8.4 No "What's new" landing

`docs/changelog.md` is an mdBook include of `CHANGELOG.md`. Recommend a
**curated quarterly "What's new"** page that summarises each minor
release in 2–3 bullets and links to the changelog for detail.

---

## 9. Per-Document Notes (selected)

Only docs needing explicit attention are listed.

### 9.1 README.md (639 lines)

- Strong reference content; weak as an introduction.
- **History and Motivation** section is interesting and well written
  but probably belongs in a separate `docs/PROJECT_HISTORY.md` —
  it pushes the actual feature list below the fold.
- TPC-H validation section is too detailed for the README; move all
  but the headline numbers to BENCHMARK.md.
- "Try it in 30 seconds" is the right idea but immediately requires
  Rust to install the TUI. Either provide a one-line `docker run`
  for the TUI too, or label this section "Try it in 5 minutes".

### 9.2 docs/introduction.md

- Redundant with README and ESSENCE. Either delete and redirect, or
  rewrite as the canonical "What is pg_trickle?" per §8.1.

### 9.3 docs/GETTING_STARTED.md (1,490 lines)

- Excellent, but too long for a single tutorial. Split per §6.12.
- Some chapter headings refer to "DBSP-style version frontier" — push
  to glossary/internals.

### 9.4 docs/SQL_REFERENCE.md (4,897 lines)

- Comprehensive. Two improvements:
  - Add a one-line "**See also:**" link on each function entry to a
    pattern, tutorial, or troubleshooting section.
  - Surface the TOC in the docs site as a sidebar (mdBook does this
    if heading-split-level is bumped to 4).

### 9.5 docs/CONFIGURATION.md (2,904 lines)

- Excellent reference. Add a "**Tuning by goal**" section at the top:
  "Lower latency", "Lower write overhead", "Larger DAGs", "Pooler
  compatibility" — each linking to ~5 GUCs.

### 9.6 docs/FAQ.md (3,156 lines, 158 questions)

- The "New User FAQ — Top 15" preamble is one of the best onboarding
  artifacts in the project. Promote to its own short page and link
  from the landing.

### 9.7 docs/ARCHITECTURE.md (869 lines)

- Audience overlap with `plans/`. Would benefit from being clearly
  marked "for contributors" and from a one-page **"How it works (for
  users)"** companion in the Discover section.

### 9.8 docs/PATTERNS.md (838 lines)

- Excellent. Promote each pattern to a discoverable card on a Use
  Cases page. Add a "*When NOT to use this pattern*" footer to each.

### 9.9 docs/PERFORMANCE_COOKBOOK.md (553 lines) — **not in SUMMARY**

- High-quality recipes. **Add to SUMMARY immediately.** Currently
  invisible to docs-site readers.

### 9.10 docs/ERRORS.md (469 lines) — **not in SUMMARY**

- High-quality reference. Add to SUMMARY. Add cross-links from
  TROUBLESHOOTING into ERRORS by SQLSTATE.

### 9.11 docs/INBOX.md / docs/OUTBOX.md / docs/PUBLICATIONS.md / docs/RELAY*.md / docs/SLA_SCHEDULING.md

- All four exist; **none are in SUMMARY**. They form a coherent
  "distributed and streaming" chapter (see §8.2).

### 9.12 docs/CDC_MODES.md (408 lines)

- Excellent narrative. Add a diagram of the trigger → transitioning →
  WAL transition (currently only described in prose).

### 9.13 docs/BACKUP_AND_RESTORE.md (83 lines)

- Critically thin — see §6.1.

### 9.14 docs/SCALING.md (359 lines)

- Worker-pool focus is good. Missing: predictive cost model, parallel
  refresh failure modes, multi-tenant scaling, large-DAG memory
  considerations.

### 9.15 docs/integrations/citus.md

- Should be promoted to a top-level chapter — see §5.5.

### 9.16 docs/integrations/dbt.md

- Stub including `dbt-pgtrickle/README.md`. Inline the first
  ~50 lines (intro + minimal example) and link to the full README for
  the rest.

### 9.17 docs/research/*

- Three files are stubs (§4.2). The remaining three are genuinely
  research-grade material; group under "Internals → Research".

### 9.18 ESSENCE.md (25 lines)

- Should grow to ~200–300 lines and become the canonical "What is
  pg_trickle?" — see §5.1, §8.1.

### 9.19 INSTALL.md (437 lines)

- Comprehensive. Add a one-paragraph "By environment" decision tree at
  the top: "Local dev → playground / docker; Self-hosted → INSTALL §X;
  Managed Postgres → INSTALL §Y; Kubernetes → cnpg/".

### 9.20 ROADMAP.md (153 lines)

- Excellent. The "audience" callout is very good. The version table
  truncates after v0.9 in the sample read; ensure the rendered table
  remains scannable on mobile.

---

## 10. Specific Inconsistencies and Errors Found

| # | Location | Issue | Suggested fix |
|---|---|---|---|
| 1 | README "Testing" | "~1,670 unit tests + … + ~1,340 E2E tests + ~140 TUI tests" | Source from a single counted artifact. |
| 2 | ESSENCE | "1,200+ unit tests and 570+ E2E tests across eleven releases" | Same — likely stale. |
| 3 | PATTERNS | `> **Version:** v0.14.0+` while project is v0.34+ | Update or remove. |
| 4 | docs/changelog/contributing/installation/roadmap/security | mdBook stubs | See §4.2. |
| 5 | docs/research/* | Three stubs pointing into `plans/` | See §4.2. |
| 6 | introduction.md | Uses "stream table" before defining it | Define on first use. |
| 7 | README §"Try it in 30 seconds" | Requires `cargo install --path pgtrickle-tui` and `cd playground && docker compose up -d` | Realistically ≥ 5 minutes; rename or split the two paths. |
| 8 | RELAY.md "Architecture" | Reads as a code map, not an introduction | Add an intro paragraph explaining when to use the relay. |
| 9 | SECURITY.md vs docs/security.md | The latter is just an include of the former; the user-security guide is missing | See §6.7. |
| 10 | SUMMARY.md | Missing 17 files | See §4.3. |
| 11 | DEMO.md / PLAYGROUND.md | Both exist; both compelling; only one is in SUMMARY | Add DEMO.md to SUMMARY. |
| 12 | PATTERNS.md | Pattern 7/8/9 dated by version, mixing release-note style with tutorial style | Move version markers to inline tags. |

---

## 11. Proposed New Documents

In priority order:

| # | Document | Size | Audience | Replaces / consolidates |
|---|---|---|---|---|
| 1 | `docs/GLOSSARY.md` | 200–400 lines | All | — |
| 2 | `docs/USE_CASES.md` | 300–500 lines | Evaluators | — |
| 3 | `docs/QUICKSTART_5MIN.md` | 80–120 lines | Evaluators | Splits GETTING_STARTED |
| 4 | `docs/SNAPSHOTS.md` | 200–400 lines | DBA / SRE | New (api/snapshot.rs) |
| 5 | `docs/CITUS.md` (promote) | (existing) | Distributed Postgres users | Replaces integrations/citus.md as canonical |
| 6 | `docs/SECURITY_GUIDE.md` | 200–400 lines | DBA / SRE | New |
| 7 | `docs/COMPARISONS.md` | 200–300 lines | Evaluators | Promotes pg_ivm/DBSP comparisons |
| 8 | `docs/CLI_REFERENCE.md` | 300–500 lines | Operators | Companion to TUI.md |
| 9 | `docs/CAPACITY_PLANNING.md` | 200–300 lines | DBA / SRE | New |
| 10 | `docs/MULTI_DATABASE.md` | 100–200 lines | DBA | New |
| 11 | `docs/COST_MODEL.md` | 100–200 lines | Performance-tuners | Pulls from CONFIGURATION + cookbook |
| 12 | `docs/HA_AND_REPLICATION.md` | 200–400 lines | DBA / SRE | New |
| 13 | `docs/PROJECT_HISTORY.md` | 100–200 lines | Curious | Pulled out of README |
| 14 | `docs/_data/stats.md` (or similar) | < 50 lines | Internal | Single source of truth for test counts etc. |
| 15 | `docs/WHATS_NEW.md` | rolling | All | Curated companion to changelog |

Existing pages to expand:

- `docs/BACKUP_AND_RESTORE.md` → 300–500 lines.
- `docs/SCALING.md` → add multi-DB, parallel refresh failure, memory.
- `docs/integrations/prometheus.md` → add full metrics catalogue.

---

## 12. Phased Action Plan

### Phase A — Quick wins (target: 1 documentation milestone)

1. Add the 17 missing entries to SUMMARY.md.
2. Fix the nine stub files (curate or inline).
3. Write `docs/GLOSSARY.md` (200 entries minimum 30).
4. Write `docs/USE_CASES.md`.
5. Write `docs/QUICKSTART_5MIN.md` and link it as the new "first
   touch" CTA.
6. Expand ESSENCE.md to canonical "What is pg_trickle?" form; redirect
   introduction.md to it.
7. Reconcile test-count and version drift (§6.11) into a single
   include.
8. Add Mermaid versions of the four core diagrams.
9. Add TUI and Grafana screenshots and one animated GIF.
10. Add persona-routing tiles to the landing page.
11. Add `See also:` footers to the top 20 most-trafficked docs.
12. Add "Available since vX.Y" tag convention and apply to all feature
    pages.

### Phase B — Restructure (target: 2 milestones after A)

13. Promote Citus, Snapshots, Multi-Database to top-level chapters.
14. Restructure SUMMARY.md into the eight-chapter form proposed in
    §8.2.
15. Split GETTING_STARTED into 5-min, 15-min, and in-depth tours.
16. Write SECURITY_GUIDE, COMPARISONS, CLI_REFERENCE,
    CAPACITY_PLANNING, COST_MODEL, HA_AND_REPLICATION,
    PROJECT_HISTORY, WHATS_NEW.
17. Expand BACKUP_AND_RESTORE to production grade.
18. Add full Prometheus metrics catalogue.
19. Add `When NOT to use this pattern` footer to each PATTERNS entry.
20. Run a link-and-style lint pass (markdownlint + mdbook-linkcheck).

### Phase C — Maintenance discipline (ongoing)

21. Add a CI job that fails the build if a file in `docs/` is not in
    `SUMMARY.md` (with an allow-list).
22. Add a CI job that fails the build if a function in
    `src/api/*.rs` annotated `#[pg_extern]` does not have a section in
    `docs/SQL_REFERENCE.md` (best effort — a grep gate is enough).
23. Establish that **every new feature ships with at least one
    documentation entry in the relevant chapter** (codify in
    AGENTS.md / CONTRIBUTING.md).
24. Establish a quarterly "Doc gardening" milestone for deferred items.

---

## 13. Out of Scope (for follow-up plans)

- Internationalisation / translation.
- Accessibility audit (alt text, contrast, keyboard navigation).
- Video tutorials.
- A formal style guide (could become Round 2 of this plan).
- A dedicated marketing site beyond the mdBook output.

---

## 14. Appendix — File-by-file inventory

(See `wc -l docs/**/*.md` for the raw counts. Highlights:)

- 9 stub files (1 line each).
- 11 files > 500 lines (the main reference and tutorial set).
- 4 files > 1,500 lines (SQL_REFERENCE, FAQ, CONFIGURATION,
  GETTING_STARTED) — candidates for splitting or improved TOCs.
- Total documented surface: ~32,000 lines.

---

## 15. Recommendation

Implement **Phase A in full as the next documentation milestone**.
It is mechanical, low-risk, and would dramatically improve the
experience of a non-technical reader landing on the project. Phase B
should be planned immediately after A but treated as a structural
change requiring review, since it moves files around and changes
URLs. Phase C is ongoing discipline.

The single most valuable deliverable is the **"What is pg_trickle?"
+ Use Cases + Glossary** triplet (items 3, 4, 6 in §12). With those
three pages alone, the project's accessibility to a non-technical
reader improves by an order of magnitude — and none of the existing
content needs to be rewritten.
