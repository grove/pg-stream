# Plan: Devcontainer Unit Test Workflow Stability & Build Reuse

Date: 2026-03-04
Status: IMPLEMENTED

---

## Overview

This document captures the dev-path work completed to make unit tests reliable
inside containerized development and to reduce unnecessary rebuilds.

The original issue observed during `just test-unit` was a permission mismatch on
the workspace `target/` directory when running in container contexts. That made
unit tests fail before execution and also risked repeated full recompiles.

---

## Goals

1. Ensure `just test-unit` works in devcontainer-style execution.
2. Avoid forced full rebuilds when source/tests change.
3. Keep scope focused on development workflow (not production packaging).

---

## Root Cause

- `just test-unit` delegates to `scripts/run_unit_tests.sh`.
- The script previously assumed `target/` was writable.
- In container runs, workspace bind mounts can present UID/GID ownership that
  makes `target/` unwritable to the container user.
- This caused failures during stub compilation and cargo lock writes.

---

## Implemented Changes

### 1) Unit-test target directory fallback logic

Updated `scripts/run_unit_tests.sh` to select a writable target directory in a
stable order:

1. `<project>/target` (preferred)
2. `<project>/.cargo-target` (project-local fallback)
3. `$HOME/.cache/pg_trickle-target` (persistent user cache)
4. `${TMPDIR:-/tmp}/pg_trickle-target` (last resort)

Other script updates:

- Stub library path now uses `CARGO_TARGET_DIR`.
- Test binary discovery now resolves from `CARGO_TARGET_DIR`.
- Fallback directory creation is non-fatal when unwritable.

### 2) Devcontainer cache mount enhancement

Updated `.devcontainer/devcontainer.json` mounts to add persistent Cargo git
cache reuse:

- Added mount for `/home/vscode/.cargo/git`.

This complements existing registry/target/pgrx mounts and reduces dependency
re-download/re-resolution overhead when rebuilding/reopening devcontainers.

---

## Validation Performed

- Ran `just test-unit` inside containerized execution path.
- Confirmed successful unit-test completion (`1007 passed`).
- Confirmed permission-path failures were eliminated by target fallback logic.

---

## Scope Decisions

- We intentionally kept final scope on dev workflow improvements.
- E2E/CNPG Dockerfile optimization experiments were not retained as the primary
  deliverable for this task.

---

## Follow-ups (Optional)

1. Add a short note in `CONTRIBUTING.md` about container UID/GID effects on
   bind-mounted `target/` and how fallback target selection works.
2. Add a lightweight CI/unit check that executes `scripts/run_unit_tests.sh` in
   a containerized environment with non-root user to guard against regressions.
