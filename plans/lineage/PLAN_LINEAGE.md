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

**Priority:** Low. DCAT is a catalog discovery standard, not a lineage standard;
it duplicates what OpenLineage's dataset schema facet already provides for the
consumer tools we care about. Defer unless a specific data portal integration
is requested.

### 2.4 ISO/IEC 11179 (Metadata Registry)

The ISO metadata registry standard defines how to represent data element
properties in interoperable registries. It is the formal basis for enterprise
data catalogs. pg_trickle's column type/nullability information could be
serialised in this format for integration with government and enterprise MDM
(Master Data Management) platforms.

**Priority:** Very low / on request only. Relevant only for government and
enterprise MDM use cases with hard ISO compliance requirements. Not on the
implementation roadmap.

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

### Idea D — OpenLineage Event Generation (→ implemented as Phase 5)

> **Note:** This idea is fully incorporated into Phase 5. See §4 Phase 5 for
> the complete design. The refresh hook generates a full `RunEvent` JSON and
> writes it to `pgtrickle_changes.lineage_outbox`; `pgtrickle-relay` handles
> delivery to Marquez, DataHub, or any OpenLineage-compatible endpoint.

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

### Phase 1 — Enhanced Column Lineage (v0.41.x)

**Goal:** Upgrade F12's stored `column_lineage` JSON to include full
OpenLineage-compatible transformation subtypes (DIRECT/INDIRECT, subtype,
masking flag), and update `stream_table_lineage()` to expose the richer format.

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
   `stream_table_lineage()` function to return the new columns:

   ```sql
   -- Updated return type (extends existing 3-col signature):
   SELECT * FROM pgtrickle.stream_table_lineage(name TEXT)
   RETURNS TABLE (
       output_col             TEXT,
       source_table           TEXT,
       source_col             TEXT,
       transformation_type    TEXT,   -- 'DIRECT' | 'INDIRECT'
       transformation_subtype TEXT,   -- 'IDENTITY' | 'AGGREGATION' | 'FILTER' | ...
       masking                BOOLEAN
   );
   ```

   The three existing columns are preserved in the same positions for
   backward compatibility. Clients reading only the first three columns
   continue to work without changes.

**Estimated effort:** 3–4 days. No schema migration required — the JSONB column
already exists; only its content and the function's return type change.

---

### Phase 2 — Transitive Lineage Function (v0.41.x or v0.42.x)

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

### Phase 3 — Property Lineage: Type & Nullability Propagation (v0.42.x)

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

### Phase 4 — Sensitivity Labels, Audit Trail & Masking Policies (v0.43.x)

**Goal:** Allow users to tag source columns with arbitrary hierarchical labels,
have those labels propagate automatically through the DAG, record every label
change in an immutable audit trail, and define reusable masking policies.

#### 4a — Hierarchical Custom Tags

Labels are free-form dot-separated strings, not a closed enum. Built-in
prefixes (`pii`, `phi`, `confidential`) are conventions only.

**Schema:**

```sql
-- Per-column sensitivity labels on source tables (user-managed).
-- 'label' is a free-form hierarchical tag, e.g. 'pii', 'compliance.gdpr.article_17',
-- 'business_domain.financial', 'data_quality.verified'.
CREATE TABLE pgtrickle.pgt_column_labels (
    relation_oid  OID NOT NULL,
    column_name   TEXT NOT NULL,
    label         TEXT NOT NULL,
    added_by      TEXT NOT NULL DEFAULT current_user,
    added_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (relation_oid, column_name, label)
);
```

**SQL functions:**

```sql
-- Set / remove a label on a column (any free-form tag).
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

-- Cross-DAG impact: which stream tables carry columns with a given label?
-- (Replaces the PII-specific function with a general-purpose one.)
pgtrickle.label_impact_report(label_prefix TEXT DEFAULT 'pii')
RETURNS TABLE (
    stream_table TEXT, col TEXT, source_table TEXT, source_col TEXT, label TEXT
);
```

**Propagation rules** (applied at create/alter time via DVM tree walk):

| Transform | Label propagation |
|-----------|------------------|
| IDENTITY | Inherits all source labels |
| COUNT(*) | No labels propagated (aggregate anonymises) |
| COUNT(DISTINCT col) | Inherits labels if group is small (warn if group < k) |
| SUM / AVG | Inherits labels unless group_by makes it safe |
| HASH(col) — non-reversible | Drops labels, adds `derived_pii` |
| JOIN on labelled key | Joined output columns inherit labels from the key source |

