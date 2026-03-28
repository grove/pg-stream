# Upgrading pg_trickle

This guide covers upgrading pg_trickle from one version to another.

---

## Quick Upgrade (Recommended)

```sql
-- 1. Check current version
SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle';

-- 2. Replace the binary files (.so/.dylib, .control, .sql)
--    See the installation method below for your platform.

-- 3. Restart PostgreSQL (required for shared library changes)
--    sudo systemctl restart postgresql

-- 4. Run the upgrade in each database that has pg_trickle installed
ALTER EXTENSION pg_trickle UPDATE;

-- 5. Verify the upgrade
SELECT pgtrickle.version();
SELECT * FROM pgtrickle.health_check();
```

---

## Step-by-Step Instructions

### 1. Check Current Version

```sql
SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle';
-- Returns your current installed version, e.g. '0.9.0'
```

### 2. Install New Binary Files

Replace the extension files in your PostgreSQL installation directory.
The method depends on how you originally installed pg_trickle.

**From release tarball:**

```bash
# Replace <new-version> with the target release, for example 0.2.3
curl -LO https://github.com/getretake/pg_trickle/releases/download/v<new-version>/pg_trickle-<new-version>-pg18-linux-amd64.tar.gz
tar xzf pg_trickle-<new-version>-pg18-linux-amd64.tar.gz

# Copy files to PostgreSQL directories
sudo cp pg_trickle-<new-version>-pg18-linux-amd64/lib/* $(pg_config --pkglibdir)/
sudo cp pg_trickle-<new-version>-pg18-linux-amd64/extension/* $(pg_config --sharedir)/extension/
```

**From source (cargo-pgrx):**

```bash
cargo pgrx install --release
```

### 3. Restart PostgreSQL

The shared library (`.so` / `.dylib`) is loaded at server start via
`shared_preload_libraries`. A restart is required for the new binary to
take effect.

```bash
sudo systemctl restart postgresql
# or on macOS with Homebrew:
brew services restart postgresql@18
```

### 4. Run ALTER EXTENSION UPDATE

Connect to **each database** where pg_trickle is installed and run:

```sql
ALTER EXTENSION pg_trickle UPDATE;
```

This executes the upgrade migration scripts in order (for example,
`pg_trickle--0.5.0--0.6.0.sql` → `pg_trickle--0.6.0--0.7.0.sql`).
PostgreSQL automatically determines the full upgrade chain from your current
version to the new `default_version`.

### 5. Verify the Upgrade

```sql
-- Check version
SELECT pgtrickle.version();

-- Run health check
SELECT * FROM pgtrickle.health_check();

-- Verify stream tables are intact
SELECT * FROM pgtrickle.stream_tables_info;

-- Test a refresh
SELECT pgtrickle.refresh_stream_table('your_stream_table');
```

---

## Version-Specific Notes

### 0.1.3 → 0.2.0

**New functions added:**
- `pgtrickle.list_sources(name)` — list source tables for a stream table
- `pgtrickle.change_buffer_sizes()` — inspect CDC change buffer sizes
- `pgtrickle.health_check()` — diagnostic health checks
- `pgtrickle.dependency_tree()` — visualize the dependency DAG
- `pgtrickle.trigger_inventory()` — audit CDC triggers
- `pgtrickle.refresh_timeline(max_rows)` — refresh history
- `pgtrickle.diamond_groups()` — diamond dependency group info
- `pgtrickle.version()` — extension version string
- `pgtrickle.pgt_ivm_apply_delta(...)` — internal IVM delta application
- `pgtrickle.pgt_ivm_handle_truncate(...)` — internal TRUNCATE handler
- `pgtrickle._signal_launcher_rescan()` — internal launcher signal

**No schema changes** to `pgtrickle.pgt_stream_tables` or
`pgtrickle.pgt_dependencies` catalog tables.

**No breaking changes.** All v0.1.3 functions and views continue to work
as before.

### 0.2.0 → 0.2.1

**Three new catalog columns** added to `pgtrickle.pgt_stream_tables`:

| Column | Type | Default | Purpose |
|--------|------|---------|--------|
| `topk_offset` | `INT` | `NULL` | Pre-provisioned for paged TopK OFFSET (activated in v0.2.2) |
| `has_keyless_source` | `BOOLEAN NOT NULL` | `FALSE` | EC-06: keyless source flag; switches apply strategy from MERGE to counted DELETE |
| `function_hashes` | `TEXT` | `NULL` | EC-16: stores MD5 hashes of referenced function bodies for change detection |

