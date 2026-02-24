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
| Linux x86_64 | `pg_stream-<ver>-pg18-linux-amd64.tar.gz` |
| macOS Apple Silicon | `pg_stream-<ver>-pg18-macos-arm64.tar.gz` |
| Windows x64 | `pg_stream-<ver>-pg18-windows-amd64.zip` |

Optionally verify the checksum against `SHA256SUMS.txt` from the same release:

```bash
sha256sum -c SHA256SUMS.txt
```

### 2. Extract and install

**Linux / macOS:**

```bash
tar xzf pg_stream-0.2.0-pg18-linux-amd64.tar.gz
cd pg_stream-0.2.0-pg18-linux-amd64

sudo cp lib/*.so  "$(pg_config --pkglibdir)/"
sudo cp extension/*.control extension/*.sql "$(pg_config --sharedir)/extension/"
```

**Windows (PowerShell):**

```powershell
Expand-Archive pg_stream-0.2.0-pg18-windows-amd64.zip -DestinationPath .
cd pg_stream-0.2.0-pg18-windows-amd64

Copy-Item lib\*.dll  "$(pg_config --pkglibdir)\"
Copy-Item extension\* "$(pg_config --sharedir)\extension\"
```

### 3. Using the Docker image

Alternatively, skip the manual install and use the CNPG-ready Docker image:

```bash
docker pull ghcr.io/<owner>/pg_stream:0.2.0

docker run --rm -e POSTGRES_PASSWORD=postgres \
  ghcr.io/<owner>/pg_stream:0.2.0 \
  postgres -c "shared_preload_libraries=pg_stream"
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
shared_preload_libraries = 'pg_stream'

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
CREATE EXTENSION pg_stream;
```

This creates:

- The `pgstream` schema with catalog tables and SQL functions
- The `pgstream_changes` schema for change buffer tables
- Event triggers for DDL tracking
- The `pgstream.pg_stat_stream_tables` monitoring view

## Verification

After installation, verify everything is working:

```sql
-- Check the extension is loaded
SELECT * FROM pg_extension WHERE extname = 'pg_stream';

-- Check the pgstream schema exists
SELECT schema_name FROM information_schema.schemata WHERE schema_name = 'pgstream';

-- Check GUC variables are registered
SHOW pg_stream.enabled;
SHOW pg_stream.scheduler_interval_ms;

-- Quick functional test
CREATE TABLE test_source (id INT PRIMARY KEY, val TEXT);
INSERT INTO test_source VALUES (1, 'hello');

SELECT pgstream.create_stream_table(
    'test_st',
    'SELECT id, val FROM test_source',
    '1m',
    'FULL'
);

SELECT * FROM test_st;
-- Should return: 1 | hello

-- Clean up
SELECT pgstream.drop_stream_table('test_st');
DROP TABLE test_source;
```

## Uninstallation

```sql
-- Drop all stream tables first
SELECT pgstream.drop_stream_table(pgs_schema || '.' || pgs_name)
FROM pgstream.pgs_stream_tables;

-- Drop the extension
DROP EXTENSION pg_stream CASCADE;
```

Remove `pg_stream` from `shared_preload_libraries` in `postgresql.conf` and restart PostgreSQL.

## Troubleshooting

### Extension fails to load

Ensure `shared_preload_libraries = 'pg_stream'` is set and PostgreSQL has been **restarted** (not just reloaded). The extension requires shared memory initialization at startup.

### Background worker not starting

Check that `max_worker_processes` is high enough to accommodate the scheduler worker plus any refresh workers. The default of 8 is usually sufficient.

### Check logs for details

The extension logs at various levels. Enable debug logging for more detail:

```sql
SET client_min_messages TO debug1;
```