#### 4b — Immutable Audit Trail

Every label change (add, remove) and every lineage metadata update is
recorded in an append-only audit log, enabling temporal queries and compliance
evidence.

```sql
CREATE TABLE pgtrickle.lineage_audit_log (
    event_id      BIGSERIAL PRIMARY KEY,
    stream_table  TEXT NOT NULL,
    event_type    TEXT NOT NULL CHECK (event_type IN (
                      'label_added', 'label_removed',
                      'column_lineage_updated', 'schema_evolved',
                      'masking_policy_applied', 'masking_policy_removed')),
    column_name   TEXT,
    label         TEXT,
    changed_by    TEXT NOT NULL,
    changed_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    details       JSONB
);
```

**New SQL function:**

```sql
-- What did the label set look like on a given date?
pgtrickle.column_labels_as_of(name TEXT, as_of TIMESTAMPTZ)
RETURNS TABLE (col TEXT, label TEXT, inherited_from TEXT);
```

#### 4c — Masking Policies

A masking policy is a named rule that specifies a masking function and which
label tags it applies to. Policies are defined once and applied globally.

```sql
CREATE TABLE pgtrickle.pgt_masking_policies (
    policy_name       TEXT PRIMARY KEY,
    masking_function  TEXT NOT NULL,   -- e.g. 'sha256_hex', 'nullif_non_admin'
    applies_to_tags   TEXT[] NOT NULL, -- label prefixes this policy covers
    exceptions        TEXT[],          -- role/condition expressions
    created_by        TEXT NOT NULL DEFAULT current_user,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Define and apply:
pgtrickle.create_masking_policy(
    policy_name TEXT, masking_function TEXT,
    applies_to_tags TEXT[], exceptions TEXT[] DEFAULT NULL
) RETURNS void;

pgtrickle.apply_masking_policy(
    policy_name TEXT, table_name TEXT, column_name TEXT
) RETURNS void;

-- Query effective masks:
pgtrickle.column_masks(table_name TEXT)
RETURNS TABLE (col TEXT, policy_name TEXT, masking_function TEXT, applied_at TIMESTAMPTZ);
```

#### 4d — Schema Evolution Timeline

Track how column types and nullability change over time, enabling "when did
this column become nullable?" queries for debugging and compliance.

```sql
CREATE TABLE pgtrickle.column_schema_history (
    stream_table  TEXT NOT NULL,
    column_name   TEXT NOT NULL,
    pg_type       TEXT NOT NULL,
    not_null      BOOLEAN NOT NULL,
    effective_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    changed_by    TEXT NOT NULL DEFAULT current_user,
    reason        TEXT,
    PRIMARY KEY (stream_table, column_name, effective_at)
);
```

**New SQL function:**

```sql
-- Full type/nullability history for a column.
pgtrickle.column_schema_history(stream_table TEXT, column_name TEXT)
RETURNS TABLE (pg_type TEXT, not_null BOOLEAN, effective_at TIMESTAMPTZ, reason TEXT);
```

This table is written at `create_stream_table` time (initial snapshot) and on
every `alter_stream_table` that changes the output schema.

#### 4e — Pre-built Governance Query Patterns

Phase 4 ships a `sql/governance_queries.sql` file with annotated examples:

```sql
-- Q1: All stream tables with any 'pii' tag
SELECT DISTINCT st.name FROM pgt_stream_tables st
JOIN pgtrickle.lineage_audit_log al ON al.stream_table = st.name
WHERE al.label LIKE 'pii%' AND al.event_type = 'label_added'
  AND NOT EXISTS (SELECT 1 FROM pgtrickle.lineage_audit_log al2
      WHERE al2.stream_table = al.stream_table AND al2.column_name = al.column_name
        AND al2.label = al.label AND al2.event_type = 'label_removed'
        AND al2.changed_at > al.changed_at);

-- Q2: Schema stability — which columns change type most often?
SELECT stream_table, column_name, COUNT(*) AS schema_changes
FROM pgtrickle.column_schema_history
GROUP BY stream_table, column_name
ORDER BY schema_changes DESC;

-- Q3: Freshness SLA check — stream tables not refreshed in the last hour
SELECT st.name, MAX(rh.refresh_at) AS last_refresh,
       EXTRACT(EPOCH FROM (now() - MAX(rh.refresh_at))) AS age_s
FROM pgt_stream_tables st
LEFT JOIN pgt_refresh_history rh ON rh.stream_table_oid = st.relation_oid
GROUP BY st.name
HAVING MAX(rh.refresh_at) < now() - interval '1 hour';
```

