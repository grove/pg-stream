# CLI Reference

`pgtrickle` is the standalone command-line tool for managing and
monitoring stream tables from outside SQL. It is shipped as the
`pgtrickle-tui` binary and works as both an interactive dashboard
and a scriptable CLI.

For the interactive views (Dashboard, Refresh Log, Workers, …) and
keybindings, see the [TUI guide](TUI.md). This page is the
**command-line subcommand reference** — one section per subcommand,
with synopsis, options, exit codes, and examples.

> **Install:** `cargo install --path pgtrickle-tui` (Rust toolchain
> required). Make sure `~/.cargo/bin` is on your `PATH`.

---

## Global options

These apply to every subcommand.

| Option | Default | Description |
|---|---|---|
| `--url <postgres-url>` | `$DATABASE_URL` | PostgreSQL connection URL |
| `--format <fmt>` | `table` | `table` · `json` · `yaml` · `csv` |
| `--no-color` | off | Disable ANSI colours |
| `-q`, `--quiet` | off | Suppress non-error output |
| `-h`, `--help` | — | Show help for the subcommand |

Exit codes (consistent across subcommands):

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Critical condition (e.g. `health` reports `ERROR`) |
| `2` | User error (bad arguments, unknown stream table) |
| `3` | Connection failure |
| `4` | Operation timeout |

---

## list

List every stream table with its key fields.

```bash
pgtrickle list                           # default table format
pgtrickle list --format json
pgtrickle list --filter 'order_*'        # glob match on name
```

| Option | Description |
|---|---|
| `--filter <glob>` | Restrict to matching names |
| `--schema <name>` | Only list a given schema |
| `--status <s>` | Filter by status (ACTIVE, SUSPENDED, INITIALIZING) |

---

## status

Show extended status for a single stream table.

```bash
pgtrickle status order_totals
pgtrickle status order_totals --json
```

Reports schedule, refresh mode, last-refresh timestamp, staleness,
recent error count, and dependency summary.

---

## refresh

Trigger a manual refresh of one stream table.

```bash
pgtrickle refresh order_totals           # synchronous
pgtrickle refresh order_totals --async   # fire-and-forget
pgtrickle refresh order_totals --mode FULL
```

| Option | Description |
|---|---|
| `--mode <m>` | Override refresh mode for this run |
| `--async` | Don't wait for the refresh to complete |
| `--timeout <s>` | Fail with exit 4 after `s` seconds |

---

## create

Create a new stream table from the command line.

```bash
pgtrickle create order_totals \
    --query "SELECT customer_id, SUM(amount) FROM orders GROUP BY customer_id" \
    --schedule 5s

# From a file
pgtrickle create order_totals --query-file ./order_totals.sql --schedule '@hourly'
```

| Option | Description |
|---|---|
| `--query <sql>` | Defining query inline |
| `--query-file <path>` | Read defining query from file |
| `--schedule <s>` | Duration / cron / `calculated` / `null` |
| `--mode <m>` | Refresh mode (defaults to `AUTO`) |
| `--initial-refresh / --no-initial-refresh` | Run a full refresh on creation |

---

## drop

Drop a stream table.

```bash
pgtrickle drop order_totals
pgtrickle drop order_totals --cascade   # also drop dependents
```

---

## alter

Change schedule, refresh mode, status, or defining query.

```bash
pgtrickle alter order_totals --schedule 30s
pgtrickle alter order_totals --mode DIFFERENTIAL
pgtrickle alter order_totals --query-file ./order_totals_v2.sql
pgtrickle alter order_totals --suspend
pgtrickle alter order_totals --resume
```

---

## export

Export a stream table's definition (or the whole catalogue) as
SQL you can replay with `psql`.

```bash
pgtrickle export order_totals > order_totals.sql
pgtrickle export --all > all_stream_tables.sql
```

---

## diag

One-shot diagnostic dump suitable for attaching to a bug report.

```bash
pgtrickle diag > pgtrickle-diag.txt
pgtrickle diag --include-explain --include-buffer-stats
```

---

## cdc

Inspect CDC pipeline state.

```bash
pgtrickle cdc                       # summary of all sources
pgtrickle cdc orders                # detail for one source table
pgtrickle cdc orders --tail 50      # most recent 50 buffered changes
```

---

## graph

Render the dependency DAG.

```bash
pgtrickle graph                       # ASCII tree
pgtrickle graph --format dot > dag.dot
pgtrickle graph --highlight order_totals
```

`dot` output can be piped to Graphviz: `pgtrickle graph --format dot | dot -Tsvg > dag.svg`.

---

## config

Inspect and (with care) set GUC variables.

```bash
pgtrickle config                              # all pg_trickle.* GUCs
pgtrickle config pg_trickle.cdc_mode
pgtrickle config pg_trickle.cdc_mode --set wal   # ALTER SYSTEM
```

---

## health

Run health checks. Exits with code 1 if any check is `ERROR`.

```bash
pgtrickle health                              # summary
pgtrickle health --severity warn              # include WARN
pgtrickle health --json                       # machine-readable
```

Use this in monitoring jobs and CI smoke tests.

---

## workers

Show the parallel-refresh worker pool.

```bash
pgtrickle workers
pgtrickle workers --watch                     # refresh every 2 s
```

---

## fuse

Inspect or reset stream-table circuit breakers.

```bash
pgtrickle fuse                                # list tripped fuses
pgtrickle fuse reset order_totals             # re-enable
pgtrickle fuse reset --all
```

---

## watermarks

Show watermark status for sources that publish them.

```bash
pgtrickle watermarks
pgtrickle watermarks --group ingest_pipeline
```

---

## explain

Show the delta SQL pg_trickle would run on the next refresh.

```bash
pgtrickle explain order_totals
pgtrickle explain order_totals --analyze       # adds EXPLAIN ANALYZE
```

---

## watch

Continuous (interactive) view, like `top` but for stream tables.

```bash
pgtrickle watch                                # default 2 s
pgtrickle watch -n 5                           # every 5 s
pgtrickle watch --filter 'order_*'
```

---

## completions

Emit shell completion scripts.

```bash
# bash / zsh / fish / elvish / powershell
pgtrickle completions bash > ~/.local/share/bash-completion/completions/pgtrickle
pgtrickle completions zsh  > "${fpath[1]}/_pgtrickle"
```

---

## Scripting tips

- Always pass `--format json` for parsing — table output is for
  humans and is not a stable interface.
- Use `health` as a CI gate (`pgtrickle health || exit 1`).
- Use `--no-color` in environments with no TTY.
- Set `PGTRICKLE_URL` instead of passing `--url` everywhere.

---

**See also:**
[TUI guide (interactive views)](TUI.md) ·
[SQL Reference (the underlying functions)](SQL_REFERENCE.md) ·
[Troubleshooting](TROUBLESHOOTING.md)