The migration script (`pg_trickle--0.2.0--0.2.1.sql`) adds these columns
via `ALTER TABLE … ADD COLUMN IF NOT EXISTS`.

**No breaking changes.** All v0.2.0 functions, views, and event triggers
continue to work as before.

**What's also new:**
- Upgrade migration safety infrastructure (scripts, CI, E2E tests)
- GitHub Pages book expansion (6 new documentation pages)
- User-facing upgrade guide (this document)

### 0.2.1 → 0.2.2

**No catalog table DDL changes.** The `topk_offset` column needed for paged
TopK was already added in v0.2.1.

**Two SQL function updates** are applied by
`pg_trickle--0.2.1--0.2.2.sql`:

- `pgtrickle.create_stream_table(...)`
  - default `schedule` changes from `'1m'` to `'calculated'`
  - default `refresh_mode` changes from `'DIFFERENTIAL'` to `'AUTO'`
- `pgtrickle.alter_stream_table(...)`
  - adds the optional `query` parameter used by ALTER QUERY support

Because PostgreSQL stores argument defaults and function signatures in
`pg_proc`, the migration script must `DROP FUNCTION` and recreate both
signatures during `ALTER EXTENSION ... UPDATE`.

**Behavioral notes:**

- Existing stream tables keep their current catalog values. The migration only
  changes the defaults used by future `create_stream_table(...)` calls.
- Existing applications can opt a table into the new defaults explicitly via
  `pgtrickle.alter_stream_table(...)` after the upgrade.
- After installing the new binary and restarting PostgreSQL, the scheduler now
  warns if the shared library version and SQL-installed extension version do
  not match. This helps detect stale `.so`/`.dylib` files after partial
  upgrades.

### 0.2.2 → 0.2.3

**One new catalog column** is added to `pgtrickle.pgt_stream_tables`:

| Column | Type | Default | Purpose |
|--------|------|---------|--------|
| `requested_cdc_mode` | `TEXT` | `NULL` | Optional per-stream-table CDC override (`'auto'`, `'trigger'`, `'wal'`) |

**The upgrade script also recreates two SQL functions**:

- `pgtrickle.create_stream_table(...)`
  - adds the optional `cdc_mode` parameter
- `pgtrickle.alter_stream_table(...)`
  - adds the optional `cdc_mode` parameter

**Monitoring view updates:**

- `pgtrickle.pg_stat_stream_tables` gains the `cdc_modes` column
- `pgtrickle.pgt_cdc_status` is added for per-source CDC visibility

Because PostgreSQL stores function signatures and defaults in `pg_proc`, the
upgrade script drops and recreates both lifecycle functions during
`ALTER EXTENSION ... UPDATE`.

### 0.6.0 → 0.7.0

**One new catalog column** is added to `pgtrickle.pgt_stream_tables`:

| Column | Type | Default | Purpose |
|--------|------|---------|--------|
| `last_fixpoint_iterations` | `INT` | `NULL` | Records how many rounds the last circular-dependency fixpoint run required |

**Two new catalog tables** are added:

| Table | Purpose |
|------|---------|
| `pgtrickle.pgt_watermarks` | Stores per-source watermark progress reported by external loaders |
| `pgtrickle.pgt_watermark_groups` | Stores groups of sources that must stay temporally aligned before refresh |

**The upgrade script also updates and adds SQL functions**:

- Recreates `pgtrickle.pgt_status()` so the result includes `scc_id`
- Adds `pgtrickle.pgt_scc_status()` for circular-dependency monitoring
- Adds `pgtrickle.advance_watermark(source, watermark)`
- Adds `pgtrickle.create_watermark_group(name, sources[], tolerance_secs)`
- Adds `pgtrickle.drop_watermark_group(name)`
- Adds `pgtrickle.watermarks()`
- Adds `pgtrickle.watermark_groups()`
- Adds `pgtrickle.watermark_status()`

**Behavioral notes:**

- Circular stream table dependencies can now run to convergence when
  `pg_trickle.allow_circular = true` and every member of the cycle is safe for
  monotone DIFFERENTIAL refresh.
