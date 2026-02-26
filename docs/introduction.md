# pg_stream

**pg_stream** is a PostgreSQL 18 extension that turns ordinary SQL views into
self-maintaining stream tables that refresh automatically whenever their source
data changes.

```sql
-- Declare a stream table — a view that maintains itself
SELECT pgstream.create_stream_table(
    'public',
    'active_orders',
    'SELECT * FROM orders WHERE status = ''active''',
    '{"schedule": "30s"}'::jsonb
);

-- Insert a row — the stream table updates automatically
INSERT INTO orders (id, status) VALUES (42, 'active');
SELECT count(*) FROM active_orders;  -- 1
```

**Key features:**

| Feature | Description |
|---------|-------------|
| Automatic refresh | Defined schedule or CDC-triggered on every write |
| Differential maintenance | Only the changed rows are recomputed (semi-naive + DRed) |
| Cascading DAG | Stream tables that query stream tables propagate changes downstream |
| Hybrid CDC | Row-level triggers (default) or WAL-based change capture |
| CloudNativePG-ready | Ships as a Docker image for Kubernetes deployments |

---

## Explore this documentation

- **[Getting Started](GETTING_STARTED.md)** — build a three-layer DAG from scratch in minutes
- **[SQL Reference](SQL_REFERENCE.md)** — every function and option
- **[Architecture](ARCHITECTURE.md)** — how the engine works internally
- **[Configuration](CONFIGURATION.md)** — GUC variables and tuning

---

## Source & releases

- Repository: [github.com/grove/pg-stream](https://github.com/grove/pg-stream)
- Install instructions: [INSTALL.md](https://github.com/grove/pg-stream/blob/main/INSTALL.md)
- Changelog: [CHANGELOG.md](https://github.com/grove/pg-stream/blob/main/CHANGELOG.md)
- Roadmap: [ROADMAP.md](https://github.com/grove/pg-stream/blob/main/ROADMAP.md)
