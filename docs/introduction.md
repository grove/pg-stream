# pg_trickle

**pg_trickle** is a PostgreSQL 18 extension that adds *self-maintaining*
materialized views — **stream tables** — and keeps them up to date
incrementally as the underlying data changes. No external streaming
engine, no sidecars, no bespoke refresh pipeline. Just install the
extension and write SQL.

```sql
SELECT pgtrickle.create_stream_table(
    name     => 'active_orders',
    query    => 'SELECT * FROM orders WHERE status = ''active''',
    schedule => '30s'
);

INSERT INTO orders (id, status) VALUES (42, 'active');
SELECT count(*) FROM active_orders;  -- 1, automatically
```

> **New here?** Read **[What is pg_trickle?](ESSENCE.md)** for the
> plain-language overview, or jump to the
> **[5-Minute Quickstart](QUICKSTART_5MIN.md)** to try it.

---

## Choose your path

| Persona | Start here |
|---|---|
| **Curious / evaluator** | [What is pg_trickle?](ESSENCE.md) → [Use Cases](USE_CASES.md) → [Comparisons](COMPARISONS.md) → [Playground](PLAYGROUND.md) |
| **Application developer** | [5-Minute Quickstart](QUICKSTART_5MIN.md) → [Getting Started tutorial](GETTING_STARTED.md) → [Patterns](PATTERNS.md) → [SQL Reference](SQL_REFERENCE.md) |
| **DBA / SRE** | [Pre-Deployment Checklist](PRE_DEPLOYMENT.md) → [Configuration](CONFIGURATION.md) → [Troubleshooting](TROUBLESHOOTING.md) → [Capacity Planning](CAPACITY_PLANNING.md) |
| **Data / analytics engineer** | [Use Cases](USE_CASES.md) → [dbt integration](integrations/dbt.md) → [Migrating from materialized views](tutorials/MIGRATING_FROM_MATERIALIZED_VIEWS.md) |
| **Confused by jargon** | [Glossary](GLOSSARY.md) |

---

## What's new

See [What's New](WHATS_NEW.md) for a curated summary of recent
releases, or the [Changelog](changelog.md) for the full history.

---

## Project & licensing

- Written in Rust using [pgrx](https://github.com/pgcentralfoundation/pgrx).
- Targets PostgreSQL 18.
- Apache 2.0 licensed.
- Repository: <https://github.com/grove/pg-trickle>
- [Project history](PROJECT_HISTORY.md) ·
  [Roadmap](roadmap.md) ·
  [Contributing](contributing.md) ·
  [Security policy](security.md)