- The scheduler can now hold back refreshes until related source tables are
  aligned within a configured watermark tolerance.
- Existing non-circular stream tables continue to work as before. The new
  catalog objects are additive.

### 0.7.0 → 0.8.0

**No catalog schema changes.** The upgrade migration script contains no DDL.

**New operational features:**

- `pg_dump` / `pg_restore` support: stream tables are now safely exported and
  re-connected after restore without manual intervention.
- Connection pooler opt-in was introduced at the per-stream level (superseded
  by the more comprehensive `pooler_compatibility_mode` added in v0.10.0).

**No breaking changes.** All v0.7.0 functions, views, and event triggers
continue to work as before.

### 0.8.0 → 0.9.0

**No catalog schema DDL changes** to `pgtrickle.pgt_stream_tables` or the
dependency catalog.

**New API function added:**

- `pgtrickle.restore_stream_tables()` — re-installs CDC triggers and
  re-registers stream tables after a `pg_restore` from a `pg_dump`.

**Hidden auxiliary columns for AVG / STDDEV / VAR aggregates.** Stream tables
using these aggregates will automatically receive hidden `__pgt_aux_*`
columns on the next refresh after upgrading. No manual action is needed —
pg_trickle detects missing auxiliary columns and performs a single full
reinitialise to add them.

**Behavioral notes:**

- COUNT, SUM, and AVG now update in constant time (O(changed rows)) instead
  of rescanning the whole group.
- STDDEV and VAR variants likewise update in O(changed rows) via hidden
  sum-of-squares auxiliary columns.
- MIN/MAX still requires a group rescan only when the deleted value is the
  current extreme.
- Refresh groups (`create_refresh_group`, `drop_refresh_group`,
  `refresh_groups()`) are available starting from this version.

### 0.9.0 → 0.10.0

**Two new catalog columns** added to `pgtrickle.pgt_stream_tables`:

| Column | Type | Default | Purpose |
|--------|------|---------|--------|
| `pooler_compatibility_mode` | `BOOLEAN NOT NULL` | `FALSE` | Disables prepared statements and NOTIFY for this stream table — required when accessed through PgBouncer in transaction-pool mode |
| `refresh_tier` | `TEXT NOT NULL` | `'hot'` | Tiered scheduling tier: `hot`, `warm`, `cold`, or `frozen` |

**One new catalog table** is added:

| Table | Purpose |
|------|---------|
| `pgtrickle.pgt_refresh_groups` | Stores refresh groups for snapshot-consistent multi-table refresh |

**The upgrade script also updates and adds SQL functions**:

- `pgtrickle.create_stream_table(...)` gains the `pooler_compatibility_mode` parameter
- `pgtrickle.create_stream_table_if_not_exists(...)` likewise
- `pgtrickle.create_or_replace_stream_table(...)` likewise
- `pgtrickle.alter_stream_table(...)` likewise
- Adds `pgtrickle.create_refresh_group(name, members, isolation)`
- Adds `pgtrickle.drop_refresh_group(name)`
- Adds `pgtrickle.refresh_groups()` — lists all declared groups

**Behavioral notes:**

- `pooler_compatibility_mode` defaults to `false`. Existing stream tables are
  unaffected. Enable it only for stream tables accessed through PgBouncer
  transaction-mode pooling.
- `pg_trickle.auto_backoff` now defaults to `on` (was `off`). The backoff
  threshold is raised from 80 % → 95 % and the maximum slowdown is capped at
  8× (was 64×). If you relied on the old opt-in behaviour, set
  `pg_trickle.auto_backoff = off` explicitly.
- `diamond_consistency` now defaults to `'atomic'` for new stream tables
  (was `'none'`). Existing stream tables keep their current setting.
- The scheduler now uses row-level locking for concurrency control instead of
  session-level advisory locks, making pg_trickle compatible with PgBouncer
  transaction-pool and similar connection poolers.
- Statistical aggregates (`CORR`, `COVAR_*`, `REGR_*`) now update
  incrementally using Welford-style accumulation, no longer requiring a
  group rescan.
- Materialized view sources can now be used in DIFFERENTIAL mode when
  `pg_trickle.matview_polling = on` is set.
- Recursive CTE stream tables with DELETE/UPDATE now use the Delete-and-Rederive
  algorithm (O(delta) instead of O(n)).

