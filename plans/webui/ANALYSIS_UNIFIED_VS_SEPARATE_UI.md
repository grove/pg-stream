# Unified vs Separate UIs — Deep Analysis

> **Status:** Analysis (DRAFT — 2026-04-22, rev 4)
> **Decision:** One unified web console covering admin + exploration + curation.
> **Scope:** Should pg-trickle admin, pg-ripple admin, data exploration,
> and future record linkage be one UI, two, or more?

---

## Repo & Deployment Reality

Before analysing UI boundaries, the physical layout matters:

| Repo | Contains | HTTP service |
|------|----------|--------------|
| `grove/pg-trickle` | IVM extension + `pgtrickle-relay` binary + TUI + dbt | Relay: Axum on `:9090` (metrics, health, webhook, API) |
| `grove/pg-ripple` | Triplestore extension + `pg_ripple_http` binary | HTTP: Axum (SPARQL Protocol, Prometheus, health) |
| `grove/surface` | Unified web console (NEW, Next.js) | — |

**pg-ripple is independent.** It works without pg-trickle installed.
A user may run pg-ripple alone (knowledge graph, SPARQL, Datalog) and
never touch pg-trickle or the relay. Hardwiring admin UI into the relay
binary means pg-ripple-only users get **no admin interface at all**.
This was a flaw in the original rev 1 analysis.

**Both Rust binaries are Axum servers.** The relay and `pg_ripple_http`
share the same HTTP stack (Axum + Tokio). Either can embed static
frontend files via `rust-embed`. Both already serve health/metrics.

---

## The UIs Under Consideration

### 1. pg-trickle Admin (planned — PLAN_WEBUI.md)

Operational console for streaming materialized views. Topology graph,
SLA monitoring, refresh timeline, DVM operator inspector, relay pipeline
management, column-level lineage, CDC health, health scorecard.

**Users:** SRE, data engineer, platform engineer, LLM agents.

### 2. pg-ripple Admin (not yet planned)

Operational monitoring for the knowledge graph engine: dictionary cache
stats, VP table sizes, delta→main compaction, named graph metrics, SHACL
validation status, inference job monitoring, CDC subscription health.

**Users:** DBA, data engineer managing the knowledge graph.

### 3. moire — Data Explorer (BLUEPRINT exists, nothing built)

Faceted parallax navigation for knowledge graphs. SPARQL-driven entity
set browsing, type/relationship/graph browsers, semantic zoom. Works
against ANY SPARQL 1.1 endpoint, not just pg-ripple.

**Users:** Data analyst, knowledge engineer, researcher, developer.

### 4. Record Linkage Tool (future)

Entity resolution / deduplication workflow for pg-ripple. Interactive:
review candidate match pairs, tune similarity thresholds, confirm/reject
links, bulk-apply `owl:sameAs` assertions. Likely combines graph
exploration (browse entities being compared) with a review workflow
(queue of candidate pairs, accept/reject/skip).

**Users:** Data steward, knowledge engineer, data quality analyst.

---

## Key Architectural Facts

| | pg-trickle admin | pg-ripple admin | moire | Record linkage |
|--|---|---|---|---|
| **Purpose** | Monitor streaming pipelines | Monitor KG engine | Explore graph data | Curate entity matches |
| **Activity** | Reactive monitoring | Reactive monitoring | Open-ended exploration | Guided workflow |
| **Backend** | `pgtrickle-relay` API | `pg_ripple_http` API | Any SPARQL 1.1 | `pg_ripple_http` API |
| **Protocol** | REST/JSON + WebSocket | REST/JSON | SPARQL Protocol | REST/JSON + SPARQL |
| **Coupling** | pg-trickle relay | pg-ripple | Endpoint-agnostic | pg-ripple |
| **Session** | Brief, reactive | Brief, reactive | Long, interactive | Medium, task-oriented |

**Critical relationship:** pg-ripple uses pg-trickle for IVM. A SPARQL
view materialised by pg-ripple appears as a pg-trickle stream table in
the topology graph. Both extensions CAN run in the same PostgreSQL
instance — but pg-ripple also runs alone.

---

## Deployment Scenarios

The UI architecture must work for ALL of these:

| Scenario | Extensions | HTTP services | Needs UI for |
|----------|-----------|---------------|--------------|
| **A. Full stack** | pg-trickle + pg-ripple | relay + pg_ripple_http | Streaming admin + KG admin + data exploration |
| **B. pg-ripple only** | pg-ripple | pg_ripple_http | KG admin + data exploration |
| **C. pg-trickle only** | pg-trickle | relay | Streaming admin |
| **D. External SPARQL** | (none) | (any SPARQL endpoint) | Data exploration only |

