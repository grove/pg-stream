# WebUI — Analysis & Implementation Plan

> **Status:** Analysis & Design (DRAFT — 2026-04-21)
> **Created:** 2026-04-21
> **Category:** Tooling — Web Dashboard & Management Console
> **Related:** [PLAN_RELAY_CLI.md](../relay/PLAN_RELAY_CLI.md) ·
> [PLAN_TUI.md](../ui/PLAN_TUI.md) ·
> [PLAN_WEBUI_STYLE.md](PLAN_WEBUI_STYLE.md) ·
> [ROADMAP.md](../../ROADMAP.md)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Motivation](#motivation)
- [Target Users](#target-users)
- [Architecture](#architecture)
  - [Hosting Model](#hosting-model)
  - [Authentication](#authentication)
  - [Frontend Technology](#frontend-technology)
  - [Backend API Layer](#backend-api-layer)
  - [Real-Time Data](#real-time-data)
- [Feature Tiers](#feature-tiers)
  - [Tier 1 — Read-Only Dashboard](#tier-1--read-only-dashboard)
  - [Tier 2 — Relay Pipeline Management](#tier-2--relay-pipeline-management)
  - [Tier 3 — Stream Table Wizard & Full Setup](#tier-3--stream-table-wizard--full-setup)
- [Key Features — Detailed Design](#key-features--detailed-design)
  - [Pipelines (End-to-End Flows)](#pipelines-end-to-end-flows)
  - [Unified Topology Graph](#unified-topology-graph)
  - [SLA Budget & Lag Monitoring](#sla-budget--lag-monitoring)
  - [DVM Operator Inspector](#dvm-operator-inspector)
  - [Column-Level Lineage](#column-level-lineage)
  - [Live Change Flow](#live-change-flow)
  - [Data Explorer](#data-explorer)
  - [SQL Preview Model](#sql-preview-model)
  - [Health Scorecard](#health-scorecard)
  - [Refresh Timeline](#refresh-timeline)
- [LLM Agent Integration & MCP Server](#llm-agent-integration--mcp-server)
  - [Impact on Feature Tiers](#impact-on-feature-tiers)
  - [MCP Server](#mcp-server)
  - [OpenAPI Specification](#openapi-specification)
- [Comparison with RisingWave Dashboard](#comparison-with-risingwave-dashboard)
- [Sequencing & Dependencies](#sequencing--dependencies)
  - [Prerequisites](#prerequisites)
  - [Delivery Phases](#delivery-phases)
  - [Relay `AppState` Requirements (Day 1)](#relay-appstate-requirements-day-1)
  - [Self-Monitoring via Stream Tables (Dogfooding)](#self-monitoring-via-stream-tables-dogfooding)
- [Open Questions](#open-questions)

---

## Executive Summary

The pg-trickle WebUI is a browser-based management console and operational
dashboard embedded in the `pgtrickle-relay` binary. It provides a complete
view of the streaming materialization pipeline — from external sources
through relay ingestion, stream table chains, outboxes, and relay sinks —
in a single unified interface.

The WebUI replaces the need for direct SQL interaction for common operations
while keeping SQL as the canonical configuration layer. Every write action
in the WebUI generates SQL, shows a preview, and executes it against
PostgreSQL on confirmation. Power users can copy the generated SQL into
version-controlled migrations.

The JSON API layer (`/api/v1/`) that backs the WebUI is simultaneously
an **MCP (Model Context Protocol) server** — the same endpoints that the
frontend calls are discoverable and callable by LLM agents (Copilot,
Claude, custom agents). This means the API is a first-class integration
surface, not just WebUI plumbing, and is designed with semver stability
guarantees from Phase 0.

The scope is deliberately broader than a monitoring dashboard: at full
maturity, the WebUI enables setting up a complete streaming solution —
CDC sources, stream tables, relay pipelines — entirely through the
browser or via LLM agents calling the API. This positions pg-trickle as
an accessible alternative to managed streaming platforms (RisingWave
Cloud, Confluent Cloud, Materialize) while retaining PostgreSQL as the
foundation.

### Key architectural decisions

**No logic in JavaScript.** Every derived value that an external tool
(Grafana, Prometheus, alertmanager) could usefully see is computed in
the database or API layer, never in the browser. The three layers are:
- *DB* — structural table types, E2E lag, SLA %, root-cause/blast-radius
  classification. All queryable directly from SQL.
- *API* — assembles transport annotations from relay config tables;
  computes nothing that SQL cannot.
- *Browser* — graph layout positions, relative timestamps, UI state.

**Topology = schemas only.** The Level 0 topology graph shows one node
per PostgreSQL schema. External transports (Kafka, NATS, webhook) are
small icon annotations on schema nodes, not separate graph nodes. A
typical shop uses one Kafka cluster for everything — a Kafka node would
add noise, not information. Layout is computed automatically from
inter-schema edge direction by dagre/elkjs with no manual tier
assignment.

**Tables list, not a pipelines list.** There is no separate "pipelines"
concept. A pipeline is just a table plus its upstream lineage — the same
thing. Each stream table has a structural type (inbox / outbox / leaf /
intermediate / union / orphan) inferred from the DAG by
`pgtrickle.table_classifications`. E2E lag is shown on every table, not
just leaf tables. Fan-in/fan-out topologies (10 sources × 10 sinks)
produce N table rows, not 100 path rows.

**Root cause vs inherited breach in the DB.** `pgtrickle.breach_events`
is a stream table that classifies each breach as `root` (independent
failure) or `inherited` (cascade from upstream). The UI, Grafana
dashboards, and Prometheus alerts all read from this single source of
truth. The sidebar badge counts root cause events only — not downstream
noise. A single broken inbox cascading to 10 outputs produces 1 alert
entry and 1 badge count, not 11.

**Bidirectional lineage on every table.** The table detail page shows
upstream (root cause analysis — what feeds this table?) and downstream
(blast radius — what breaks if this table fails?) in a single centred
lineage view. Clicking any node re-centres the view on that node.
Toggling between table-level and column-level granularity happens in
the same canvas.

**Single-schema auto-navigate with breadcrumb.** If the deployment has
only one schema, the topology opens directly at Level 1 (individual
nodes) rather than a pointless single-node Level 0. The breadcrumb
always shows `Systems > <schema>` so the operator can navigate back,
see the schemas-as-groups model, and understand how a second schema
would appear.

---

## Motivation

The current getting-started path requires:

1. Install the PostgreSQL extension
2. Write SQL to create stream tables and configure CDC
3. Install the relay binary and configure it via SQL
4. Use the TUI or raw SQL to monitor

Each step requires a different tool and a different skill set. The WebUI
collapses steps 2–4 into a single browser interface. The three audiences
this unlocks:

- **"I just want it working in 10 minutes"** — the wizard path, no SQL
  knowledge required.
- **"I want to understand what's happening"** — the SQL preview at every
  step teaches the pg-trickle SQL API by example.
- **"I need full control"** — the SQL escape hatch and CLI still work. The
  WebUI is additive.

---

## Target Users

### 1. Data Engineer

**Primary use:** Author stream tables and relay pipelines without writing
SQL from scratch. The DVM compatibility checker gives instant feedback on
whether a view definition supports DIFFERENTIAL refresh before committing.

**Key features:** Stream table wizard, DVM checker, relay pipeline editor,
topology graph, column-level lineage.

### 2. SRE / Operator (on-call)

**Primary use:** Assess health during incidents without SSH access. The
relay's HTTP port is already exposed — `/ui` is just another route.

**Key features:** Health scorecard, SLA budget dashboard, live alert feed,
fuse management, relay topology with lag indicators, refresh timeline.

### 3. Data Analyst

**Primary use:** Explore derived data and understand lineage — where does
this metric come from? What tables does it depend on?

**Key features:** Column-level lineage, data explorer (sample rows),
schema browser, staleness badges.

### 4. Platform Engineer

**Primary use:** Manage relay pipelines at scale, monitor CDC health,
verify worker pool utilization.

**Key features:** Relay pipeline management, watermark/frontier view,
worker pool status, fuse control, change buffer inspection.

### 5. LLM Agent / Automation

**Primary use:** Programmatically create stream tables, configure relay
pipelines, and diagnose issues via the `/api/v1/` JSON API. The same
API that backs the WebUI frontend is the natural integration surface
for LLM agents (Copilot, Claude, custom agents) and automation scripts.

**Key features:** `POST /api/v1/sql/preview` (DVM compatibility check
before applying), `POST /api/v1/sql/execute` (apply changes),
`GET /api/v1/dag/topology` (understand full pipeline),
`GET /api/v1/health` (diagnose problems), MCP server endpoints.

**Key insight:** LLM agents will handle most initial setup tasks
(creating stream tables, configuring relay pipelines) via natural
language → SQL generation. This reduces the urgency of Tier 3 wizard
features while increasing the importance of the API layer as a
first-class integration surface. See
[LLM Agent Integration & MCP Server](#llm-agent-integration--mcp-server).

---

## Architecture

### Hosting Model

The WebUI is embedded in the `pgtrickle-relay` binary. This follows the
RisingWave model: the frontend is built to static files (Next.js `out/`
or equivalent), bundled into the Rust binary via `rust-embed`, and served
by the existing Axum HTTP server.

```
pgtrickle-relay serve
  ├── :9090/metrics          Prometheus scrape
  ├── :9090/health           Health check (K8s probes)
  ├── :9090/health/drained   Drain check (rolling deploys)
  ├── :9090/webhook/...      Reverse-mode webhook receiver
  ├── :9090/api/v1/...       JSON API (WebUI backend)
  ├── :9090/api/v1/ws        WebSocket (live alerts, topology updates)
  └── :9090/ui/...           Static frontend (rust-embed)
```

**Key property:** No Node.js in production. The frontend is pre-built at
compile time and embedded as static assets. The relay binary is the only
thing to deploy.

**Feature-flagged:** `--features webui` controls whether the static assets
and API routes are compiled in. The minimal relay sidecar image ships
without it; a "full" image includes the UI. Both images are published.

### Authentication

Three modes, selectable via `--auth none|pg|oidc`:

| Mode | How it works | Use case |
|------|-------------|----------|
| **`none`** (default) | No login. WebUI uses the relay's PG connection. | Sidecar behind VPN/ingress |
| **`pg`** | Login form with PG username/password. Per-session connection pool. Respects PG roles and RLS. | Shared team deployment |
| **`oidc`** | OAuth2/OIDC login (Google, GitHub, Okta). Maps `sub` claim → PG role via config. | Enterprise / multi-tenant |

Auth is implemented as Axum middleware on the `/api/v1/` and `/ui/` routes.
The `/metrics` and `/health` endpoints remain unauthenticated (Prometheus
and K8s probes need direct access).

### Frontend Technology

**Recommended: Next.js (React), built to static HTML, embedded via
`rust-embed`.**

Rationale:

- The DAG/topology visualization, live backpressure animation, and
  time-series charts are client-heavy features. Server-side rendering
  (HTMX) cannot match the UX quality for these use cases.
- RisingWave uses the same stack (Next.js + static export) and has
  maintained it for 4+ years successfully.
- The JS ecosystem for graph rendering (Cytoscape.js, reactflow, D3),
  charting (ECharts, Recharts), and code editing (Monaco) is
  unmatched.
- Build process: `npm run build` → `out/` → `rust-embed` → single
  binary. CI builds the frontend as a pre-step before `cargo build`.

**Decision: Next.js from the start.** The topology graph, live
backpressure animations, and time-series charts are the centrepiece
features and require a proper JS framework. Starting with HTMX would
require a rewrite for Tier 2/3. CI will include a Node.js + `npm run
build` pre-step before `cargo build`.

**Alternatives rejected:** HTMX + Askama templates (insufficient for
graph rendering and real-time UX). Leptos (too small an ecosystem for
charting and graph visualization).

### Backend API Layer

A JSON REST API under `/api/v1/` that serves three consumers:

1. **WebUI frontend** — the primary consumer, calls these endpoints
   for all data and write operations.
2. **LLM agents** — use the same endpoints to understand, configure,
   and diagnose pg-trickle programmatically. The API is the MCP/tool
   integration surface (see [MCP Server](#mcp-server)).
3. **Scripts and third-party tools** — Grafana annotations, CI/CD
   pipelines, custom dashboards.

The API is treated as a **first-class public contract** with semver
guarantees from Phase 0. Breaking changes require a version bump
(`/api/v2/`). This stability commitment is critical because LLM agents
and automation scripts depend on predictable schemas.

```
# Tables — stream tables with type annotations and E2E lag
GET  /api/v1/tables                     All stream tables (type, staleness, E2E lag, SLA)
GET  /api/v1/tables/:schema/:table      Table detail + bidirectional lineage
GET  /api/v1/tables/:schema/:table/history       Refresh history
GET  /api/v1/tables/:schema/:table/lineage       Column-level lineage
GET  /api/v1/tables/:schema/:table/operator-tree DVM operator tree (DOT format)
GET  /api/v1/tables/:schema/:table/sample        Sample rows (LIMIT 50)

# Health (aggregates CDC, fuses, slots, workers, relay status)
GET  /api/v1/health                     Health scorecard
GET  /api/v1/health/checks              Individual health checks

# DAG / topology
GET  /api/v1/dag                        Full DAG (nodes + edges + metadata)
GET  /api/v1/dag/topology               Extended DAG with relay nodes

# Relay connectors
GET  /api/v1/relay/pipelines            All relay pipelines (forward + reverse)
GET  /api/v1/relay/pipelines/:id        Single pipeline detail
GET  /api/v1/relay/pipelines/:id/lag    Lag metrics for a pipeline

# Dead letter queue
GET   /api/v1/dlq                        All unresolved dead letters (paginated)
GET   /api/v1/dlq/:pipeline              Dead letters for one pipeline
PATCH /api/v1/dlq/:id/annotate           Update operator_notes (raw_payload immutable)
POST  /api/v1/dlq/:id/retry              Re-attempt delivery of one dead letter
POST  /api/v1/dlq/:id/discard           Mark resolved without retry
POST  /api/v1/dlq/discard-all/:pipeline Bulk discard for a pipeline

# Operational detail (sub-resources of Health in the UI)
GET  /api/v1/cdc/sources                CDC source table status
GET  /api/v1/cdc/buffers                Change buffer sizes
GET  /api/v1/cdc/slots                  Replication slot health
GET  /api/v1/scheduler/workers          Worker pool status
GET  /api/v1/scheduler/jobs             Job queue depth
GET  /api/v1/fuses                      Fuse/circuit-breaker state

# Activity
GET  /api/v1/timeline                   Refresh timeline (time-series)
GET  /api/v1/alerts                     Recent alerts (paginated)

# Write operations
POST /api/v1/sql/preview                Validate SQL, return DVM analysis
POST /api/v1/sql/execute                Execute SQL (with auth check)
POST /api/v1/fuses/:name/reset          Reset a blown fuse
POST /api/v1/tables/:name/refresh       Trigger manual refresh
```

The CDC, fuse, scheduler, and slot endpoints remain in the API for
programmatic access and the Health page's drill-down views. They are
not top-level navigation items in the WebUI — they live under Health.

Every write endpoint (`POST`) requires authentication in `pg` and `oidc`
modes. In `none` mode, writes use the relay's PG connection.

### Real-Time Data

Two distinct live-update tiers, both delivered via the same WebSocket
channel (`/api/v1/ws`):

#### Tier A — State Live (badge/metric updates)

Node decorations update without changing graph structure: staleness
counters tick up, buffer depth badges change, relay lag updates, SLA
colour transitions (green → amber → red), alert feed entries appear.

Source: `pg_trickle_alert` NOTIFY channel + relay pipeline state
changes broadcast from `AppState.ws_broadcast`. Frontend receives
`WsEvent::StateUpdate` and patches node/edge data in-place — no
graph re-layout required.

#### Tier B — Structure Live (topology changes)

Graph nodes and edges appear or disappear: a new stream table is
created, a relay pipeline is added, a CDC trigger is enabled on a
source table. This requires a graph re-fetch and re-render.

Source: a new `pgtrickle_structure_changed` NOTIFY channel emitted
by the existing DDL event trigger hooks in the extension. The relay
listens on this channel alongside `pg_trickle_alert` and broadcasts
`WsEvent::TopologyChanged` to connected WebSocket clients.

Frontend behaviour on `WsEvent::TopologyChanged`:
1. Re-fetch `GET /api/v1/dag/topology`
2. Diff the new node/edge list against the current graph
3. Animate nodes appearing (fade-in) or disappearing (fade-out)
4. Preserve current zoom/pan position and node layout where possible

The topology payload is small (tens of nodes in typical deployments),
so a full re-fetch is simpler and sufficient — no differential graph
patching needed.

**Round-trip latency** from "Apply" click to node appearing on graph:
- PG DDL execution + trigger: ~5–20 ms
- NOTIFY delivery: ~1 ms
- Relay LISTEN → `WsEvent::TopologyChanged`: ~1 ms
- Browser re-fetch + re-render: ~50–100 ms
- **Total: ~100 ms** — tight enough to feel instantaneous

**Multi-user coordination:** All connected WebUI clients (multiple
browser tabs, multiple team members) receive the same
`WsEvent::TopologyChanged` event simultaneously. An SRE watching the
topology dashboard sees a new stream table appear the moment a data
engineer or LLM agent applies the `CREATE STREAM TABLE` SQL — without
refreshing the page.

**Optimistic updates (Tier 2+):** When the current user applies SQL
via the WebUI's SQL preview model, the frontend can show the new node
*immediately* (before PG confirms) and reconcile with the server
response. If execution fails, the optimistic node is removed and the
error is shown. This requires the frontend to predict the topology
change from the SQL statement — feasible for simple cases
(`CREATE STREAM TABLE`, relay pipeline insert) but not for complex DDL.
Implemented as a Tier 2 enhancement, not required for Tier 1.

**Polling fallback:** For environments where WebSocket is blocked
(some corporate proxies), the frontend falls back to polling
`/api/v1/health` and `/api/v1/dag/topology` at a configurable
interval (default 5s). Structure changes are detected by comparing
the node list on each poll.

---

## Feature Tiers

### Tier 1 — Read-Only Dashboard

**Goal:** Replace the TUI's monitoring capabilities in a browser. Zero
write operations. Ships with the first WebUI release.

| Feature | Source data | TUI equivalent |
|---------|------------|----------------|
| **Health scorecard** | `health_check()`, `health_summary()` | View 8 (Health Checks) |
| **Stream table list** | `pg_stat_stream_tables`, `pgt_status()` | View 1 (Dashboard) |
| **Stream table detail** | `st_refresh_stats()`, `get_staleness()` | View 2 (Detail) |
| **Unified topology graph** | `dependency_tree()` + relay pipeline config | View 3 (Dependencies) — enhanced |
| **Refresh timeline** | `refresh_timeline()` | View 4 (Refresh Log) — as chart |
| **CDC health** | `check_cdc_health()`, `slot_health()` | View 6 (CDC Health) |
| **Live alerts** | `pg_trickle_alert` via WebSocket | View 9 (Alerts) |
| **Relay pipeline status** | Relay metrics + pipeline config | *New — no TUI equivalent* |
| **SLA budget indicators** | `get_staleness()` + schedule interval | *New — no TUI equivalent* |
| **Worker pool status** | `worker_pool_status()` | View W (Workers) |
| **Fuse status** | `fuse_status()` | View F (Fuse) — read only |
| **Configuration view** | GUC values, extension version | View 7 (Configuration) |

### Tier 2 — Relay Pipeline Management

**Goal:** Create, edit, and manage relay pipelines through the UI. First
write operations. SQL preview before every action.

| Feature | Generated SQL |
|---------|--------------|
| **Create forward pipeline** | `INSERT INTO pgtrickle.relay_outbox_config(...)` |
| **Create reverse pipeline** | `INSERT INTO pgtrickle.relay_inbox_config(...)` |
| **Edit pipeline** | `UPDATE pgtrickle.relay_outbox_config SET ... WHERE ...` |
| **Enable/disable pipeline** | `UPDATE ... SET enabled = true/false` |
| **Delete pipeline** | `DELETE FROM pgtrickle.relay_outbox_config WHERE ...` |
| **Fuse reset** | `SELECT pgtrickle.reset_fuse(...)` |
| **Manual refresh** | `SELECT pgtrickle.refresh(...)` |
| **Pause/resume stream table** | `SELECT pgtrickle.alter_stream_table(...)` |

**Backend form:** When creating a pipeline, the form generates the
`INSERT INTO pgtrickle.relay_outbox_config` or `relay_inbox_config` SQL
with the JSONB config object. Credential fields are shown as
`${env:VAR_NAME}` or `${file:/path/to/secret}` reference tokens —
the relay resolves these from the process environment at startup.
Secrets never pass through the WebUI or get stored in the database.

### Tier 3 — Stream Table Wizard & Full Setup (Deprioritized)

**Goal:** Set up a complete streaming solution through the browser without
writing SQL manually.

> **Note:** Tier 3 is deprioritized relative to Tiers 1 and 2. LLM agents
> can already handle most setup tasks (creating stream tables, configuring
> CDC, writing relay pipeline config) via natural language → SQL generation.
> The DVM compatibility checker remains high-value as an API endpoint
> (`POST /api/v1/sql/preview`) that agents call directly. The wizard UI
> around it is a nice-to-have, not a prerequisite for the "10-minute setup"
> experience — an agent achieves that today. Tier 3 should be built when
> there is specific user demand for form-based setup that agents don't
> cover well.

| Feature | Description |
|---------|-------------|
| **DVM compatibility checker** | Paste/type a view definition. Live analysis shows: DIFFERENTIAL eligible ✅ or FULL REFRESH ⚠️ with explanation of unsupported constructs. Uses `validation.rs` logic via the `/api/v1/sql/preview` endpoint. |
| **Stream table creation wizard** | Form: pick source view, schedule (slider 1s→1h), refresh mode, SLA threshold. Generates `CREATE STREAM TABLE` SQL with preview. |
| **CDC source enablement** | Pick a table → enable CDC trigger. Generates `pgtrickle.enable_cdc(...)`. |
| **Schema browser** | Stream tables grouped by schema. Column types, nullability, PK columns marked. Staleness badge on every table. |
| **Monaco SQL editor** | Embedded SQL editor with pg-trickle function autocomplete, syntax highlighting, and DVM compatibility checking as you type. |
| **Column-level lineage** | Click a stream table column → trace derivation back to source columns through the entire OpTree. Rendered as a focused sub-graph. |
| **DVM operator inspector** | For each stream table: visualize the operator tree (Scan → Filter → Join → Aggregate), annotated with which operators are incrementally maintained, which use auxiliary columns, and which force FULL refresh. Rendered via `graphviz-wasm` in the browser. |

---

## Key Features — Detailed Design

### Tables

The primary list view. Every stream table in the deployment, annotated
with its structural role derived from the DAG. No separate "pipelines"
concept — a pipeline is just a table plus its upstream lineage, so the
two views are the same thing.

**Structural type** is served by the API from `pgtrickle.table_classifications`,
a plain SQL view computed in the database — not derived in the API
server or browser. This means Grafana dashboards and external tools
can query the same type classifications directly:

| Type | How inferred |
|------|-------------|
| **inbox** | A relay inbox config writes into this table |
| **outbox** | A relay outbox config reads from this table |
| **source** | Base table with CDC; no stream-table ancestors |
| **intermediate** | Has both upstream and downstream stream tables |
| **leaf** | Stream table with no stream-table descendants |
| **orphan** | No upstream source and no relay inbox |
| **union** | Multiple upstream stream tables (fan-in) |

These types are not mutually exclusive — a table can be `leaf + union`.
They are shown as small badge chips on each row.

**E2E lag** is served from `pgtrickle.end_to_end_lag` (a stream table
maintained by the relay). It holds the maximum cumulative staleness
from any root source to each table — not computed on request and not
computed in the browser. Grafana can plot E2E lag time-series directly
from this table.

```
Table                 Type          Staleness  E2E Lag   SLA
────────────────────  ───────────   ─────────  ────────  ────────────
regional_summary      leaf union    45s        61s       ████████ 🔴
revenue_7d            intermediate  12s        28s       ████ 🟡
orders_raw            inbox         3s         3s        █ 🟢
canonical             intermediate  5s         18s       ██ 🟢
customers             orphan        —          —         —
```

- **Row click:** Opens table detail with bidirectional lineage.
- **Sort:** Default is worst SLA first (problems at the top).
- **Faceted filters:** Schema, type badge, refresh mode (DIFF/FULL), SLA status.
- **Search:** Matches table name or schema.

**Table detail** shows the table's own metrics plus its **bidirectional
lineage**: upstream on the left (root cause analysis — what feeds this
table and how stale is each branch?) and downstream on the right (blast
radius — what depends on this table and what breaks if it's unhealthy?).
The upstream and downstream DAGs are shown together in a single lineage
view with the selected table centred. Both upstream and downstream nodes
are clickable to navigate to their own detail pages.

The **Tables** page and the **Topology** page are two views of the
same data:

| Task | Best view |
|---|---|
| Understand system architecture | Topology (graph) |
| Find the table breaching SLA | Tables (sorted by worst SLA) |
| Trace what feeds a specific table | Table detail (lineage — upstream side) |
| Trace what depends on a table | Table detail (lineage — downstream side) |
| Filter by type or refresh mode | Tables list with faceted filters |
| See end-to-end latency | Tables list (E2E lag column) |

**Sub-views within lineage.** Sub-sections are accessed by interacting
within the view:

- **Click a node** in the lineage view → navigates to that table's
  own detail page (centering the lineage on the new node).
- **Topology `?focus=<name>&depth=N`** → scopes the graph to the
  N-hop neighbourhood around any named node.
- **Click an edge between two schema groups** in Topology Level 0 →
  drills into Level 1 scoped to flows between those two schemas.

### Unified Topology Graph

The centrepiece of the WebUI. A multi-level interactive graph using
**semantic zoom** — the graph shows the logical structure at whatever
level of detail the user is looking at, from systems overview to
individual node.

**Three zoom levels:**

**Level 0 — Systems overview (landing).** One node per PostgreSQL
schema. That's it — no external system nodes. The schemas are the
business domains; external transports (Kafka, webhook, NATS, etc.)
are implementation details shown as small icon annotations on the
schema node that uses them.

A schema that ingests from Kafka shows a `⟵ kafka` badge on its left
edge. A schema that publishes via webhook shows a `webhook ⟶` badge
on its right edge. A bidirectional schema shows both. Hovering the
badge shows connection status and which relay config(s) are active.
This keeps Level 0 readable at a glance — every node is a named
business domain, and the transport annotations answer "how does data
get in/out?" without cluttering the graph with infrastructure nodes.

**Level 1 — Group detail.** Click a schema group or an edge between
two groups to see all individual nodes within that scope. Shows
partial pipelines between adjacent groups (e.g., all flows from
`erp_raw` to `canonical`). This is the level where individual stream
table nodes appear with their status badges.

**Level 2 — Individual flow.** Click a specific flow line to see the
full pipeline detail — every node from source to sink with staleness,
buffer depth, SLA budget.

**Single-schema auto-navigation.** If the deployment has only one
schema, the topology automatically opens at Level 1 (the schema
detail view) rather than stopping at a Level 0 with a single node.
The breadcrumb always shows `Systems > <schema>` so the user can
navigate back to Level 0, see that schemas are the grouping unit,
and understand how a second schema would appear.

**Node types and visual encoding:**

| Node type | Shape | Colour | Badge |
|-----------|-------|--------|-------|
| Schema group (Level 0) | Rounded rectangle, thick border | Worst-child SLA colour | Object count + status summary; transport icon annotations on edges |
| Relay pipeline | Hexagon | Blue (connected) / Red (disconnected) | Lag rows |
| Inbox / Outbox table | Rectangle, dashed border | Teal | Buffer depth |
| Source base table | Rectangle | White | CDC mode |
| Stream table | Rectangle, bold border | Green / Amber / Red (by SLA) | Staleness |

**Edge encoding:**

- Width proportional to throughput (rows/sec over last 5 min)
- At Level 0: width proportional to pipeline count, colour = worst SLA
- Colour: green (healthy), amber (lag growing), red (SLA breach)
- Animated dots for active data flow (like RisingWave's backpressure)

**Interactions:**

- Click a group node (Level 0) → drill into Level 1 (schema detail)
- Click an edge between groups → show partial pipelines between them
- Click a node (Level 1) → opens detail panel (stats, config, history)
- Hover an edge → shows throughput, lag, last update timestamp
- Breadcrumb trail at top: Systems > erp_raw > orders_raw
- Back button / breadcrumb returns to previous zoom level
- Time slider → replays historical state using `refresh_timeline()` data
- Zoom/pan, drag-to-rearrange
- Deep-linkable: `/ui/topology?focus=revenue_7d` highlights a node

**Live updates:**
- `WsEvent::StateUpdate` → badge values update in-place (no re-layout)
- `WsEvent::TopologyChanged` → re-fetch topology, animate new/removed
  nodes, preserve zoom/pan and existing node positions
- Optimistic node insertion when the current user applies SQL (Tier 2+)

**Implementation:** Cytoscape.js or reactflow for the graph layout. DAG
data from `/api/v1/dag/topology` (extends `dependency_tree()` with relay
pipeline nodes). WebSocket-driven updates; 5s polling fallback.

### SLA Budget & Lag Monitoring

Inspired by Kafka consumer lag dashboards. Three lag dimensions, each
visible inline on the stream table list and topology graph:

| Dimension | Metric source | Kafka analog |
|-----------|--------------|--------------|
| **Change buffer depth** | `change_buffer_sizes()` | Messages not yet consumed |
| **Staleness** | `get_staleness()` vs schedule interval | Consumer group time lag |
| **Relay pipeline lag** | `relay_consumer_lag_rows` | Sink-side consumer lag |

**SLA budget display:**

```
Table              Staleness    Buffer   SLA Budget   Trend
revenue_7d         12s / 60s    0 rows   ██░░░ 20%    ↔ stable
regional_summary   45s / 60s    1,204    ████████ 75% ↑ burning
orders_hourly      61s / 60s    8,102    ██████████   🔴 BREACH
```

**Burn rate:** Derived from staleness trend over a sliding window. If the
SLA is "max 30s stale" and the table is 25s stale and trending upward,
the burn rate shows the remaining time before breach.

**Root cause vs inherited breach.** A table in SLA breach is classified
as either:

- **Root cause** — the breach originates here. The table's own refresh
  is failing, stalled, or producing rows too slowly independent of its
  inputs. Identified when: the table has no stream-table ancestors, or
  all stream-table ancestors are healthy.
- **Inherited breach** — the table is breached because an upstream node
  is breached. Its own refresh is keeping pace; the staleness is flowing
  down from the root. Identified when: at least one upstream node is
  itself in root-cause breach.

This distinction affects three things:

1. **Table list:** Root cause rows show `🔴 root`; inherited rows show
   `🔴 ⬆`. A "Root causes" view mode collapses all casualties under
   their cause — 1 broken inbox + 10 downstream outputs = 1 entry, not 11.
2. **Table detail header:** An inherited-breach table shows a banner:
   "Root cause: `erp_raw.orders_raw` →" so the operator can jump
   directly to the real problem without reading the lineage graph.
3. **Alerts feed:** Downstream breach alerts are grouped under the root
   cause event rather than firing separately. The sidebar badge counts
   independent root cause events, not total breached tables.

**End-to-end lag:** The topology graph shows cumulative lag through the
dependency chain. A 2s delay in `orders` CDC that compounds to 45s
end-to-end staleness in `regional_summary` is visible at a glance. No
generic monitoring tool understands this relationship.

**SLA configuration:** Inferred from the schedule interval with no
schema changes required:

- Staleness > 2× schedule interval = **warning** (amber)
- Staleness > 3× schedule interval = **breach** (red)

For example, a table with a 60s schedule triggers a warning at 120s
staleness and a breach at 180s. This provides useful SLA monitoring
out of the box without any user configuration. Explicit per-table SLA
thresholds (via a SQL function) can be added later if needed.

### DVM Operator Inspector

For each stream table, the WebUI renders the DVM operator tree showing
exactly how pg-trickle processes incremental changes:

```
Aggregate (GROUP BY region)
  ├── strategy: ALGEBRAIC_VIA_AUX
  ├── auxiliary: __pgt_aux_sum_amount, __pgt_aux_count_amount
  │
  └── Join (INNER, orders.customer_id = customers.id)
        ├── strategy: DIFFERENTIAL (hash join)
        │
        ├── Scan: orders (CDC: trigger, PK: order_id)
        │     └── change buffer: 1,204 rows pending
        │
        └── Scan: customers (CDC: trigger, PK: customer_id)
              └── change buffer: 0 rows pending
```

**Annotations per node:**

- Differential strategy (invertible, aux columns, group rescan)
- Auxiliary columns generated and their purpose
- Why a node might force FULL refresh (volatile function, unsupported
  construct)
- Change buffer depth at leaf nodes

**Rendering:** The `OpTree` is serialized as DOT format by the Rust API,
rendered client-side via `graphviz-wasm` or `d3-graphviz`. This matches
the RisingWave approach for `explain_distsql`.

### Column-Level Lineage

pg-trickle's `OpTree` AST contains complete derivation information:
`Scan` nodes with `table_oid` and columns, `Project` nodes with
expressions and aliases, `Aggregate` nodes with `AggExpr`. This enables
tracing any output column back to its source columns.

**Example:** Clicking `revenue_7d.total_revenue` shows:

```
revenue_7d.total_revenue
  └── SUM(orders.amount)
        └── orders.amount (numeric, CDC: trigger)
              └── base table: public.orders, column: amount
```

**Cross-table lineage:** For chained stream tables, the lineage traces
through intermediate stream tables back to base tables. This is the
data catalog feature that teams currently pay for in dbt lineage,
DataHub, or Amundsen — built-in and always up to date.

### Live Change Flow

A real-time view of data moving through the system. Powered by the
`pg_trickle_alert` NOTIFY channel over WebSocket.

Events displayed as a scrolling timeline:
- `orders` table: +1,200 rows captured by CDC trigger
- `revenue_7d` refresh triggered (DIFFERENTIAL, 45ms, +3 rows / -1 row)
- `regional_summary` refresh triggered (DIFFERENTIAL, 12ms, +1 row)
- `summary_outbox`: 1 message published to relay forward pipeline
- Relay forward: message delivered to NATS `analytics.summary`

This is the streaming equivalent of watching Kafka consumer offsets
advance in real time.

### Data Explorer

A stream-table-aware schema browser and data preview. **Not** a general
PostgreSQL browser — scoped to pg-trickle objects only.

**Features:**

- Schema tree: stream tables grouped by schema, source tables with CDC
  badges
- Click a table → column list with types, nullability, PK markers
- "Sample" button: runs `SELECT * FROM ... LIMIT 50` via the API
- Side-by-side: view definition SQL and the compiled delta SQL (from
  `explain_delta()`)
- Staleness badge inline on every table
- Refresh mode badge with tooltip from `explain_refresh_mode()`
- Change buffer preview: pending rows for a source table before the next
  refresh

### SQL Preview Model

Every write action in the WebUI follows the same pattern:

```
User fills form (or LLM agent generates SQL)
    ↓
WebUI renders SQL in a Monaco/code block for review
    ↓
User reviews SQL, optionally edits it
    ↓
"Apply" button executes the SQL against PostgreSQL
    ↓
Result displayed (success + affected rows, or error + explanation)
```

The SQL preview is also the **human review layer for LLM-generated
configuration**. When an agent generates a `CREATE STREAM TABLE`
statement, the WebUI provides the confirmation step — showing exactly
what will be applied before execution. This is more valuable than a
form wizard because LLM output is less predictable than a constrained
form.

**Properties:**

- SQL is the source of truth. The WebUI generates it, never bypasses it.
- Power users copy the SQL into their migration files.
- The preview is educational — users learn the pg-trickle API by using
  the wizard or reviewing agent-generated SQL.
- Every generated SQL statement is idempotent where possible (`IF NOT
  EXISTS`, `ON CONFLICT DO NOTHING`).
- The `POST /api/v1/sql/preview` endpoint enables agents to validate SQL
  (DVM compatibility, syntax) before proposing it to the user.

### Health Scorecard

Aggregates all `health_check()` results, slot health, change buffer
sizes, relay pipeline lag, and dead letter counts into a single page.
Severity-sorted.

| Check | Status | Detail | Remediation |
|-------|--------|--------|-------------|
| Scheduler | ✅ Running | 12 tables, 3 workers | — |
| CDC triggers | ⚠️ Warning | `customers`: trigger disabled | [Re-enable CDC →] |
| Replication slot | ✅ Active | 2.1 MB retained WAL | — |
| Buffer growth | 🔴 Critical | `orders`: 15,204 rows, growing | [Investigate →] |
| Relay: kafka-fwd | ✅ Connected | 0 lag rows | — |
| Relay: nats-rev | ⚠️ Degraded | 342 lag rows, 12s behind | [View pipeline →] |
| Dead letters | 🔴 Critical | 47 unresolved across 2 pipelines | [View DLQ →] |
| Fuses | ✅ All armed | 0 blown | — |

**Dead letter queues.** When a relay message fails permanently —
payload decode error, schema mismatch, permanent sink rejection — the
message is written to `pgtrickle.relay_dead_letters` instead of being
silently discarded. A non-zero and growing DLQ count is a health check
failure surfaced here and in `breach_events`.

```sql
CREATE TABLE pgtrickle.relay_dead_letters (
    id            BIGSERIAL PRIMARY KEY,
    failed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    pipeline_name TEXT NOT NULL,         -- FK to relay_inbox/outbox_config.name
    direction     TEXT NOT NULL,          -- 'inbox' or 'outbox'
    error_type    TEXT NOT NULL,          -- 'decode', 'sink_permanent', 'inbox_permanent', 'claim_check'
    error_message TEXT NOT NULL,
    raw_payload   JSONB,                  -- immutable; best-effort capture of the failed message
    retry_count   INT NOT NULL DEFAULT 0,
    operator_notes TEXT,                 -- free-text annotation added before retry or discard
    resolved_at   TIMESTAMPTZ,            -- NULL = unresolved
    resolved_by   TEXT                   -- 'manual', 'retry_success', 'discarded'
);
```

**`raw_payload` is immutable.** The relay writes it once on failure and
neither the WebUI nor the API may modify it. It is the forensic record
of exactly what the upstream system sent. Editing it would silently
corrupt the audit trail (e.g. "Kafka offset 4521" would no longer
match the actual bytes received).

**Handling bad data (permanent schema/data mismatch).** Retrying the
original message will never succeed. The correct pattern is:

1. Annotate the DLQ entry with `operator_notes` explaining the root
   cause (e.g. "NULL in non-nullable column `user_id`").
2. **Discard** the DLQ entry.
3. Fix the data manually: insert a corrected row directly into the
   target inbox table using the Tier 2 `POST /api/v1/sql/execute`
   endpoint. The inbox is a plain PostgreSQL table. The execute
   endpoint records the operator, timestamp, and SQL in the audit
   log — making the correction fully traceable.

This pattern keeps `raw_payload` honest, reuses existing infrastructure,
and produces a clear audit trail: DLQ entry (what failed + why) +
execute-log entry (what the operator inserted instead).

The WebUI DLQ view (linked from Health) shows unresolved dead letters
per pipeline with payload inspection, annotation, retry, and discard
actions. A table's inbox dead letter count is also visible in the
Tables list when the table has an associated relay inbox — a non-zero
count is another signal that data is not arriving as expected.

API:
```
GET   /api/v1/dlq                        All unresolved dead letters (paginated)
GET   /api/v1/dlq/:pipeline              Dead letters for one pipeline
PATCH /api/v1/dlq/:id/annotate           Update operator_notes on one entry
POST  /api/v1/dlq/:id/retry              Re-attempt delivery of one dead letter
POST  /api/v1/dlq/:id/discard           Mark as resolved without retry
POST  /api/v1/dlq/discard-all/:pipeline Bulk discard for a pipeline
```

Links in the "Remediation" column navigate to the relevant detail page
or action form.

### Refresh Timeline

Interactive time-series chart of refresh performance across all stream
tables or filtered to one.

**Axes:**
- X: time (selectable range: 1h, 6h, 24h, 7d)
- Y (left): refresh duration (ms) as stacked area per table
- Y (right): row delta size (inserts + deletes) as bar chart

**Overlays:**
- SLA threshold line (horizontal, red dashed)
- Error markers (red dots on the timeline where refreshes failed)
- Scheduler falling-behind periods (shaded background)

**Data source:** `refresh_timeline(max_rows)` with time-range filtering.
Cached server-side for the last 24h; older data from the `pgt_refresh_log`
table.

---

## LLM Agent Integration & MCP Server

### Impact on Feature Tiers

LLM agents (Copilot, Claude, custom tooling) fundamentally change which
WebUI features are urgent and which are deferrable:

| Feature area | Without agents | With agents | Impact |
|-------------|---------------|-------------|--------|
| **Tier 1: Dashboard / monitoring** | Essential | Essential | No change — visual observability is irreducible |
| **Tier 2: Relay pipeline management** | Essential | Still useful | Forms remain valuable for non-technical users; agents use the API directly |
| **Tier 3: Setup wizard** | Essential for adoption | Largely covered by agents | Deprioritized — agents generate SQL faster than any form wizard |
| **API layer (`/api/v1/`)** | WebUI backend | Agent integration surface | Elevated — must be first-class, stable, well-documented |
| **DVM checker** | Wizard feature | Agent-callable endpoint | Elevated as API; deprioritized as form UI |
| **SQL preview** | Wizard output | Human review layer for agent-generated SQL | Reframed — more valuable, not less |

The durable value of the WebUI is **operational visibility** (topology
graph, SLA monitoring, live change flow, operator inspector). These
features have no LLM substitute — you need to *see* the system to
understand it. The setup/configuration features are increasingly handled
by agents calling the API.

### MCP Server

The `/api/v1/` JSON API is a natural **Model Context Protocol (MCP)**
server. MCP is an open standard for LLM tool integration — each API
endpoint maps to a tool that an agent can discover and call.

The relay ships an MCP-compatible tool manifest at
`GET /api/v1/mcp/tools` that describes all available operations:

```json
{
  "tools": [
    {
      "name": "list_stream_tables",
      "description": "List all pg-trickle stream tables with status, staleness, refresh mode, and SLA budget",
      "endpoint": "GET /api/v1/tables",
      "parameters": {}
    },
    {
      "name": "get_topology",
      "description": "Get the full DAG including relay pipeline nodes, with lag and buffer depth per edge",
      "endpoint": "GET /api/v1/dag/topology",
      "parameters": {}
    },
    {
      "name": "check_health",
      "description": "Get health scorecard: scheduler, CDC, slots, buffers, relay pipelines, fuses",
      "endpoint": "GET /api/v1/health",
      "parameters": {}
    },
    {
      "name": "preview_sql",
      "description": "Validate SQL and return DVM compatibility analysis. Returns whether the query supports DIFFERENTIAL refresh and why.",
      "endpoint": "POST /api/v1/sql/preview",
      "parameters": { "sql": "string" }
    },
    {
      "name": "execute_sql",
      "description": "Execute SQL against PostgreSQL. Use preview_sql first to validate.",
      "endpoint": "POST /api/v1/sql/execute",
      "parameters": { "sql": "string" }
    },
    {
      "name": "get_table_detail",
      "description": "Get detailed status for a stream table: refresh stats, staleness, refresh mode explanation, column list",
      "endpoint": "GET /api/v1/tables/:name",
      "parameters": { "name": "string" }
    },
    {
      "name": "get_column_lineage",
      "description": "Trace column-level lineage from output columns back to source columns through the full derivation chain",
      "endpoint": "GET /api/v1/tables/:name/lineage",
      "parameters": { "name": "string" }
    },
    {
      "name": "get_relay_pipelines",
      "description": "List all relay pipelines (forward and reverse) with connection status and lag",
      "endpoint": "GET /api/v1/relay/pipelines",
      "parameters": {}
    },
    {
      "name": "diagnose_table",
      "description": "Get diagnostics for a stream table: why it uses FULL vs DIFFERENTIAL refresh, operator tree, change buffer depth",
      "endpoint": "GET /api/v1/tables/:name/operator-tree",
      "parameters": { "name": "string" }
    },
    {
      "name": "trigger_refresh",
      "description": "Trigger an immediate refresh of a stream table",
      "endpoint": "POST /api/v1/tables/:name/refresh",
      "parameters": { "name": "string" }
    },
    {
      "name": "reset_fuse",
      "description": "Reset a blown circuit breaker (fuse) for a stream table",
      "endpoint": "POST /api/v1/fuses/:name/reset",
      "parameters": { "name": "string" }
    }
  ]
}
```

This manifest enables any MCP-compatible agent (VS Code Copilot,
Claude Desktop, custom agents) to discover and use pg-trickle's full
API without custom integration code. The agent sees the tool list,
understands each tool's purpose from the description, and calls the
HTTP endpoints.

**MCP transport options:**

| Transport | Endpoint | Use case |
|-----------|----------|----------|
| **HTTP+SSE (Streamable HTTP)** | `POST /api/v1/mcp` | Standard MCP transport. Agents connect over HTTP; server pushes events (alerts, status changes) via SSE. Works through proxies and firewalls. |

**Decision: HTTP+SSE only** for Phase 0.5. This is the simplest and
most firewall-friendly transport. WebSocket and stdio transports can
be added later if demand arises.

**Auth for agents:** Agents authenticate the same way as the WebUI
(see [Authentication](#authentication)). In `none` mode, agents call
the API directly. In `pg` mode, agents pass credentials via HTTP
Basic auth or a session token. In `oidc` mode, agents use a service
account token.

### OpenAPI Specification

The `/api/v1/` layer ships with an OpenAPI 3.1 specification at
`GET /api/v1/openapi.json`. This serves three purposes:

1. **LLM agent integration** — agents can read the spec to understand
   all available endpoints, parameter types, and response schemas
   without hardcoded knowledge.
2. **Client generation** — TypeScript types for the Next.js frontend
   are generated from the spec, ensuring type safety between backend
   and frontend.
3. **Documentation** — Swagger UI at `/api/v1/docs` (behind
   `--features webui`) provides interactive API documentation.

The spec is generated from Rust types using `utoipa` (compile-time
OpenAPI generation from Axum handlers). This ensures the spec is
always in sync with the implementation.

---

## Comparison with RisingWave Dashboard

pg-trickle's streaming materialization architecture is closely comparable
to RisingWave. The relay's sources and sinks are the equivalent of
RisingWave's built-in source and sink connectors. Stream tables are the
equivalent of materialized views in RisingWave's streaming engine.

| RisingWave Dashboard Feature | pg-trickle WebUI Equivalent |
|-----------------------------|-----------------------------|
| Materialized Views page | Stream table list + detail |
| Sources page | CDC sources + relay reverse pipelines |
| Sinks page | Relay forward pipelines |
| Fragment graph (backpressure per edge) | Unified topology graph (lag/buffer per edge) |
| Relation graph | DAG view with relay nodes |
| `explain_distsql` (graphviz-wasm) | DVM operator inspector (graphviz-wasm) |
| Await tree | Worker pool + job queue |
| Cluster status | Health scorecard |
| Internal tables | Change buffer inspection |
| Historical backpressure | Refresh timeline + historical lag |

**What pg-trickle adds beyond RisingWave:**

- **SLA budgets** — RisingWave has no concept of freshness SLA because
  it's always-streaming. pg-trickle's scheduled refresh model enables
  explicit SLA thresholds with burn-rate tracking.
- **Cross-cluster relay topology** — The relay can bridge multiple PG
  clusters. The topology graph shows end-to-end flow across clusters
  in a single view — no equivalent in RisingWave's single-system model.
- **Column-level lineage** — derivable from the `OpTree` AST. RisingWave
  does not expose this in the dashboard.
- **SQL preview model** — configuration-by-wizard that generates auditable
  SQL. RisingWave's dashboard is read-only.
- **End-to-end lag through the dependency chain** — cumulative staleness
  from base table through N-level stream table chains. RisingWave shows
  per-fragment backpressure but not pipeline-level cumulative lag.

---

## Sequencing & Dependencies

### Prerequisites

The WebUI is hosted inside `pgtrickle-relay`. The relay must ship first
with:

- `AppState` designed as a proper service state (`Arc<AppState>` with
  PG pool, pipeline registry, metrics handle, shutdown token)
- A single Axum instance shared across health, metrics, webhook
  receiver, and future WebUI routes
- A stubbed `/ui` route returning 200 OK (placeholder for the frontend)
- The JSON API layer (`/api/v1/...`) as the backend for the WebUI

The pg-trickle extension must emit a `pgtrickle_structure_changed`
NOTIFY whenever topology changes: stream table created/dropped,
CDC trigger enabled/disabled, relay pipeline config changed. The relay
listens on this channel alongside `pg_trickle_alert` and uses it to
drive `WsEvent::TopologyChanged` broadcasts. This NOTIFY is cheap
(~2 µs, inside the existing DDL trigger path) and can be added in the
same release as the relay.

### Delivery Phases

| Phase | Content | Depends on |
|-------|---------|------------|
| **Phase 0: API scaffold** | `/api/v1/` routes returning JSON for all read endpoints. OpenAPI spec via `utoipa`. No frontend. Useful immediately for scripting, Grafana, and LLM agents. | Relay core (RELAY-1 through RELAY-4) |
| **Phase 0.5: MCP server** | MCP tool manifest at `/api/v1/mcp/tools`. HTTP+SSE transport at `POST /api/v1/mcp`. Enables LLM agents to discover and use all API endpoints. | Phase 0 |
| **Phase 1: Tier 1 frontend** | Next.js app with: health scorecard, stream table list, topology graph, refresh timeline, alerts, relay pipeline status. Read-only. | Phase 0 |
| **Phase 2: Tier 2 forms** | Relay pipeline create/edit/delete forms with SQL preview. Fuse reset, manual refresh, pause/resume. SQL preview doubles as human review layer for agent-generated SQL. | Phase 1 + relay pipeline config tables |
| **Phase 3: Tier 3 wizard** (deprioritized) | Stream table creation wizard, CDC enablement, Monaco editor. Deferred until user demand — LLM agents cover most setup tasks via the API. Column-level lineage and operator inspector may ship earlier as Tier 1 read-only features. | Phase 2 + OpTree serialization API |

### Relay `AppState` Requirements (Day 1)

The relay scaffold (RELAY-1) must be designed with the WebUI in mind from
the start. Specifically:

```rust
pub struct AppState {
    /// PostgreSQL connection pool (read replicas for API, primary for writes)
    pub pg_pool: PgPool,

    /// Pipeline registry (active forward/reverse pipelines with status)
    pub pipelines: Arc<RwLock<PipelineRegistry>>,

    /// Prometheus metrics handle (shared with relay workers)
    pub metrics: Arc<RelayMetrics>,

    /// Shutdown token (coordinates relay workers + HTTP server)
    pub shutdown: CancellationToken,

    /// WebSocket broadcast channel (NOTIFY alerts, topology changes, state updates)
    /// WsEvent variants:
    ///   StateUpdate { table, staleness_secs, buffer_rows, lag_rows, sla_status }
    ///   TopologyChanged { source: "ddl" | "relay_config" }
    ///   Alert { channel, payload }
    ///   RelayPipelineChanged { pipeline_name, status }
    pub ws_broadcast: broadcast::Sender<WsEvent>,

    /// Configuration (auth mode, log level, feature flags)
    pub config: Arc<RelayConfig>,
}
```

This structure is shared between relay worker tasks and HTTP handlers.
Designing it correctly in RELAY-1 avoids a painful refactor when the
WebUI is added.

### Self-Monitoring via Stream Tables (Dogfooding)

The WebUI uses pg-trickle stream tables to power several of its own
derived metrics. This is the canonical dogfooding use case: the product
monitoring itself, with those monitoring stream tables visible as
first-class nodes in the topology graph.

**Governing rule:** The `/health` endpoint and the live-badge
`WsEvent::StateUpdate` path **always use direct queries** — they must
never depend on stream tables. If the scheduler is stopped or the
extension is degraded, those paths must still work. Stream tables are
only used for **derived aggregate metrics** where a small amount of
staleness is acceptable and the value comes from incremental maintenance
over time.

#### Computation layer assignments

Every derived value must be assigned to exactly one layer. The test:
**if Grafana or an external alerting tool should see it, it belongs in
the database — not in the API server and definitely not in JavaScript.**

| Derived value | Layer | Rationale |
|---|---|---|
| Structural type (inbox / outbox / leaf / intermediate / union / orphan) | **DB view** `pgtrickle.table_classifications` | Trivial SQL joins on `pgt_stream_tables` + relay config tables; Grafana panels showing table-type breakdown need it |
| SLA budget % (`staleness / sla_threshold`) | **DB view** alongside `sla_burn_rate` | Same number used in Grafana progress bars and Prometheus gauge |
| E2E lag (cumulative staleness root→table) | **Stream table** `pgtrickle.end_to_end_lag` | Recursive DAG walk; updated on schedule |
| Root cause / inherited breach classification | **Stream table** `pgtrickle.breach_events` | Grafana and Prometheus alert on `breach_type = 'root'` directly |
| Transport annotations (which schemas have relay inboxes/outboxes) | **API layer** (relay reads `relay_inbox_config`, `relay_outbox_config`) | DB has the data; API assembles it; JS renders only |
| Graph node positions (topology layout) | **Browser** (dagre / elkjs) | Rendering algorithm; meaningless outside the viewport |
| Relative timestamps ("2m ago") | **Browser** | Time-zone-aware display formatting |
| Collapsed / expanded state | **Browser** | UI state only |

`pgtrickle.table_classifications` is a plain SQL view (not a stream
table) because it is cheap to compute on read — it is a join of
`pg_class`, `pgt_stream_tables`, `pgtrickle.dag_edges`,
`relay_inbox_config`, and `relay_outbox_config`, all of which are
already indexed. It does not need incremental maintenance.

#### Candidate stream tables

The relay creates these stream tables during its own initialisation
(via `pgtrickle.relay_setup()`), sourced exclusively from stable,
public-facing catalog views and relay-owned sample tables:

| Stream table | Source | WebUI consumer | Refresh mode |
|---|---|---|---|
| `pgtrickle._staleness_samples` | Relay worker polls `get_staleness()`, inserts rows | N/A — source table | N/A |
| `pgtrickle.sla_burn_rate` | `_staleness_samples` (1 h window) | SLA budget progress bars, burn-rate trend arrows | DIFFERENTIAL (`regr_slope` via aux cols) |
| `pgtrickle.refresh_percentiles` | `pgt_refresh_log` (rolling 1 h / 24 h) | Refresh timeline P50/P95/P99 overlays | DIFFERENTIAL (algebraic aggregates) |
| `pgtrickle.buffer_growth_rate` | `_staleness_samples` (derivative per table) | Buffer depth trend badges | DIFFERENTIAL |
| `pgtrickle.end_to_end_lag` | `pg_stat_stream_tables` + DAG edges | Topology graph edge widths | FULL (recursive CTE — upgraded when recursive IVM ships) |
| `pgtrickle._breach_state` | `_breach_snapshots` (latest snapshot) | Internal — feeds `breach_events` | FULL |
| `pgtrickle.breach_events` | `_breach_state` + DAG ancestry | Root cause / inherited breach classification; Grafana alerts | FULL |

#### Why `breach_events` must be a stream table, not WebUI logic

Root cause detection and blast radius attribution must live in the
**database layer**, not in the WebUI. The reasons are:

1. **Grafana / external alerting.** A Grafana dashboard pointing at
   `pgtrickle.breach_events` gets the same root-cause deduplication
   as the WebUI, for free. An alertmanager rule can fire on
   `WHERE breach_type = 'root'` and suppress `inherited` rows —
   without the WebUI involved at all.
2. **Prometheus / OpenMetrics export.** The relay can expose
   `/metrics` scraped from `breach_events` — one time-series per
   independent root cause, not one per affected table.
3. **API consumers.** `GET /api/v1/tables` simply reads the
   `breach_type` and `root_cause_table` columns from `breach_events`
   — the server does not recompute the DAG walk on every request.
4. **WebSocket events.** `WsEvent::BreachStarted` carries the
   `breach_type` field from the stream table row, not computed inline.

The WebUI is a consumer of `breach_events`, not the owner of the logic.

#### `pgtrickle.breach_events` schema

The breach classification requires two passes — first determine which
tables are in breach, then attribute root cause vs inherited. A single
self-referential query cannot compute `breach_type` while simultaneously
reading it. The design uses two stream tables:

```sql
-- Source table: one row per breach-check tick per table
-- Populated by relay worker alongside _staleness_samples
CREATE TABLE pgtrickle._breach_snapshots (
    snapped_at      timestamptz NOT NULL DEFAULT now(),
    table_name      text        NOT NULL,
    staleness_s     float4      NOT NULL,
    sla_threshold_s float4      NOT NULL,  -- 3× schedule interval by default
    in_breach       boolean     NOT NULL
);

-- Pass 1: current breach state per table (no attribution yet)
CREATE STREAM TABLE pgtrickle._breach_state AS
SELECT
    s.table_name,
    s.staleness_s,
    s.sla_threshold_s,
    s.in_breach
FROM pgtrickle._breach_snapshots s
WHERE s.snapped_at = (SELECT max(snapped_at) FROM pgtrickle._breach_snapshots);
SCHEDULE '15 seconds';

-- Pass 2: root-cause attribution, reading _breach_state (not itself)
CREATE STREAM TABLE pgtrickle.breach_events AS
SELECT
    s.table_name,
    s.staleness_s,
    s.sla_threshold_s,
    s.in_breach,
    CASE
        -- Root cause: in breach AND no upstream stream table is also in breach
        WHEN s.in_breach AND NOT EXISTS (
            SELECT 1
            FROM pgtrickle._breach_state b
            JOIN pgtrickle.dag_edges e
              ON e.upstream_table = b.table_name
             AND e.downstream_table = s.table_name
            WHERE b.in_breach
        ) THEN 'root'
        WHEN s.in_breach THEN 'inherited'
        ELSE NULL
    END AS breach_type,
    -- For inherited breaches: name the root cause table
    (
        SELECT b.table_name
        FROM pgtrickle._breach_state b
        JOIN pgtrickle.dag_ancestors a
          ON a.ancestor = b.table_name
         AND a.descendant = s.table_name
        WHERE b.in_breach
          AND NOT EXISTS (
              SELECT 1
              FROM pgtrickle._breach_state u
              JOIN pgtrickle.dag_edges e2
                ON e2.upstream_table = u.table_name
               AND e2.downstream_table = b.table_name
              WHERE u.in_breach
          )
        ORDER BY a.depth ASC
        LIMIT 1
    ) AS root_cause_table,
    -- Downstream blast radius count
    (
        SELECT count(*)
        FROM pgtrickle._breach_state b
        JOIN pgtrickle.dag_ancestors a
          ON a.ancestor = s.table_name
         AND a.descendant = b.table_name
        WHERE b.in_breach
    ) AS downstream_casualties
FROM pgtrickle._breach_state s;
SCHEDULE '15 seconds';
```

`_breach_state` is a thin materialisation of the latest breach
snapshot — it exists solely to break the circular reference.
`breach_events` reads `_breach_state` (not itself) for the upstream
breach check, so there is no self-referential join. Both are FULL
refresh tables until recursive IVM ships. At 15 s intervals and
typical table counts (< 1000), the full rescan is fast.

**Grafana usage example:**

```promql
# Alert on independent root cause breaches only (no noise from cascades)
SELECT table_name, staleness_s, downstream_casualties
FROM pgtrickle.breach_events
WHERE breach_type = 'root'
```

**Prometheus export:**

The relay's `/metrics` endpoint exposes:
```
pgtrickle_breach_root_total            # count of independent root causes
pgtrickle_breach_inherited_total       # count of downstream casualties
pgtrickle_table_staleness_seconds{table="...",breach_type="root|inherited|none"}
```

#### Example: `sla_burn_rate`

The relay inserts a staleness sample for each stream table every
`pg_trickle.relay_sample_interval` seconds (default 15 s):

```sql
-- Source table: populated by the relay worker, not user-managed
CREATE TABLE pgtrickle._staleness_samples (
    sampled_at  timestamptz NOT NULL DEFAULT now(),
    table_name  text        NOT NULL,
    staleness_s float4      NOT NULL
);

-- Stream table: rolling SLA burn rate, refreshed every 15 s
CREATE STREAM TABLE pgtrickle.sla_burn_rate AS
SELECT
    table_name,
    avg(staleness_s)                                            AS mean_staleness_s,
    max(staleness_s)                                            AS max_staleness_s,
    regr_slope(staleness_s, extract(epoch FROM sampled_at))     AS staleness_trend_s_per_s,
    count(*)                                                    AS sample_count
FROM pgtrickle._staleness_samples
WHERE sampled_at > now() - interval '1 hour'
GROUP BY table_name;
SCHEDULE '15 seconds';
```

`staleness_trend_s_per_s` is the burn rate: a value of `0.5` means the
table is gaining half a second of staleness for every wall-clock second
elapsed. Given a 30 s SLA and current staleness of 20 s, it will breach
in `(30 − 20) / 0.5 = 20 s`. This is exactly the value the SLA budget
progress bar displays as remaining budget.

The aggregate uses `regr_slope()`, which is algebraic and maintained
incremantally by the DVM engine via auxiliary sum/count columns — no
full-table rescan on each 15 s tick.

#### Meta-observability property

Because these stream tables are registered in `pgt_stream_tables`, they
appear in the topology graph as first-class nodes. An SRE investigating
a staleness spike can see at a glance whether `sla_burn_rate` itself is
stale — meaning the burn-rate indicators in the WebUI are running on
outdated data. The monitoring is transparent about its own freshness.
No external monitoring tool provides this self-describing property.

Further: clicking the `sla_burn_rate` node in the topology graph opens
the DVM operator inspector showing the `regr_slope` aggregate strategy,
its auxiliary columns, and the current change buffer depth of
`_staleness_samples`. The observability stack is fully introspectable.

#### Bootstrap safety rule

| Data path | Source | Stream table allowed? |
|---|---|---|
| `/health` endpoint | `health_check()` — direct query | **No** |
| `WsEvent::StateUpdate` live badges | NOTIFY + direct queries | **No** |
| SLA burn rate chart | `pgtrickle.sla_burn_rate` | **Yes** |
| Refresh percentiles overlay | `pgtrickle.refresh_percentiles` | **Yes** |
| Topology edge width (throughput) | `pgtrickle.end_to_end_lag` | **Yes** (with polling fallback to direct query) |
| Buffer depth badge (live) | Direct `change_buffer_sizes()` | **No** |
| Buffer growth trend line | `pgtrickle.buffer_growth_rate` | **Yes** |
| Root cause / blast radius classification | `pgtrickle.breach_events` | **Yes** |
| `WsEvent::BreachStarted` breach type | `pgtrickle.breach_events` | **Yes** |
| Prometheus `/metrics` breach gauges | `pgtrickle.breach_events` | **Yes** |

The invariant: anything that must work when the scheduler is stopped or
the extension is degraded uses direct queries. Derived, time-aggregated,
trend data uses stream tables.

---

## Open Questions

### OQ-1: Naming

The binary currently planned as `pgtrickle-relay` is now the operational
companion process — relay + dashboard + management console. Should it be
renamed?

| Option | Reads as |
|--------|----------|
| `pgtrickle-relay` | "A relay tool that happens to have a UI" |
| `pgtrickle-server` | "The companion server process for pg-trickle" |
| `pgtrickle-ctl` | "The control/admin binary" |

**Current recommendation:** Keep `pgtrickle-relay` for the initial relay
release. Rename to `pgtrickle-server` when the WebUI ships, if the
scope warrants it.

### OQ-2: Separate repo for the frontend

The Next.js frontend could live in `pgtrickle-relay/dashboard/` (monorepo)
or a separate `pgtrickle-dashboard` repo. Monorepo is simpler for CI and
versioning. Separate repo allows independent frontend releases.

**Current recommendation:** Monorepo (`pgtrickle-relay/dashboard/`),
matching RisingWave's approach.

### OQ-3: SLA schema design

~~Should SLA thresholds be:

- (a) A column on `pgtrickle.pgt_stream_tables` (`sla_max_staleness_seconds`)
- (b) A separate `pgtrickle.pgt_sla_config` table
- (c) Inferred only from the schedule interval (no explicit config)~~

**Decision: (c) Inferred only.** SLA thresholds are derived from the
schedule interval: staleness > 2× schedule = warning, > 3× = breach.
No schema changes required. Explicit SLA configuration can be added
later if users need custom thresholds per table.

### OQ-4: Multi-database visibility

~~The relay connects to one PostgreSQL database. Should the WebUI support
connecting to multiple databases simultaneously (e.g., for cross-cluster
topology visualization)?~~

**Decision: Single database only.** The WebUI connects to the same
PostgreSQL database as the relay. Cross-cluster topology visualization
is deferred — it significantly increases scope (connection management,
auth per cluster, merged DAG). Can be revisited when relay-to-relay
bridging is implemented.

### OQ-5: Dark mode

The TUI is terminal-themed by nature. The WebUI should support dark mode
(developer preference). Should it be the default?

### OQ-6: Mobile / responsive

The SRE on-call use case implies phone/tablet access. Should Tier 1 be
responsive, or is desktop-only acceptable initially?

### OQ-7: Embedding in other tools

Should the WebUI be embeddable as an iframe in Grafana dashboards or
internal portals? This affects CSP headers and auth token forwarding.

### OQ-8: API stability

~~The `/api/v1/` routes will be consumed by the WebUI frontend, but also
potentially by scripts and third-party tools. Should the API be treated
as a public contract with semver guarantees from Phase 0?~~

**Decision: Yes.** The API is a first-class integration surface for LLM
agents, automation scripts, and the WebUI frontend. It ships with an
OpenAPI 3.1 spec and follows semver: breaking changes require `/api/v2/`.
This is critical because LLM agents and MCP clients depend on stable
schemas to function correctly.

### OQ-9: TUI deprecation timeline

The TUI (`pgtrickle-tui`) will eventually be superseded by the WebUI.
When should the TUI be deprecated? After Tier 1 feature parity? After
Tier 2?

### OQ-10: MCP protocol version

~~MCP is evolving rapidly. Should the relay target a specific MCP spec
version (e.g., 2025-03-26), or implement a minimal subset (tool
discovery + HTTP+SSE transport) and extend as the spec stabilizes?~~

**Decision: HTTP+SSE only.** Phase 0.5 implements a single MCP
transport: Streamable HTTP (HTTP+SSE). This is the simplest,
most firewall-friendly option and covers remote agents, CI/CD
pipelines, and browser-based tools. stdio transport (for local agents)
and raw WebSocket can be added later if needed.

### OQ-12: Large-deployment UI scalability — schema-based semantic zoom

~~pg-trickle can scale to 1000+ stream tables. The WebUI's topology
graph and list views must handle this, but the right approach is an
open design question.~~

**Decision: Multi-level semantic zoom using schemas only.** The topology
graph groups all objects — including inbox and outbox tables — by
PostgreSQL schema. No new metadata, no JSONB parsing, no naming
conventions required.

**Single grouping axis: PostgreSQL schemas.** Every PG object (stream
table, base table, inbox table, outbox table) has exactly one schema
via `pg_class.relnamespace`. That schema IS the group. External
transports (Kafka, NATS, webhooks, etc.) are not graph nodes — they
are annotations on the schema nodes that use them.

**Transport annotations.** A schema with an active relay inbox shows
a small inbound transport badge on its left side; a schema with a
relay outbox shows an outbound badge on its right side. The badge
shows the backend type icon (Kafka, NATS, webhook, S3, etc.) and
colour-codes connected/disconnected. This answers "how does data flow
in and out?" without adding infrastructure noise to the graph.
A single Kafka cluster feeding five schemas is implicit — each schema
just shows `⟵ kafka` on its inbound side.

```
┌─────────────────┐     ┌───────────────┐     ┌───────────────┐
│    erp_raw      │────▶│   canonical   │────▶│   analytics   │
│    5 tables     │     │   45 tables   │     │   8 tables    │
│ ⟵kafka ⟵webhook │     │  ●43 🟡1 🔴1  │     │     ●8        │ kafka⟶
└─────────────────┘     └───────────────┘     └───────────────┘
       ↑↓ webhook  (bidirectional sync)
```

**Layout is fully automatic.** The graph layout algorithm (dagre /
elkjs) positions schema nodes based on inter-schema edge direction
alone — no manual tier assignment and no configuration required.

**Level 1 — Schema detail.** Click a schema group → shows all individual
nodes within that schema, with edges to adjacent schemas and to the
external relay endpoints. Supports **partial pipelines**: clicking the
edge between `erp_raw` and `canonical` shows only the flows between
those two schemas.

**Level 2 — Individual flow.** Click a specific flow → full pipeline
detail (every node with staleness, buffer depth, SLA). Same as the
pipeline detail view.

**Single-schema auto-navigation.** If only one schema exists, the
topology opens directly at Level 1. The breadcrumb always renders
`Systems > <schema>` so the user can click back to Level 0 and
discover that schemas are the grouping primitive.

**Why schemas only, not a custom grouping table.** Schemas are
PostgreSQL's native organizational primitive. They align with dbt's
`schema` config, data vault patterns (raw/business vault schemas),
medallion architecture (bronze/silver/gold), and standard access
control (`GRANT USAGE ON SCHEMA`). Inbox and outbox tables already
live in meaningful schemas by convention — no additional metadata
needed to group them.

**API support:**

```
GET /api/v1/dag/topology?level=0             Systems overview
GET /api/v1/dag/topology?level=1&schema=erp_raw  Schema drill-in
GET /api/v1/dag/topology?level=1&from=erp_raw&to=canonical
                                              Partial pipeline view
GET /api/v1/dag/topology?focus=revenue_7d&depth=2
                                              Per-object neighbourhood
```

### OQ-11: Agent guardrails

~~Should the API enforce guardrails for agent-initiated writes? For
example: require a `X-Agent-Name` header on `POST /api/v1/sql/execute`
so the audit log records which agent made each change, or require
`preview_sql` to be called before `execute_sql` for non-human callers.~~

**Decision: Both guardrails.**

1. **`X-Agent-Name` header** — optional but logged. If present on any
   write endpoint, the agent name is recorded in the audit log
   alongside the SQL and result. Helps operators trace "who changed
   this stream table?" back to a specific agent.
2. **`preview_sql` before `execute_sql`** — the `POST /api/v1/sql/execute`
   endpoint accepts an optional `preview_token` returned by
   `POST /api/v1/sql/preview`. When provided, execute validates that
   the SQL matches the previewed statement. This is not enforced
   (agents can skip it), but the audit log records whether a preview
   was performed. This encourages the safe pattern (validate → review
   → apply) without blocking automation that needs to skip it.

### OQ-13: Pagination strategy

Multiple API endpoints return "paginated" data but no pagination
contract is defined. Questions:

- Cursor-based (opaque token) or offset-based (`?page=N&limit=M`)?
  Cursor-based is more robust for live data (no skips/duplicates when
  rows arrive between pages), but offset-based is simpler for UIs.
- Default page size? Max limit? (Suggested: 50 default, 200 max.)
- Response format: `{ data: [...], next_cursor: "..." }` or
  RFC 8288 `Link` headers?

### OQ-14: API rate limiting and agent throttling

The API is a public contract for LLM agents. A runaway agent loop
calling `POST /api/v1/sql/execute` repeatedly could be destructive.
Questions:

- Per-IP or per-session rate limiting on write endpoints?
- Per-agent quotas (via `X-Agent-Name`)?
- Circuit-breaker on the API itself (e.g. 429 after N writes/minute)?
- Read endpoints: rate-limited or unlimited?

### OQ-15: Audit log schema

Multiple features reference "the audit log" (DLQ bad-data remediation,
agent guardrails, SQL execute) but no schema exists. Need to define:

- Table schema (`pgtrickle.audit_log`?)
- Columns: timestamp, operator/agent, action type, SQL, result, source IP
- Retention policy (rolling window? Partition by month? Manual cleanup?)
- Whether the audit log is a stream table itself (dogfooding)

### OQ-16: `table_classifications` relay dependency

The `table_classifications` view joins `relay_inbox_config` and
`relay_outbox_config`. If the extension is installed without the relay
(valid — the relay is optional), the view breaks. Options:

- (a) `LEFT JOIN` with `NULL` transport columns — view always works
- (b) View is only created by `pgtrickle.relay_setup()`, not by the
  extension itself
- (c) `IF EXISTS` conditional join (not standard SQL)

### OQ-17: WebSocket reconnection and event replay

The plan describes WS events and a 5s polling fallback but no
client-side reconnection strategy. Questions:

- Exponential backoff on disconnect? (Suggested: 1s → 2s → 4s → 30s cap)
- Event sequence numbers for replay of missed events during disconnect?
- Stale badge detection: how does the frontend know its live data is
  outdated after a long disconnect? (Suggested: server sends a
  `WsEvent::Heartbeat` every 5s; if missed for 15s, show "stale" banner.)

### OQ-18: HTTP caching strategy

The API serves the frontend (high-frequency), Grafana (periodic scrape),
and scripts (ad hoc). No caching is documented. Questions:

- `Cache-Control` headers for read endpoints? (Suggested: `max-age=5`
  for topology, `no-cache` for alerts and health.)
- `ETag` / `If-None-Match` for large payloads (topology, table list)?
- Server-side response caching for `/api/v1/dag/topology`?

### OQ-19: MCP manifest derivation

The plan defines both a hand-written MCP tool manifest and an
auto-generated OpenAPI spec (via `utoipa`). These describe the same
endpoints and can drift. Should the MCP manifest be generated from the
OpenAPI spec rather than maintained separately?

### OQ-20: Frontend version negotiation

The frontend is embedded via `rust-embed`. After a relay upgrade,
browser tabs with the old frontend cached may call new/changed API
endpoints. Questions:

- Force-reload mechanism? (Suggested: relay serves
  `X-App-Version: <hash>` header; frontend compares on each API
  response; mismatch triggers a reload prompt.)
- `ETag`-based cache busting for static assets?
- Graceful degradation for minor API additions?

### OQ-21: SLA inference for slow-refresh tables

The "2× schedule = warning, 3× = breach" formula does not account for
refresh duration. A table with a 60s schedule and a 50s P95 refresh
duration reaches ~110s staleness even when perfectly healthy (refresh
starts, takes 50s, next refresh not due for 60s). This table would be
perpetually in warning. Should the formula be:

```
effective_threshold = multiplier × (schedule_interval + p95_refresh_duration)
```

### OQ-22: CORS and Content Security Policy

The API may be called cross-origin (Grafana annotation queries,
embedded iframes — see OQ-7). Questions:

- CORS policy: allow `*` for read endpoints? Restrict writes to
  same-origin?
- CSP headers for the frontend: ECharts, graphviz-wasm, and Monaco
  all require `script-src` relaxation. Document the minimum CSP.
- Security headers: `X-Frame-Options`, `X-Content-Type-Options`,
  `Strict-Transport-Security`.

### OQ-23: DLQ retention policy

`relay_dead_letters` stores forensic payload data indefinitely
(`resolved_at` is set but rows are never deleted). For high-throughput
systems with persistent schema mismatches, this table grows without
bound. Questions:

- Auto-cleanup of resolved entries after N days? (Suggested: 30 days.)
- Archival to cold storage before deletion?
- Partition by month for efficient cleanup?
- Size-based cap (e.g. max 100K unresolved entries, oldest auto-discarded)?

### OQ-24: Internationalisation

English only for the initial release. Is there a future need for i18n?
If not, document the decision explicitly.

### OQ-25: Bundle size budget

ECharts (~400 KB), graphviz-wasm (~1.5 MB), Monaco (~2 MB) — the
embedded binary carries all of these. Questions:

- Total budget for `rust-embed` static assets? (Suggested: < 5 MB
  gzipped for Tier 1.)
- CI gate: fail build if bundle exceeds budget?
- Lazy-load everything except the core shell + topology page?
