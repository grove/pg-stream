# Security Model — pg_trickle

> **Version:** v0.40.0  
> **Audience:** Database administrators, security engineers, and operators
> deploying pg_trickle in production environments.

---

## Overview

pg_trickle is a PostgreSQL extension that runs inside the PostgreSQL server
process. Its security surface spans:

1. **SQL-callable functions** — the `pgtrickle` schema
2. **CDC trigger bodies** — fire as `SECURITY DEFINER` to write to the change
   buffer, which lives in the `pgtrickle_changes` schema
3. **Background worker** — a scheduler process that runs as the PostgreSQL
   superuser
4. **Relay credentials** — connection strings used by `pgtrickle-relay` to
   read from published stream tables and forward changes downstream
5. **Secret handling** — credentials in configuration files, environment
   variables, and shell history

---

## SECURITY DEFINER Usage

### CDC trigger functions

All CDC trigger functions created by `create_stream_table()` are
`SECURITY DEFINER` and owned by the superuser. This is necessary because:

- The change buffer tables (`pgtrickle_changes.changes_<oid>`) are owned by
  the superuser.
- DML sessions on source tables must be able to write to the change buffer
  without being granted direct access to `pgtrickle_changes`.

**Implication:** Any user with `INSERT`, `UPDATE`, or `DELETE` access to a
source table will indirectly write to the change buffer. This is by design —
the trigger captures every committed change regardless of who made it.

### `search_path` hardening

