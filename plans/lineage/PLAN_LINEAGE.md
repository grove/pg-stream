# Property Lineage & Data Lineage Standards — Research Report and Implementation Plan

**Status:** Proposed
**Created:** 2026-04-30
**Authors:** Research document for pg_trickle maintainers

---

## 0. Scope and Terminology

This document uses *property lineage* broadly to mean the full spectrum of
metadata that can be tracked as data flows through a transformation pipeline:

| Term | Meaning |
|------|---------|
| **Column lineage** | Which output columns derive from which input columns (already in F12, v0.36.0) |
| **Property lineage** | Properties *about* a column — type, nullability, constraints, sensitivity labels — traced across transformations |
| **Statistical lineage** | Cardinality, null-rates, data distributions, and quality metrics traced through the DAG |
| **Operational lineage** | Runtime metadata: when data was refreshed, from which WAL LSN, latency, and row-delta volume |
| **Semantic lineage** | Business meaning, PII/PHI labels, ownership, glossary terms, and ontology anchors |

pg_trickle's operator tree (OpTree / DVM engine) already knows the exact
derivation graph at query-parse time. No other IVM system has this advantage.
The goal of this plan is to *expose* and *broadcast* that knowledge in
standard, interoperable formats.

---

## 1. What Exists Today (Baseline)

### 1.1 Column Lineage (F12 — v0.36.0)

`pgtrickle.stream_table_lineage(name)` returns:

```
output_col | source_table | source_col
-----------+--------------+-----------
revenue    | orders       | amount
region     | customers    | region
```

Stored in `pgt_stream_tables.column_lineage JSONB` at creation time.

**Gaps versus a complete lineage story:**

- No transformation subtype (IDENTITY vs AGGREGATION vs TRANSFORMATION)
- No indirect lineage (filter columns, join keys)
- No type/nullability propagation
- No statistical properties
- No cross-system export format
- No sensitivity label propagation
- No operational freshness metadata
- No recursive transitive lineage built into the function (users must write CTEs)

### 1.2 Dependency Graph

`pgt_dependencies` records which source relations a stream table depends on,
with `columns_used TEXT[]` per edge. This is the structural backbone of the
lineage graph.

### 1.3 Refresh History

`pgt_refresh_history` records every refresh event with timestamps, row counts,
LSN, duration, and error state. This is raw operational lineage.

---

## 2. Standards Landscape

### 2.1 OpenLineage

