# Plan: FAQ Expansion

Date: 2026-03-03
Status: IMPLEMENTED

---

## Overview

This plan proposed expanding the FAQ (`docs/FAQ.md`) with additional questions
that users of pg_trickle are likely to ask. The original FAQ covered 63 questions
across 13 sections. After implementation, the FAQ now contains **158 questions**
across **24 sections** (~2066 lines).

**All 94 proposed questions have been implemented** (78 net-new + 16 overlapping
entries expanded/cross-linked within existing sections).

### Priority Key

| Priority | Meaning |
|----------|---------|
| **P1** | Must-have — core concepts every user needs |
| **P2** | Should-have — common operational questions |
| **P3** | Nice-to-have — advanced topics and edge cases |

### Cross-Reference: Existing FAQ Sections

The current FAQ already covers these sections (do NOT duplicate):

- General (5 questions)
- Installation & Setup (4 questions)
- Creating & Managing Stream Tables (10 questions)
- SQL Support (4 questions)
- Change Data Capture (7 questions)
- Performance & Tuning (5 questions)
- Interoperability (8 questions)
- Monitoring & Alerting (3 questions)
- Configuration Reference (GUC table)
- Troubleshooting (6 questions)
- Why Are These SQL Features Not Supported? (10 questions)
- Why Are These Stream Table Operations Restricted? (8 questions)

---

## Proposed New Questions

### 1. Getting Started / Conceptual

Questions that help users build an accurate mental model of how pg_trickle
works before they write any SQL.

| ID | Priority | Question |
|----|----------|----------|
| GS-01 | P1 | What is incremental view maintenance (IVM) and why does it matter? |
| GS-02 | P1 | What is the difference between a stream table and a regular materialized view, in practice? |
| GS-03 | P1 | What happens behind the scenes when I INSERT a row into a table tracked by a stream table? |
| GS-04 | P1 | What does "differential" mean in the context of pg_trickle? |
| GS-05 | P2 | What is a frontier, and why does pg_trickle track LSNs? |
| GS-06 | P2 | What is the `__pgt_row_id` column and why does it appear in my stream tables? |
| GS-07 | P2 | What is the auto-rewrite pipeline and how does it affect my queries? |
| GS-08 | P3 | How does pg_trickle compare to DBSP (the academic framework)? |
| GS-09 | P3 | How does pg_trickle compare to pg_ivm? |

### 2. Data Freshness & Consistency

The #1 conceptual hurdle for users coming from synchronous materialized views.

| ID | Priority | Question |
|----|----------|----------|
| DC-01 | P1 | How stale can a stream table be? |
| DC-02 | P1 | Can I read my own writes immediately after an INSERT? |
| DC-03 | P1 | What consistency guarantees does pg_trickle provide? |
| DC-04 | P2 | What are "Delayed View Semantics" (DVS)? |
| DC-05 | P2 | What happens if the scheduler is behind — does data get lost? |
| DC-06 | P3 | How does pg_trickle ensure deltas are applied in the right order across cascading stream tables? |

### 3. IMMEDIATE Mode (Transactional IVM)

This is a major v0.2.0 feature — users switching from pg_ivm need detailed
guidance.

| ID | Priority | Question |
|----|----------|----------|
| IM-01 | P1 | When should I use IMMEDIATE mode instead of DIFFERENTIAL? |
| IM-02 | P1 | What SQL features are NOT supported in IMMEDIATE mode? |
| IM-03 | P1 | What happens when I TRUNCATE a source table in IMMEDIATE mode? |
| IM-04 | P2 | Can I have cascading IMMEDIATE stream tables (ST A → ST B)? |
| IM-05 | P2 | What locking does IMMEDIATE mode use? |
| IM-06 | P2 | How do I switch an existing DIFFERENTIAL stream table to IMMEDIATE? |
| IM-07 | P2 | What happens to IMMEDIATE mode during a manual `refresh_stream_table()` call? |
| IM-08 | P3 | How much write-side overhead does IMMEDIATE mode add? |

### 4. CDC — Triggers vs. WAL

Users need to understand the trade-offs behind the hybrid CDC model.

| ID | Priority | Question |
|----|----------|----------|
| CDC-01 | P1 | Why does pg_trickle default to triggers instead of logical replication? |
| CDC-02 | P1 | What is the write-side overhead of CDC triggers? |
| CDC-03 | P2 | How does the trigger-to-WAL automatic transition work? |
| CDC-04 | P2 | What happens to CDC if I restore a database backup? |
| CDC-05 | P2 | Do CDC triggers fire for rows inserted via logical replication (subscribers)? |
| CDC-06 | P3 | Can I inspect the change buffer tables directly? |
| CDC-07 | P3 | How does pg_trickle prevent its own refresh writes from re-triggering CDC? |

### 5. Aggregates & Group-By

Aggregate handling is complex and a common source of user confusion.