All pg_trickle `SECURITY DEFINER` functions and trigger procedures set
`search_path = pgtrickle, pgtrickle_changes, pg_catalog, pg_temp` at creation
time to prevent search-path injection attacks. This follows PostgreSQL best
practice (see [CWE-89](https://cwe.mitre.org/data/definitions/89.html) and the
PostgreSQL docs on
[writing SECURITY DEFINER functions](https://www.postgresql.org/docs/current/sql-createfunction.html#SQL-CREATEFUNCTION-SECURITY)).

To verify:

```sql
SELECT proname, prosecdef, proconfig
FROM pg_proc
WHERE pronamespace = 'pgtrickle'::regnamespace
  AND prosecdef
ORDER BY proname;
```

---

## Row-Level Security (RLS)

pg_trickle **does not enforce RLS** on stream tables by default. Stream tables
are ordinary PostgreSQL tables — RLS can be applied to them with `ALTER TABLE
... ENABLE ROW LEVEL SECURITY` as with any table.

**Important caveats:**

- The background worker refreshes stream tables as the superuser. RLS policies
  do **not** apply to the superuser by default.
- To enforce RLS during refresh, use `FORCE ROW LEVEL SECURITY` on the stream
  table and ensure the superuser is explicitly covered by a permissive policy.
- The defining query for a stream table runs as the superuser regardless of who
  created the stream table. This means RLS on **source tables** is bypassed
  during refresh unless those tables also use `FORCE ROW LEVEL SECURITY`.

---

## CDC Buffer Access

The `pgtrickle_changes` schema contains one unlogged table per source table
OID. These tables are only meant for internal pg_trickle use:

- **Do not grant** `SELECT`, `INSERT`, `UPDATE`, or `DELETE` on
  `pgtrickle_changes.*` to application users.
- **Do not include** `pgtrickle_changes.*` in logical replication publications
  (they are UNLOGGED by default and thus not replicatable).
- The scheduler reads and truncates change buffer tables during each refresh.
  External reads during active refresh may observe partial or inconsistent
  intermediate state.

---

## TRUNCATE Semantics

When a FULL refresh completes, pg_trickle uses `TRUNCATE pgtrickle_changes.changes_<oid>`
(or `DELETE`, depending on the `pg_trickle.cleanup_use_truncate` GUC) to clear
the change buffer after consuming all pending changes.

**TRUNCATE behaviour:**

- Acquires `ACCESS EXCLUSIVE` lock on the change buffer table for the duration
  of the TRUNCATE. This briefly blocks concurrent DML on the **change buffer**
  (not the source table). Source table DML is unaffected.
- Is WAL-logged if the change buffer table is `LOGGED`, or simply resets the
  relation's fork if `UNLOGGED` (the default).
- When `pg_trickle.cdc_paused = on`, CDC trigger bodies return `NULL`
  regardless of this setting — the change buffer is not written, so there
  is nothing to TRUNCATE.

### `cdc_paused` vs `drain()` semantics

| Mechanism | Effect | Change buffer | Stream table |
|-----------|--------|---------------|--------------|
| `pg_trickle.cdc_paused = on` | New changes are discarded (triggers return NULL) | Not written | Stale |
| `pgtrickle.drain(timeout)` | Wait for in-flight refreshes to finish; stop scheduling new ones | Unchanged | Consistent after drain |
| `pg_trickle.enabled = off` | Disable the entire scheduler | Accumulates | Stale |

When resuming from `cdc_paused`, call
`SELECT pgtrickle.reinitialize('schema.stream_table')` to restore consistency,
since changes that arrived during the pause were discarded.

---

## Relay Credentials

`pgtrickle-relay` connects to PostgreSQL to read from published stream tables.
It requires a connection string with `replication = database`.

### Recommended credential storage (most secure first)

1. **`.pgpass` file** — `~/.pgpass` (mode 0600) or the path set by `PGPASSFILE`.
   The relay reads libpq environment variables automatically.
   ```
   # ~/.pgpass
   hostname:5432:dbname:relay_user:s3cr3t
   ```

2. **`pg_service.conf`** — define a named service:
   ```ini
   # ~/.pg_service.conf (or /etc/pg_service.conf)
   [pgtrickle_relay]
   host=hostname
   port=5432
   dbname=mydb
   user=relay_user
   password=s3cr3t
   ```
   Then set `PGSERVICEFILE` and use `service=pgtrickle_relay` in the relay config.

3. **Environment variable** — `PGPASSWORD` is read by libpq if set. Do not
   persist this in systemd unit files or shell scripts where it would be
   world-readable. Use a secrets manager (Vault, AWS Secrets Manager, etc.)
   to inject it at runtime.

4. **Relay config file** — only as a last resort. If you must store credentials
   in `relay.toml`:
   - Set file permissions to `0600` (`chmod 600 relay.toml`).
   - Ensure the file is excluded from version control (`.gitignore`).
   - Document the rotation procedure.

### What to avoid

- Passing credentials on the command line (`--password=...`): they appear in
  `ps aux` output and shell history.
- Storing credentials in world-readable files or environment files with
  permissions wider than `0640`.
- Using superuser credentials for the relay. Create a dedicated replication
  role: `CREATE ROLE relay_user WITH LOGIN REPLICATION`.

---

## Background Worker Privilege

The scheduler background worker runs with full superuser privilege because
PostgreSQL requires it for dynamic background worker registration. pg_trickle
uses this privilege only to:

- Read `pgtrickle.*` catalog tables
- Write to `pgtrickle_changes.*` change buffers
- Execute MERGE/INSERT/UPDATE/DELETE on stream tables
- Register and manage dynamic refresh workers

The worker does **not**:

- Write to user application tables (except stream tables owned by the extension)
- Execute arbitrary SQL from untrusted input
- Access credentials or secrets at runtime

---

## Incident Response: TRUNCATE Semantics Under Pause

When `cdc_paused` was active during an incident:

1. `SELECT pgtrickle.cdc_pause_status()` — confirm pause mode and scope.
2. Set `cdc_paused = off` to re-enable captures.
3. For each affected stream table, call
   `SELECT pgtrickle.reinitialize('schema.table_name')` to trigger a full
   resync from source. In-flight refresh will overwrite any stale data.
4. Monitor `pgtrickle.health_check()` until all tables report `status = 'ok'`.

---

## v1.0 Supply-Chain Preparation

The following supply-chain controls are staged for v1.0 (tracked by O40-9):

- [ ] SBOM generation (`cargo sbom` or `cyclonedx-rust-cargo`)
- [ ] Artifact signing (sigstore/cosign) for Docker images and PGXN archives
- [ ] Provenance attestation via `actions/attest-build-provenance`
- [ ] Reproducible builds verification (`cargo auditable`)

---

## Related Documentation

- [docs/CONFIGURATION.md](CONFIGURATION.md) — GUC reference
- [docs/RUNBOOK_DRAIN.md](RUNBOOK_DRAIN.md) — drain-mode operational guide
- [docs/RELAY_GUIDE.md](RELAY_GUIDE.md) — relay deployment guide
- [docs/GUC_CATALOG.md](GUC_CATALOG.md) — generated GUC catalog
- [docs/SQL_API_CATALOG.md](SQL_API_CATALOG.md) — generated SQL API catalog
