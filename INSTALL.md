# Installation Guide

## Prerequisites

| Requirement | Version |
|---|---|
| PostgreSQL | 18.x |

> **Building from source** additionally requires Rust 1.82+ and pgrx 0.17.x.
> Pre-built release artifacts only need a running PostgreSQL 18.x instance.

---

## Installing from a Pre-built Release

### 1. Download the release archive

Download the archive for your platform from the
[GitHub Releases](../../releases) page:

| Platform | Archive |
|---|---|
| Linux x86_64 | `pg_trickle-<ver>-pg18-linux-amd64.tar.gz` |
| macOS Apple Silicon | `pg_trickle-<ver>-pg18-macos-arm64.tar.gz` |
| Windows x64 | `pg_trickle-<ver>-pg18-windows-amd64.zip` |

Optionally verify the checksum against `SHA256SUMS.txt` from the same release:

```bash
sha256sum -c SHA256SUMS.txt
```

### 2. Extract and install

**Linux / macOS:**

```bash
tar xzf pg_trickle-0.1.2-pg18-linux-amd64.tar.gz
cd pg_trickle-0.1.2-pg18-linux-amd64

sudo cp lib/*.so  "$(pg_config --pkglibdir)/"
sudo cp extension/*.control extension/*.sql "$(pg_config --sharedir)/extension/"
```

**Windows (PowerShell):**

```powershell
Expand-Archive pg_trickle-0.1.2-pg18-windows-amd64.zip -DestinationPath .
cd pg_trickle-0.1.2-pg18-windows-amd64

Copy-Item lib\*.dll  "$(pg_config --pkglibdir)\"
Copy-Item extension\* "$(pg_config --sharedir)\extension\"
```

### 3. Using with CloudNativePG (Kubernetes)

