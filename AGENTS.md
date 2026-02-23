# AGENTS.md — Development Guidelines for pg_stream

## Project Overview

PostgreSQL 18 extension written in Rust using **pgrx 0.17.x** that implements
streaming tables with incremental view maintenance (differential dataflow).
Targets PostgreSQL 18.x.

Key docs: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) ·
[docs/SQL_REFERENCE.md](docs/SQL_REFERENCE.md) ·
[docs/CONFIGURATION.md](docs/CONFIGURATION.md) ·
[INSTALL.md](INSTALL.md)

---

## Workflow — Always Do This

After **any** code change:

```bash
just fmt          # Format code
just lint         # clippy + fmt-check (must pass with zero warnings)
```

After changes to SQL-facing code, run the relevant test tier:

```bash
just test-unit         # Pure Rust unit tests (no DB)
just test-integration  # Testcontainers-based integration tests
just test-e2e          # Full extension E2E tests (builds Docker image)
just test-all          # All of the above + pgrx tests
```

> E2E tests require a Docker image. Run `just build-e2e-image` if the image is
> stale, or use `just test-e2e` which rebuilds automatically.

When done, output a `git commit` command summarising the change.out

---

## Coding Conventions

### Error Handling

- Define errors in `src/error.rs` as `PgStreamError` enum variants.
- Never `unwrap()` or `panic!()` in code reachable from SQL.
- Propagate via `Result<T, PgStreamError>`; convert at the API boundary with
  `pgrx::error!()` or `ereport!()`.

### SPI

- All catalog access via `Spi::connect()`.
- Keep SPI blocks short — no long operations while holding a connection.

### Unsafe Code

- Minimize `unsafe` blocks. Wrap `pg_sys::*` in safe abstractions.
- Every `unsafe` block must have a `// SAFETY:` comment.

### Memory & Shared State

- Be explicit about PostgreSQL memory contexts.
- Use `PgLwLock` / `PgAtomic` for shared state; initialize via `pg_shmem_init!()`.

### Background Workers

- Register via `BackgroundWorkerBuilder`.
- Check `pg_stream.enabled` GUC before doing work.
- Handle `SIGTERM` gracefully.

### Logging

- Use `pgrx::log!()`, `info!()`, `warning!()`, `error!()`.
- Never `println!()` or `eprintln!()`.

### SQL Functions

- Annotate with `#[pg_extern(schema = "pgstream")]`.
- Catalog tables live in schema `pgstream`, change buffers in `pgstream_changes`.

---

## Module Layout

```
src/
├── lib.rs          # Extension entry point, GUCs, shmem init
├── api.rs          # SQL-callable functions (create/alter/drop/refresh)
├── catalog.rs      # pgstream.pgs_stream_tables CRUD
├── cdc.rs          # Change-data-capture (trigger-based)
├── config.rs       # GUC definitions
├── dag.rs          # Dependency graph, topological sort, cycle detection
├── error.rs        # PgStreamError enum
├── hash.rs         # Content hashing for change detection
├── hooks.rs        # DDL event trigger hooks
├── monitor.rs      # Monitoring / metrics
├── refresh.rs      # Full + differential refresh orchestration
├── scheduler.rs    # Background worker scheduling
├── shmem.rs        # Shared memory structures
├── version.rs      # Extension version
└── dvm/            # Differential view maintenance engine
    ├── mod.rs
    ├── diff.rs     # Delta application
    ├── parser.rs   # Query analysis
    ├── row_id.rs   # Row identity tracking
    └── operators/  # Per-SQL-operator differentiation rules
```

See [plans/PLAN.md](plans/PLAN.md) for the full design plan.

---

## Testing

Three test tiers, each with its own infrastructure:

| Tier | Location | Runner | Needs DB? |
|------|----------|--------|-----------|
| Unit | `src/**` (`#[cfg(test)]`) | `just test-unit` | No |
| Integration | `tests/*_tests.rs` (not `e2e_*`) | `just test-integration` | Yes (Testcontainers) |
| E2E | `tests/e2e_*_tests.rs` | `just test-e2e` | Yes (custom Docker image) |

- Shared helpers live in `tests/common/mod.rs`.
- E2E Docker images are built from `tests/Dockerfile.e2e`.
- Use `#[tokio::test]` for all integration/E2E tests.
- Name tests: `test_<component>_<scenario>_<expected>`.
- Test both success and failure paths.

---

## CDC Architecture

The extension uses **row-level AFTER triggers** (not logical replication) to
capture changes into buffer tables (`pgstream_changes.changes_<oid>`). This
was chosen for single-transaction atomicity — see
[adrs/adr-triggers-instead-of-logical-replication.md](adrs/adr-triggers-instead-of-logical-replication.md)
for the full rationale.

---

## Code Review Checklist

- [ ] No `unwrap()` / `panic!()` in non-test code
- [ ] All `unsafe` blocks have `// SAFETY:` comments
- [ ] SPI connections are short-lived
- [ ] New SQL functions use `#[pg_extern(schema = "pgstream")]`
- [ ] Tests use Testcontainers — never a local PG instance
- [ ] Error messages include context (table name, query fragment)
- [ ] GUC variables are documented with sensible defaults
- [ ] Background workers handle `SIGTERM` and check `pg_stream.enabled`