| ID | Priority | Question |
|----|----------|----------|
| AG-01 | P1 | Which aggregates are fully incremental (O(1) per change) vs. group-rescan? |
| AG-02 | P1 | Why do some aggregates have hidden auxiliary columns (`__pgt_count`, `__pgt_sum`)? |
| AG-03 | P2 | How does HAVING work with incremental refresh? |
| AG-04 | P2 | What happens to a group when all its rows are deleted? |
| AG-05 | P3 | Why are `CORR`, `COVAR_*`, and `REGR_*` limited to FULL mode? |

### 6. Joins

Join delta semantics can be surprising.

| ID | Priority | Question |
|----|----------|----------|
| JN-01 | P1 | How does a DIFFERENTIAL refresh handle a join when both sides changed? |
| JN-02 | P2 | Does pg_trickle support FULL OUTER JOIN incrementally? |
| JN-03 | P2 | What happens when a join key is updated and the joined row is simultaneously deleted? |
| JN-04 | P3 | Why is NATURAL JOIN rejected? |

### 7. CTEs & Recursive Queries

Recursive CTE support is a differentiator — document it well.

| ID | Priority | Question |
|----|----------|----------|
| CTE-01 | P1 | Do recursive CTEs work in DIFFERENTIAL mode? |
| CTE-02 | P2 | What are the three strategies for recursive CTE maintenance (semi-naive / DRed / recomputation)? |
| CTE-03 | P2 | What triggers a fallback from semi-naive to recomputation? |
| CTE-04 | P3 | What happens when a CTE is referenced multiple times in the same query? |

### 8. Window Functions & LATERAL

| ID | Priority | Question |
|----|----------|----------|
| WL-01 | P2 | How are window functions maintained incrementally? |
| WL-02 | P2 | Why can't I use a window function inside a CASE or COALESCE expression? |
| WL-03 | P2 | What LATERAL constructs are supported (SRFs, subqueries, JSON_TABLE)? |
| WL-04 | P3 | What happens when a row moves between window partitions during a refresh? |

### 9. TopK (ORDER BY … LIMIT)

| ID | Priority | Question |
|----|----------|----------|
| TK-01 | P1 | How does `ORDER BY … LIMIT N` work in a stream table? |
| TK-02 | P2 | Why is OFFSET not supported with TopK? |
| TK-03 | P2 | What happens when a row below the top-N cutoff rises above it? |
| TK-04 | P3 | Can I use TopK with aggregates or joins? |

### 10. Tables Without Primary Keys (Keyless Sources)

| ID | Priority | Question |
|----|----------|----------|
| KL-01 | P1 | Do source tables need a primary key? |
| KL-02 | P2 | What are the risks of using tables without primary keys? |
| KL-03 | P3 | How does content-based row identity work for duplicate rows? |

### 11. Diamond Dependencies & DAG Scheduling

| ID | Priority | Question |
|----|----------|----------|
| DD-01 | P2 | What is a diamond dependency and why does it matter? |
| DD-02 | P2 | What does `diamond_consistency = 'atomic'` do? |
| DD-03 | P2 | What is the difference between `'fastest'` and `'slowest'` schedule policy? |
| DD-04 | P3 | What happens when an atomic diamond group partially fails? |
| DD-05 | P3 | How does pg_trickle determine topological refresh order? |

### 12. Schema Changes & DDL Events

| ID | Priority | Question |
|----|----------|----------|
| SC-01 | P1 | What happens when I add a column to a source table? |
| SC-02 | P1 | What happens when I drop a column used in a stream table's query? |
| SC-03 | P2 | What happens when I `CREATE OR REPLACE` a view used by a stream table? |
| SC-04 | P2 | What happens when I alter or drop a function used in a stream table's query? |
| SC-05 | P2 | What is reinitialize and when does it trigger? |
| SC-06 | P3 | Can I block DDL on tracked source tables? |

### 13. Performance & Sizing

| ID | Priority | Question |
|----|----------|----------|
| PF-01 | P1 | How much disk space do change buffer tables consume? |
| PF-02 | P1 | What determines whether DIFFERENTIAL or FULL is faster for a given workload? |
| PF-03 | P2 | What are the planner hints and when should I disable them? |
| PF-04 | P2 | How do prepared statements help refresh performance? |
| PF-05 | P2 | How does the adaptive FULL fallback threshold work in practice? |
| PF-06 | P3 | How many stream tables can a single PostgreSQL instance handle? |
| PF-07 | P3 | What is the TRUNCATE vs DELETE cleanup trade-off for change buffers? |

### 14. dbt Integration

| ID | Priority | Question |
|----|----------|----------|
| DBT-01 | P1 | How do I use pg_trickle with dbt? |
| DBT-02 | P1 | What dbt commands work with stream tables? |
| DBT-03 | P2 | How does `dbt run --full-refresh` work with stream tables? |
| DBT-04 | P2 | How do I check stream table freshness in dbt? |
| DBT-05 | P2 | What happens when the defining query changes in dbt? |
| DBT-06 | P3 | Can I use `dbt snapshot` with stream tables? |
| DBT-07 | P3 | What dbt versions are supported? |

### 15. Deployment & Operations

