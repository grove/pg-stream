# Contributing to pg_trickle

Thank you for your interest in contributing! pg_trickle is an Apache 2.0-licensed
open-source project and welcomes contributions of all kinds.

## Before You Start

- Check the [open issues](../../issues) and [discussions](../../discussions) to
  avoid duplicating work.
- For non-trivial changes, open an issue first to discuss the approach.
- Read [AGENTS.md](AGENTS.md) — it is the authoritative guide for all coding
  conventions, error handling rules, module layout, and test requirements.
- Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) to understand the system.
- Read [ROADMAP.md](ROADMAP.md) to see what work is planned.

## Ways to Contribute

| Type | Where to start |
|------|----------------|
| Bug report | [Open an issue](../../issues/new?template=bug_report.md) |
| Feature request | [Open an issue](../../issues/new?template=feature_request.md) or [start a discussion](../../discussions) |
| Documentation fix | Open a PR directly — no issue needed for typos/clarity |
| Code fix or feature | Open an issue first, then a PR |
| Performance improvement | Include benchmark numbers (see `just bench`) |

## Development Setup

```bash
# Install pgrx
cargo install cargo-pgrx --version "=0.17.0"
cargo pgrx init --pg18 download

# Build
cargo build

# Format + lint (required before every PR)
just fmt
just lint

# Run tests
just test-unit          # fast, no DB
just test-integration   # Testcontainers
just test-e2e           # full E2E (builds Docker image)
```

Full setup instructions are in [INSTALL.md](INSTALL.md).

## Making a Pull Request

1. Fork the repository and create a branch: `git checkout -b fix/my-fix`
2. Make your changes following the conventions in [AGENTS.md](AGENTS.md)
3. Run `just fmt && just lint` — both must pass with zero warnings
4. Add or update tests — see [AGENTS.md § Testing](AGENTS.md#testing)
5. Open a PR against `main`

The PR template will walk you through the checklist.

## Coding Conventions (summary)

- No `unwrap()` or `panic!()` in non-test code
- All `unsafe` blocks require a `// SAFETY:` comment
- Errors go through `PgTrickleError` in `src/error.rs`
- New SQL functions use `#[pg_extern(schema = "pgtrickle")]`
- Tests use Testcontainers — never a local PostgreSQL instance

Full details are in [AGENTS.md](AGENTS.md).

## Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
fix: correct pgoutput action parsing for tables named INSERT_LOG
feat: add CUBE explosion guard (max 64 UNION ALL branches)
docs: document JOIN key change limitation in SQL_REFERENCE
test: add E2E test for keyless table duplicate-row behaviour
```

## License

By contributing you agree that your contributions will be licensed under the
[Apache License 2.0](LICENSE).
