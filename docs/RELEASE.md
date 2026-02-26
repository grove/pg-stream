# Release Process

This document describes how to create a release of **pg_stream**.

## Overview

Releases are fully automated via GitHub Actions. Pushing a version tag (`v*`)
triggers the [Release workflow](../.github/workflows/release.yml), which:

1. Builds extension packages for Linux (amd64), macOS (arm64), and Windows (amd64)
2. Smoke-tests the Linux artifact against a live PostgreSQL 18 instance
3. Creates a GitHub Release with archives and SHA256 checksums
4. Builds and pushes a multi-arch Docker image to GHCR

## Prerequisites

- Push access to the repository (or a PR merged by a maintainer)
- All CI checks passing on `main`
- The version in `Cargo.toml` matches the tag you intend to push

## Step-by-Step

### 1. Decide the version number

Follow [Semantic Versioning](https://semver.org/):

| Change type                        | Bump    | Example         |
|------------------------------------|---------|-----------------|
| Breaking SQL API or config change  | Major   | `1.0.0 → 2.0.0` |
| New feature, backward-compatible   | Minor   | `0.1.0 → 0.2.0` |
| Bug fix, no API change             | Patch   | `0.2.0 → 0.2.1` |
| Pre-release / release candidate    | Suffix  | `0.3.0-rc.1`     |

### 2. Update the version in `Cargo.toml`

```bash
# Edit Cargo.toml — change the version field
# e.g., version = "0.2.0"
```

The extension control file (`pgstream.control`) uses
`default_version = '@CARGO_VERSION@'`, which pgrx replaces automatically at
build time — no manual edit needed.

### 3. Commit the version bump

```bash
git add Cargo.toml
git commit -m "release: v0.2.0"
git push origin main
```

### 4. Wait for CI to pass

Ensure the [CI workflow](../.github/workflows/ci.yml) passes on `main` with
the version bump commit. All unit, integration, E2E, and pgrx tests must be
green.

### 5. Create and push the tag

```bash
git tag -a v0.2.0 -m "Release v0.2.0"
git push origin v0.2.0
```

This triggers the Release workflow automatically.

### 6. Monitor the release

Watch the [Actions tab](../../actions/workflows/release.yml) for progress.
The workflow runs these jobs in order:

```
build-release (linux, macos, windows)  ──►  test-release  ──►  publish-release
                                                           ──►  publish-docker
```

### 7. Make the GHCR package public (first release only)

When a package is pushed to GHCR for the first time it is **private** by
default. Because this is an open-source project, packages linked to the
public repository inherit public visibility — but you must make the package
public once to unlock that:

1. Go to **github.com/⟨owner⟩ → Packages → pg_stream**
2. Click **Package settings**
3. Scroll to **Danger Zone** → **Change package visibility** → set to **Public**

After that first change:
- All future pushes keep the package public automatically
- Unauthenticated `docker pull ghcr.io/grove/pg_stream:...` works
- Storage and bandwidth are free (GHCR open-source advantage)
- The package page shows the README, linked repository, license, and
  description from the OCI labels

### 8. Verify the release

Once the workflow completes:

- [ ] Check the [GitHub Releases](../../releases) page for the new release
- [ ] Verify all three platform archives are attached (`.tar.gz` for Linux/macOS, `.zip` for Windows)
- [ ] Verify `SHA256SUMS.txt` is present
- [ ] Verify the Docker image is available at `ghcr.io/grove/pg_stream:<version>`
- [ ] Optionally pull and test the Docker image:

```bash
docker pull ghcr.io/grove/pg_stream:0.2.0
docker run --rm -e POSTGRES_PASSWORD=test ghcr.io/grove/pg_stream:0.2.0 \
  postgres -c "shared_preload_libraries=pg_stream"
```

## Release Artifacts

Each release produces:

| Artifact | Description |
|----------|-------------|
| `pg_stream-<ver>-pg18-linux-amd64.tar.gz` | Extension files for Linux x86_64 |
| `pg_stream-<ver>-pg18-macos-arm64.tar.gz` | Extension files for macOS Apple Silicon |
| `pg_stream-<ver>-pg18-windows-amd64.zip`  | Extension files for Windows x64 |
| `SHA256SUMS.txt` | SHA-256 checksums for all archives |
| `ghcr.io/grove/pg_stream:<ver>` | CNPG-ready Docker image (amd64 + arm64) |

### Installing from an archive

```bash
tar xzf pg_stream-0.2.0-pg18-linux-amd64.tar.gz
cd pg_stream-0.2.0-pg18-linux-amd64

sudo cp lib/*.so "$(pg_config --pkglibdir)/"
sudo cp extension/*.control extension/*.sql "$(pg_config --sharedir)/extension/"
```

Then add to `postgresql.conf` and restart:

```
shared_preload_libraries = 'pg_stream'
```

See [INSTALL.md](../INSTALL.md) for full installation details.

## Pre-releases

Tags containing `-rc`, `-beta`, or `-alpha` (e.g., `v0.3.0-rc.1`) are
automatically marked as pre-releases on GitHub. Pre-release Docker images are
tagged but do **not** update the `latest` tag.

## Hotfix Releases

For urgent fixes on an older release:

```bash
# Branch from the tag
git checkout -b hotfix/v0.2.1 v0.2.0

# Apply fix, bump version to 0.2.1
git commit -am "fix: ..."
git push origin hotfix/v0.2.1

# Tag from the branch (CI will still run the release workflow)
git tag -a v0.2.1 -m "Release v0.2.1"
git push origin v0.2.1
```

## Files to Update for Each Release

Every release requires manual updates to the files below. Missing any of them leads to version skew between the code, the docs, and the packages.

| File | What to change | Why |
|------|----------------|-----|
| `Cargo.toml` | `version = "x.y.z"` field | The canonical version source. pgrx reads this at build time and substitutes it into `pg_stream.control` via `@CARGO_VERSION@`. The git tag must match. |
| `CHANGELOG.md` | Rename `## [Unreleased]` → `## [x.y.z] — YYYY-MM-DD`; add a new empty `## [Unreleased]` at the top | Keeps the public changelog accurate and gives downstream users a dated record of changes. |
| `ROADMAP.md` | Update `**Current version:**` in the preamble; move the released milestone to a collapsed "Released" section or delete it; advance the "We are here" pointer to the next milestone | Keeps the forward-looking plan aligned with reality. Leaves no confusion about which milestone is current. |
| `README.md` | Update test-count line (`~N unit tests + M E2E tests`) if test counts changed significantly | The README is the first thing users read; stale numbers erode trust. |
| `INSTALL.md` | Update any version numbers in install commands or example URLs | Users copy-paste installation commands; stale versions cause failures. |
| `pg_stream.control` | **No manual edit needed** — `default_version` is set to `'@CARGO_VERSION@'` and pgrx substitutes it at build time. Verify the substitution in the built artifact. | Ensures the SQL `CREATE EXTENSION` command installs the right version. |

### Checklist summary

```
[ ] Cargo.toml — version bumped
[ ] CHANGELOG.md — [Unreleased] renamed to [x.y.z] with date; new empty [Unreleased] added
[ ] ROADMAP.md — current version updated; released milestone marked done
[ ] README.md — test counts current (if materially changed)
[ ] INSTALL.md — version references current
[ ] git tag matches Cargo.toml version
```

---

## Troubleshooting

### Release workflow failed

1. Check the failed job in the [Actions tab](../../actions/workflows/release.yml)
2. Common issues:
   - **Version mismatch**: `Cargo.toml` version doesn't match the tag — update and re-tag
   - **Build failure**: Fix the issue on `main`, delete the tag, and re-tag:
     ```bash
     git tag -d v0.2.0
     git push origin :refs/tags/v0.2.0
     # After fixing:
     git tag -a v0.2.0 -m "Release v0.2.0"
     git push origin v0.2.0
     ```
   - **Docker push failed**: Check that `packages: write` permission is enabled and `GITHUB_TOKEN` has GHCR access

### Yanking a release

If a release has a critical issue:

1. Mark it as pre-release on the GitHub Releases page (uncheck "Set as the latest release")
2. Add a warning to the release notes
3. Publish a patch release with the fix