Scenario B is the killer constraint. If the console is hardwired into
the relay binary, pg-ripple-only users get nothing. pg-ripple must have
its own admin story independent of pg-trickle.

---

## Activity Classification

Four distinct activity types, which drives the UI split:

| Activity | Nature | Session | Example tools in the wild |
|----------|--------|---------|--------------------------|
| **Streaming pipeline admin** | Reactive monitoring, incident response | Brief (2–10 min) | RisingWave Dashboard, Confluent Control Center |
| **Knowledge graph admin** | Reactive monitoring, health checks | Brief (2–10 min) | Fuseki admin, Stardog Studio (ops tab) |
| **Data exploration** | Open-ended browsing, discovery | Long (10–60 min) | Stardog Studio (query tab), Metaphactory, Linked Data Fragments |
| **Data curation / record linkage** | Guided workflow, review queue | Medium (10–30 min) | Dedupe.io, OpenRefine, Tamr |

Key insight: the first two are both "admin monitoring" — same cognitive
mode, same session length, same UX pattern (list → detail → remediation).
The last two are both "data work" — interactive, user-driven, longer
sessions. This suggests the split.

---

## Option A: One App Per Concern (4 UIs)

Four apps: trickle-admin, ripple-admin, moire, record-linkage.

**Verdict: Over-split.** Both admin UIs are thin (~15 panels each in
isolation). Maintaining four separate Next.js apps with four CI
pipelines and four sets of dependencies is excessive. The two admin
concerns share the same UX pattern and will share components (health
cards, timeline charts, alert feeds).

---

## Option B: One Unified UI (Chosen — see rev 4 update)

Everything in one app. Revs 1–3 dismissed this as "over-merged" because
it would destroy moire's endpoint agnosticism. Rev 4 reconsiders:

- **moire doesn't exist yet.** There are zero users of a standalone
  SPARQL explorer. The endpoint-agnostic use case is speculative.
- **One developer, one codebase.** Two Next.js apps = 2× CI, 2×
  dependency maintenance, 2× deployment stories. The overhead isn't
  justified at this stage.
- **Precedent.** Stardog Studio, pgAdmin, and Grafana all combine
  monitoring + data work in one shell. Nobody complains.
- **Extraction is easy.** Going from 1 UI → 2 (extract the exploration
  module into its own app) is straightforward. Going from 2 → 1 (merge
  codebases, reconcile shared state, deduplicate components) is painful.

The sidebar shows/hides sections based on detected backends — the same
progressive detection model from the original console design, just
extended to cover exploration and curation too.

