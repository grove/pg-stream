---
name: enrich-release-roadmap
description: "Enrich a pg_trickle release roadmap with prioritised items across six quality pillars: correctness, stability, performance, scalability, ease-of-use, and test coverage. Use when planning a new release, fleshing out a milestone, or reviewing what gaps exist before tagging a version."
argument-hint: "Target release version (e.g. 0.18.0)"
---

# Enrich Release Roadmap

Proposes a rich, prioritised set of roadmap items for a target pg_trickle
release, grouped under six quality pillars, ready to paste into `ROADMAP.md`.

## When to Use

- Planning a new milestone / release
- Fleshing out a skeletal roadmap section
- Pre-release gap review (correctness, stability, performance, scalability, ease-of-use, coverage)

## Inputs to Gather First

Before generating proposals, read the following files so you have current context:

1. `ROADMAP.md` — existing entry for the target release (theme, listed items)
2. `plans/PLAN_0_<version>.md` — implementation order and open items (if it exists)
3. `CHANGELOG.md` — last 2-3 releases to avoid duplicating shipped work
4. `AGENTS.md` — coding conventions and constraints that all items must respect

## Procedure

### Step 1 — Collect Context

Read the four files above in parallel. Identify:
- Items already listed for the target release
- Items recently shipped (do not re-propose)
- Known deferred items (explicitly call these out if proposing them again)

### Step 2 — Propose Items per Pillar

For each of the six pillars below, propose 3-6 items. For every item provide:

| Field | Format |
|-------|--------|
| **ID** | Pillar prefix + number, e.g. `CORR-1`, `STAB-2`, `PERF-3`, `SCAL-4`, `UX-5`, `TEST-6` |
| **Title** | ≤ 10-word action phrase |
| **Effort** | `XS` / `S` / `M` / `L` / `XL` |
| **Priority** | `P0` (must-have) · `P1` (high) · `P2` (nice-to-have) |
| **Description** | 2-4 sentences covering what, why, and how to verify |
| **Dependencies** | Other item IDs or prior-release features it builds on |
| **Schema change?** | Yes / No (SQL migration required) |

---

### Pillar 1 — CORRECTNESS

Guiding question: *"Could a user silently get the wrong answer from their stream table?"*

Focus areas:
- Differential refresh producing wrong results
- Delta weight / `__pgt_count` accounting errors
- NULL edge cases in PK joins and aggregate algebraic invariants (SUM/COUNT sign, Welford aux columns)
- PK collision under concurrent DML
- GROUP BY / DISTINCT / HAVING correctness under deletes
- CDC ordering guarantees (TRUNCATE + INSERT, UPDATE old/new)
- Recursive CTEs and LATERAL subquery scoping
- Any issues surfaced by SQLancer fuzzing

---

### Pillar 2 — STABILITY

Guiding question: *"Could this crash, corrupt, or leave the system in a broken state?"*

Focus areas:
- `unwrap()` / `panic!()` elimination in SQL-reachable paths
- Parallel worker crash recovery (orphaned transactions, leftover change buffer rows, advisory locks)
- Background worker `SIGTERM` handling
- WAL decoder slot lag and slot invalidation
- SPI error propagation and SQLSTATE classification
- Extension upgrade migration safety (catalog drift, column additions)
- Error message quality and actionability

---

### Pillar 3 — PERFORMANCE

Guiding question: *"Are we leaving measurable performance on the table?"*

Focus areas:
- Differential refresh throughput and latency (P50/P99)
- Cost-model accuracy for FULL vs. DIFFERENTIAL strategy selection
- Delta branch pruning for zero-change sources
- Index-aware MERGE planning (covering index, seqscan disable)
- `changed_cols` bitmask short-circuiting for unchanged projections
- Change buffer compaction efficiency
- Scheduling overhead under high table counts
- Benchmark regression prevention (Criterion baseline gate)

---

### Pillar 4 — SCALABILITY

Guiding question: *"Does this hold up at 10× the current scale?"*

Focus areas:
- High-table-count DAG rebuild time
- Parallel refresh worker utilisation
- Partitioned stream table throughput
- Multi-tenant scheduling fairness
- Change buffer growth under write-heavy workloads
- Memory usage under large delta sets
- CNPG / cloud-native operational patterns

---

### Pillar 5 — EASE OF USE

Guiding question: *"Can a new user be productive in under 30 minutes?"*

Focus areas:
- SQL API ergonomics (function naming, defaults, return types)
- Error message actionability (include table name, query fragment, remediation hint)
- Documentation gaps (GETTING_STARTED, TROUBLESHOOTING, CONFIGURATION)
- Migration guides (pg_ivm, native materialized views)
- Runbook coverage for each known failure mode
- Playground / quickstart Docker experience
- TUI quality and completeness
- dbt macro coverage and documentation
- Observability (Prometheus metrics, Grafana dashboards)
- pgxn / GHCR packaging and install verification

---

### Pillar 6 — TEST COVERAGE

Guiding question: *"What scenario could regress silently because no test covers it?"*

Focus areas:
- Logic currently tested only via E2E that could be unit-tested
- Property-based / SQLancer fuzzing gaps
- TPC-H query coverage (22 queries, all SF levels)
- Upgrade migration tests (column drift, constraint additions)
- Benchmark regression gates (Criterion baseline on every PR)
- CDC edge cases: NULL PKs, composite PKs, generated columns, domain types, arrays
- dbt integration test breadth
- Light E2E eligibility (move tests off full E2E image where possible)

---

### Step 3 — Write the Release Theme

Write a single paragraph (≤ 5 sentences) summarising the spirit of the
release: what user-visible problem it solves, what internal foundations it
lays, and what the headline capability is.

### Step 4 — Flag Conflicts & Risks

In a "Conflicts & Risks" subsection, call out:
- Items that contradict each other
- Items that depend on features not yet shipped
- Items that require a SQL migration (schema freeze risk)
- Items that touch the DVM engine core (high regression risk — require property tests)

---

## Output Format

Return a Markdown block ready to paste directly into `ROADMAP.md`, following
the same style as existing release sections:

```markdown
## v<X.Y.Z> — <Theme Title>

> **Release Theme**
> <one-paragraph theme>

### Correctness
| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| CORR-1 | ... | S | P0 |
...

<brief description per item>

### Stability
...

### Performance
...

### Scalability
...

### Ease of Use
...

### Test Coverage
...

### Conflicts & Risks
...
```

---

## Constraints (Always Respect)

These come from `AGENTS.md` and must not be violated by any proposed item:

- Differential refresh is the primary mode; full refresh is a last resort
- No data loss under any failure scenario — correctness and durability over performance
- No `unwrap()` / `panic!()` in SQL-reachable Rust code
- All `unsafe` blocks require `// SAFETY:` comments
- New SQL-facing functions use `#[pg_extern(schema = "pgtrickle")]`
- `name`-typed SPI columns must be cast to `text`
- Tests use Testcontainers — never a local PG instance
- Background workers must handle `SIGTERM` and respect `pg_trickle.enabled` GUC
- Keep SPI connections short-lived — no long operations while holding a connection