| ID | Priority | Question |
|----|----------|----------|
| OP-01 | P1 | How many background workers does pg_trickle use? |
| OP-02 | P1 | Does pg_trickle work with connection poolers (PgBouncer, pgpool)? |
| OP-03 | P2 | How do I upgrade pg_trickle to a new version? |
| OP-04 | P2 | What happens to stream tables during a PostgreSQL restart? |
| OP-05 | P2 | Can I use pg_trickle on a read replica / standby? |
| OP-06 | P2 | How does pg_trickle work with CloudNativePG / Kubernetes? |
| OP-07 | P3 | Does pg_trickle work with partitioned source tables? |
| OP-08 | P3 | Can I run pg_trickle in multiple databases on the same cluster? |

### 16. Error Recovery & Debugging

| ID | Priority | Question |
|----|----------|----------|
| ER-01 | P1 | What happens when a refresh fails repeatedly? |
| ER-02 | P1 | How do I resume a suspended stream table? |
| ER-03 | P2 | How do I see the delta SQL that pg_trickle generates for my query? |
| ER-04 | P2 | How do I interpret the refresh history? |
| ER-05 | P2 | How can I tell if the scheduler is running? |
| ER-06 | P3 | How do I debug a stream table that shows stale data? |
| ER-07 | P3 | What does the `needs_reinit` flag mean and how do I clear it? |

---

## Summary

| Category | Count | P1 | P2 | P3 | Status |
|----------|------:|---:|---:|---:|--------|
| Getting Started / Conceptual | 9 | 4 | 3 | 2 | ✅ Implemented |
| Data Freshness & Consistency | 6 | 3 | 2 | 1 | ✅ Implemented |
| IMMEDIATE Mode | 8 | 3 | 4 | 1 | ✅ Implemented |
| CDC — Triggers vs. WAL | 7 | 2 | 3 | 2 | ✅ Implemented |
| Aggregates & Group-By | 5 | 2 | 2 | 1 | ✅ Implemented |
| Joins | 4 | 1 | 2 | 1 | ✅ Implemented |
| CTEs & Recursive Queries | 4 | 1 | 2 | 1 | ✅ Implemented |
| Window Functions & LATERAL | 4 | 0 | 3 | 1 | ✅ Implemented |
| TopK (ORDER BY … LIMIT) | 4 | 1 | 2 | 1 | ✅ Implemented |
| Tables Without Primary Keys | 3 | 1 | 1 | 1 | ✅ Implemented |
| Diamond Dependencies & DAG | 5 | 0 | 3 | 2 | ✅ Implemented |
| Schema Changes & DDL Events | 6 | 2 | 3 | 1 | ✅ Implemented |
| Performance & Sizing | 7 | 2 | 3 | 2 | ✅ Implemented |
| dbt Integration | 7 | 2 | 3 | 2 | ✅ Implemented |
| Deployment & Operations | 8 | 2 | 4 | 2 | ✅ Implemented |
| Error Recovery & Debugging | 7 | 2 | 3 | 2 | ✅ Implemented |
| **Total** | **94** | **28** | **43** | **23** | **All done** |

---

## Overlap Analysis

Some proposed questions overlap with existing FAQ entries. These should either
be merged into the existing section or cross-linked rather than duplicated:

| Proposed ID | Overlaps with existing FAQ entry | Recommendation |
|-------------|----------------------------------|----------------|
| GS-06 | "What is `__pgt_row_id`?" (Performance & Tuning) | Expand existing |
| CDC-02 | "What is the overhead of CDC triggers?" (CDC section) | Expand existing |
| AG-05 | "Why are unsupported aggregates limited to FULL mode?" (SQL Features section) | Expand existing |
| JN-04 | "How does `NATURAL JOIN` work?" (SQL Features section) | Cross-link |
| ER-01 | "What happens when a stream table keeps failing?" (Monitoring) | Expand existing |
| ER-02 | "My stream table is stuck in INITIALIZING status" (Troubleshooting) | Cross-link |
| OP-02 | "Does pg_trickle work with PgBouncer?" (Interoperability) | Expand existing |
| DC-01 | Partially in "What schedule formats are supported?" | New section |
| IM-01 | Partially in "When should I use FULL vs. DIFFERENTIAL vs. IMMEDIATE?" | Expand existing |

After deduplication, **~78 net-new questions** remain.

---

## Implementation Notes

1. **Group placement** — New questions should be organized into clear sections
   in the FAQ. Some existing sections should be split or renamed:
   - Split "SQL Support" into per-feature subsections (Aggregates, Joins, CTEs,
     Windows, TopK) for discoverability.
   - Add a new "Data Freshness & Consistency" top-level section.
   - Add a new "IMMEDIATE Mode" section.
   - Add a new "dbt Integration" section.

2. **Answer depth** — P1 questions should have complete, self-contained answers
   with code examples. P2 questions can reference other docs. P3 questions
   can be brief with a link to the relevant architecture/plan document.

3. **Cross-links** — Every answer should link to the relevant section of
   SQL_REFERENCE.md, ARCHITECTURE.md, or CONFIGURATION.md for deeper reading.

4. **Code examples** — Prefer runnable SQL snippets. Use the employee/department
   schema from GETTING_STARTED.md for consistency.

5. **Ordering** — Within each section, P1 questions come first, then P2, then P3.
