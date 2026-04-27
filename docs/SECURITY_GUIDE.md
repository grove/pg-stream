# Security Guide

This page is the practical security reference for operators of
pg_trickle. It covers roles and grants, what privileges the
extension needs, how stream tables interact with PostgreSQL Row-Level
Security (RLS), how triggers behave under SECURITY DEFINER vs
INVOKER, what to lock down in production, and how to handle secrets
when running the relay.

> **Reporting a vulnerability?** See
> [SECURITY.md](https://github.com/grove/pg-trickle/blob/main/SECURITY.md)
> in the repository root for the disclosure policy.

---

## Threat model in one paragraph

pg_trickle runs *inside* PostgreSQL. Anyone who can connect as a
superuser, or as a role that owns the relevant tables, can already
read, modify, or destroy the data the extension manages — they do
not need pg_trickle to do that. The threats this guide focuses on
are: privilege escalation through stream tables (e.g., a low-privilege
role gaining access to source data via a stream table), accidental
exposure of source data through CDC change buffers, and operational
mistakes (running everything as the postgres superuser).

---

## Roles & grants

### What pg_trickle needs

The extension installs into the `pgtrickle` and `pgtrickle_changes`
schemas. The role that runs `CREATE EXTENSION pg_trickle` must be a
**superuser** because the extension installs background workers, but
day-to-day usage can (and should) be done with a less-privileged
role.

The role that **creates a stream table** needs:

- `USAGE` on the schemas containing source tables.
- `SELECT` on the source tables referenced in the defining query.
- `CREATE` on the schema where the stream table will live.
- `EXECUTE` on the relevant `pgtrickle.*` functions.

### Recommended split

```sql
-- Owner of stream tables (your application's "data engineer" role)
CREATE ROLE st_author NOINHERIT;
GRANT USAGE       ON SCHEMA public TO st_author;
GRANT SELECT      ON ALL TABLES IN SCHEMA public TO st_author;
GRANT CREATE      ON SCHEMA public TO st_author;
GRANT EXECUTE     ON ALL FUNCTIONS IN SCHEMA pgtrickle TO st_author;
GRANT USAGE       ON SCHEMA pgtrickle TO st_author;

-- Read-only consumer (your application)
CREATE ROLE app_reader;
GRANT USAGE       ON SCHEMA public TO app_reader;
GRANT SELECT      ON ALL TABLES IN SCHEMA public TO app_reader;
```

`app_reader` can read stream tables exactly as it reads any other
table — the extension does not require special privileges for
*reading* a stream table.

---

## Stream tables and Row-Level Security (RLS)

A stream table is the **materialized result** of its defining query.
RLS policies on **source** tables are evaluated **at the time the
defining query runs**, which is during refresh, under the **owner's**
identity (not the consumer's).

This has two important consequences:

1. **Stream-table contents do not honour the consumer's RLS context.**
   Two consumers with different RLS contexts will read the same rows
   from the stream table.
2. **You can apply RLS *to the stream table itself*** to filter rows
   per consumer. pg_trickle does not interfere with RLS policies on
   stream tables (they are ordinary heap tables under the hood).

The recommended pattern is therefore:

```sql
-- Define the ST without RLS at the source level
SELECT pgtrickle.create_stream_table(
    'order_summary',
    $$SELECT tenant_id, customer_id, SUM(amount) AS total
      FROM orders GROUP BY tenant_id, customer_id$$
);

-- Apply RLS to the stream table for per-tenant isolation
ALTER TABLE order_summary ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON order_summary
    FOR SELECT USING (tenant_id = current_setting('app.tenant_id')::int);
```

See the [Row-Level Security tutorial](tutorials/ROW_LEVEL_SECURITY.md)
for a complete worked example.

---

## CDC triggers — SECURITY DEFINER vs INVOKER

In trigger CDC mode, pg_trickle installs `AFTER` row-level triggers
on every source table. These triggers run as **SECURITY DEFINER**
under the role that owns the stream table — so they can write to
`pgtrickle_changes.*` regardless of who issued the source-table
write.

**What this means for you:**

- Any role that can write to a source table will indirectly write to
  the corresponding change buffer. That is by design.
- The change buffer table is owned by the stream-table owner. Other
  roles get no implicit access.
- If you revoke `INSERT` on the change buffer, the trigger keeps
  working (it runs as the owner).

In WAL CDC mode, no triggers are installed; capture happens in
PostgreSQL's logical decoding pipeline and is governed by the
`max_replication_slots` and `wal_level` settings.

---

## What change buffers contain

`pgtrickle_changes.changes_<oid>` tables contain the **post-image**
of each changed row, restricted to the columns referenced by the
defining query (columnar tracking). Two consequences:

1. If your defining query references a sensitive column, that
   column ends up in the change buffer.
2. The change buffer table inherits the same `tablespace` and disk
   layout rules as ordinary tables. If you encrypt your data
   directory, the change buffers are encrypted at rest the same way.

You can lock change buffers down further:

```sql
REVOKE ALL ON ALL TABLES IN SCHEMA pgtrickle_changes FROM PUBLIC;
GRANT  SELECT ON ALL TABLES IN SCHEMA pgtrickle_changes TO st_owner;
```

---

## Lock down circular dependencies

`pg_trickle.allow_circular` is `off` by default and should generally
stay that way. Cycles in the DAG are accepted only when this GUC is
on, and only for *monotone* queries — but enabling it widens the
class of queries pg_trickle accepts, which deserves explicit
attention. Set it via `ALTER SYSTEM` and require a superuser to
flip it.

---

## Audit & monitoring

pg_trickle records every refresh in
`pgtrickle.pgt_refresh_history`. For audit:

```sql
-- Last 100 refreshes across the whole installation
SELECT pgt_name, refresh_mode, started_at, finished_at,
       success, rows_in, rows_out, error_message
FROM pgtrickle.pgt_refresh_history
ORDER BY started_at DESC
LIMIT 100;

-- Failed refreshes in the last hour
SELECT * FROM pgtrickle.pgt_refresh_history
WHERE NOT success AND started_at > now() - interval '1 hour';
```

Combine with `pg_audit` for full DDL/DML coverage. The
[Monitoring & Alerting tutorial](tutorials/MONITORING_AND_ALERTING.md)
includes recommended Prometheus alerts.

---

## Secrets in the relay

[`pgtrickle-relay`](RELAY_GUIDE.md) connects to PostgreSQL and to
external messaging systems (NATS, Kafka, Redis, SQS, RabbitMQ,
webhooks). It reads credentials from environment variables and from
its config file, never from PostgreSQL.

Recommendations:

- Run the relay under its **own** PostgreSQL role, with `SELECT`
  and `UPDATE` only on the inbox/outbox tables it touches — not as
  a superuser.
- Inject external-system credentials via the platform's secret
  manager (Kubernetes Secrets, HashiCorp Vault, AWS Secrets Manager).
- Enable TLS for every external endpoint — the relay supports it
  for all backends.

---

## Hardening checklist

- [ ] `pg_trickle.allow_circular = off` unless explicitly needed.
- [ ] Stream tables owned by a dedicated, non-superuser role.
- [ ] `REVOKE ... FROM PUBLIC` on `pgtrickle_changes` if change
      buffers contain sensitive columns.
- [ ] RLS policies applied to stream tables that present per-tenant
      data.
- [ ] Audit logging in place for `pgtrickle.pgt_refresh_history`.
- [ ] Relay running under a dedicated, least-privilege role.
- [ ] External-system credentials managed by a secret store, not
      committed to the relay config file.
- [ ] TLS enabled on all relay endpoints.
- [ ] `pg_trickle.enabled = on` only in environments that should
      run refreshes (you can disable extension behaviour without
      uninstalling it).

---

**See also:**
[Row-Level Security tutorial](tutorials/ROW_LEVEL_SECURITY.md) ·
[Pre-Deployment Checklist](PRE_DEPLOYMENT.md) ·
[Configuration](CONFIGURATION.md) ·
[Relay Service](RELAY_GUIDE.md) ·
[SECURITY policy](https://github.com/grove/pg-trickle/blob/main/SECURITY.md)
