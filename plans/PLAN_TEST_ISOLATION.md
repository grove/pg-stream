# PLAN_TEST_ISOLATION

## Goal

Allow `just build-e2e-image` and `just test-e2e` to run simultaneously on the
same laptop (shared Docker daemon) without colliding. Also allows running
multiple parallel test sessions (e.g. two branches checked out side-by-side).

---

## Problem Summary

The current setup hardcodes the image tag as `pg_trickle_e2e:latest` in
`tests/build_e2e_image.sh`. Both `just build-e2e-image` and `just test-e2e`
(which calls `build-e2e-image` first) reference the same mutable tag. This
causes two failure modes:

| Failure mode | How it happens |
|---|---|
| **Image tag clobber** | A background `build-e2e-image` overwrites `:latest` while an in-flight `test-e2e` session is still spinning up containers from it |
| **BuildKit cache thrash** | Two concurrent `docker build` runs targeting the same tag fight over the layer cache, causing one to produce a corrupt or stale image |

The upgrade image (`pg_trickle_upgrade_e2e:latest`) has the same problem
because it is built on top of the base image by tag name.

---

## Solution: Git-SHA image tags + tag-file handoff

Tag every built image with the short git SHA of the working tree at build
time. Write that tag to a `.e2e-image-tag` file in the project root so that
`just test-e2e-fast` and the upgrade build can consume the same tag without
rebuilding.

```
build-e2e-image  →  pg_trickle_e2e:<sha>  →  .e2e-image-tag
test-e2e-fast    ←  reads .e2e-image-tag  →  PGS_E2E_IMAGE=pg_trickle_e2e:<sha>
```

---

## Changes Required

### 1. `tests/build_e2e_image.sh`

- Compute `GIT_SHA=$(git -C "$PROJECT_ROOT" rev-parse --short HEAD)`.
- If the working tree is dirty append `-dirty` so images from uncommitted
  changes do not collide with clean-SHA images.
- Set `IMAGE_TAG="${GIT_SHA}"` instead of `"latest"`.
- After a successful build write the full `name:tag` string to
  `"$PROJECT_ROOT/.e2e-image-tag"`.
- Keep printing the tag prominently so the caller can see what was built.

```bash
# Pseudocode — concrete diff in §Implementation Notes
GIT_SHA=$(git -C "$PROJECT_ROOT" rev-parse --short HEAD)
DIRTY=$(git -C "$PROJECT_ROOT" status --porcelain | wc -l)
[[ "$DIRTY" -gt 0 ]] && GIT_SHA="${GIT_SHA}-dirty"

IMAGE_TAG="${GIT_SHA}"
IMAGE_REF="${IMAGE_NAME}:${IMAGE_TAG}"

docker build -t "$IMAGE_REF" -f "${SCRIPT_DIR}/Dockerfile.e2e" ${EXTRA_ARGS} "${PROJECT_ROOT}"

echo "$IMAGE_REF" > "$PROJECT_ROOT/.e2e-image-tag"
echo "  Tag file written: .e2e-image-tag → $IMAGE_REF"
```

### 2. `tests/build_e2e_upgrade_image.sh`

- Read the base image from `.e2e-image-tag` if `PGS_E2E_BASE_IMAGE` is not
  already set (it already honours that env var — just fall back to the file):

```bash
BASE_IMAGE="${PGS_E2E_BASE_IMAGE:-}"
if [[ -z "$BASE_IMAGE" ]] && [[ -f "$PROJECT_ROOT/.e2e-image-tag" ]]; then
    BASE_IMAGE=$(cat "$PROJECT_ROOT/.e2e-image-tag")
fi
BASE_IMAGE="${BASE_IMAGE:-pg_trickle_e2e:latest}"
```

- Tag the upgrade image with the same SHA suffix:
  `pg_trickle_upgrade_e2e:<sha>`.
- Write to `.e2e-upgrade-image-tag` for symmetry.

### 3. `justfile`

`test-e2e` currently depends on `build-e2e-image` and then runs cargo tests
without propagating the tag. Change it to:

```makefile
# Build the E2E image and record its tag
build-e2e-image:
    ./tests/build_e2e_image.sh

# Run E2E tests — builds image first, then reads .e2e-image-tag
test-e2e: build-e2e-image
    PGS_E2E_IMAGE=$(cat .e2e-image-tag) \
        cargo test --test 'e2e_*' -- --test-threads=1

# Run E2E tests without rebuilding — uses the last built image tag
test-e2e-fast:
    PGS_E2E_IMAGE=$(cat .e2e-image-tag) \
        cargo test --test 'e2e_*' -- --test-threads=1

# Pipeline subset
test-pipeline: build-e2e-image
    PGS_E2E_IMAGE=$(cat .e2e-image-tag) \
        cargo test --test e2e_pipeline_dag_tests -- --test-threads=1 --nocapture

test-pipeline-fast:
    PGS_E2E_IMAGE=$(cat .e2e-image-tag) \
        cargo test --test e2e_pipeline_dag_tests -- --test-threads=1 --nocapture

# TPC-H
test-tpch: build-e2e-image
    PGS_E2E_IMAGE=$(cat .e2e-image-tag) \
        cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture

test-tpch-fast:
    PGS_E2E_IMAGE=$(cat .e2e-image-tag) \
        cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture

test-tpch-large: build-e2e-image
    PGS_E2E_IMAGE=$(cat .e2e-image-tag) \
        TPCH_SCALE=0.1 cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture

# Upgrade
build-upgrade-image from="0.1.3" to="0.2.1": build-e2e-image
    ./tests/build_e2e_upgrade_image.sh {{from}} {{to}}

test-upgrade: build-upgrade-image
    PGS_E2E_IMAGE=$(cat .e2e-upgrade-image-tag) \
        cargo test --test e2e_upgrade_tests -- --ignored --test-threads=1 --nocapture
```