See [Option C: One Unified Console](#option-c-one-unified-console-recommendation)
for the full design.

---

## Option C: One Unified Console — **Surface** (Recommendation)

A single Next.js app named **surface** (`grove/surface`) that covers ALL
four concerns: pg-trickle admin, pg-ripple admin, data exploration,
and record linkage.

**Why "surface":** trickles and ripples are both visible at the water's
surface. The console literally is the surface layer of the platform —
the point where all the underlying data motion becomes observable.
It also avoids encoding either extension's name into the UI brand,
keeping it neutral ground between pg-trickle and pg-ripple.

**Not hardwired to either binary** — it's a
standalone app that connects to whichever backends are available.

### Progressive feature detection at startup

| Backend detected | Sections shown |
|-----------------|----------------|
| Relay API reachable | Streaming: topology, tables, SLA, CDC, relay pipelines, timeline |
| pg_ripple_http reachable | Knowledge Graph: admin, exploration, SHACL, inference, record linkage |
| Both reachable | Full console — all sections + cross-cutting IVM views |
| Neither reachable | Connection setup screen |

### Deployment flexibility — three hosting options

```
Option 1: Embedded in relay via rust-embed
  relay binary serves /console/ (pg-trickle sections always visible,
  pg-ripple sections if pg_ripple_http is reachable)

Option 2: Embedded in pg_ripple_http via rust-embed
  pg_ripple_http serves /console/ (KG sections always visible,
  streaming sections if relay is reachable)

Option 3: Standalone deployment (Docker, Vercel, etc.)
  Connects to relay and/or pg_ripple_http via configured URLs
```

This solves the pg-ripple-only problem: scenario B users embed the
console in `pg_ripple_http` and get KG admin + data exploration. The
streaming sections are simply hidden (no relay detected). If they
later add pg-trickle and the relay, those sections light up
automatically.

### Why one app works

1. **Same operator.** The person monitoring stream table health is the
   same person who will browse the knowledge graph and do record
   linkage. In a small-to-medium deployment, there's one DBA or data
   engineer wearing all hats. Sending them to two different apps with
   two different URLs is unnecessary friction.

2. **Cross-cutting objects.** An IVM SPARQL view IS a pg-trickle
   stream table. Showing its pipeline health AND its graph data in
   the same app eliminates context-switching.

3. **Unified alerting.** pg-ripple events (SHACL violation, compaction
   stall, cache thrashing) and pg-trickle events (SLA breach, CDC
   failure, fuse blown) belong in the same alert feed.

4. **Shared component surface.** Health scorecards, alert feeds,
   timeline charts, entity detail views, data tables — massive
   component reuse within one codebase.

5. **One codebase to maintain.** One CI pipeline, one set of
   dependencies, one deployment story. For a solo developer, this
   matters more than architectural purity.

6. **Extract later if needed.** If the standalone SPARQL explorer
   use case proves real (someone wanting to browse Wikidata without
   pg-trickle), the exploration module can be extracted into its own
   app. Going from 1 → 2 apps is easy. Going from 2 → 1 is hard.

### Navigation (full stack, both backends detected)

```
  Dashboard         ← unified health summary
  Streaming
  ├── Tables        ← stream tables, IVM views get 🔷 graph badge
  ├── Topology
  ├── Relay Pipelines
  ├── CDC Health
  └── Timeline
  Knowledge Graph
  ├── Explore       ← faceted entity navigation, type/graph browsers
  ├── Graphs        ← named graph list with sizes
  ├── Dictionary & Storage
  ├── SHACL Shapes
  ├── Inference Rules
  ├── Subscriptions
  └── Record Linkage ← candidate pairs, compare, accept/reject
  Health            ← unified scorecard (both systems)
  Alerts            ← unified feed
```

When only one backend is detected, the other section's nav group is
simply absent. If only `pg_ripple_http` is detected, the user sees
Knowledge Graph (with Explore, admin, and record linkage) + Health +
Alerts — a complete experience without pg-trickle.

### API design — two base URLs, one frontend

The console frontend talks to whichever backends are available:

```
Relay API:           relay:9090/api/v1/tables, /dag, /health, ...
pg_ripple_http API:  ripple:3030/api/v1/graphs, /cache, /shacl, ...
                     ripple:3030/sparql  (SPARQL Protocol for exploration)
```

The frontend stores both base URLs in config. When embedded in one
binary, that binary's API is always `localhost`; the other binary's
API is configured via environment variable or auto-discovered.

### Record linkage as a console module

Record linkage lives in the console under Knowledge Graph → Record
Linkage. It reuses the same entity detail components from Explore.
This works because:

1. **Same backend.** Record linkage uses `pg_ripple_http` — same API
   the console already talks to for KG admin.

2. **Shared components.** Entity detail views, predicate tables,
   graph neighbourhood visualisation — all built for the Explore
   section, reused by record linkage.

3. **Same session.** A data steward doing record linkage will browse
   entities to decide if they match. Having Explore one click away in
   the same app (not a different URL) is essential.

4. **Progressive enablement.** Record linkage appears under Knowledge
   Graph only when `pg_ripple_http` is detected. It's never shown to
   pg-trickle-only users.

---

## Where Does the Code Live?

### Repo structure

```
grove/pg-trickle              ← IVM extension + relay + TUI + dbt
grove/pg-ripple               ← Triplestore + pg_ripple_http
grove/surface                 ← Unified web console (“surface of the platform”)
```

See [ANALYSIS_MONOREPO.md](../ANALYSIS_MONOREPO.md) for the full
rationale for 3 separate repos.

**Why the console gets its own repo:**

1. **Serves both extensions equally.** Putting it inside either
   extension's repo creates an asymmetry. A standalone repo is neutral
   ground.

2. **Different technology.** Next.js/TypeScript app with its own CI
   (ESLint, TypeScript, Playwright) — not Rust, not pgrx.

3. **Independent release cycle.** Frontend ships when frontend is
   ready, not when either extension releases.

### Embedding workflow

Both Rust binaries consume the console's pre-built static files:

```
surface CI:
  npm run build → out/ → published as GitHub release asset (tar.gz)

pgtrickle-relay build (when --features console):
  Download surface-vX.Y.Z.tar.gz → embed via rust-embed

pg_ripple_http build (when --features console):
  Download surface-vX.Y.Z.tar.gz → embed via rust-embed
```

This keeps the frontend build out of `cargo build` entirely. The Rust
binaries just embed a pre-built tarball. The console version is pinned
in each binary's build config, allowing independent upgrades.

---

## How This Handles Each Scenario

| Scenario | Console deployment | Result |
|----------|--------------------|--------|
| **A. Full stack** | Embedded in relay (all sections visible) | Streaming admin + KG admin + data exploration + record linkage |
| **B. pg-ripple only** | Embedded in pg_ripple_http (KG sections only) | KG admin + data exploration + record linkage |
| **C. pg-trickle only** | Embedded in relay (streaming sections only) | Streaming admin |
| **D. Standalone** | Docker / Vercel, configured with backend URLs | Connects to whichever backends are reachable |

Every scenario works. No user is left without a UI. No product is
forced to depend on another product just to get admin tooling.

---

## Comparison Matrix

| Concern | 4 UIs | 2 UIs (rev 3) | 1 UI (chosen) |
|---------|-------|---------------|---------------|
| Maintenance burden | High (4 codebases) | Medium (2 codebases) | **Low (1 codebase)** |
| pg-ripple standalone admin | ✅ Own app | ✅ Console embedded in pg_ripple_http | ✅ Same — embedded in pg_ripple_http |
| Data exploration | ✅ moire standalone | ✅ moire standalone | ✅ Explore section in console |
| Record linkage home | Own thin app | Module in moire | Module in console (KG section) |
| Cross-cutting IVM view | ❌ Context switch | ✅ Console + moire linked | **✅ Same page** |
| Deployment flexibility | 4 deployments | Console: 3 options; moire: standalone | Console: 3 options |
| Standalone SPARQL explorer | ✅ moire works alone | ✅ moire works alone | ❌ Not available — extract later if needed |
| Cognitive coherence | Good per UI | Good (monitoring vs data) | Good enough — sidebar sections separate activities |
| Solo dev overhead | Prohibitive | Manageable | **Minimal** |
| Future extensibility | Add more thin apps | New admin → console; new data → moire | New sections → console |

The main tradeoff: losing the standalone SPARQL explorer use case
(someone exploring Wikidata without any pg-trickle/pg-ripple). This
is speculative — moire has zero users and zero code. If this use case
proves real later, the Explore module can be extracted into its own
app.

---

## Implementation Sequencing

### Phase 0: API scaffolds (parallel work in both repos)

- `pg-trickle` relay: `/api/v1/` routes as planned in PLAN_WEBUI.md
- `pg-ripple` pg_ripple_http: add `/api/v1/` routes for KG operational
  data (graphs, cache, SHACL, inference, subscriptions)

Both APIs serve JSON. Both are useful immediately for scripting,
Grafana, and LLM agents — no frontend needed yet.

### Phase 1: Console with streaming admin (`grove/surface`)

Next.js app. Connects to relay API. Progressive feature detection.
Tier 1 read-only streaming dashboard (topology, tables, SLA, CDC,
timeline).

### Phase 2: KG admin sections

Add Knowledge Graph admin sections. Connect to `pg_ripple_http` API.
Graphs, dictionary, SHACL, inference, subscriptions.

### Phase 3: Explore section

Add faceted entity navigation under Knowledge Graph → Explore. SPARQL
queries against `pg_ripple_http`'s SPARQL endpoint. Entity detail,
type/graph browsers, semantic zoom.

### Phase 4: Console Tier 2 + Record linkage

Console write operations (relay pipeline management, fuse reset).
Record linkage module under Knowledge Graph.

---

## Recommendation

**Build one unified web console (`grove/surface`).**

A single Next.js app that covers admin monitoring, data exploration,
and data curation for both pg-trickle and pg-ripple. Progressive
backend detection shows only relevant sections. Embeddable in either
Rust binary or deployed standalone.

**Key design decisions:**

- Console is a **standalone Next.js app in its own repo**, not inside
  either extension's repo. It serves both equally.
- Console uses **two API base URLs** (relay + pg_ripple_http), not a
  unified API. Each backend owns its own API surface.
- **Progressive sections.** Streaming sections appear when the relay
  is reachable. Knowledge Graph sections (admin, explore, record
  linkage) appear when `pg_ripple_http` is reachable.
- Record linkage is a **module in the console** under Knowledge Graph,
  reusing the Explore section's entity components.
- The standalone SPARQL explorer use case (moire) is **deferred**.
  If it proves needed, the Explore module can be extracted. This is
  cheaper than maintaining two apps from the start.