pg_trickle is distributed as an OCI extension image for use with
[CloudNativePG Image Volume Extensions](https://cloudnative-pg.io/docs/1.28/imagevolume_extensions/).

**Requirements:** Kubernetes 1.33+, CNPG 1.28+, PostgreSQL 18.

```bash
# Pull the extension image
docker pull ghcr.io/grove/pg_trickle-ext:0.1.2
```

See [cnpg/cluster-example.yaml](cnpg/cluster-example.yaml) and
[cnpg/database-example.yaml](cnpg/database-example.yaml) for complete
Cluster and Database deployment examples.

### 4. Local Docker development (without Kubernetes)

For local development without Kubernetes, install the extension files manually
into a standard PostgreSQL container from a release archive:

```bash
# Extract extension files from the release archive
tar xzf pg_trickle-0.1.2-pg18-linux-amd64.tar.gz
cd pg_trickle-0.1.2-pg18-linux-amd64

# Run PostgreSQL with the extension mounted
docker run --rm \
  -v $PWD/lib/pg_trickle.so:/usr/lib/postgresql/18/lib/pg_trickle.so:ro \
  -v $PWD/extension/:/tmp/ext/:ro \
  -e POSTGRES_PASSWORD=postgres \
  postgres:18.1 \
  sh -c 'cp /tmp/ext/* /usr/share/postgresql/18/extension/ && \
         exec postgres -c shared_preload_libraries=pg_trickle'
```

---

## Building from Source

### 1. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Install pgrx

```bash
cargo install --locked cargo-pgrx --version 0.17.0
cargo pgrx init --pg18 $(pg_config --bindir)/pg_config
```

### 3. Build the Extension

```bash
# Development build (faster compilation)
cargo pgrx install --pg-config $(pg_config --bindir)/pg_config

# Release build (optimized, for production)
cargo pgrx install --release --pg-config $(pg_config --bindir)/pg_config

# Package for deployment (creates installable artifacts)
cargo pgrx package --pg-config $(pg_config --bindir)/pg_config
```

## PostgreSQL Configuration

Add the following to `postgresql.conf` **before starting PostgreSQL**:

```ini
# Required — loads the extension shared library at server start
shared_preload_libraries = 'pg_trickle'

# Recommended — must accommodate scheduler + refresh workers
max_worker_processes = 8
```

> **Note:** `wal_level = logical` and `max_replication_slots` are **not** required. The extension uses lightweight row-level triggers for CDC, not logical replication.

Restart PostgreSQL after modifying these settings:

```bash
pg_ctl restart -D /path/to/data
# or
systemctl restart postgresql
```

## Extension Installation

Connect to the target database and run:

```sql
CREATE EXTENSION pg_trickle;
```

This creates:

- The `pgtrickle` schema with catalog tables and SQL functions
- The `pgtrickle_changes` schema for change buffer tables
- Event triggers for DDL tracking
- The `pgtrickle.pg_stat_stream_tables` monitoring view

## Verification

After installation, verify everything is working:

```sql
-- Check the extension version
SELECT extname, extversion FROM pg_extension WHERE extname = 'pg_trickle';

-- Or get a full status overview (includes version, scheduler state, stream table count)
SELECT * FROM pgtrickle.pgt_status();
```

### Inspecting the installation

```sql
-- Check the installed version
SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle';

-- Check which schemas were created
SELECT schema_name
FROM information_schema.schemata
WHERE schema_name IN ('pgtrickle', 'pgtrickle_changes');

-- Check all registered GUC variables
SHOW pg_trickle.enabled;
SHOW pg_trickle.scheduler_interval_ms;
SHOW pg_trickle.max_concurrent_refreshes;

-- Check the scheduler background worker is running
SELECT * FROM pgtrickle.pgt_status();

-- List all stream tables
SELECT pgt_schema, pgt_name, status, refresh_mode, is_populated
FROM pgtrickle.pgt_stream_tables;

-- Check that the shared library loaded correctly
SELECT * FROM pg_extension WHERE extname = 'pg_trickle';

-- Verify the catalog tables exist
SELECT tablename
FROM pg_tables
WHERE schemaname = 'pgtrickle'
ORDER BY tablename;
```

### Quick functional test

```sql
CREATE TABLE test_source (id INT PRIMARY KEY, val TEXT);
INSERT INTO test_source VALUES (1, 'hello');

SELECT pgtrickle.create_stream_table(
    'test_st',
    'SELECT id, val FROM test_source',
    '1m',
    'FULL'
);

SELECT * FROM test_st;
-- Should return: 1 | hello

-- Clean up
SELECT pgtrickle.drop_stream_table('test_st');
DROP TABLE test_source;
```

## Upgrading

To upgrade pg_trickle to a newer version without losing data:

### 1. Install the new extension files

Follow the same steps as [Installing from a Pre-built Release](#installing-from-a-pre-built-release)
to overwrite the shared library and SQL files with the new version. You do **not** need to drop
the extension from your databases first.

**Linux / macOS:**

```bash
tar xzf pg_trickle-<new-ver>-pg18-linux-amd64.tar.gz
cd pg_trickle-<new-ver>-pg18-linux-amd64

sudo cp lib/*.so  "$(pg_config --pkglibdir)/"
sudo cp extension/*.control extension/*.sql "$(pg_config --sharedir)/extension/"
```

### 2. Restart PostgreSQL (when required)

If the shared library ABI has changed, restart PostgreSQL before proceeding so the
new `.so`/`.dll` is loaded. The release notes for each version will call this out
explicitly when a restart is required.

```bash
pg_ctl restart -D /path/to/data
# or
systemctl restart postgresql
```

### 3. Apply the schema migration in each database

Connect to every database where pg_trickle is installed and run:

```sql
-- Upgrade to the latest bundled version
ALTER EXTENSION pg_trickle UPDATE;

-- Or upgrade to a specific version
ALTER EXTENSION pg_trickle UPDATE TO '0.2.0';
```

PostgreSQL uses the versioned SQL migration scripts bundled with the release
(e.g. `pg_trickle--0.1.0--0.2.0.sql`) to apply any catalog changes. The command
is a no-op when no migration script is needed for a given release.

You can confirm the active version afterwards:

```sql
SELECT extversion FROM pg_extension WHERE extname = 'pg_trickle';
```

> **Coming soon:** A future release will include a helper function
> (`pgtrickle.upgrade()`) that automates steps 2–3 across all databases in the
> cluster and validates catalog integrity after the migration. Until then, the
> manual steps above are the supported upgrade path.

---

## Uninstallation

```sql
-- Drop all stream tables first
SELECT pgtrickle.drop_stream_table(pgt_schema || '.' || pgt_name)
FROM pgtrickle.pgt_stream_tables;

-- Drop the extension
DROP EXTENSION pg_trickle CASCADE;
```

Remove `pg_trickle` from `shared_preload_libraries` in `postgresql.conf` and restart PostgreSQL.

## Troubleshooting

### Unit tests crash on macOS 26+ (`symbol not found in flat namespace`)

macOS 26 (Tahoe) changed `dyld` to eagerly resolve all flat-namespace symbols
at binary load time. pgrx extensions reference PostgreSQL server-internal
symbols (e.g. `CacheMemoryContext`, `SPI_connect`) via the
`-Wl,-undefined,dynamic_lookup` linker flag. These symbols are normally
provided by the `postgres` executable when the extension is loaded as a shared
library — but for `cargo test --lib` there is no postgres process, so the test
binary aborts immediately:

```
dyld[66617]: symbol not found in flat namespace '_CacheMemoryContext'
```

**This affects local development only** — integration tests, E2E tests, and the
extension itself running inside PostgreSQL are unaffected.

The fix is built into the `just test-unit` recipe. It automatically:

1. Compiles a tiny C stub library (`scripts/pg_stub.c` → `target/libpg_stub.dylib`)
   that provides NULL/no-op definitions for the ~28 PostgreSQL symbols.
2. Compiles the test binary with `--no-run`.
3. Runs the binary with `DYLD_INSERT_LIBRARIES` pointing to the stub.

The stub is only built on macOS 26+. On Linux or older macOS, `just test-unit`
runs `cargo test --lib` directly with no changes.

> **Note:** The stub symbols are never called — unit tests exercise pure Rust
> logic only. If a test accidentally calls a PostgreSQL function it will crash
> with a NULL dereference (the desired fail-fast behavior).

If you run unit tests without `just` (e.g. directly via `cargo test --lib`),
you can use the wrapper script instead:

```bash
./scripts/run_unit_tests.sh pg18

# With test name filter:
./scripts/run_unit_tests.sh pg18 -- test_parse_basic
```

### Extension fails to load

Ensure `shared_preload_libraries = 'pg_trickle'` is set and PostgreSQL has been **restarted** (not just reloaded). The extension requires shared memory initialization at startup.

### Background worker not starting

Check that `max_worker_processes` is high enough to accommodate the scheduler worker plus any refresh workers. The default of 8 is usually sufficient.

### Check logs for details

The extension logs at various levels. Enable debug logging for more detail:

```sql
SET client_min_messages TO debug1;
```
