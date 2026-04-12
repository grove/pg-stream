# pg_trickle TUI — User Guide

`pgtrickle` is a terminal tool for managing and monitoring pg_trickle stream
tables. It works in two modes:

- **Interactive dashboard** — run `pgtrickle` with no arguments to launch
  a live-updating TUI that shows all your stream tables, their health,
  dependencies, and configuration.
- **One-shot CLI** — run `pgtrickle <command>` to perform a single operation
  and exit. Output goes to stdout in table, JSON, or CSV format. Designed
  for scripts, CI pipelines, and automation.

---

## Building

The TUI is a standalone Rust binary in the `pgtrickle-tui` workspace member.
It does **not** require the PostgreSQL extension to compile — only a Rust
toolchain.

```bash
# Build (debug)
cargo build -p pgtrickle-tui

# Build (release, optimized)
cargo build --release -p pgtrickle-tui

# The binary is at:
#   target/debug/pgtrickle       (debug)
#   target/release/pgtrickle     (release)
```

To install it on your `PATH`:

```bash
cargo install --path pgtrickle-tui
```

Verify:

```bash
pgtrickle --version
pgtrickle --help
```

### Requirements

- Rust 2024 edition (1.85+)
- A running PostgreSQL 18 server with the `pg_trickle` extension installed
- Network access to the database (no local socket required)

---

## Connecting to a Database

`pgtrickle` resolves connection parameters in this order (first match wins):

| Priority | Method | Example |
|----------|--------|---------|
| 1 | `--url` flag | `pgtrickle --url postgres://user:pass@host:5432/mydb list` |
| 2 | `PGTRICKLE_URL` env var | `export PGTRICKLE_URL=postgres://...` |
| 3 | Individual flags | `--host`, `--port`, `--dbname`, `--user`, `--password` |
| 4 | Standard libpq env vars | `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD` |
| 5 | Defaults | `localhost:5432/postgres` as user `postgres` |

Connection flags work with every subcommand and with the interactive dashboard:

```bash
# URL-style connection
pgtrickle --url postgres://admin:secret@db.example.com:5432/analytics

# Environment variables (most common in production)
export PGHOST=db.example.com
export PGPORT=5432
export PGDATABASE=analytics
export PGUSER=admin
export PGPASSWORD=secret
pgtrickle list

# Explicit flags
pgtrickle --host db.example.com --dbname analytics --user admin list
```

---

## Interactive Dashboard

Run `pgtrickle` with no subcommand:

```bash
pgtrickle
```

This opens a full-screen terminal UI that auto-refreshes every 2 seconds. The
screen has three areas:

- **Header** — application name, current view, connection status (`● connected`
  / `✗ disconnected`), and time since last poll.
- **Body** — the active view (see below).
- **Footer** — keyboard shortcuts for switching views and a filter indicator.

Press `q` or `Ctrl+C` to exit.

### Views

There are 14 views. Switch between them by pressing the key shown:

| Key | View | What it shows |
|-----|------|---------------|
| `1` | **Dashboard** | All stream tables in a sortable list with status, mode, staleness, and last refresh time. A status ribbon at the top summarizes active/error/stale counts. |
| `2` | **Detail** | Deep dive into the selected stream table: properties (schema, status, mode, schedule, tier, refresh mode explanation), source tables, refresh history, CDC health, and diagnosed errors for error-state tables. |
| `3` | **Dependencies** | The stream table dependency graph rendered as an ASCII tree. Edges are color-coded by status (green = active, red = error). |
| `4` | **Refresh Log** | A scrollable timeline of recent refreshes across all tables — timestamp, mode (DIFF/FULL), table name, status, duration, and rows affected. |
| `5` | **Diagnostics** | Output of `recommend_refresh_mode()` — shows each table's current mode vs. recommended mode with confidence level and reasoning. |
| `6` | **CDC Health** | Change buffer sizes and byte counts per source table, plus the CDC mode (trigger/WAL). Large buffers are highlighted as warnings. |
| `7` | **Configuration** | All `pg_trickle.*` GUC parameters: current value, unit, category, and description. |
| `8` | **Health Checks** | Results of `health_check()` — each check displays a name, severity (OK/WARN/CRITICAL), and detail message. Critical items are shown in red. |
| `9` | **Alerts** | Real-time alert feed from `LISTEN pg_trickle_alert`. Shows timestamp, severity icon, and message for each event. |
| `w` | **Workers** | Background scheduler worker pool: each worker's state (running/idle), the table it's refreshing, and duration. Below that, the pending job queue with priority and wait time. |
| `f` | **Fuse** | Circuit breaker status for each stream table: fuse state (ARMED/TRIPPED/BLOWN), consecutive error count, and last error message. |
| `m` | **Watermarks** | Watermark group alignment: group name, member count, min/max watermarks, and whether the group is gated. Two tabs: Groups and Gates. |
| `d` | **Delta Inspector** | Fetches and displays the auto-generated delta SQL for the selected stream table (two tabs: Delta SQL and Auxiliary Columns). Press `e` to show the table's CREATE DDL. |
| `i` | **Issues** | All detected DAG issues (cycles, orphans, missing sources) sorted by severity and blast radius. |