**Estimated effort:** 7–9 days (expanded from original 4–5 due to audit trail,
schema history, and masking policy infrastructure).

---

### Phase 5 — OpenLineage Event Generation & pgtrickle-relay Sink (v0.44.x)

**Goal:** Generate OpenLineage `RunEvent` payloads in PostgreSQL, write them
to an outbox table, and deliver them via `pgtrickle-relay` to Marquez, DataHub,
or any OpenLineage-compatible backend.

**Why external relay?** PostgreSQL should not perform external HTTP calls.
The outbox pattern keeps the database clean, enables retry logic and batching
outside the transaction context, and allows the relay to be deployed/scaled
independently. **`pgtrickle-relay` already exists** (v0.29.0) with a full HTTP
webhook sink (`reqwest`), LISTEN/NOTIFY support, advisory locks, and a
SQL-driven pipeline configuration API. Adding OpenLineage delivery is a
**new `Sink` implementation** in the relay — not a new sidecar project.

**Architecture:**

```
PostgreSQL (refresh completes)
    ↓  hook in refresh path
    ├─ Assembles OpenLineage RunEvent JSON
    └─ INSERTs into pgtrickle_changes.lineage_outbox
            ↓  NOTIFY 'pgtrickle_relay' (reuses existing relay channel)
pgtrickle-relay (existing binary, v0.29.0+)
    ├─ Polls lineage_outbox via existing outbox source infrastructure
    ├─ Dispatches to new `openlineage` Sink type
    │       POST /api/v1/lineage  →  Marquez
    │       POST /api/v1/lineage  →  DataHub
    │       POST /api/v1/lineage  →  custom backend
    └─ Marks events delivered (existing ack/retry logic)
```

**Schema additions (PostgreSQL side):**

```sql
-- Outbox for OpenLineage events. LOGGED (not UNLOGGED) because events
-- are not reconstructible if lost after a crash — they represent point-in-time
-- refresh executions. Size is small (one row per refresh).
CREATE TABLE pgtrickle_changes.lineage_outbox (
    event_id         BIGSERIAL PRIMARY KEY,
    stream_table_oid OID NOT NULL,
    stream_table_name TEXT NOT NULL,
    event_type       TEXT NOT NULL CHECK (event_type IN ('START', 'COMPLETE', 'FAIL')),
    event_json       JSONB NOT NULL, -- Full OpenLineage RunEvent payload
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_lineage_outbox_undelivered
    ON pgtrickle_changes.lineage_outbox(event_id)
    WHERE event_id > 0;  -- relay uses offset-based polling, same as stream table outboxes
```

**PostgreSQL-side changes (Phase 5):**

1. **Hook on refresh completion** — After `refresh_stream_table()` finishes
   (successfully or with error), INSERT into `lineage_outbox` with the
   assembled `RunEvent` JSON.

2. **New SQL function:**

   ```sql
   -- On-demand: generate the OpenLineage JSON for a stream table (no outbox write).
   -- Useful for testing, curl-based ad-hoc delivery, and relay debugging.
   pgtrickle.stream_table_obl_event(
       name       TEXT,
       event_type TEXT DEFAULT 'COMPLETE'
   ) RETURNS JSONB;
   ```

3. **New GUCs:**

   ```
   pg_trickle.openlineage_enabled           = false  -- write to outbox on refresh
   pg_trickle.openlineage_namespace         = ''     -- defaults to current_database()
   pg_trickle.openlineage_catalog           = ''     -- logical catalog/hierarchy name, e.g. 'production.analytics'
   pg_trickle.openlineage_include_sql       = true
   pg_trickle.openlineage_include_stats     = true
   pg_trickle.openlineage_include_column_lineage = true
   ```

   The `catalog` setting is included in the OpenLineage dataset namespace path so that
   consumers (Marquez, DataHub, custom backends) can scope pg_trickle datasets within
   a multi-source or multi-tenant deployment without name collisions.

