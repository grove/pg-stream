# Research: Triggers vs. WAL Replication for CDC

This document analyses the architectural tradeoffs between trigger-based
CDC (pg_trickle's default) and WAL logical-replication CDC. It provides
the engineering rationale behind ADR-001 and ADR-002.

> **User-facing CDC documentation** is in [CDC Modes](../CDC_MODES.md).

---

{{#include ../../plans/sql/REPORT_TRIGGERS_VS_REPLICATION.md}}