### Keyboard Shortcuts

**Navigation** — works in all views:

| Key | Action |
|-----|--------|
| `j` or `↓` | Move selection down |
| `k` or `↑` | Move selection up |
| `Page Down` / `Page Up` | Scroll 20 rows |
| `Home` | Jump to first row |
| `End` | Jump to last row |
| `Enter` | Drill into detail (Dashboard → Detail view; Delta Inspector → reload delta SQL) |
| `Esc` | Go back to Dashboard / close overlay / clear filter |
| `Tab` | Switch sub-tabs (Delta Inspector: SQL ↔ Auxiliary Columns; Watermarks: Groups ↔ Gates) |

**Write actions** (view-specific):

| Key | View | Action |
|-----|------|--------|
| `r` | Dashboard, Detail | Refresh selected stream table |
| `R` | Dashboard | Refresh all active tables (with confirmation) |
| `p` | Dashboard, Detail | Pause selected (with confirmation) |
| `P` | Dashboard, Detail | Resume selected |
| `e` | Detail, Delta Inspector | Show CREATE DDL overlay for selected table |
| `A` | Fuse | Re-arm fuse for selected (with confirmation) |
| `g` | Watermarks (Gates tab) | Gate / ungate selected source (confirmation for gate) |

**Global actions:**

| Key | Action |
|-----|--------|
| `/` | Open filter — type to search, `Enter` to apply, `Esc` to cancel |
| `:` | Open command palette |
| `s` / `S` | Cycle sort field / reverse sort direction (Dashboard) |
| `t` | Toggle light/dark theme |
| `Ctrl+R` | Force an immediate poll |
| `Ctrl+E` | Export current view to JSON file (`/tmp/pgtrickle_export_*.json`) |
| `?` | Toggle help overlay |
| `q` or `Ctrl+C` | Quit |

**View switching:**

Press `1`–`9`, `w`, `f`, `m`, `d`, `g`, or `i` to jump directly to any view.
The active view and selected table are shown in both the header bar and the
footer nav bar.

### Command Palette

Press `:` to open the command palette. Tab-completion works on stream table
names. Available commands:

| Command | Description |
|---------|-------------|
| `refresh <name>` | Refresh a stream table (or `refresh all`) |
| `pause <name>` | Pause a stream table |
| `resume <name>` | Resume a paused stream table |
| `repair <name>` | Re-install CDC triggers |
| `export <name>` | Show CREATE DDL overlay |
| `explain <name>` | Fetch and display delta SQL for a stream table |
| `validate <SQL>` | Validate a SQL query against the extension |
| `fuse reset <name>` | Reset the circuit breaker fuse |
| `quit` | Exit the TUI |

### LISTEN/NOTIFY

The TUI opens a second, dedicated database connection that runs
`LISTEN pg_trickle_alert`. Alerts (refresh failures, auto-suspension events,
etc.) appear in the **Alerts** view (`9`) in real time, without waiting for
the next poll cycle.

---

## CLI Subcommands

Every subcommand runs non-interactively: it connects, executes one query,
prints the result, and exits. This makes them suitable for shell scripts,
cron jobs, CI pipelines, and monitoring probes.

### Output Formats

All subcommands that produce tabular output accept `--format` / `-f`:

| Format | Flag | Description |
|--------|------|-------------|
| Table | `--format table` (default) | Human-readable aligned columns |
| JSON | `--format json` | Array of objects on stdout |
| CSV | `--format csv` | Comma-separated values |

### Command Reference

#### `pgtrickle list`

List all stream tables with status, mode, schedule, tier, and refresh stats.

```bash
pgtrickle list
pgtrickle list --format json
```

#### `pgtrickle status <name>`

Show detailed status for a single stream table.

```bash
pgtrickle status order_totals
pgtrickle status order_totals --format json
```

#### `pgtrickle refresh <name>`

Trigger a manual refresh of one stream table, or all of them.

```bash
pgtrickle refresh order_totals
pgtrickle refresh --all
```

#### `pgtrickle create <name> <query>`

Create a new stream table with the given defining query.