[OpenLineage](https://openlineage.io) is the de-facto industry standard for
lineage metadata exchange, governed by the LF AI & Data Foundation. It is used
by Apache Airflow, Apache Spark, dbt, Apache Flink, Trino, and dozens of
commercial tools.

**Core model:**

```
RunEvent
├── job  { namespace, name, facets }   ← the transformation (stream table)
├── run  { runId, facets }             ← one refresh execution
├── inputs  [ Dataset ]                ← source tables
└── outputs [ Dataset ]               ← the stream table itself
```

**Relevant facets:**

| Facet | Type | Content |
|-------|------|---------|
| `columnLineage` | OutputDatasetFacet | `fields: { col: { inputFields: [{ns, name, field, transformations}] } }` |
| `schema` | DatasetFacet | Column names and types |
| `sql` | JobFacet | The defining query |
| `dataQualityMetrics` | InputDatasetFacet | Row count, null count, distinct count, min, max per column |
| `nominalTime` | RunFacet | Scheduled vs actual execution time |
| `errorMessage` | RunFacet | Error details on failed runs |
| `parent` | RunFacet | Parent run reference (for DAG hierarchies) |

The `columnLineage` facet's `transformations` array uses a two-level type
system that maps cleanly to pg_trickle's OpTree:

| OpenLineage type | subtype | pg_trickle OpTree node |
|-----------------|---------|------------------------|
| `DIRECT` | `IDENTITY` | `Project` pass-through |
| `DIRECT` | `TRANSFORMATION` | `Project` computed expression |
| `DIRECT` | `AGGREGATION` | `Aggregate` node |
| `INDIRECT` | `FILTER` | `Filter` predicate column |
| `INDIRECT` | `GROUP_BY` | `Aggregate.group_by` |
| `INDIRECT` | `JOIN` | `InnerJoin / LeftJoin` condition |
| `INDIRECT` | `WINDOW` | `WindowExpr` partition/order |
| `INDIRECT` | `SORT` | `ORDER BY` expression |
| `INDIRECT` | `CONDITIONAL` | `CASE`/`COALESCE` expressions |

The `masking` boolean on each transformation entry indicates whether the
transformation is privacy-preserving (e.g., `COUNT`, `HASH`). pg_trickle can
derive this for well-known aggregate functions.

### 2.2 W3C PROV

[PROV](https://www.w3.org/TR/prov-overview/) is a W3C Recommendation from 2013
that defines a foundational provenance model. Its three core concepts are:

- **Entity** — a thing (dataset, column, tuple)
- **Activity** — a process that used/generated entities (a refresh run)
- **Agent** — a responsible party (user, system)

Relations: `wasGeneratedBy`, `used`, `wasDerivedFrom`, `wasAttributedTo`,
`wasAssociatedWith`, `actedOnBehalfOf`.

PROV-O provides an OWL2 ontology for RDF serialisation. PROV is more expressive
than OpenLineage but heavier to implement. It is the right choice for:

- Fine-grained tuple-level provenance
- Linking to semantic web / knowledge graphs
- Regulatory use cases requiring formal provenance graphs

**Relationship to OpenLineage:** The two standards are complementary. OpenLineage
is optimised for pipeline-level runtime events; PROV is optimised for
entity-level derivation graphs. pg_trickle could emit both, mapping its catalog
data to PROV-O RDF and its refresh events to OpenLineage RunEvents.

### 2.3 DCAT / schema.org / Dublin Core

[DCAT](https://www.w3.org/TR/vocab-dcat-3/) (Data Catalog Vocabulary) is a W3C
standard for describing datasets in a catalog. Combined with Dublin Core terms
for `dc:creator`, `dc:modified`, `dc:description`, pg_trickle stream tables can
be described as first-class catalog entries discoverable by tools like CKAN,
Socrata, and data.gov portals.

For pg_trickle: emit `pgtrickle.stream_table_dcat_json(name)` that produces a
DCAT `Dataset` resource with `dct:source` links pointing to each input relation.

### 2.4 ISO/IEC 11179 (Metadata Registry)

The ISO metadata registry standard defines how to represent data element
properties in interoperable registries. It is the formal basis for enterprise
data catalogs. pg_trickle's column type/nullability information could be
serialised in this format for integration with government and enterprise MDM
(Master Data Management) platforms.

### 2.5 Great Expectations / OpenMetadata / DataHub

**OpenMetadata** and **DataHub** are open-source data catalog / data governance
platforms that consume OpenLineage events. They store schema, lineage, quality,
and ownership metadata.

Key integration point: both support the OpenLineage HTTP API as an event sink.
If pg_trickle emits OpenLineage events, it gets lineage in both platforms for
free.

**Great Expectations** produces `dataQualityAssertions` OpenLineage facets,
making quality and lineage a unified view.

---

## 3. Novel Ideas for pg_trickle

### Idea A — Differential Lineage (Delta-Level Provenance)

**Unique to pg_trickle:** Most lineage systems track lineage at the dataset
level (table to table). pg_trickle uniquely tracks *which rows changed* (the
delta). This enables **row-level provenance** at refresh granularity:

> "Row 42 of `revenue_by_region` changed because order 9871 in `orders` was
> inserted, contributing +150.00 to the SUM."

This is **differential provenance** — the lineage of a *change*, not just a
row. Implementation:

1. Annotate change-buffer rows with a `provenance_run_id UUID` at CDC time.
2. When a row in the output changes, record which input delta tuples caused it.
3. Query: `pgtrickle.explain_row_change(st_name, pk_values, run_id)` → chain of
   input events that produced this output change.

This is radically more powerful than what any existing lineage tool supports.

### Idea B — Live Statistical Property Propagation

Track statistical properties of columns *through* the operator tree at each
refresh cycle, without executing any queries against the data:

- Input `orders.amount` has null-rate 0.02, cardinality 50 000, min 0.01, max 9999.99
- After `SUM`, `revenue.revenue` has null-rate 0 (SUM never produces NULL for groups),
  cardinality ≤ group count, and estimated value range.
- After `FILTER pred`, columns downstream have adjusted cardinalities based on
  pg_trickle's selectivity estimate.

These derived statistics become "property lineage" — the properties of
downstream columns are explained as a function of upstream properties plus the
transformation. Stored in `pgt_stream_tables.property_lineage JSONB`.

Query: `pgtrickle.column_properties(name)` →

```
col     | null_rate | cardinality | min     | max   | derived_from
--------+-----------+-------------+---------+-------+-------------
revenue | 0.0       | 1200        | 0.01    | 9999  | SUM(orders.amount)
region  | 0.02      | 12          | 'APAC'  | 'US'  | IDENTITY(customers.region)
```

### Idea C — Sensitivity Label Propagation

PII / PHI / sensitivity labels flow through the transformation graph via
defined propagation rules:

| Rule | Logic |
|------|-------|
| IDENTITY pass-through | Output inherits source label |
| COUNT(*) of PII column | Output is NOT PII (aggregate is anonymised) |
| HASH(email) | Still PII by default (reversible); `masking=true` only if non-reversible hash |
| SUM(salary) with GROUP BY | Output is PII if group size < k (k-anonymity threshold) |
| JOIN on PII key | Joined output columns inherit PII from the key source |

Implementation:
1. Extend `pgt_stream_tables` with `sensitivity_labels JSONB` (per-column labels).
2. Add `pgtrickle.set_column_label(st_name, col, label)` to set manual labels on source tables.
3. Infer derived labels at create time via DVM operator tree traversal.
4. Surface via `pgtrickle.column_labels(name)`.

This enables automated GDPR compliance: `pgtrickle.pii_impact_report()` shows
all stream tables that contain PII-derived columns.

### Idea D — OpenLineage Background Worker

A background worker (or configurable hook on refresh completion) that emits
OpenLineage `RunEvent` payloads to an HTTP endpoint:

```
pg_trickle.openlineage_endpoint = 'http://marquez:5000'
pg_trickle.openlineage_namespace = 'production-db'
pg_trickle.openlineage_enabled = true
```

On each refresh completion the worker posts:

```json
{
  "eventType": "COMPLETE",
  "eventTime": "2026-04-30T14:23:01Z",
  "job": {
    "namespace": "production-db",
    "name": "pgtrickle.revenue_by_region",
    "facets": {
      "sql": { "query": "SELECT region, SUM(amount) FROM orders JOIN ..." },
      "documentation": { "description": "Daily revenue rollup by customer region" }
    }
  },
  "run": { "runId": "<uuid>" },
  "inputs": [ { "namespace": "...", "name": "public.orders" } ],
  "outputs": [
    {
      "namespace": "...",
      "name": "public.revenue_by_region",
      "facets": {
        "columnLineage": { ... },
        "outputStatistics": { "rowCount": 12, "size": 384 }
      }
    }
  ]
}
```

### Idea E — Transitive Lineage SQL Function

Built-in recursive traversal via `pgtrickle.transitive_lineage(name)`:

```sql
SELECT * FROM pgtrickle.transitive_lineage('dashboard_summary');
```

Returns the full provenance chain to base tables, computing OpenLineage-style
`DIRECT`/`INDIRECT` transformation subtypes at each hop. Much faster than
a user-written recursive CTE because it short-circuits at base tables and
avoids re-parsing.

### Idea F — PROV-O RDF Export

For regulated industries (healthcare, finance, government), generate PROV-O
RDF Turtle for a stream table's lineage graph:

```sparql
pgt:revenue_by_region prov:wasDerivedFrom pgt:orders .
pgt:revenue_by_region prov:wasGeneratedBy pgt:refresh_run_<uuid> .
pgt:refresh_run_<uuid> prov:used pgt:orders .
pgt:refresh_run_<uuid> prov:wasAssociatedWith pgt:pg_trickle_agent .
```

Exposed via `pgtrickle.lineage_rdf(name, format)` returning `TEXT` (Turtle,
JSON-LD, or N-Triples). Useful for integration with enterprise governance
platforms (Collibra, Alation, Atlan) and semantic web tools.

### Idea G — Column Fingerprint Propagation

Track cryptographic fingerprints (not of data values, but of *schema structure*)
as columns flow through the DAG. A "column fingerprint" is a hash of:

- source table OID
- source column name + type
- transformation expression

This fingerprint is deterministic and survives rename/refactor of intermediate
stream tables. A column with the same fingerprint in two different stream tables
is guaranteed to be semantically identical (same derivation chain). Useful for:

- Auto-detecting duplicate stream tables
- Finding all stream tables that ultimately derive from `orders.amount`
- Building an "identity lattice" for impact analysis

### Idea H — dbt Lineage Bridge

dbt's OpenLineage integration emits lineage events for every `dbt run`. When
dbt creates a model that is then consumed by a pg_trickle stream table, or vice
versa, the two lineage graphs are disconnected.

The `dbt-pgtrickle` package can be extended to:

1. Emit OpenLineage events for `stream_table` materializations that include the
   dbt run's `runId` as a `parent` facet reference.
2. Read pg_trickle's `stream_table_lineage()` output and inject it as column-level
   lineage into the dbt artifact.

This connects dbt's lineage graph to pg_trickle's, giving a single unified
lineage view in Marquez, OpenMetadata, or DataHub.

### Idea I — Freshness Lineage (Temporal Provenance)

For each output column, track the "staleness bound":

> "This column is at most T seconds behind its source data, because it is derived
> from `orders.created_at` which was last processed at LSN X."

Store in `pgt_stream_tables.freshness_spec JSONB` and surface via:

```sql
SELECT * FROM pgtrickle.column_freshness('revenue_by_region');
```

```
col     | max_lag_s | last_source_lsn | last_refresh_at
--------+-----------+-----------------+--------------------
revenue | 301       | 0/A3BC0000      | 2026-04-30 14:23:01
region  | 301       | 0/A3BC0000      | 2026-04-30 14:23:01
```

This is especially valuable for SLA monitoring: "Is my dashboard column fresh
enough for this business rule?"

---

## 4. Implementation Plan

### Phase 1 — Enhanced Column Lineage (v0.40.x)

**Goal:** Upgrade F12's stored `column_lineage` JSON to include full
OpenLineage-compatible transformation subtypes (DIRECT/INDIRECT, subtype,
masking flag). No new SQL functions yet — only the stored metadata is enriched.

**Changes:**

1. **`src/dvm/parser/types.rs`** — Add `LineageEntry` and `TransformationType`
   structs to the DVM type system:

   ```rust
   #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
   pub struct LineageEntry {
       pub output_col: String,
       pub input_fields: Vec<LineageInputField>,
   }

   #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
   pub struct LineageInputField {
       pub source_table: String,
       pub source_col: String,
       pub transformations: Vec<LineageTransformation>,
   }

   #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
   pub struct LineageTransformation {
       pub type_: LineageType,      // DIRECT | INDIRECT
       pub subtype: LineageSubtype, // IDENTITY | TRANSFORMATION | AGGREGATION | FILTER | JOIN | GROUP_BY | WINDOW | SORT | CONDITIONAL
       pub description: String,
       pub masking: bool,
   }
   ```

2. **`src/dvm/parser/mod.rs`** — Extend `build_column_lineage()` (or create it
   if it doesn't already exist) to emit `Vec<LineageEntry>` from the OpTree. The
   traversal:
   - `OpTree::Project` → `DIRECT/IDENTITY` for pass-through column refs,
     `DIRECT/TRANSFORMATION` for expressions, `DIRECT/AGGREGATION` for
     aggregate calls, with `masking=true` for COUNT/HASH
   - `OpTree::Filter` → `INDIRECT/FILTER` for all column refs in the predicate
   - `OpTree::Aggregate` → `INDIRECT/GROUP_BY` for group-by columns
   - `OpTree::InnerJoin / LeftJoin / FullJoin` → `INDIRECT/JOIN` for join
     condition column refs
   - `OpTree::RecursiveCte` → `INDIRECT/CONDITIONAL` for the recursive term

3. **`src/api/mod.rs`** — Serialise the `Vec<LineageEntry>` to JSONB on
   `create_stream_table` and `alter_stream_table`. Update the
   `stream_table_lineage()` function to expand the new richer format.

**Estimated effort:** 3–4 days. No schema migration required — the JSONB column
already exists; only its content changes.

---

### Phase 2 — Transitive Lineage Function (v0.41.x)

**Goal:** Built-in recursive lineage traversal without requiring users to write
CTEs.

**New SQL function:**

```sql
-- Returns lineage entries for all stream tables in the chain, back to base tables.
-- max_depth prevents infinite loops (default 32).
SELECT * FROM pgtrickle.transitive_lineage(
    name        TEXT,
    max_depth   INT DEFAULT 32
) RETURNS TABLE (
    hop         INT,          -- 0 = immediate, 1 = one level up, etc.
    stream_table TEXT,
    output_col   TEXT,
    source_table TEXT,        -- base table name when at leaf
    source_col   TEXT,
    transformation_type    TEXT,
    transformation_subtype TEXT,
    masking      BOOLEAN
);
```

**Implementation:** Pure SQL using the catalog + `stream_table_lineage()`,
but implemented as a native Rust function for performance. The traversal uses
the `pgt_dependencies` table to identify which source relations are themselves
stream tables, and recurses into each.

**Estimated effort:** 2 days.

---

### Phase 3 — Property Lineage: Type & Nullability Propagation (v0.41.x)

**Goal:** Track how column types and nullability propagate through transforms.

**Propagation rules:**

| Transformation | Nullability propagation |
|----------------|------------------------|
| IDENTITY pass-through | Same as source |
| Expression (non-null inputs) | NOT NULL |
| SUM / AVG over nullable col | Nullable (SUM of empty group = NULL) |
| COUNT | NOT NULL (always returns 0) |
| JOIN column (inner join) | Inherits FROM source |
| LEFT JOIN right-side col | Always nullable |
| CASE expression | Nullable if any branch is nullable |

Store in `pgt_stream_tables.property_lineage JSONB` (column → `{type, not_null,
has_default, source_properties[]}`). This is populated at create time alongside
`column_lineage`.

**New SQL function:**

```sql
SELECT * FROM pgtrickle.column_properties(name TEXT)
RETURNS TABLE (
    col          TEXT,
    pg_type      TEXT,
    not_null     BOOLEAN,
    derived_from TEXT,   -- e.g. 'SUM(orders.amount)' or 'IDENTITY(customers.region)'
    masked       BOOLEAN
);
```

**Estimated effort:** 3 days.

---

### Phase 4 — Sensitivity Label Store (v0.42.x)

**Goal:** Allow users to tag source columns as PII/PHI/sensitive, and have those
labels propagate automatically through the DAG.

**Schema additions:**

```sql
-- Per-column sensitivity labels on source tables (user-managed).
CREATE TABLE pgtrickle.pgt_column_labels (
    relation_oid  OID NOT NULL,
    column_name   TEXT NOT NULL,
    label         TEXT NOT NULL,  -- 'PII', 'PHI', 'CONFIDENTIAL', custom
    added_by      TEXT NOT NULL DEFAULT current_user,
    added_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (relation_oid, column_name, label)
);
```

**New SQL functions:**

```sql
-- Set / remove a label on a column.
pgtrickle.set_column_label(
    table_name TEXT, column_name TEXT, label TEXT
) RETURNS void;

pgtrickle.remove_column_label(
    table_name TEXT, column_name TEXT, label TEXT
) RETURNS void;

-- Show derived labels for all output columns of a stream table.
pgtrickle.column_labels(
    name TEXT
) RETURNS TABLE (col TEXT, label TEXT, inherited_from TEXT, masked BOOLEAN);

-- Cross-DAG impact: which stream tables expose PII-derived columns?
pgtrickle.pii_impact_report()
RETURNS TABLE (
    stream_table TEXT, col TEXT, source_table TEXT, source_col TEXT, label TEXT
);
```

**Propagation rules** (applied at create/alter time via DVM tree walk):

| Transform | Label propagation |
|-----------|------------------|
| IDENTITY | Inherits all source labels |
| COUNT(*) | No PII label (aggregate anonymises) |
| COUNT(DISTINCT pii_col) | Inherits PII if group is small (warn if group < k) |
| SUM / AVG | Inherits PII unless group_by makes it safe |
| HASH(pii_col) — non-reversible | Drops PII, adds DERIVED_PII |
| JOIN on PII key | Joined relation may inherit PII if key is exposed |

**Estimated effort:** 4–5 days.

---

### Phase 5 — OpenLineage Event Generation & Outbox (v0.43.x)

**Goal:** Generate OpenLineage `RunEvent` payloads in PostgreSQL and place them
in a durable outbox table. HTTP emission is delegated to an external relay
service (`pgtrickle-relay` or a standalone sidecar).

**Why external relay?** PostgreSQL should not perform external HTTP calls.
The outbox pattern keeps the database clean, enables retry logic and batching
outside the transaction context, and allows independent scaling/deployment of
the relay component.

**Architecture:**

```
PostgreSQL (refresh completes)
    ↓
    ├─ Generates OpenLineage RunEvent JSON
    ├─ Writes to pgtrickle_changes.openlineage_events (outbox table)
    └─ Sends NOTIFY 'pgtrickle_lineage' event
            ↓
pgtrickle-relay service (stateless sidecar)
    ├─ Listens for NOTIFY or polls outbox table
    ├─ Reads events from pgtrickle_changes.openlineage_events
    ├─ POST to Marquez/DataHub/custom backend with exponential backoff
    ├─ Marks events as sent via UPDATE
    └─ Can be deployed/scaled independently
```

**Schema additions:**

```sql
-- Durable outbox table (UNLOGGED for performance; data is non-critical).
-- Persists across server restarts so relay can catch up.
CREATE TABLE pgtrickle_changes.openlineage_events (
    event_id         BIGSERIAL PRIMARY KEY,
    stream_table_oid OID NOT NULL,
    stream_table_name TEXT NOT NULL,
    event_type       TEXT NOT NULL,  -- 'START', 'COMPLETE', 'FAIL'
    event_json       JSONB NOT NULL, -- Full OpenLineage RunEvent
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at          TIMESTAMPTZ,
    attempt_count    INT NOT NULL DEFAULT 0,
    last_error       TEXT,
    CONSTRAINT check_event_type CHECK (event_type IN ('START', 'COMPLETE', 'FAIL'))
);

CREATE INDEX idx_openlineage_events_sent_at 
    ON pgtrickle_changes.openlineage_events(sent_at) 
    WHERE sent_at IS NULL;
```

**PostgreSQL-side (Phase 5):**

1. **Hook on refresh completion** — After `refresh_stream_table()` finishes
   (successfully or with error), insert a row into `openlineage_events` with
   the assembled `RunEvent` JSON.

2. **New SQL function:**

   ```sql
   -- On-demand: generate the OpenLineage JSON for a stream table (no outbox write).
   -- Useful for testing and manual inspection.
   pgtrickle.stream_table_obl_event(
       name TEXT,
       event_type TEXT DEFAULT 'COMPLETE'
   ) RETURNS JSONB;
   ```

3. **New GUCs** (for relay configuration, stored in database):

   ```
   pg_trickle.openlineage_enabled = false  -- When true, write to outbox on refresh
   pg_trickle.openlineage_namespace = ''   -- Defaults to current database
   pg_trickle.openlineage_include_sql = true
   pg_trickle.openlineage_include_stats = true
   pg_trickle.openlineage_include_column_lineage = true
   ```

4. **Optional monitoring function:**

   ```sql
   pgtrickle.openlineage_queue_status()
   RETURNS TABLE (
       event_id INT, stream_table TEXT, event_type TEXT, created_at TIMESTAMPTZ,
       sent_at TIMESTAMPTZ, attempt_count INT, last_error TEXT
   );
   ```

**Relay service (separate project: `pgtrickle-relay` or new sidecar):**

- Configurable endpoint(s): `--marquez-url http://localhost:5000`, `--dataHub-url ...`
- Connects to PostgreSQL via libpq connection string
- Polls `openlineage_events` for `sent_at IS NULL` rows (or listens via LISTEN)
- For each unsent event:
  1. POST to backend with timeout (30s)
  2. If success (2xx): `UPDATE openlineage_events SET sent_at = now() WHERE event_id = ...`
  3. If failure: `UPDATE openlineage_events SET attempt_count = attempt_count + 1, last_error = ... WHERE event_id = ...`
  4. Exponential backoff: retry if `attempt_count < 5` and `created_at > now() - interval '7 days'`
- Can run as a systemd service, Kubernetes sidecar, or Docker container

**Effort breakdown:**
- PostgreSQL side (outbox table + hook + function): **2–3 days**
- Relay service (new Rust project or Go sidecar): **4–6 days** (depends on existing relay infrastructure)

**Total estimated effort:** 6–9 days (split between core + relay).

---

### Phase 6 — Statistical Property Propagation (v0.44.x)

**Goal:** Track lightweight statistical summaries of columns through the DAG,
updated incrementally at each refresh.

**What is tracked per output column:**

| Property | Type | How derived |
|----------|------|-------------|
| `estimated_cardinality` | BIGINT | Propagated from `pg_statistic` via rules |
| `null_rate` | FLOAT4 | Propagated through joins/aggregates |
| `min_value` | TEXT | min(source min) for IDENTITY; 0 for COUNT |
| `max_value` | TEXT | max(source max) for IDENTITY; NULL for AGG |
| `distinct_count` | BIGINT | Propagated from group-by cardinality |
| `freshness_lag_s` | FLOAT4 | Schedule interval + CDC capture latency |

Stored in `pgt_stream_tables.stats_lineage JSONB`. Updated on each refresh by
sampling row counts from `pg_class.reltuples` and null counts from
`pg_statistic`. Full ANALYZE is never triggered — only catalog reads.

**New SQL function:**

```sql
pgtrickle.column_stats(name TEXT)
RETURNS TABLE (
    col TEXT, null_rate FLOAT4, cardinality BIGINT,
    min_value TEXT, max_value TEXT, freshness_lag_s FLOAT4
);
```

**Estimated effort:** 4–5 days.

---

### Phase 7 — PROV-O RDF Export (v0.45.x, optional)

**Goal:** Generate W3C PROV-O Turtle or JSON-LD for formal provenance
representation, targeting regulated industries.

**New SQL function:**

```sql
pgtrickle.lineage_rdf(
    name   TEXT,
    format TEXT DEFAULT 'turtle'  -- 'turtle', 'jsonld', 'ntriples'
) RETURNS TEXT;
```

Example Turtle output for `revenue_by_region`:

```turtle
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix pgt:  <urn:pg-trickle:db:> .
@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .

pgt:revenue_by_region
    a prov:Entity ;
    prov:wasDerivedFrom pgt:orders ;
    prov:wasDerivedFrom pgt:customers ;
    prov:wasGeneratedBy pgt:refresh_run_2026-04-30T14:23:01Z .

pgt:refresh_run_2026-04-30T14:23:01Z
    a prov:Activity ;
    prov:startedAtTime "2026-04-30T14:23:00Z"^^xsd:dateTime ;
    prov:endedAtTime   "2026-04-30T14:23:01Z"^^xsd:dateTime ;
    prov:used pgt:orders ;
    prov:used pgt:customers ;
    prov:wasAssociatedWith pgt:pg_trickle .

pgt:pg_trickle
    a prov:SoftwareAgent ;
    prov:label "pg_trickle" .
```

**Estimated effort:** 3–4 days (pure serialisation logic, no new data model).

---

### Phase 8 — Differential Provenance (Stretch Goal, v0.46.x+)

**Goal:** Row-level change provenance — explain *which input delta tuples* caused
a specific output row to change.

This is the most novel feature in this plan and has no equivalent in any lineage
tool today. It requires:

1. **Change-buffer annotations:** Tag each change-buffer entry with a
   `provenance_batch_id UUID` at CDC capture time.
2. **Refresh journal:** During differential refresh, record a mapping
   `(output_row_pk, provenance_batch_id[])` for each output row that changed.
   Stored in `pgtrickle_changes.refresh_provenance_<oid>`.
3. **Query function:**

   ```sql
   pgtrickle.explain_row_change(
       st_name    TEXT,
       pk_values  JSONB,  -- e.g. '{"customer_id": 42}'
       since      TIMESTAMPTZ DEFAULT NULL
   ) RETURNS TABLE (
       refresh_run_id   UUID,
       refreshed_at     TIMESTAMPTZ,
       output_delta     TEXT,  -- '+' or '-'
       source_table     TEXT,
       source_pk        JSONB,
       source_delta     TEXT,
       source_col       TEXT,
       old_value        TEXT,
       new_value        TEXT
   );
   ```

**Use cases:** Audit trails ("show me every source change that affected invoice
total 42"), debugging ("why did this metric suddenly spike?"), compliance
("prove that only authorised input data contributed to this report row").

**Estimated effort:** 10–14 days. This is significant engineering work and
requires careful design to avoid storage explosion (change-capture overhead
mitigation needed).

---

## 5. Ecosystem Integration Map

```
pg_trickle lineage APIs
        │
        ├── stream_table_lineage()          ← today (F12)
        ├── transitive_lineage()            ← Phase 2
        ├── column_properties()             ← Phase 3
        ├── column_labels()                 ← Phase 4
        ├── stream_table_obl_event()        ← Phase 5
        ├── lineage_rdf()                   ← Phase 7
        └── explain_row_change()            ← Phase 8 (stretch)
                │
    ┌───────────┼──────────────────────┐
    │           │                      │
OpenLineage  PROV-O RDF             DCAT JSON
    │                                  │
    ├── Marquez                         └── Data catalogs
    ├── OpenMetadata                        (CKAN, Socrata)
    ├── DataHub
    ├── Apache Airflow
    ├── dbt integration
    ├── Apache Spark jobs
    └── Custom consumers
```

---

## 6. dbt Integration Extension

Extend `dbt-pgtrickle` to:

1. Auto-emit OpenLineage events for `stream_table` materializations, using
   the dbt project's `run_id` as a `parent` facet so dbt → pg_trickle lineage
   is fully connected.
2. Add a new macro `stream_table_column_lineage()` that reads
   `pgtrickle.stream_table_lineage()` and injects it into the dbt artifact
   JSON for downstream consumption by catalog tools.

---

## 7. Implementation Priority Summary

| Phase | Feature | Version | Effort (PG-side) | Value | Priority |
|-------|---------|---------|----------|-------|----------|
| 1 | Enhanced column lineage (OL-compatible subtypes) | v0.40.x | 3–4d | High | **P1** |
| 2 | Transitive lineage function | v0.41.x | 2d | High | **P1** |
| 3 | Type & nullability propagation | v0.41.x | 3d | Medium | **P2** |
| 5 | OpenLineage event generation & outbox | v0.43.x | 2–3d | Very High | **P1** |
| 4 | Sensitivity label propagation | v0.42.x | 4–5d | High | **P2** |
| 6 | Statistical property propagation | v0.44.x | 4–5d | Medium | **P3** |
| 7 | PROV-O RDF export | v0.45.x | 3–4d | Low–Medium | **P3** |
| 8 | Differential row provenance | v0.46.x+ | 10–14d | Very High (niche) | **Stretch** |

**Note:** Phase 5 effort is PostgreSQL-side work only. A separate relay service
(pgtrickle-relay or custom sidecar) would add 4–6 additional days, deployed
independently.

---

## 8. Open Questions

1. **Relay infrastructure:** Where should the OpenLineage relay live?
   - Option A: Extend `pgtrickle-relay` (if it exists and is suitable)
   - Option B: Create a new lightweight Rust/Go sidecar (`pgtrickle-ol-relay`)
   - Option C: User writes their own polling loop + HTTP client
   - Recommendation: Start with Option C (documented best practice), then add
     Option B as a reference implementation if demand warrants.

2. **NOTIFY vs polling:** Should the relay listen for NOTIFY events or poll the
   outbox table periodically?
   - NOTIFY: real-time, but requires a persistent connection
   - Polling: simpler deployment (can be a cron job or systemd timer)
   - Recommendation: Support both; relay can use LISTEN if connected, fall back
     to polling if configured.

3. **Storage cost for differential provenance (Phase 8):** Change-buffer
   provenance annotations could multiply storage usage. A configurable
   `provenance_retention_days` GUC and a background vacuumer are needed.

4. **k-anonymity threshold for Phase 4:** The sensitivity label rule for
   small-group COUNT(DISTINCT pii_col) requires a configurable k-threshold.
   Default k=5 following HIPAA Safe Harbor guidance.

5. **Naming:** "property lineage" is not a widely standardised term. The
   implementation uses "column properties" internally to avoid confusion with
   OpenLineage's `columnLineage` facet. The user-visible name can be decided
   during implementation.

---

## 9. References

- [OpenLineage specification](https://openlineage.io/docs/)
- [OpenLineage Column Lineage Facet](https://openlineage.io/docs/spec/facets/dataset-facets/column_lineage_facet)
- [W3C PROV-Overview](https://www.w3.org/TR/prov-overview/)
- [W3C PROV-O OWL Ontology](https://www.w3.org/TR/prov-o/)
- [W3C DCAT v3](https://www.w3.org/TR/vocab-dcat-3/)
- [Marquez — OpenLineage reference backend](https://marquezproject.ai/)
- [OpenMetadata](https://open-metadata.org/)
- [DataHub](https://datahubproject.io/)
- [Great Expectations OpenLineage integration](https://openlineage.io/docs/integrations/great-expectations)
- [dbt OpenLineage integration](https://github.com/OpenLineage/OpenLineage/tree/main/integration/dbt)
- pg_trickle existing column lineage: [blog/column-level-lineage.md](../../blog/column-level-lineage.md)
- pg_trickle DVM operator tree: [src/dvm/parser/types.rs](../../src/dvm/parser/types.rs)
- pg_trickle future directions: [plans/REPORT_FUTURE_DIRECTIONS.md](../REPORT_FUTURE_DIRECTIONS.md)
