# PLAN_XATA — pg_trickle on Xata

**Status:** Research / Planning
**Created:** 2026-04-23
**Priority:** Low–Medium (depends on Xata's extension policy)
**Depends on:** Xata Postgres platform, pg_trickle Docker images, future
allow-listing of `pg_trickle` by Xata

---

## 1  Executive Summary

[Xata](https://xata.io) pivoted in 2024 from a serverless table-API product to
a **dedicated-Postgres platform** built on top of vanilla PostgreSQL. Each
project is provisioned with its own PG instance (rather than the previous
multi-tenant architecture), with managed branching, schema migrations
(via [pgroll](https://github.com/xataio/pgroll)), data anonymization, and an
open-source operational AI assistant ([Xata Agent](https://github.com/xataio/agent)).

**Can pg_trickle run on Xata today?** **Not without Xata's cooperation.**
Like every managed Postgres provider, Xata maintains an allow-list of
extensions; `pg_trickle` is not (currently) on it. Self-hosting is also not
a real option because Xata's compute/storage layer is not a deployable
runtime in the way Neon's is — Xata is fundamentally a SaaS control plane.

**Two realistic paths exist:**

1. **Xata-side allow-listing** — Xata loads `pg_trickle.so` into the base
   image, adds it to `shared_preload_libraries`, and exposes the GUCs via
   their console. Most pg_trickle features then "just work" because each
   project has a dedicated PG instance.
2. **Hybrid: Xata as primary, pg_trickle on a downstream replica** — users
   keep their OLTP data in Xata and stream changes via logical replication
   into a separate, self-managed PG running pg_trickle. This is the only
   path available to users *today* without any Xata changes.

This plan documents the technical requirements, blocking issues, and a
proposed integration roadmap.

---

## 2  Xata Architecture (Relevant to pg_trickle)

### 2.1  Platform Components

| Component | Role | Open source? |
|---|---|---|
| **Xata Postgres** | Per-project dedicated PG instance (PG 15/16/17, currently no PG 18). Fully-fledged Postgres. | No — managed |
| **Branching layer** | Copy-on-write storage forks (uses block-level snapshots from the underlying cloud volume). Branches are full PG instances. | No |
| **pgroll** | Zero-downtime schema migrations using expand/contract + dual-write views. | Yes (Apache 2.0) |
| **pgstream** | Logical-replication-based CDC tool that streams changes to downstream sinks (Kafka, OpenSearch, webhooks, another PG). | Yes (Apache 2.0) |
| **Xata Agent** | Operational AI assistant that observes PG metrics/logs and recommends actions. Connects via standard `libpq`. | Yes (Apache 2.0) |
| **Console / API** | Web UI + REST API for project, branch, role, and extension management. | No |

### 2.2  Implications for pg_trickle

| Capability needed by pg_trickle | Available on Xata? | Notes |
|---|---|---|
| Custom `.so` in the PG installation | **No (today)** | Requires Xata to bake `pg_trickle.so` into the base image. |
| `shared_preload_libraries = '...,pg_trickle'` | **No (today)** | Allow-listed GUCs only; needs platform support. |
| `CREATE EXTENSION pg_trickle` | **No (today)** | Extensions are allow-listed. |
| Background workers (BGW) | **Yes** (potentially) | Each project is its own PG instance, so BGW slots are user-controllable once the extension is allowed. |
| Shared memory (`PgLwLock`, `PgAtomic`) | **Yes** | Standard PG primitives — work in any dedicated instance. |
| Row-level AFTER triggers (CDC) | **Yes** | Triggers are unrestricted PG features. |
| `CREATE EVENT TRIGGER` (DDL hooks) | **Likely yes** | Standard PG, but some managed providers restrict it; needs confirmation. |
| Logical replication slots | **Yes** | Xata uses them internally for `pgstream`; users can create their own. |
| PostgreSQL **18** | **No (as of 2026-04)** | Xata currently tracks PG 15–17. pg_trickle targets PG 18. |
| Custom `postgresql.conf` settings | **Limited** | Restricted to a curated set; new GUCs (`pg_trickle.*`) need allow-listing. |
| Superuser / `pg_read_server_files` | **No** | Standard managed-PG restriction. pg_trickle does **not** need superuser at runtime, but `CREATE EXTENSION` does in vanilla PG (Xata wraps this). |

---

## 3  Blocking Issues

### 3.1  Extension Allow-Listing (Hard Blocker)

Xata, like every managed Postgres vendor, only loads vetted extensions.
The current public list (per the Xata console / docs) includes mainstream
extensions such as `pgvector`, `pg_stat_statements`, `pgcrypto`, `pgaudit`,
`postgis`, etc. **`pg_trickle` is not on the list.**

**What Xata would need to do:**

1. Bundle `pg_trickle.so` and SQL scripts into the Xata Postgres base image
   for each supported PG major.
2. Add `pg_trickle` to the platform's extension allow-list and surface it
   in the console (`Settings → Extensions`).
3. Allow the user-facing GUCs:
   - `pg_trickle.enabled`
   - `pg_trickle.max_workers`
   - `pg_trickle.scheduler_interval_ms`
   - `pg_trickle.refresh_timeout_ms`
   - `pg_trickle.cdc_buffer_max_rows`
   - …and the rest in [docs/CONFIGURATION.md](../../docs/CONFIGURATION.md).
4. Add `pg_trickle` to `shared_preload_libraries` in the base config (it
   needs to load before user sessions start because of shmem and BGW
   registration).
5. Bump pg_trickle's `.so` whenever the platform applies a minor PG
   upgrade (ABI compatibility within a major is fine).

### 3.2  PostgreSQL 18 Lag

pg_trickle is built against PG 18. As of 2026-04, Xata's newest supported
major is PG 17. Either:

- Xata adds PG 18 support (their roadmap typically tracks the upstream
  release within ~6 months), **or**
- pg_trickle adds PG 17 build targets in `Cargo.toml` / `pg_trickle.control`
  as part of the Xata onboarding work.

Backporting to PG 17 is feasible — the codebase is already pgrx-based and
most of the API surface is identical. The main risk is anything that
relies on PG 18-only catalog columns or planner hooks. A scoping pass
should grep for `#[cfg(feature = "pg18")]`-style guards and
PG 18-specific SPI queries.

### 3.3  Branching Semantics

Xata branches are copy-on-write at the block layer, so they fork the
**entire** PG cluster atomically. This is the same model as Neon and is
benign for pg_trickle:

- Stream tables, `pgtrickle.pgt_stream_tables` catalog entries, change
  buffers in `pgtrickle_changes.*`, and the BGW scheduler state on disk
  are all forked consistently.
- After a fork, each branch's BGW launcher independently re-discovers
  catalog state and resumes scheduling.
- CDC triggers fire on each branch independently — no cross-branch
  contamination.

**One caveat:** Xata's "data anonymization" feature for branches rewrites
column values on read (or on branch creation, depending on mode). If
anonymization runs **after** stream tables are populated, the materialized
state will contain real values while the source tables show anonymized
values — a correctness footgun. Documentation must call this out, and
ideally pg_trickle could expose a "mark all stream tables stale" SPI
function that Xata's anonymizer hooks into.

### 3.4  pgroll Interaction (Schema Migrations)

pgroll performs zero-downtime DDL by:

1. Adding new columns / tables in an "expand" phase.
2. Creating a versioned **view** that maps the new schema to the old
   (and vice versa).
3. Backfilling via triggers.
4. Dropping old columns in a "contract" phase.

This interacts with pg_trickle in two ways:

| Interaction | Risk | Mitigation |
|---|---|---|
| Stream table source becomes a pgroll view during a migration | Stream tables defined on the *underlying* table keep working; stream tables defined on the *versioned view* may break if the view definition changes mid-migration. | Document: "Always declare stream tables against base tables, not pgroll-versioned views." |
| pgroll's backfill triggers fire alongside pg_trickle's CDC trigger | Both fire AFTER the user statement — order matters for trigger naming (`pg_trickle_cdc_*` is alphabetical). | Confirm trigger ordering doesn't double-count; add an integration test using pgroll. |
| pgroll's `ALTER TABLE` events are observed by pg_trickle's DDL event trigger | The hook may attempt to invalidate plans for stream tables built on the changing column. | This is desirable behaviour, but the invalidation logic must tolerate the "shadow column" pattern (`_pgroll_new_*`). |

### 3.5  pgstream vs. pg_trickle CDC

Xata's [pgstream](https://github.com/xataio/pgstream) and pg_trickle both
do CDC, but for **different purposes**:

- **pgstream**: external sink (Kafka, search index, another DB). Uses
  logical replication slots.
- **pg_trickle**: in-database materialization. Uses row-level triggers
  (per ADR-001) for atomicity with the source transaction.

They can coexist on the same source table with no interference (different
mechanisms, different downstream targets). The plan should document this
explicitly so users understand when to choose which — see also
[gap analyses in plans/ecosystem/](.).

---

## 4  Deployment Paths

### 4.1  Path A — Hybrid (Available Today, Zero Xata Changes)

```
┌─────────────┐  logical replication   ┌──────────────────────────┐
│  Xata PG    │ ─────────────────────► │  Self-hosted PG 18       │
│  (primary)  │   (subscription via    │  + pg_trickle            │
│             │    pgstream or native  │  (stream tables here)    │
│             │    PG subscriber)      │                          │
└─────────────┘                        └──────────────────────────┘
       ▲                                           │
       │ writes                                    │ reads
       │                                           ▼
   App tier ◄───────────────────────────── Analytical queries
```

**Pros:**

- Works **today**, no platform changes needed.
- Xata stays focused on OLTP; pg_trickle handles incremental
  materialization on a sized-for-IVM replica.
- Branch-friendly: one IVM replica per long-lived branch if needed.

**Cons:**

- Two systems to operate (Xata + a self-hosted PG instance).
- Logical-replication lag becomes the IVM freshness floor (typically
  sub-second on a quiet system, seconds under load).
- IMMEDIATE (transactional) IVM is **not** available — by definition
  the materialization is on a different DB. Only DEFERRED/SCHEDULED
  modes apply.

**User documentation deliverable:** a tutorial under
`docs/integrations/xata-hybrid.md` walking through:

1. Enable logical replication in the Xata project (`wal_level = logical`
   is already set on Xata; users just create a publication).
2. Provision a self-hosted PG 18 + pg_trickle (Docker image or CNPG).
3. Create a `SUBSCRIPTION` from the replica to Xata.
4. Define stream tables on the replica's mirrored tables.
5. Point analytics dashboards at the replica.

### 4.2  Path B — Native (Requires Xata Cooperation)

Once Xata allow-lists `pg_trickle`:

```bash
# In the Xata console:
# 1. Settings → Extensions → enable "pg_trickle"
# 2. Settings → Postgres parameters → confirm pg_trickle.* GUCs are visible

# Then in any branch:
psql "$XATA_PG_URL" <<SQL
CREATE EXTENSION pg_trickle;

SELECT pgtrickle.create_stream_table(
  'analytics.daily_orders',
  $$SELECT date_trunc('day', created_at) AS day,
           sum(amount) AS revenue
    FROM orders
    GROUP BY 1$$,
  refresh_mode => 'differential'
);
SQL
```

**Pros:**

- Single system, single transaction boundary.
- IMMEDIATE mode available for sub-second freshness.
- Branches automatically carry stream tables.

**Cons:**

- Requires Xata engineering work (allow-list, base image, GUC plumbing).
- Requires PG 17 backport **or** Xata adopting PG 18 first.

### 4.3  Path C — Xata Agent Skill (Complementary)

Independent of Path A or B: write a [Xata Agent](https://github.com/xataio/agent)
**playbook / skill** that teaches the agent about pg_trickle's catalog
and operational metrics. The agent can then:

- Detect stream tables that haven't refreshed within their freshness SLO
  and surface them in its diagnostics view.
- Recommend `pg_trickle.max_workers` / `scheduler_interval_ms` tuning
  based on observed refresh lag and CDC buffer growth.
- Spot stream tables stuck in FULL mode and suggest the
  `pgtrickle.explain_diff()` output for a path to differential.

This is pure agent configuration — no Xata platform changes needed — and
adds value even under Path A. Deliverable: a YAML playbook in
`integrations/xata-agent/` plus a contribution PR upstream.

---

## 5  Engineering Work Breakdown

Estimated complexity tags: **S** (≤1 week), **M** (1–4 weeks), **L** (>1 month).

### 5.1  Path A (Hybrid) — pg_trickle side

| Task | Size | Owner |
|---|---|---|
| Tutorial: Xata → self-hosted IVM replica | S | Docs |
| Integration test: subscription-driven CDC end-to-end | S | Test infra |
| Confirm row-level triggers fire on logical-replication apply by default (`session_replication_role`) | S | Engine |
| Document pgroll-on-source coexistence patterns | S | Docs |

### 5.2  Path B (Native) — pg_trickle side

| Task | Size | Owner |
|---|---|---|
| Backport build to PG 17 (pgrx feature, control file, CI matrix) | M | Build |
| Audit PG 18-only SPI queries / catalog columns | M | Engine |
| Verify all GUCs are flagged `PGC_USERSET` or `PGC_SUSET` correctly so Xata can expose them | S | Config |
| Provide a Xata-ready Docker base layer (`Dockerfile.xata-overlay`) for their image team | S | Build |
| Ensure `pg_trickle` declares no `superuser`-only operations beyond `CREATE EXTENSION` | S | Engine |
| Anonymization-aware "mark stale" SPI function | S | Engine |
| Compatibility test against Xata staging (when available) | M | Test infra |

### 5.3  Path B — Xata side (advocacy only, not our work)

| Task | Owner |
|---|---|
| Bake `pg_trickle.so` into compute image | Xata |
| Add to `shared_preload_libraries` and extension allow-list | Xata |
| Surface `pg_trickle.*` GUCs in console | Xata |
| Add to extension catalog UI | Xata |
| QA against branching, anonymization, pgroll | Xata |

### 5.4  Path C (Agent Skill) — pg_trickle side

| Task | Size | Owner |
|---|---|---|
| Author Xata Agent playbook (YAML) | S | DevRel |
| Define metric queries (lag, buffer growth, FULL-mode tables) reusing those in [docs/PERFORMANCE_COOKBOOK.md](../../docs/PERFORMANCE_COOKBOOK.md) | S | DevRel |
| Open upstream PR to `xataio/agent` for inclusion | S | DevRel |

---

## 6  Open Questions

1. **PG version cadence:** When does Xata plan to support PG 18? If
   imminent, Path B's backport work becomes unnecessary.
2. **Allow-list governance:** What is Xata's process for evaluating new
   extensions? Is there a public RFC / contact path? (Likely:
   open an issue on `xataio/xata` or contact `support@xata.io`.)
3. **Anonymization timing:** Does Xata's anonymizer run pre-commit (so
   triggers see anonymized rows) or post-fact (so stream tables capture
   real values)? The answer determines the severity of §3.3's
   correctness footgun.
4. **DDL event trigger restrictions:** Does Xata restrict
   `CREATE EVENT TRIGGER`? pg_trickle relies on this for the DDL hook in
   [src/hooks.rs](../../src/hooks.rs).
5. **Long-lived background workers:** Are there per-project BGW slot
   caps on Xata's plans? `max_workers_per_database` defaults are
   typically fine, but a concrete number would help size guidance.
6. **Branch garbage collection:** When a branch is deleted, are the
   pg_trickle change buffers (which can be sizeable) freed
   instantly, or do they linger in the underlying snapshot? Affects
   storage billing for users with many branches.

---

## 7  Recommendation

**Short term (Q3–Q4 2026):** Pursue **Path A + Path C**. Both are
unblocked, low-effort, and immediately useful to existing Xata users
who want IVM. Publish the hybrid tutorial and the agent playbook.

**Medium term (2027):** Open a conversation with Xata about Path B once
either (a) Xata adds PG 18 support, or (b) we ship a maintained PG 17
build target. Use telemetry from Path A users to demonstrate demand.

**Do not:** invest in Path B engineering ahead of a credible
allow-listing commitment from Xata. The platform-side work dwarfs ours,
and without it the extension cannot be loaded regardless of how well
it builds.

---

## 8  References

- [Xata Postgres docs](https://xata.io/docs)
- [pgroll](https://github.com/xataio/pgroll) — Apache 2.0
- [pgstream](https://github.com/xataio/pgstream) — Apache 2.0
- [Xata Agent](https://github.com/xataio/agent) — Apache 2.0
- [PLAN_NEON.md](PLAN_NEON.md) — comparable managed-PG integration analysis
- [PLAN_CLOUDNATIVEPG.md](PLAN_CLOUDNATIVEPG.md) — self-hosted operator reference
- [docs/CONFIGURATION.md](../../docs/CONFIGURATION.md) — full GUC list
- ADR-001 / ADR-002 in [plans/adrs/PLAN_ADRS.md](../adrs/PLAN_ADRS.md) — rationale
  for trigger-based CDC (relevant to §3.5 pgstream comparison)