```bash
pgtrickle create my_totals "SELECT region, SUM(amount) FROM orders GROUP BY region"
pgtrickle create my_totals "SELECT ..." --schedule 5m --mode differential
pgtrickle create my_totals "SELECT ..." --no-initialize
```

| Flag | Description |
|------|-------------|
| `--schedule` | Refresh schedule (e.g. `5m`, `@hourly`) |
| `--mode` | Refresh mode: `auto`, `differential`, `full`, `immediate` |
| `--no-initialize` | Skip the initial refresh after creation |

#### `pgtrickle drop <name>`

Drop a stream table.

```bash
pgtrickle drop my_totals
```

#### `pgtrickle alter <name>`

Change a stream table's settings.

```bash
pgtrickle alter order_totals --mode full
pgtrickle alter order_totals --schedule 10m
pgtrickle alter order_totals --tier cold
pgtrickle alter order_totals --status paused
pgtrickle alter order_totals --query "SELECT ..."
```

| Flag | Description |
|------|-------------|
| `--mode` | New refresh mode |
| `--schedule` | New refresh schedule |
| `--tier` | New scheduling tier (`hot`, `warm`, `cold`, `frozen`) |
| `--status` | New status (`active`, `paused`, `suspended`) |
| `--query` | New defining query (ALTER QUERY) |

#### `pgtrickle export <name>`

Print the DDL (SQL definition) for a stream table.

```bash
pgtrickle export order_totals
```

#### `pgtrickle diag [name]`

Show refresh mode diagnostics and recommendations. Without a name, shows all
tables. With a name, shows diagnostics for that table only.

```bash
pgtrickle diag
pgtrickle diag order_totals
pgtrickle diag --format json
```

#### `pgtrickle cdc`

Show CDC change buffer sizes and health.

```bash
pgtrickle cdc
pgtrickle cdc --format json
```

#### `pgtrickle graph`

Print the stream table dependency graph as an ASCII tree.

```bash
pgtrickle graph
pgtrickle graph --format json
```

#### `pgtrickle config`

Show all `pg_trickle.*` GUC parameters, or set one.

```bash
pgtrickle config
pgtrickle config --set pg_trickle.unlogged_buffers=true
pgtrickle config --format json
```

The `--set` flag runs `ALTER SYSTEM SET` followed by `pg_reload_conf()`.

#### `pgtrickle health`

Run system health checks. Returns exit code 1 if any check is CRITICAL.

```bash
pgtrickle health
pgtrickle health --format json

# Use in CI/monitoring:
pgtrickle health || echo "Health check failed"
```

#### `pgtrickle workers`

Show the background worker pool status and pending job queue.

```bash
pgtrickle workers
pgtrickle workers --format json
```

#### `pgtrickle fuse`

Show fuse (circuit breaker) status for all stream tables.

```bash
pgtrickle fuse
pgtrickle fuse --format json
```

#### `pgtrickle watermarks`

Show watermark groups and source gating status.

```bash
pgtrickle watermarks
pgtrickle watermarks --format json
```

#### `pgtrickle explain <name>`

Inspect the generated delta SQL, DVM operator tree, or deduplication stats
for a stream table. By default shows the delta SQL.

```bash
pgtrickle explain order_totals                  # Delta SQL
pgtrickle explain order_totals --analyze        # EXPLAIN ANALYZE on the delta
pgtrickle explain order_totals --operators      # DVM operator tree
pgtrickle explain order_totals --dedup          # Dedup stats per source
pgtrickle explain order_totals --format json
```

| Flag | Description |
|------|-------------|
| `--analyze` | Run `EXPLAIN ANALYZE` on the delta query |
| `--operators` | Show the DVM operator tree instead of raw SQL |
| `--dedup` | Show change buffer deduplication statistics |

#### `pgtrickle watch`

Non-interactive continuous output mode. Polls the database and prints a
status table at regular intervals. Useful for CI logs, monitoring, and
terminals without TUI support.

```bash
pgtrickle watch                     # Default: every 2 seconds
pgtrickle watch -n 10               # Every 10 seconds
pgtrickle watch --compact           # One line per table
pgtrickle watch --no-color          # No ANSI color codes
pgtrickle watch --append            # Append mode (don't clear screen)

# Log to a file
pgtrickle watch --compact --no-color --append >> /var/log/pgtrickle.log
```

| Flag | Short | Description |
|------|-------|-------------|
| `--interval` | `-n` | Poll interval in seconds (default: 2) |
| `--compact` | | One-line-per-table output |
| `--no-color` | | Disable ANSI color codes |
| `--append` | | Append to stdout instead of clearing the screen |

#### `pgtrickle completions <shell>`

Generate shell completion scripts. Install them once and get tab-completion
for all subcommands and flags.

