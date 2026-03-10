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
cargo pgrx init --pg18 /usr/lib/postgresql/18/bin/pg_config

# Build
cargo build

# Format + lint (required before every PR)
just fmt
just lint

# Run tests
just test-unit          # fast, no DB
just test-integration   # Testcontainers
just test-light-e2e     # PR-equivalent Light E2E tier (stock postgres)
just test-e2e           # full E2E (builds Docker image)
```

Full setup instructions are in [INSTALL.md](INSTALL.md).

### Devcontainer / Containerized Development

If you are developing in a devcontainer, use the default non-root `vscode` user
and run the normal commands from the workspace root:

```bash
just fmt
just lint
just test-unit
```

`just test-unit` uses `scripts/run_unit_tests.sh`, which now selects a writable
and cache-friendly target directory in this order:

1. `target/` (preferred)
2. `.cargo-target/` (project-local fallback)
3. `$HOME/.cache/pg_trickle-target`
4. `${TMPDIR:-/tmp}/pg_trickle-target` (last resort)

This avoids permission failures on bind mounts and preserves incremental builds
when source or test files change.

If you see permission errors in containerized runs, verify you are not forcing a
different container user/UID than expected by your workspace mount.

#### Run E2E tests in devcontainer

E2E tests use Testcontainers and require Docker access from inside the
devcontainer (provided by the Docker-in-Docker feature in
`.devcontainer/devcontainer.json`).

Run from the workspace root inside the devcontainer:

```bash
just build-e2e-image
just test-e2e
```

Notes:

- The E2E harness starts containers via `testcontainers` (`tests/e2e/mod.rs`).
- The default E2E image is `pg_trickle_e2e:latest` (built by
  `tests/build_e2e_image.sh`).
- A plain `docker run` of the dev image is not equivalent to a full VS Code
  devcontainer session with features/lifecycle hooks enabled.

## Making a Pull Request

1. Fork the repository and create a branch: `git checkout -b fix/my-fix`
2. Make your changes following the conventions in [AGENTS.md](AGENTS.md)
3. Run `just fmt && just lint` — both must pass with zero warnings
4. Add or update tests — see [AGENTS.md § Testing](AGENTS.md#testing)
5. Open a PR against `main`

The PR template will walk you through the checklist.

### CI Coverage on PRs

PR CI runs a three-tier gate:

- **Unit tests (Linux only)**
- **Integration tests**
- **Light E2E** — curated PR-friendly end-to-end coverage split across three
  shards and executed against stock `postgres:18.3`

Full E2E, TPC-H tests, benchmarks, dbt, CNPG smoke, and the extra macOS /
Windows unit jobs stay off the PR critical path and run on push-to-main,
schedule, or manual dispatch. This keeps typical PR feedback closer to the
single-digit-minute range while preserving broader scheduled coverage.

To trigger the **full CI matrix** on your PR branch (recommended for DVM
engine, refresh, or CDC changes):

```bash
gh workflow run ci.yml --ref <your-branch>
```

To run all tests locally before pushing:

```bash
just test-all          # unit + integration + e2e

# PR-equivalent fast path:
just test-unit
just test-integration
just test-light-e2e

# TPC-H correctness tests (requires e2e Docker image):
cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture
```

See [AGENTS.md § Testing](AGENTS.md#testing) for the full CI coverage matrix.

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
