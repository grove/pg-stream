[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# Making pg_trickle Work Through PgBouncer

## Connection pooling, session state, and the gotchas nobody warns you about

---

PgBouncer is the standard connection pooler for PostgreSQL. It sits between your application and the database, multiplexing hundreds or thousands of application connections onto a smaller pool of database connections.

pg_trickle works with PgBouncer. But there are configuration requirements that, if you get wrong, produce confusing failures — stream tables that don't refresh, LISTEN/NOTIFY that doesn't fire, and CDC triggers that silently miss changes.

Here's what you need to know.

---

## Pool Modes and pg_trickle

PgBouncer has three pool modes:

| Mode | How it works | pg_trickle compatibility |
|---|---|---|
| **session** | One PgBouncer connection = one database connection for the session lifetime | ✅ Full compatibility |
| **transaction** | PgBouncer returns the connection to the pool after each transaction | ⚠️ Works with caveats |
| **statement** | PgBouncer returns the connection after each statement | ❌ Not compatible |

### Session mode

Session mode is fully compatible. Each application connection gets a dedicated database connection. Session-level state (prepared statements, temp tables, advisory locks, LISTEN) works normally.

If you can afford the connection overhead, session mode is the simplest option.

### Transaction mode (the common case)

Transaction mode is what most production deployments use. It's also where the gotchas live.

**What works:**
- `pgtrickle.create_stream_table()` — runs in a single transaction, works fine.
- `pgtrickle.alter_stream_table()` — same, single transaction.
- `pgtrickle.refresh_stream_table()` — single transaction.
- `pgtrickle.pgt_status()` — single query, works fine.
- Reading from stream tables — normal SELECT queries, no issues.
- CDC triggers — fire inside the source transaction, work fine.

**What doesn't work without configuration:**
- `LISTEN/NOTIFY` — requires a persistent connection. PgBouncer in transaction mode recycles the connection after the transaction, dropping the LISTEN registration.
- Advisory locks (`pg_advisory_lock`) — used by the relay for HA leader election. The lock is released when the connection is returned to the pool, which may happen unexpectedly in transaction mode.
- The background worker — this connects directly to PostgreSQL, bypassing PgBouncer entirely. Not affected by pool mode.

### Statement mode

Statement mode returns the connection to the pool after every statement. This breaks multi-statement transactions, which pg_trickle's internal operations require. Don't use statement mode with pg_trickle.

---

## The Background Worker Bypass

pg_trickle's background worker — the process that runs the scheduler and executes refresh cycles — connects directly to PostgreSQL using the `shared_preload_libraries` mechanism. It doesn't go through PgBouncer.

This means:
- The scheduler works regardless of your PgBouncer configuration.
- Refresh cycles are not affected by pool mode.
- The worker uses its own dedicated connection, separate from the application pool.

The background worker's connection is configured via PostgreSQL's `pg_trickle.database` GUC, not via the PgBouncer connection string. Make sure this points to PostgreSQL directly:

```sql
-- postgresql.conf
pg_trickle.database = 'mydb'  -- Direct connection, not through PgBouncer
```

---

## LISTEN/NOTIFY Through PgBouncer

If your application uses pg_trickle's reactive subscriptions (LISTEN/NOTIFY for stream table changes), you need a persistent connection for the LISTEN registration.

**Option 1: Dedicated non-pooled connection.**

Most PgBouncer configurations allow specifying a pool that uses session mode:

```ini
# pgbouncer.ini
[databases]
mydb = host=localhost port=5432 dbname=mydb
mydb_listen = host=localhost port=5432 dbname=mydb pool_mode=session pool_size=5
```

Your application uses `mydb` (transaction mode) for normal queries and `mydb_listen` (session mode) for LISTEN connections.

**Option 2: Use the outbox instead of LISTEN/NOTIFY.**

If you don't want to manage a separate connection pool, skip LISTEN/NOTIFY and use the outbox + relay pattern. The relay maintains its own persistent connection to PostgreSQL (bypassing PgBouncer) and delivers notifications to your application via Kafka, NATS, or webhooks.

---

## Advisory Locks and the Relay

The relay uses advisory locks for leader election. In transaction mode, PgBouncer might return the connection to the pool between transactions, releasing the advisory lock.

**Solution:** The relay should connect directly to PostgreSQL, not through PgBouncer. Configure the relay's `postgres_url` to point to the PostgreSQL port, not the PgBouncer port:

```toml
# relay.toml
[global]
# Direct connection to PostgreSQL (port 5432), not PgBouncer (port 6432)
postgres_url = "postgres://user:pass@localhost:5432/mydb"
```

---

## Prepared Statements

PgBouncer in transaction mode can optionally disable server-side prepared statements (the default behavior) or support them with `prepared_statements` mode.

pg_trickle's SQL functions don't use prepared statements internally — they use SPI (Server Programming Interface), which executes queries directly. So this setting doesn't affect pg_trickle's operation.

However, if your application uses prepared statements to query stream tables (e.g., `PREPARE get_orders AS SELECT * FROM order_summary WHERE region = $1`), you need PgBouncer's `prepared_statements` mode or session mode.

---

## Configuration Checklist

```ini
# pgbouncer.ini — recommended for pg_trickle

[databases]
mydb = host=localhost port=5432 dbname=mydb

[pgbouncer]
pool_mode = transaction
max_client_conn = 200
default_pool_size = 20

# Important: allow DEALLOCATE ALL (pg_trickle cleanup)
server_reset_query = DISCARD ALL
```

```sql
-- postgresql.conf — background worker connects directly
shared_preload_libraries = 'pg_trickle'
pg_trickle.enabled = on
pg_trickle.database = 'mydb'
```

```toml
# relay.toml — relay connects directly, not through PgBouncer
[global]
postgres_url = "postgres://user:pass@localhost:5432/mydb"
```

---

## Monitoring Through PgBouncer

All of pg_trickle's monitoring queries work through PgBouncer in transaction mode:

```sql
-- These all work through PgBouncer
SELECT * FROM pgtrickle.pgt_status();
SELECT * FROM pgtrickle.health_check();
SELECT * FROM pgtrickle.change_buffer_sizes();
SELECT * FROM pgtrickle.st_refresh_stats();
```

They're single-transaction, read-only queries with no session state requirements.

---

## The Short Version

| Component | Through PgBouncer? | Notes |
|---|---|---|
| Application reads from stream tables | ✅ Yes | Normal SELECT queries |
| Application creates/alters/drops stream tables | ✅ Yes | Single-transaction DDL |
| Application LISTEN for notifications | ⚠️ Session mode only | Or use outbox + relay |
| Background worker (scheduler) | ❌ Direct to PostgreSQL | Automatic, no configuration |
| Relay | ❌ Direct to PostgreSQL | Configure postgres_url to skip PgBouncer |
| Monitoring queries | ✅ Yes | Transaction mode is fine |

pg_trickle works with PgBouncer in transaction mode for all common operations. The two things that need direct connections — the background worker and the relay — already bypass PgBouncer by design. The only thing you need to plan for is LISTEN/NOTIFY, and that has straightforward workarounds.