### 0.10.0 → 0.11.0

**New catalog columns** added to `pgtrickle.pgt_stream_tables`:

| Column | Type | Default | Purpose |
|--------|------|---------|--------|
| `effective_refresh_mode` | `TEXT` | `NULL` | Actual refresh mode used in the last cycle (FULL / DIFFERENTIAL / APPEND_ONLY / TOP_K / NO_DATA); populated by the scheduler after each completed refresh |
| `fuse_mode` | `TEXT NOT NULL` | `'off'` | Circuit-breaker mode: `off`, `on`, or `auto` |
| `fuse_state` | `TEXT NOT NULL` | `'armed'` | Circuit-breaker state: `armed`, `blown`, or `disabled` |
| `fuse_ceiling` | `BIGINT` | `NULL` | Maximum change-row count that can pass through in one refresh before the fuse blows; `NULL` = unlimited |
| `fuse_sensitivity` | `INT` | `NULL` | Sensitivity multiplier for auto-fuse detection |
| `blown_at` | `TIMESTAMPTZ` | `NULL` | Timestamp when the fuse last triggered |
| `blow_reason` | `TEXT` | `NULL` | Human-readable reason the fuse blew |
| `st_partition_key` | `TEXT` | `NULL` | Partition key column for declaratively partitioned stream tables; `NULL` = not partitioned |

**Updated function signatures** — existing calls continue to work because new parameters all have defaults:

- `pgtrickle.create_stream_table(...)` gains `partition_by TEXT DEFAULT NULL`
- `pgtrickle.create_stream_table_if_not_exists(...)` likewise
- `pgtrickle.create_or_replace_stream_table(...)` likewise
- `pgtrickle.alter_stream_table(...)` gains `fuse TEXT DEFAULT NULL`, `fuse_ceiling BIGINT DEFAULT NULL`, `fuse_sensitivity INT DEFAULT NULL`

**New functions**:

- `pgtrickle.reset_fuse(name TEXT, action TEXT DEFAULT 'apply')` — clear a blown fuse and resume scheduling
- `pgtrickle.fuse_status()` — returns circuit-breaker state for every stream table
- `pgtrickle.explain_refresh_mode(name TEXT)` — shows configured mode, effective mode, and the reason for any downgrade

**Behavioral notes:**

- Event-driven wake (`pg_trickle.event_driven_wake`) is `on` by default — the background worker now wakes within ~15 ms of a source-table write instead of waiting up to 500 ms.
- Stream-table-to-stream-table chains now refresh **incrementally** — downstream tables receive a small insert/delete delta rather than cascading full refreshes.
- `pg_trickle.tiered_scheduling` now defaults to `on`.
- Declaratively partitioned stream tables are supported via `partition_by` — the refresh MERGE is automatically restricted to only the changed partitions.

### 0.11.0 → 0.12.0

**No schema changes.** This release adds four new diagnostic SQL functions only:

| Function | Returns | Purpose |
|----------|---------|--------|
| `pgtrickle.explain_query_rewrite(query TEXT)` | `TABLE(pass_name TEXT, changed BOOL, sql_after TEXT)` | Walk a query through every DVM rewrite pass to see how pg_trickle transforms it |
| `pgtrickle.diagnose_errors(name TEXT)` | `TABLE(event_time TIMESTAMPTZ, error_type TEXT, error_message TEXT, remediation TEXT)` | Last 5 FAILED refresh events with error classification and suggested fixes |
| `pgtrickle.list_auxiliary_columns(name TEXT)` | `TABLE(column_name TEXT, data_type TEXT, purpose TEXT)` | List all hidden `__pgt_*` auxiliary columns on a stream table's storage relation |
| `pgtrickle.validate_query(query TEXT)` | `TABLE(valid BOOL, mode TEXT, reason TEXT)` | Parse and validate a query for stream-table compatibility without creating one |

**Behavioral notes:**

- The incremental engine now handles multi-table join deletes correctly — phantom rows after simultaneous deletes from multiple join sides no longer occur.
- Stream-table-to-stream-table row identity is now computed consistently between the change buffer and the downstream table, eliminating stale duplicate rows after upstream UPDATEs.
- `pg_trickle.tiered_scheduling` defaults to `on` (same as 0.11.0 runtime behaviour; this release makes it the explicit default).

---