4. **Monitoring function:**

   ```sql
   pgtrickle.openlineage_queue_status()
   RETURNS TABLE (
       event_id BIGINT, stream_table TEXT, event_type TEXT,
       created_at TIMESTAMPTZ, age_s FLOAT4
   );
   ```

**pgtrickle-relay changes (new `openlineage` feature + Sink):**

Add `openlineage` as a new optional feature flag alongside the existing
`webhook`, `nats`, `kafka` etc. The new `OpenLineageSink` implements the
existing `Sink` trait:

```rust
// pgtrickle-relay/src/sink/openlineage.rs
#[cfg(feature = "openlineage")]
pub struct OpenLineageSink {
    client: reqwest::Client,  // reuses existing reqwest dependency
    endpoint: reqwest::Url,   // e.g. http://marquez:5000/api/v1/lineage
    timeout_secs: u64,
}

#[async_trait]
impl Sink for OpenLineageSink {
    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        // Each message.payload is already a full OL RunEvent JSON.
        // POST one event per request (OL API is not batched).
        for msg in messages {
            self.client.post(self.endpoint.clone())
                .json(&msg.payload)
                .send().await?.error_for_status()?;
        }
        Ok(())
    }
}
```

Register a lineage pipeline via the existing SQL API:

```sql
SELECT pgtrickle.set_relay_outbox(
    'lineage-to-marquez',
    config => '{
        "stream_table": "__lineage_outbox__",
        "sink_type": "openlineage",
        "openlineage_url": "http://marquez:5000/api/v1/lineage",
        "timeout_secs": 30
    }'::jsonb
);
SELECT pgtrickle.enable_relay('lineage-to-marquez');
```

The relay's existing advisory-lock coordination, offset-based polling, retry
logic, Prometheus metrics, and hot-reload via NOTIFY all apply without
additional work.

**Effort breakdown:**
- PostgreSQL side (outbox table + hook + GUCs + function): **2–3 days**
- `pgtrickle-relay` new `openlineage` sink + feature flag: **1–2 days**

**Total estimated effort:** 3–5 days (much less than an independent sidecar).

---

### Phase 6 — Statistical Property Propagation (v0.44.x or v0.45.x)

**Goal:** Track lightweight statistical summaries of columns through the DAG,
updated incrementally at each refresh.

**Elevated to P2:** Data quality metrics (null rates, cardinality, freshness)
are cheap to collect at refresh time (catalog reads only, no ANALYZE) and are
among the most-requested operational signals. Prefer shipping Phase 6 alongside
or immediately after Phase 5 rather than deferring to P3.

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

### Phase 7 — PROV-O RDF Export (v0.46.x, optional)

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

### Phase 8 — Differential Provenance (Stretch Goal, v0.47.x+)

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
        ├── stream_table_lineage()          ← today (F12), enhanced Phase 1
        ├── transitive_lineage()            ← Phase 2
        ├── column_properties()             ← Phase 3
        ├── column_labels()                 ← Phase 4
        ├── stream_table_obl_event()        ← Phase 5 (inspection / ad-hoc)
        ├── lineage_rdf()                   ← Phase 7
        └── explain_row_change()            ← Phase 8 (stretch)
                │
    ┌───────────┼─────────────────────────────────────┐
    │           │                                     │
    │  pgtrickle_changes.lineage_outbox          PROV-O RDF
    │           │                                     │
    │   pgtrickle-relay                       Turtle / JSON-LD
    │   (existing, v0.29.0+)                      │
    │   openlineage Sink (new, Phase 5)    Regulated industries
    │           │
    ├── POST /api/v1/lineage
    │       │
    │       ├── Marquez (OL reference backend)
    │       ├── OpenMetadata
    │       ├── DataHub
    │       └── Custom backends
    │
    └── dbt-pgtrickle (Phase 9)
            └── dbt artifacts → OL parent facet
