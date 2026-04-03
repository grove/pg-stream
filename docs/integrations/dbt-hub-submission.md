# dbt Hub Submission Guide

This document describes how to publish `dbt-pgtrickle` to
[dbt Hub](https://hub.getdbt.com/) so users can install it with a simple
package name instead of a git URL.

## Background

dbt Hub is a package registry maintained by dbt Labs. Packages are indexed
by the [hubcap](https://github.com/dbt-labs/hubcap) automation, which runs
hourly and scans listed GitHub repositories for new tagged releases containing
a `dbt_project.yml` at the repository root.

## Current Status

`dbt-pgtrickle` lives in the `dbt-pgtrickle/` subdirectory of the
[grove/pg-trickle](https://github.com/grove/pg-trickle) monorepo. Because
hubcap expects `dbt_project.yml` at the repository root, a monorepo layout
requires one of the approaches below.

## Submission Approaches

### Option A: Separate Repository (recommended)

Create a standalone repository (e.g., `grove/dbt-pgtrickle`) that mirrors the
`dbt-pgtrickle/` directory. This is the standard pattern used by most Hub
packages (Fivetran, Snowplow, etc.).

1. Create `grove/dbt-pgtrickle` repository on GitHub.
2. Copy (or subtree-split) the `dbt-pgtrickle/` contents into the repo root.
3. Tag a release matching the version in `dbt_project.yml` (e.g., `v0.15.0`).
4. Submit a PR to [dbt-labs/hubcap](https://github.com/dbt-labs/hubcap)
   adding `"grove": ["dbt-pgtrickle"]` to `hub.json`.
5. Once merged, hubcap will automatically index new tags and publish versions.

After listing, users install with:

```yaml
packages:
  - package: grove/dbt_pgtrickle
    version: [">=0.15.0", "<1.0.0"]
```

### Option B: Keep Monorepo, Git Install Only

Continue using the git-based install with `subdirectory:`. This is fully
functional but requires users to specify a git URL and revision:

```yaml
packages:
  - git: "https://github.com/grove/pg-trickle.git"
    revision: v0.15.0
    subdirectory: "dbt-pgtrickle"
```

## Submission Checklist

- [x] `dbt_project.yml` has `name`, `version`, `config-version`, `require-dbt-version`
- [x] `dbt_project.yml` version synced with pg_trickle release (0.15.0)
- [x] `README.md` documents both git and Hub installation methods
- [x] Macros are in `macros/` directory
- [x] Tests are in `tests/` directory
- [x] Package has been tested with `dbt deps && dbt run && dbt test`
- [ ] Separate `grove/dbt-pgtrickle` repository created (if using Option A)
- [ ] Tagged release published on the standalone repo
- [ ] PR submitted to `dbt-labs/hubcap` adding `"grove": ["dbt-pgtrickle"]`
- [ ] Hub listing verified at `https://hub.getdbt.com/grove/dbt_pgtrickle/latest`

## Hub.json Entry Format

The PR to hubcap adds an entry to `hub.json`:

```json
{
    "grove": [
        "dbt-pgtrickle"
    ]
}
```

The key is the GitHub organization name (`grove`), and the value is an array of
repository names. Hubcap will scan `grove/dbt-pgtrickle` for tags matching
semantic versioning and index each version automatically.

## Version Syncing

The `dbt_project.yml` version should track the pg_trickle extension version to
avoid confusion. When releasing a new pg_trickle version:

1. Update `dbt-pgtrickle/dbt_project.yml` version.
2. If using a separate repo, sync the changes and tag a new release.
3. Hubcap will pick up the new tag within ~1 hour.

## References

- [dbt Packages documentation](https://docs.getdbt.com/docs/build/packages)
- [Building dbt packages guide](https://docs.getdbt.com/guides/building-packages)
- [hubcap repository](https://github.com/dbt-labs/hubcap)
- [hub.getdbt.com repository](https://github.com/dbt-labs/hub.getdbt.com)