```bash
# Bash
pgtrickle completions bash > /etc/bash_completion.d/pgtrickle
# or for the current user:
pgtrickle completions bash > ~/.local/share/bash-completion/completions/pgtrickle

# Zsh
pgtrickle completions zsh > ~/.zfunc/_pgtrickle

# Fish
pgtrickle completions fish > ~/.config/fish/completions/pgtrickle.fish

# PowerShell
pgtrickle completions powershell > pgtrickle.ps1
```

---

## Examples

### Quick health check in CI

```bash
#!/bin/bash
set -e
export PGHOST=db.example.com PGDATABASE=analytics PGUSER=monitor

pgtrickle health || { echo "pg_trickle health check failed"; exit 1; }
pgtrickle list --format json | jq '.[] | select(.status != "ACTIVE")'
```

### Monitor stream tables in a tmux pane

```bash
pgtrickle watch -n 5
```

### Export all definitions for version control

```bash
for name in $(pgtrickle list --format json | jq -r '.[].name'); do
  pgtrickle export "$name" > "sql/stream_tables/${name}.sql"
done
```

### Debug a slow differential refresh

```bash
pgtrickle explain order_totals --analyze
pgtrickle explain order_totals --operators
pgtrickle explain order_totals --dedup
```

---

## How It Works

The TUI connects to PostgreSQL using `tokio-postgres` (async, no TLS by
default) and queries pg_trickle's built-in SQL API functions:

| View | SQL function(s) |
|------|-----------------|
| Dashboard | `pgtrickle.st_refresh_stats()` |
| Detail | `pgtrickle.explain_refresh_mode()`, `pgtrickle.list_sources()`, `pgtrickle.get_refresh_history()`, `pgtrickle.diagnose_errors()` |
| Dependencies | `pgtrickle.dependency_tree()` |
| Refresh Log | `pgtrickle.refresh_timeline()` |
| Diagnostics | `pgtrickle.recommend_refresh_mode()` |
| CDC Health | `pgtrickle.change_buffer_sizes()`, `pgtrickle.check_cdc_health()` |
| Configuration | `pg_settings WHERE name LIKE 'pg_trickle.%'` |
| Health Checks | `pgtrickle.health_check()` |
| Alerts | `LISTEN pg_trickle_alert` (real-time) |
| Workers | `pgtrickle.worker_pool_status()`, `pgtrickle.parallel_job_status()` |
| Fuse | `pgtrickle.fuse_status()` |
| Watermarks | `pgtrickle.watermark_groups()`, `pgtrickle.source_gate_status()` |
| Delta Inspector | `pgtrickle.explain_delta()`, `pgtrickle.list_auxiliary_columns()`, `pgtrickle.pgt_stream_tables` (DDL) |
| Issues | `pgtrickle.dag_issues()` |

In interactive mode, a background task polls all of these every 2 seconds
and pushes state updates to the rendering loop. A second connection runs
`LISTEN pg_trickle_alert` for real-time notifications.

The TUI is purely a client — it reads from pg_trickle's monitoring API and
sends commands (refresh, create, drop, alter) through the same SQL functions
you would call from `psql`. It does not require any special privileges beyond
what the pg_trickle SQL API requires.

### Planned: cache_stats() and health_summary() Integration

> **Status:** Not yet surfaced in the TUI (v0.18.0 gap).

The following SQL functions are available but not yet integrated into the TUI:

- `pgtrickle.cache_stats()` — template cache hit rate, L1 hits, evictions,
  delta cache entries. Useful for monitoring cache effectiveness.
- `pgtrickle.health_summary()` — single-row deployment overview with
  total/active/error/stale stream table counts, P99 refresh latency,
  scheduler status, and cache hit rate.

**Lightest integration path:** Add cache hit rate to the Dashboard status
ribbon (currently shows scheduler status from `quick_health`). The Health
Checks view (`8`) could display `health_summary()` fields alongside the
existing `health_check()` results. Both functions are already available via
raw SQL (`psql`, Grafana, or the Prometheus exporter).

### Tech Stack

| Component | Crate | Purpose |
|-----------|-------|---------|
| Terminal rendering | `ratatui` 0.29 + `crossterm` 0.28 | Full-screen TUI with color, layout, widgets |
| Async runtime | `tokio` 1.x | Background polling, LISTEN/NOTIFY, signals |
| PostgreSQL | `tokio-postgres` 0.7 | Async database queries |
| CLI parsing | `clap` 4.x | Subcommands, flags, env var integration |
| Table output | `comfy-table` 7.x | Aligned text tables for CLI mode |
| Serialization | `serde` + `serde_json` | JSON and CSV output formats |
| Shell completions | `clap_complete` 4.x | bash/zsh/fish/PowerShell completions |
