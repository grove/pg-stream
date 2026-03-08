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
-- Returns: '0.1.3'
```

### 2. Install New Binary Files

Replace the extension files in your PostgreSQL installation directory.
The method depends on how you originally installed pg_trickle.

**From release tarball:**

```bash
# Download the new release
curl -LO https://github.com/getretake/pg_trickle/releases/download/v0.2.3/pg_trickle-0.2.3-pg18-linux-amd64.tar.gz
tar xzf pg_trickle-0.2.3-pg18-linux-amd64.tar.gz

# Copy files to PostgreSQL directories
sudo cp pg_trickle-0.2.3-pg18-linux-amd64/lib/* $(pg_config --pkglibdir)/
sudo cp pg_trickle-0.2.3-pg18-linux-amd64/extension/* $(pg_config --sharedir)/extension/
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
`pg_trickle--0.1.3--0.2.0.sql` → `pg_trickle--0.2.0--0.2.1.sql` →
`pg_trickle--0.2.1--0.2.2.sql` → `pg_trickle--0.2.2--0.2.3.sql`). PostgreSQL automatically determines the
upgrade chain from your current version to the new `default_version`.

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

---

## Supported Upgrade Paths

The following upgrade chains are supported:

| From | To | Scripts used |
|------|----|-------------|
| 0.1.3 | 0.2.0 | `pg_trickle--0.1.3--0.2.0.sql` |
| 0.1.3 | 0.2.1 | `pg_trickle--0.1.3--0.2.0.sql` → `pg_trickle--0.2.0--0.2.1.sql` |
| 0.1.3 | 0.2.2 | `pg_trickle--0.1.3--0.2.0.sql` → `pg_trickle--0.2.0--0.2.1.sql` → `pg_trickle--0.2.1--0.2.2.sql` |
| 0.1.3 | 0.2.3 | `pg_trickle--0.1.3--0.2.0.sql` → `pg_trickle--0.2.0--0.2.1.sql` → `pg_trickle--0.2.1--0.2.2.sql` → `pg_trickle--0.2.2--0.2.3.sql` |
| 0.2.0 | 0.2.1 | `pg_trickle--0.2.0--0.2.1.sql` |
| 0.2.0 | 0.2.2 | `pg_trickle--0.2.0--0.2.1.sql` → `pg_trickle--0.2.1--0.2.2.sql` |
| 0.2.0 | 0.2.3 | `pg_trickle--0.2.0--0.2.1.sql` → `pg_trickle--0.2.1--0.2.2.sql` → `pg_trickle--0.2.2--0.2.3.sql` |
| 0.2.1 | 0.2.2 | `pg_trickle--0.2.1--0.2.2.sql` |
| 0.2.1 | 0.2.3 | `pg_trickle--0.2.1--0.2.2.sql` → `pg_trickle--0.2.2--0.2.3.sql` |
| 0.2.2 | 0.2.3 | `pg_trickle--0.2.2--0.2.3.sql` |

PostgreSQL automatically chains migration scripts. Running
`ALTER EXTENSION pg_trickle UPDATE` from v0.1.3 will apply all required
scripts in sequence.

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