## Supported Upgrade Paths

The following migration hops are available. PostgreSQL chains them
automatically when you run `ALTER EXTENSION pg_trickle UPDATE`.

| From | To | Script |
|------|----|--------|
| 0.1.3 | 0.2.0 | `pg_trickle--0.1.3--0.2.0.sql` |
| 0.2.0 | 0.2.1 | `pg_trickle--0.2.0--0.2.1.sql` |
| 0.2.1 | 0.2.2 | `pg_trickle--0.2.1--0.2.2.sql` |
| 0.2.2 | 0.2.3 | `pg_trickle--0.2.2--0.2.3.sql` |
| 0.2.3 | 0.3.0 | `pg_trickle--0.2.3--0.3.0.sql` |
| 0.3.0 | 0.4.0 | `pg_trickle--0.3.0--0.4.0.sql` |
| 0.4.0 | 0.5.0 | `pg_trickle--0.4.0--0.5.0.sql` |
| 0.5.0 | 0.6.0 | `pg_trickle--0.5.0--0.6.0.sql` |
| 0.6.0 | 0.7.0 | `pg_trickle--0.6.0--0.7.0.sql` |
| 0.7.0 | 0.8.0 | `pg_trickle--0.7.0--0.8.0.sql` |
| 0.8.0 | 0.9.0 | `pg_trickle--0.8.0--0.9.0.sql` |
| 0.9.0 | 0.10.0 | `pg_trickle--0.9.0--0.10.0.sql` |
| 0.10.0 | 0.11.0 | `pg_trickle--0.10.0--0.11.0.sql` |
| 0.11.0 | 0.12.0 | `pg_trickle--0.11.0--0.12.0.sql` |

That means any installation currently on 0.1.3 through 0.11.0 can upgrade to
0.12.0 in one step after the new binaries are installed and PostgreSQL has been
restarted.

---

## Rollback / Downgrade

PostgreSQL does not support automatic extension downgrades. To roll back:

1. **Export stream table definitions** (if you want to recreate them later):
  ```bash
  cargo run --bin pg_trickle_dump -- --output backup.sql
  ```
  Or, if the binary is already installed in your PATH:
  ```bash
  pg_trickle_dump --output backup.sql
  ```
  Use `--dsn '<connection string>'` or standard `PG*` / `DATABASE_URL`
  environment variables when the default local connection parameters are not
  sufficient.

2. **Drop the extension** (destroys all stream tables):
   ```sql
   DROP EXTENSION pg_trickle CASCADE;
   ```

3. **Install the old version** and restart PostgreSQL.

4. **Recreate the extension** at the old version:
   ```sql
   CREATE EXTENSION pg_trickle VERSION '0.1.3';
   ```

5. **Recreate stream tables** from your backup.

---

## Troubleshooting

### "function pgtrickle.xxx does not exist" after upgrade

This means the upgrade script is missing a function. Workaround:

```sql
-- Check what version PostgreSQL thinks is installed
SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle';

-- If the version looks correct but functions are missing,
-- the upgrade script may be incomplete. Try a clean reinstall:
DROP EXTENSION pg_trickle CASCADE;
CREATE EXTENSION pg_trickle CASCADE;
-- Warning: this destroys all stream tables!
```

Report this as a bug — upgrade scripts should never silently drop functions.

### "could not access file pg_trickle" after restart

The new shared library file was not installed correctly. Verify:

```bash
ls -la $(pg_config --pkglibdir)/pg_trickle*
```

### ALTER EXTENSION UPDATE says "already at version X"

The binary files are already the new version but the SQL catalog wasn't
upgraded. This usually means the `.control` file's `default_version`
matches your current version. Check:

```bash
cat $(pg_config --sharedir)/extension/pg_trickle.control
```

---

## Multi-Database Environments

`ALTER EXTENSION UPDATE` must be run in **each database** where pg_trickle
is installed. A common pattern:

```bash
for db in $(psql -t -c "SELECT datname FROM pg_database WHERE datname NOT IN ('template0', 'template1')"); do
  psql -d "$db" -c "ALTER EXTENSION pg_trickle UPDATE;" 2>/dev/null || true
done
```

---

## CloudNativePG (CNPG)

For CNPG deployments, see [cnpg/README.md](../cnpg/) for upgrade
instructions specific to the Kubernetes operator.
