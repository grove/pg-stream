# Repo Structure Analysis — Monorepo vs Separate Repos

> **Status:** Analysis (DRAFT — 2026-04-22, rev 4)
> **Decision:** Keep separate repos (3 repos). See [Recommendation](#recommendation).
> **Related:** [ANALYSIS_UNIFIED_VS_SEPARATE_UI.md](webui/ANALYSIS_UNIFIED_VS_SEPARATE_UI.md) ·
> [PLAN_WEBUI.md](webui/PLAN_WEBUI.md)

---

## Table of Contents

- [Context](#context)
- [Current State](#current-state)
- [The Forces Pushing Toward a Monorepo](#the-forces-pushing-toward-a-monorepo)
- [The Forces Pushing Against](#the-forces-pushing-against)
- [Chosen Approach: 3 Separate Repos](#chosen-approach-3-separate-repos)
- [What This Means for CI](#what-this-means-for-ci)
- [Release & Versioning](#release--versioning)
- [Console Embedding](#console-embedding)
- [Impact on the UI Decision](#impact-on-the-ui-decision)
- [Risk Register](#risk-register)
- [Comparison: Monorepo vs 3 Repos](#comparison-monorepo-vs-3-repos)
- [Decision Criteria Checklist](#decision-criteria-checklist)
- [Recommendation](#recommendation)

---

## Context

The pg-trickle ecosystem has two PostgreSQL extensions, two HTTP services,
a web console, a TUI, and a dbt adapter. The question was whether the
two extensions and their companion binaries should share a single Cargo
workspace and git repo, or remain separate.

After analysis through revs 1–3, the decision is to **keep 3 separate
repos** — one per extension plus a standalone unified web console.

### Adoption reality

| | pg-trickle | pg-ripple |
|---|---|---|
| **GitHub stars** | ~80 | 0 |
| **External users** | Some (growing) | None (not announced) |
| **Releases** | 26 | 47 |
| **Maturity** | Production-track | Feature-complete, pre-v1.0 |

pg-ripple has **zero external adoption**. It has not been announced,
has no stars, and no external contributors. This eliminates the
strongest argument against a monorepo ("users will think pg-ripple
needs pg-trickle") because those users don't exist yet. The framing
of both products can be defined from scratch.

pg-trickle has ~80 stars and is the only product with external
traction. Making it the nucleus of a larger platform adds value to
existing users' investment rather than fragmenting attention.

### The platform is pg-trickle

Each tool alone competes in a narrow niche against established players.
But the combined pitch is unique and hard to replicate:

> **pg-trickle** is a modular data platform built into PostgreSQL 18.
> Incremental view maintenance, a knowledge graph engine, a cross-cluster
> streaming relay, and a unified admin console — all as PostgreSQL
> extensions. Use one component or all of them. No separate
> infrastructure.

Nobody else offers IVM + triplestore + relay as composable PostgreSQL
extensions with a shared admin UI. RisingWave is a separate database.
Neo4j is a separate database. Apache Jena is a separate system.
Materialize is hosted-only. The "it's all just PostgreSQL" angle is a
differentiator that gets *stronger* with more components.

**The platform name is pg-trickle** — not "Grove" or any new name.
pg-trickle already has 80 stars and brand recognition. Renaming
would throw that away. The name fits: "trickle" evokes continuous
data flow, which is what the entire platform does. IVM is a trickle
of incremental changes. The relay trickles data across clusters.
Even the triplestore's CDC subscriptions trickle change notifications.

The repo stays `grove/pg-trickle`. No rename needed. The 80-star repo
just gets richer — that's pure upside.

PostgreSQL extension names remain independent SQL-level identifiers:
`CREATE EXTENSION pg_trickle` and `CREATE EXTENSION pg_ripple`. The
platform is pg-trickle; the triplestore component has its own extension
name, just as PostgreSQL itself ships `pg_stat_statements` and
`pg_trgm` under the PostgreSQL umbrella.

This is NOT a question about the frontend apps (the unified console).
That decision is covered in
[ANALYSIS_UNIFIED_VS_SEPARATE_UI.md](webui/ANALYSIS_UNIFIED_VS_SEPARATE_UI.md).
This analysis focuses on the Rust crates and their repo structure.

---

## Current State

### grove/pg-trickle (this repo)

```
Cargo.toml          ← workspace root (members: ".", "pgtrickle-tui")
├── src/            ← pg_trickle extension (cdylib + lib)
├── sql/            ← extension SQL
├── tests/          ← E2E + integration tests
├── benches/        ← Criterion benchmarks
├── fuzz/           ← fuzz targets
├── scripts/        ← 17 build/test/bench scripts
├── pgtrickle-tui/  ← TUI binary (workspace member)
├── dbt-pgtrickle/  ← dbt adapter (Python/Jinja, not in Cargo workspace)
├── plans/          ← 18 plan docs + 12 subdirectories
├── docs/           ← mdBook documentation
├── .github/workflows/ ← 19 CI workflows
└── (relay planned but not yet in repo)
```

- **Language:** Rust (Edition 2024), pgrx 0.17, PostgreSQL 18
- **Workspace members:** pg_trickle (root), pgtrickle-tui
- **Version:** 0.26.0 (26 releases)
- **CI:** 19 workflows, path-filtered where applicable
- **Test tiers:** Unit, integration, light E2E, full E2E, TPC-H,
  dbt, benchmarks, fuzz, SQLancer, stability/soak

### grove/pg-ripple (separate repo)

```
Cargo.toml          ← single-crate (no workspace yet)
├── src/            ← pg_ripple extension (cdylib + lib)
├── sql/            ← extension SQL
├── tests/          ← pg_test + regress + integration
├── benchmarks/     ← BSBM, throughput, WatDiv
├── fuzz/           ← fuzz targets
├── scripts/        ← test/build scripts
├── pg_ripple_http/ ← HTTP service binary (likely its own crate)
├── plans/          ← plan docs
├── docs/           ← mdBook documentation
├── docker/         ← Docker infra
└── .github/workflows/ ← CI workflows
```

- **Language:** Rust (Edition 2024), pgrx 0.17, PostgreSQL 18
- **Version:** 0.47.0 (47 releases)
- **Test tiers:** Unit, pgrx regress, W3C conformance (~3,000 tests),
  Jena (~1,000), WatDiv, LUBM, BSBM, OWL 2 RL, proptest, fuzz
- **Key property:** Works independently. pg_trickle is an optional
  companion for IVM views.

### Cross-product integration surface (today)

| Integration point | Where it lives | What it does |
|---|---|---|
| IVM SPARQL views | pg-ripple SQL + docs | `pg_trickle` used as optional companion for live-updating SPARQL views |
| Shared PG instance | Deployment-level | Both extensions in one `CREATE EXTENSION` sequence |
| ExtVP statistics | pg-ripple plans | pg_trickle stream tables for predicate selectivity stats |

The integration is currently loose coupling: pg-ripple references
pg-trickle by name in docs and SQL, but there is no Rust-level crate
dependency. This could deepen (shared types, shared catalog access
patterns) or stay loose.

---

## The Forces Pushing Toward a Monorepo

### 1. Identical toolchain — zero marginal cost

Both projects use:
- Rust Edition 2024
- pgrx 0.17.0 (pinned to exact version)
- PostgreSQL 18
- Axum + Tokio (for HTTP services)
- serde + serde_json
- thiserror
- xxhash-rust
- cargo-fuzz, proptest
- The same pgrx test infrastructure
- The same Docker base images

A shared `Cargo.lock` and `deny.toml` eliminates duplicate dependency
management. When pgrx releases 0.18, one PR updates the lockfile
instead of two coordinated PRs.

### 2. Atomic cross-product changes

Today, adding a pg-trickle feature that pg-ripple's IVM layer should
use requires:
1. PR + merge in pg-trickle
2. Release pg-trickle
3. PR in pg-ripple referencing the new release
4. Test the integration

In a workspace:
1. One PR touches both `extensions/pg-trickle/` and
   `extensions/pg-ripple/`
2. `cargo check --workspace` verifies the interface immediately
3. CI runs cross-product integration tests atomically

This matters more as integration deepens (shared error types, shared
catalog access patterns, the console API serving both products).

### 3. The console problem

The web console serves both extensions. In a two-repo world it must
live in a third repo. In a monorepo it lives in `apps/console/` with
natural proximity to both backends.

**Counter-argument (decisive):** The console is a Next.js app that
talks to REST APIs over HTTP. It has no compile-time dependency on
either Rust crate. API changes are versioned — the console targets
`/api/v1/*` on whichever backends are reachable. A third repo
(`grove/surface`) is not an orphan; it's the shared meeting
point that belongs to neither extension, which is exactly right.
The console's release cycle is driven by frontend concerns (Next.js,
shadcn/ui), not Rust extension releases.

### 4. CI infrastructure deduplication

Both repos maintain:
- pgrx installation steps (slow, ~3 min each)
- Docker image builds for E2E tests
- Testcontainers setup
- Benchmark infrastructure
- Coverage collection
- Security scanning (CodeQL, Semgrep, dependency audit)
- Release automation

A monorepo shares the base CI infrastructure (pgrx install, Docker
layer caching, security scanning) while keeping product-specific test
suites in separate path-filtered workflows.

### 5. Documentation coherence

The "getting started" story for the full platform (pg-trickle +
pg-ripple + relay) currently spans two repos and two doc sites. A
monorepo enables a single mdBook with extension-specific chapters.
The tutorial "Create an auto-updating SPARQL view" naturally references
both extensions — in a monorepo, both are local links.

### 6. Shared scripts and testing patterns

Both repos have overlapping test infrastructure:
- Docker image builds for E2E
- Testcontainers helpers
- Benchmark comparison scripts
- Coverage collection
- Fuzz targets

These can be shared libraries in the workspace rather than duplicated.

### 7. Platform identity

The pg-trickle ecosystem is evolving from "two independent extensions" toward
a platform. A monorepo makes the platform structure visible in the file
tree. "This is a platform with two extensions, two services, and shared
tools" is immediately obvious from the directory listing. In two repos,
the platform is an invisible concept that only exists in documentation.

---

## The Forces Pushing Against

### 1. pg-ripple's independence is a product feature

pg-ripple's pitch is: "A knowledge graph engine built into PostgreSQL.
No separate graph database. No data pipelines. No extra infrastructure."

If pg-ripple lives inside a monorepo, does a prospective user think
"do I need pg-trickle?" even though the answer is no?

**Updated assessment (rev 3):** This concern is theoretical.
pg-ripple has zero external users and has not been announced. There
is no existing audience whose perception would be disrupted. The
framing can be set from the start: "pg-trickle is a modular data
platform. pg-ripple is its triplestore component. Install only what
you need."

Moreover, launching pg-ripple *as part of a platform* is arguably a
stronger positioning than launching it as a standalone extension
competing head-to-head against Jena, Virtuoso, and Blazegraph. The
"it's all just PostgreSQL" platform story is more compelling than
"here's another triplestore." Users discovering pg-trickle's IVM
engine also discover they can add a knowledge graph for free.

**Mitigation:** The repo stays `grove/pg-trickle` — the established
name with 80 stars. Each component gets its own
`extensions/pg-ripple/README.md` that serves as the product page.
crates.io and Docker Hub listings point to the component-specific
README. The root README frames pg-trickle as a platform with
composable components.

**Residual risk:** Low. First impressions are controlled by the repo
README and the product docs, not by the directory listing. And the
platform framing actually helps rather than hurts.

### 2. Git history loss or complexity

pg-ripple has 47 releases of git history. Options:

| Approach | Preserves history? | Clean tree? | Complexity |
|---|---|---|---|
| `git subtree add` | Yes (interleaved) | No (commits mixed) | Medium |
| `git filter-repo` + merge | Yes (rewritten paths) | Yes | High |
| Fresh copy + archive original | No (in monorepo) | Yes | Low |
| git submodule | Yes (separate) | Yes | High (ongoing) |

None of these are great. Subtree merge produces a confusing `git log`.
filter-repo is powerful but error-prone. Fresh copy is clean but loses
`git blame` for 47 releases of work. Submodules are universally hated.

**Mitigation:** Archive `grove/pg-ripple` as read-only with a
"Moved to grove/pg-trickle" banner. Developers who need historical blame
use the archived repo. The monorepo starts with a clean import commit.
Not ideal, but the simplest option that doesn't create ongoing pain.

**Residual risk:** Low-medium. Historical blame is occasionally useful
but not daily. Most blame queries are about recent code.

### 3. CI wall-clock time

Combined test suite if everything runs:
- pg-trickle: unit + integration + light E2E + full E2E + TPC-H +
  benchmarks + fuzz + dbt + stability ≈ 45 min
- pg-ripple: unit + W3C + Jena + WatDiv + LUBM + BSBM + OWL 2 RL +
  proptest + fuzz ≈ 30 min

Without path filtering, every PR runs ~75 min of CI. That's
unacceptable.

**Mitigation:** Mandatory path-filtered workflows. A change to
`extensions/pg-ripple/src/` triggers only `ci-ripple.yml`. A change
to `extensions/pg-trickle/src/` triggers only `ci-trickle.yml`. A
change touching both (or shared infra) triggers both plus
`ci-integration.yml`.

GitHub Actions `paths` and `paths-ignore` filters handle this natively.
The `dorny/paths-filter` action provides finer-grained control within
a single workflow file if needed.

**Residual risk:** Low, IF path filtering is set up correctly from day
one. Risk of "it works on my machine" if someone forgets to add a new
path to the filter.

### 4. Release coordination temptation

A monorepo creates psychological pressure to sync versions or cut
releases together. pg-ripple at v0.47 and pg-trickle at v0.26 must
remain independently versioned. If someone starts saying "let's wait
for pg-trickle to finish feature X before releasing pg-ripple," the
monorepo has caused harm.

**Mitigation:** Separate git tags (`pg-trickle-v0.26.0`,
`pg-ripple-v0.47.0`), separate release workflows, separate changelogs,
separate version fields in each `Cargo.toml`. `cargo release` supports
`--package` for workspace members. Document the "independent versions"
policy in AGENTS.md.

**Residual risk:** Low with discipline. The tooling supports it.

### 5. Contributor confusion

A new user files an issue about SPARQL performance. They land in a repo
that's mostly about streaming materialized views. The issue template
must ask "Which extension?" and the labeling must be clear.

**Mitigation:** Issue templates with extension selector. Labels:
`ext:pg-trickle`, `ext:pg-ripple`, `svc:relay`, `svc:ripple-http`,
`app:console`, `app:moire`. Good templates eliminate most confusion.

**Residual risk:** Low. Most monorepos (Babel, Next.js, Turborepo)
handle this fine with labels.

### 6. crates.io and packaging complexity

Both extensions publish to crates.io as separate crates. Workspace
publishing requires `cargo publish --package pg_trickle` and
`cargo publish --package pg_ripple` separately. PGXN archives need
separate build steps. Docker images need separate Dockerfiles.

**Mitigation:** All of this already works with Cargo workspaces.
`pgtrickle-tui` is already a workspace member with its own publish
step. Adding more members is incremental.

**Residual risk:** Very low. Cargo workspaces are designed for this.

### 7. Build time increase

`cargo build --workspace` compiles everything. A developer working only
on pg-ripple doesn't want to compile pg-trickle.

**Mitigation:** `cargo build --package pg_ripple` compiles only
pg-ripple and its dependencies. The workspace `default-members` key
can be set so that bare `cargo build` only builds a subset. The
`justfile` provides per-extension targets: `just build-ripple`,
`just build-trickle`.

**Residual risk:** Very low. Cargo handles this natively.

---

## Chosen Approach: 3 Separate Repos

Instead of a monorepo, keep each product in its own repo with a new
third repo for the unified web console:

```
grove/pg-trickle              ← IVM extension + relay + TUI + dbt adapter
├── Cargo.toml                workspace root
├── src/                      pg_trickle extension (unchanged)
├── sql/
├── tests/
├── benches/
├── fuzz/
├── pgtrickle-tui/            TUI (existing workspace member)
├── pgtrickle-relay/          Relay (NEW workspace member)
├── dbt-pgtrickle/            dbt adapter
├── plans/
├── docs/
└── .github/workflows/

grove/pg-ripple               ← Triplestore extension + SPARQL HTTP service
├── Cargo.toml                workspace root (or will become one)
├── src/                      pg_ripple extension
├── sql/
├── tests/
├── benchmarks/
├── fuzz/
├── pg_ripple_http/           SPARQL Protocol HTTP service (workspace member)
├── plans/
├── docs/
└── .github/workflows/

grove/surface           ← Unified web console (NEW repo)
├── package.json              Next.js app
├── src/
│   ├── app/                  App Router pages
│   ├── components/           UI components
│   ├── lib/                  API clients, hooks
│   │   ├── relay-client.ts   Relay API client
│   │   ├── ripple-client.ts  pg_ripple_http API client
│   │   └── capabilities.ts   Backend detection
│   └── styles/
├── public/
└── .github/workflows/
```

### Why the relay lives in pg-trickle

The relay is tightly coupled to pg-trickle:
- Streams from pg-trickle stream tables
- Uses the same CDC infrastructure
- Will share Rust types with the extension (change events, table
  metadata, DAG structures)
- Its release cadence follows pg-trickle releases

Making it a workspace member (like the TUI already is) is natural.
`pg_ripple_http` stays in `grove/pg-ripple` for the same reason —
it's tightly coupled to pg-ripple's internals.

### Why the console is its own repo

The console is a Next.js app. It talks to the relay API and
`pg_ripple_http` API over HTTP. It has:
- No Rust code
- No compile-time dependency on either extension
- A release cycle driven by frontend concerns (Next.js, React,
  shadcn/ui), not Rust extension releases
- Its own CI (ESLint, TypeScript, Playwright E2E)
- Its own deployment story (Docker, Vercel, or embedded in either
  Rust binary via `rust-embed`)

Putting it in either extension's repo would create an asymmetry —
it serves both equally. A standalone repo is the honest home.

### Cross-product integration

The integration surface between the two extensions stays at the SQL
level (pg-ripple optionally uses pg-trickle for IVM views). There is
no Rust-level crate dependency today, and the architecture doesn't
require one — both extensions talk to PostgreSQL, not to each other's
Rust APIs.

If shared Rust types ever become necessary (e.g. a shared error type
or catalog access pattern), a small `pgtrickle-common` crate published
to crates.io handles this without a monorepo.

---

## What This Means for CI

Each repo owns its entire CI pipeline. No path-filtering complexity.

| Repo | CI scope | Trigger |
|------|----------|---------|
| `grove/pg-trickle` | Extension unit/integration/E2E, relay tests, TUI tests, dbt, benchmarks, fuzz | All existing workflows, unchanged |
| `grove/pg-ripple` | Extension unit/regress/W3C/BSBM/WatDiv, pg_ripple_http tests, fuzz | All existing workflows, unchanged |
| `grove/surface` | ESLint, TypeScript, Playwright E2E against mock APIs | New, simple workflow |

**Cross-product CI:** A scheduled or manual-dispatch workflow in one
repo (or a separate integration test setup) can test the full stack:
both extensions + relay + pg_ripple_http + console. This runs nightly
or on-demand, not on every PR.

The pgrx version bump problem ("two PRs") is real but minor. It
happens a few times a year, takes minutes, and both PRs can be
opened in parallel.

---

## Release & Versioning

Independent versions, always. Simpler than a monorepo because each
repo uses plain `vX.Y.Z` tags without product prefixes:

```
grove/pg-trickle:     v0.26.0, v0.27.0, ...
grove/pg-ripple:      v0.47.0, v0.48.0, ...
grove/surface:  v0.1.0,  v0.2.0,  ...
```

Each repo has its own tags, release workflows, and changelogs. No
risk of version coupling creep — the repo boundary makes it
structurally impossible.

---

## Console Embedding

The console (`grove/surface`) publishes its static build as a
GitHub release asset (tarball of the Next.js `out/` directory). Both
Rust binaries can optionally embed it via `rust-embed`:

```
surface CI:
  npm run build → out/ → published as GitHub release asset (tar.gz)

pgtrickle-relay build (when --features console):
  Download surface-vX.Y.Z.tar.gz → embed via rust-embed

pg_ripple_http build (when --features console):
  Download surface-vX.Y.Z.tar.gz → embed via rust-embed
```

The console version is pinned in each binary's build config, allowing
independent upgrades. For standalone deployment, the same static output
is published as a Docker image or deployed to Vercel/Cloudflare.

---

## Impact on the UI Decision

The 3-repo approach **aligns with** the single unified console from
[ANALYSIS_UNIFIED_VS_SEPARATE_UI.md](webui/ANALYSIS_UNIFIED_VS_SEPARATE_UI.md):

| Decision | Monorepo | 3 repos (chosen) |
|---|---|---|
| Console location | `apps/console/` inside monorepo | `grove/surface` — own repo |
| Console API changes | Atomic PR (API + frontend) | Versioned API contract, separate PRs |
| Console neutrality | Lives inside pg-trickle repo — slight asymmetry | Own repo — serves both extensions equally |
| pg-ripple independence | Must be explained via README | Obvious from repo boundary |
| Relay ownership | Must be explained via directory structure | Obvious — it's a workspace member in pg-trickle |

The console as its own repo is actually *more natural* for a UI that
serves two independent extensions. It doesn't live inside either
product's repo — it's the shared meeting point.

---

## Risk Register

| # | Risk | Likelihood | Impact | Mitigation | Residual |
|---|---|---|---|---|---|
| R1 | Duplicate pgrx version management | Medium | Low (time) | Open both PRs in parallel, takes minutes | Low |
| R2 | Cross-product API drift | Low | Medium | Versioned `/api/v1/*` contracts, integration tests | Low |
| R3 | Console orphaned (no clear owner) | Low | Low | Own repo with own CI, linked from both product READMEs | Very low |
| R4 | Duplicate CI infrastructure | Medium | Low (maintenance) | Shared GitHub Actions (reusable workflows or actions) | Low |
| R5 | Platform identity invisible | Low | Low | Console UI makes the platform visible; README cross-links | Low |
| R6 | Shared Rust types needed later | Low | Medium | Publish `pgtrickle-common` crate if needed | Low |

---

## Comparison: Monorepo vs 3 Repos

| Concern | Monorepo | 3 repos (chosen) |
|---|---|---|
| **Dependency updates** | One PR, one lockfile | Two PRs (minor, infrequent) |
| **Cross-product changes** | Atomic PR | Separate PRs, versioned API contract |
| **Console ownership** | `apps/console/` alongside backends | Own repo — neutral ground, serves both |
| **CI infrastructure** | Shared base, path-filtered | Each repo owns its CI — simpler per-repo |
| **pg-ripple independence** | Requires README explanation | Obvious from repo boundary |
| **Release coupling** | Possible (preventable with discipline) | Structurally impossible |
| **Developer onboarding** | Clone one repo, navigate dirs | Clone only the repo you need |
| **Git history** | pg-ripple history starts at import | Full history preserved per-product |
| **Rust-level shared types** | Natural workspace dependencies | Needs published crate (if ever needed) |
| **Platform visibility** | Visible in file tree | Visible through console UI and docs |
| **Migration effort** | Significant (restructure + import) | Zero — keep what exists |
| **Path-filtering complexity** | Must get it right from day 1 | Not needed — each repo is self-contained |

---

## Decision Criteria Checklist

### Favour monorepo if:

- [ ] Cross-product integration will deepen to Rust-level shared types
- [ ] You want atomic PRs for API + frontend changes
- [ ] Duplicate CI infrastructure maintenance is a real pain point
- [ ] You expect many more workspace members in future
- [ ] The team grows large enough for a dedicated platform team

### Favour 3 repos if:

- [x] pg-ripple's independent identity matters for adoption
- [x] The cross-product integration stays at the SQL level
- [x] The console serves both extensions equally (neutral repo)
- [x] You want zero migration effort
- [x] Each product's CI should be self-contained
- [x] Solo developer — coordination cost between repos is trivial
- [x] You're not building the console any time soon (no urgency)

---

## Recommendation

**Keep 3 separate repos.**

```
grove/pg-trickle       — IVM extension + relay + TUI + dbt adapter
grove/pg-ripple        — Triplestore + pg_ripple_http
grove/surface          — Unified web console ("surface of the platform")
```

### Why this changed from rev 3

Revs 1–3 recommended a monorepo with increasing urgency. The arguments
for a monorepo are real — shared toolchain, atomic cross-product PRs,
a natural home for the console. But the arguments against are also
real, and the deciding factors are practical:

1. **Zero migration effort.** Both repos stay exactly where they are.
   No restructuring, no import, no path-filtering setup. The relay
   joins `grove/pg-trickle` as a workspace member (like the TUI), and
   `grove/surface` gets created when the console is ready.

2. **pg-ripple independence is obvious.** With its own repo, nobody
   asks "do I need pg-trickle to use the triplestore?" The repo
   boundary is the clearest possible signal.

3. **The console is neutral territory.** A unified console that serves
   both extensions shouldn't live inside either extension's repo. Its
   own repo (`grove/surface`) is the honest home — it depends
   on both backends' APIs equally.

4. **Solo developer.** The coordination cost of multi-repo is
   proportional to team size. With one developer, it's trivial —
   you just merge one PR then the other. The monorepo's "atomic
   cross-product PR" benefit matters more for teams.

5. **The platform story is told by the UI, not the repo structure.**
   Users discover the platform through the unified console, the
   documentation, and the README cross-links — not by browsing the
   GitHub directory listing.

### The platform pitch still works

The platform is pg-trickle. The pitch doesn't change:

> **pg-trickle** is a modular data platform built into PostgreSQL 18.
>
> | Component | Extension | What it does |
> |---|---|---|
> | **IVM engine** | `pg_trickle` | Streaming materialized views with differential maintenance |
> | **Triplestore** | `pg_ripple` | Knowledge graph engine — SPARQL, Datalog, SHACL, vector search |
> | **Relay** | — | Cross-cluster streaming connector (Kafka, NATS, webhook) |
> | **Console** | — | Unified admin dashboard for all components |
>
> Install one component or all of them. No separate infrastructure.
> Everything runs inside your existing PostgreSQL.

This pitch works regardless of whether the code is in 1 repo or 3.
Users don't care about repo structure. They care about what they can
`CREATE EXTENSION`.

### Future escape hatch

If cross-product integration deepens significantly (shared Rust types,
shared catalog access, workspace-level `cargo check` becomes valuable),
the monorepo option remains available. Going from 3 repos → monorepo
is straightforward. The analysis in this document (forces for/against,
proposed monorepo structure) is preserved for that eventuality.
