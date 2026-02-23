# Prior Art

This document lists the academic papers, PostgreSQL commits, open-source tools,
and standard algorithms whose techniques are reused in `pg_stream`.

Maintaining this record serves two purposes:
1. **Attribution** — credit the research and engineering work this project builds upon.
2. **Independent derivation** — demonstrate that every core technique predates and is independent of any single vendor's commercial product.

---

## Differential View Maintenance (DVM)

### DBSP — Automatic Incremental View Maintenance

> Budiu, M., Ryzhyk, L., McSherry, F., & Tannen, V. (2023).
> "DBSP: Automatic Incremental View Maintenance for Rich Query Languages."
> *Proceedings of the VLDB Endowment (PVLDB)*, 16(7), 1601–1614.
> <https://arxiv.org/abs/2203.16684>

The Z-set abstraction (rows annotated with +1/−1 multiplicity) is the
theoretical foundation for the `__pgs_action` column produced by the delta
operators in `src/dvm/operators/`. The per-operator differentiation rules
(scan, filter, project, join, aggregate, union) are direct applications of
the DBSP lifting operator (D) described in this paper.

See [DBSP_COMPARISON.md](DBSP_COMPARISON.md) for a detailed comparison of
pg_stream's architecture with the DBSP model.

### Gupta & Mumick — Materialized Views Survey

> Gupta, A. & Mumick, I.S. (1995).
> "Maintenance of Materialized Views: Problems, Techniques, and Applications."
> *IEEE Data Engineering Bulletin*, 18(2), 3–18.
>
> Gupta, A. & Mumick, I.S. (1999).
> *Materialized Views: Techniques, Implementations, and Applications.*
> MIT Press. ISBN 978-0-262-57122-7.

The per-operator differentiation rules in `src/dvm/operators/` follow the
derivation given in section 3 of the 1995 survey. The counting algorithm
for maintaining aggregates with deletions uses the approach described in
the MIT Press book.

### DBToaster — Higher-order Delta Processing

> Koch, C., Ahmad, Y., Kennedy, O., Nikolic, M., Nötzli, A., Olteanu, D.,
> & Zavodny, J. (2014).
> "DBToaster: Higher-order Delta Processing for Dynamic, Frequently Fresh Views."
> *The VLDB Journal*, 23(2), 253–278.
> <https://doi.org/10.1007/s00778-013-0348-4>

Inspiration for the recursive delta compilation strategy where the delta of a
complex query is itself a query that can be differentiated.

### DRed — Deletion and Re-derivation

> Gupta, A., Mumick, I.S., & Subrahmanian, V.S. (1993).
> "Maintaining Views Incrementally."
> *Proceedings of the 1993 ACM SIGMOD International Conference*, 157–166.

The DRed algorithm for handling deletions in recursive views is the basis for
the recursive CTE differential refresh strategy in `src/dvm/operators/recursive_cte.rs`.

---

## Scheduling

### Earliest-Deadline-First (EDF)

> Liu, C.L. & Layland, J.W. (1973).
> "Scheduling Algorithms for Multiprogramming in a Hard-Real-Time Environment."
> *Journal of the ACM*, 20(1), 46–61.
> <https://doi.org/10.1145/321738.321743>

The `schedule`-based scheduling in `src/scheduler.rs` applies the classic
EDF principle: the stream table whose freshness deadline expires soonest is
refreshed first. EDF is optimal for uniprocessor preemptive scheduling and is
a standard technique in operating systems and real-time databases.

### Topological Sort — Kahn's Algorithm

> Kahn, A.B. (1962).
> "Topological sorting of large networks."
> *Communications of the ACM*, 5(11), 558–562.
> <https://doi.org/10.1145/368996.369025>

The dependency DAG in `src/dag.rs` uses Kahn's algorithm for topological
ordering and cycle detection. This is standard computer science curriculum
and appears in every major algorithms textbook (Cormen et al., Sedgewick,
Kleinberg & Tardos).

---

## Change Data Capture (CDC)

### PostgreSQL Row-Level Triggers

Row-level `AFTER INSERT/UPDATE/DELETE` triggers have been available in
PostgreSQL since version 6.x (late 1990s). The trigger-based change capture
pattern used in `src/cdc.rs` is a well-established PostgreSQL technique:

- **PostgreSQL documentation**: [CREATE TRIGGER](https://www.postgresql.org/docs/current/sql-createtrigger.html) —
  trigger-based CDC has been a standard pattern for decades.
- PostgreSQL wiki: "Trigger-based Change Data Capture in PostgreSQL."

### Debezium

> Debezium project (Red Hat, open source since 2016).
> <https://debezium.io/>

Debezium implements trigger-based and WAL-based CDC for PostgreSQL and other
databases. The change buffer table pattern (`pg_stream_changes.changes_<oid>`)
follows a similar approach, modified for single-process consumption within
the PostgreSQL backend.

### pgaudit

> pgaudit extension (2015).
> <https://github.com/pgaudit/pgaudit>

Captures DML via `AFTER` row-level triggers for audit logging, demonstrating
the same trigger-based change-capture technique in production since 2015.

---

## Materialized View Refresh

### PostgreSQL REFRESH MATERIALIZED VIEW CONCURRENTLY

> PostgreSQL 9.4 (December 2014, commit `96ef3b8`).
> `src/backend/commands/matview.c`

The snapshot-diff strategy used for recomputation-diff refreshes (where the
full query is re-executed and anti-joined against current storage to compute
inserts and deletes) mirrors the algorithm implemented in PostgreSQL's
`REFRESH MATERIALIZED VIEW CONCURRENTLY`. This PostgreSQL feature predates
all relevant patents and is publicly documented.

### SQL MERGE Statement

> ISO/IEC 9075:2003 (SQL:2003 standard) — `MERGE` statement.
> PostgreSQL 15 (October 2022, commit `7103eba`).

The `MERGE`-based delta application in `src/refresh.rs` uses the
ISO-standard `MERGE` statement, independently implemented by Oracle, SQL
Server, DB2, and PostgreSQL. This is not derived from any vendor-specific
implementation.

---

## General Database Theory

### Relational Algebra

> Codd, E.F. (1970).
> "A Relational Model of Data for Large Shared Data Banks."
> *Communications of the ACM*, 13(6), 377–387.

The operator tree in `src/dvm/parser.rs` models standard relational algebra
operators (select, project, join, aggregate, union). These are foundational
database theory from 1970.

### Semi-Naive Evaluation

> Bancilhon, F. & Ramakrishnan, R. (1986).
> "An Amateur's Introduction to Recursive Query Processing Strategies."
> *Proceedings ACM SIGMOD*, 16–52.

General background for recursive CTE evaluation strategies. PostgreSQL's own
`WITH RECURSIVE` implementation uses iterative fixpoint evaluation based on
these principles.

---

*This document is maintained for attribution and independent-derivation
documentation purposes. It does not constitute legal advice.*