### 4. `tests/e2e/mod.rs`

No Rust changes are needed. The `PGS_E2E_IMAGE` env-var override already
exists and works. The justfile propagates the correct tag via that variable.

For documentation, update the module docstring to mention the tag-file
mechanism:

```rust
//! The Docker image tag is written to `.e2e-image-tag` by `build_e2e_image.sh`
//! and propagated by `just test-e2e` via the `PGS_E2E_IMAGE` env var.
//! To override: `PGS_E2E_IMAGE=pg_trickle_e2e:<tag> cargo test ...`
```

### 5. `.gitignore`

Add:

```
.e2e-image-tag
.e2e-upgrade-image-tag
```

---

## How Parallel Runs Become Safe

| Scenario | Before | After |
|---|---|---|
| `build-e2e-image` races `test-e2e` | Both touch `:latest`; clobber risk | Build writes `:<sha-A>`, test reads `:<sha-A>` from file — independent |
| Two branches building at once | Both overwrite `:latest` | Branch A writes `:<sha-A>`, Branch B writes `:<sha-B>`; no overlap |
| Re-run without rebuild (`test-e2e-fast`) | Already works | Still works — reads the tag file, no rebuild triggered |
| Dirty working tree | Same tag as last clean image | Gets a `-dirty` suffix, won't overwrite and won't be confused with clean builds |

---

## Migration / Compatibility Notes

- `docker-build-e2e` (alias in `justfile`) calls the same script; no extra
  changes needed.
- CI (`ci.yml`) workflow steps that call `build_e2e_image.sh` directly will
  automatically pick up the new behaviour; any subsequent `cargo test` step
  must be updated to pass `PGS_E2E_IMAGE=$(cat .e2e-image-tag)`.
- Old `:latest` images remain in the local Docker cache; run
  `docker image prune` periodically to reclaim space.
- If `.e2e-image-tag` is absent and `test-e2e-fast` is invoked, the `cat`
  command will fail with a clear error. A guard can be added:

```makefile
test-e2e-fast:
    @test -f .e2e-image-tag || (echo "ERROR: .e2e-image-tag missing — run 'just build-e2e-image' first" && exit 1)
    PGS_E2E_IMAGE=$(cat .e2e-image-tag) \
        cargo test --test 'e2e_*' -- --test-threads=1
```

---

## Isolation Audit: All Test Tiers

### Unit tests — fully isolated ✅

`just test-unit` compiles and runs a test binary with no database and no
containers. Multiple simultaneous invocations are unconditionally safe.

### Integration tests — fully isolated ✅

`just test-integration` uses `testcontainers_modules::postgres::Postgres`
(the official `postgres` image). Testcontainers assigns a fresh, random
container UUID and a random ephemeral host port for every run. Nothing is
shared between concurrent runs.

### E2E tests — containers isolated ✅, image tag not ⚠️

`just test-e2e` also uses Testcontainers for container lifecycle, so
concurrent test sessions get independent containers and ports. The only
collision risk is the `:latest` image tag being overwritten mid-run by a
simultaneous `build-e2e-image` — which is precisely what this plan fixes.

### pgrx tests — NOT isolated ⚠️ (known, low-priority)

`just test-pgrx` runs `cargo pgrx test pg18`, which starts a PostgreSQL
server against a single shared data directory: `~/.pgrx/data-18/`. A second
simultaneous invocation will attempt to start a second server against the
same directory, causing one or both runs to fail with a lock or socket
conflict.

**Impact**: only triggered by manually launching two `just test-pgrx` shells
at the same time. `just test-all` runs all tiers sequentially so it is not
affected. No fix is planned here.

**Workaround if needed**: maintain two separate `cargo pgrx init --datadir`
paths via the `PGRX_HOME` environment variable and configure each workspace to
point at a different one. This is an upstream pgrx limitation.

---

## File Checklist

- [x] `tests/build_e2e_image.sh` — SHA tag + write `.e2e-image-tag`
- [x] `tests/build_e2e_upgrade_image.sh` — read base tag from file; write `.e2e-upgrade-image-tag`
- [x] `justfile` — propagate `PGS_E2E_IMAGE` from tag file for all e2e targets
- [x] `tests/e2e/mod.rs` — update module docstring only
- [x] `.gitignore` — exclude both tag files
- [x] `.github/workflows/ci.yml` — propagate `PGS_E2E_IMAGE` in any step that runs e2e tests after the build step
