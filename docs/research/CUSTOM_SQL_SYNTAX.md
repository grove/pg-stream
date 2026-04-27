# Research: Custom SQL Syntax Options

This document surveys custom-syntax extensions considered for pg_trickle
(e.g. `CREATE STREAM TABLE`) and the tradeoffs against the
current function-based API (`pgtrickle.create_stream_table()`). It is
intended for contributors and language/parser research.

> **User documentation** on SQL functions is in [SQL Reference](../SQL_REFERENCE.md).

---

{{#include ../../plans/sql/REPORT_CUSTOM_SQL_SYNTAX.md}}