```

---

## 6. dbt Integration Extension (Phase 9)

Extend `dbt-pgtrickle` to:

1. Auto-emit OpenLineage events for `stream_table` materializations, using
   the dbt project's `run_id` as a `parent` facet so dbt → pg_trickle lineage
   is fully connected in Marquez/DataHub.
2. Add a new macro `stream_table_column_lineage()` that reads
   `pgtrickle.stream_table_lineage()` and injects it into the dbt artifact
   JSON for downstream consumption by catalog tools.

This is a `dbt-pgtrickle` change only (Python + Jinja) — no changes to the
pg_trickle Rust extension. Estimated effort: 2–3 days.

---

## 7. Implementation Priority Summary

| Phase | Feature | Version | Effort | Value | Priority |
|-------|---------|---------|--------|-------|----------|
| 1 | Enhanced column lineage (OL-compatible subtypes + updated function) | v0.41.x | 3–4d | High | **P1** |
| 2 | Transitive lineage function | v0.41–42.x | 2d | High | **P1** |
| 5 | OpenLineage outbox + pgtrickle-relay `openlineage` sink | v0.44.x | 3–5d | Very High | **P1** |
| 3 | Type & nullability propagation | v0.42.x | 3d | Medium | **P2** |
| 4 | Hierarchical labels + audit trail + masking policies + schema history | v0.43.x | 7–9d | High | **P2** |
| 6 | Statistical property propagation (null rates, cardinality, freshness) | v0.44–45.x | 4–5d | High | **P2** |
| 9 | dbt lineage bridge | — | 2–3d | High | **P2** |
| 7 | PROV-O RDF export | v0.46.x | 3–4d | Low–Medium | **P3** |
| 8 | Differential row provenance | v0.47.x+ | 10–14d | Very High (niche) | **Stretch** |

**Ideas G (Column Fingerprints) and I (Freshness Lineage)** from §3 are not
phased here. G can be added as a bonus to Phase 1 (fingerprint stored alongside
column lineage in the JSONB) at low cost. I overlaps with existing `pgt_refresh_history`
staleness data and can be surfaced as a view rather than a new phase.

---

## 8. Open Questions

1. **`stream_table_lineage()` backward compatibility:** The function currently
   returns 3 columns. Phase 1 adds 3 more. Callers using `SELECT *` will see
   the new columns — this is intentional and additive, but should be
   communicated in the CHANGELOG.

2. **IMMEDIATE mode and the lineage outbox:** In IMMEDIATE (transactional IVM)
   mode, refreshes happen inside the user's transaction. The lineage outbox
   INSERT will also be inside that transaction, which is correct — the event
   is only durably written if the refresh commits. No special handling needed.

3. **`lineage_outbox` retention:** Unlike stream table change buffers (which are
   vacuum'd after processing), lineage events are small and worth retaining for
   audit. Recommend keeping rows for 30 days. A GUC
   `pg_trickle.lineage_retention_days` (default 30) controls this; a
   background vacuumer prunes old delivered rows.

4. **k-anonymity threshold for Phase 4:** The sensitivity label rule for
   small-group COUNT(DISTINCT pii_col) requires a configurable k-threshold.
   Default k=5 following HIPAA Safe Harbor guidance.

5. **Storage cost for differential provenance (Phase 8):** Change-buffer
   provenance annotations could multiply storage usage. A configurable
   `provenance_retention_days` GUC and a background vacuumer are needed.

6. **Column fingerprints (Idea G):** Should these be computed at parse time
   and stored alongside `column_lineage` JSONB, or lazily on demand? Given the
   DVM parser runs at create time anyway, pre-computing is cheap and preferred.

7. **Label hierarchy depth and search:** The hierarchical tag system uses
   dot-separated strings (`pii.email`, `compliance.gdpr.article_17`). Prefix
   searches via `label LIKE 'pii%'` are fast on small label tables, but a
   GiST/ltree index on `pgt_column_labels.label` may be worth adding if label
   counts exceed tens of thousands.

8. **Audit log retention:** `lineage_audit_log` rows should be retained for
   the lifetime of the stream table plus a configurable grace period (e.g.
   `pg_trickle.lineage_audit_retention_days`, default 365). Unlike the
   `lineage_outbox` (30-day delivery window), audit rows are compliance
   evidence and should survive relay delivery.

9. **Masking policy execution vs. description:** Phase 4 masking policies
   *describe* how a column should be masked; they do not intercept queries.
   Enforcement at read time is PostgreSQL's job (column-level privileges,
   RLS, views). The policy store is a governance signal, not an access
   control mechanism.

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
