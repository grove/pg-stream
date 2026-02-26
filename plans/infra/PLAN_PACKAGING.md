# PLAN_PACKAGING.md — Distribution Packaging

> **Status:** Draft  
> **Target version:** v1.0.0  
> **Author:** pg_stream project

---

## 1. Overview

Three packaging targets for v1.0.0:

| Target | Audience | Priority |
|--------|----------|----------|
| PGXN | PostgreSQL community / developers | High |
| Debian/Ubuntu `.deb` | Linux sysadmins, PGDG apt users | High |
| RPM / RHEL | Enterprise Linux users, PGDG yum users | Medium |

All packages target **PostgreSQL 18** on **x86_64** and **aarch64**.

---

## 2. Prerequisites

```toml
# Cargo.toml — fields required by all packaging targets
[package]
name    = "pg_stream"
version = "1.0.0"
edition = "2021"
license = "Apache-2.0"
description = "PostgreSQL streaming tables with incremental view maintenance"
repository  = "https://github.com/geir/pg-stream"
```

Required tools:

```bash
cargo install cargo-pgrx    # pgrx build tooling
cargo install cargo-deb     # .deb builder
```

---

## 3. PGXN Packaging

### 3.1 `META.json`

```json
{
  "name": "pg_stream",
  "abstract": "Streaming tables with incremental view maintenance for PostgreSQL 18",
  "version": "1.0.0",
  "maintainer": "pg_stream contributors",
  "license": "apache_2_0",
  "provides": {
    "pg_stream": {
      "abstract": "Streaming tables with IVM",
      "file": "pg_stream.control",
      "docfile": "README.md",
      "version": "1.0.0"
    }
  },
  "prereqs": {
    "runtime": {
      "requires": { "PostgreSQL": "18.0.0" }
    }
  },
  "tags": ["ivm", "streaming", "materialized view", "cdc", "rust"],
  "release_status": "stable"
}
```

### 3.2 Build workflow

```bash
# Build the release artifact
cargo pgrx package --pg-config $(pg_config)

# Verify the package structure
ls target/release/pg_stream-pg18/

# Upload to PGXN (requires PGXN account + API key)
pgxn upload pg_stream-1.0.0.zip
```

### 3.3 CI job (`.github/workflows/pgxn.yml`)

```yaml
on:
  push:
    tags: ['v*']
jobs:
  pgxn:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - uses: pgxn/setup-pgxn@v1
        with: { pg-version: '18' }
      - run: cargo pgrx package --pg-config $(pg_config)
      - run: pgxn upload pg_stream-${GITHUB_REF_NAME#v}.zip
        env:
          PGXN_USERNAME: ${{ secrets.PGXN_USERNAME }}
          PGXN_PASSWORD: ${{ secrets.PGXN_PASSWORD }}
```

---

## 4. Debian / Ubuntu `.deb`

### 4.1 `Cargo.deb` configuration in `Cargo.toml`

```toml
[package.metadata.deb]
name            = "postgresql-18-pg-stream"
section         = "database"
priority        = "optional"
depends         = "postgresql-18"
maintainer-scripts = "debian/"
assets = [
  ["target/release/pg_stream.so",   "usr/lib/postgresql/18/lib/", "755"],
  ["pg_stream.control",             "usr/share/postgresql/18/extension/", "644"],
  ["pg_stream--*.sql",              "usr/share/postgresql/18/extension/", "644"],
]
```

### 4.2 Build

```bash
# Cross-compile for amd64 & arm64
cargo deb --target x86_64-unknown-linux-gnu
cargo deb --target aarch64-unknown-linux-gnu

# Test install on clean PG18 system
dpkg -i target/x86_64-unknown-linux-gnu/debian/postgresql-18-pg-stream_1.0.0_amd64.deb
psql -c "CREATE EXTENSION pg_stream;"
```

### 4.3 PPA Hosting

- Short term: GitHub Releases artifact
- Medium term: self-hosted apt repo via `aptly` + GitHub Pages
- Long term: PGDG community apt repository (requires PGDG maintainer sponsorship)

---

## 5. RPM / RHEL

### 5.1 Spec file outline (`pg_stream.spec`)

```spec
Name:       pg_stream_18
Version:    1.0.0
Release:    1%{?dist}
Summary:    PostgreSQL 18 streaming tables with IVM
License:    Apache-2.0
Requires:   postgresql18

%install
install -d %{buildroot}%{_libdir}/pgsql/
install -m 755 target/release/pg_stream.so %{buildroot}%{_libdir}/pgsql/
install -d %{buildroot}%{_datadir}/pgsql/extension/
install -m 644 pg_stream.control %{buildroot}%{_datadir}/pgsql/extension/
install -m 644 pg_stream--*.sql  %{buildroot}%{_datadir}/pgsql/extension/
```

### 5.2 Build

```bash
rpmbuild -ba pg_stream.spec
```

Distribution: GitHub Releases initially; PGDG yum repository if community
interest warrants it.

---

## 6. Testing Packages

Each package MUST be tested on a clean environment before release:

```bash
# Debian
docker run --rm -it ubuntu:24.04 bash -c "
  apt-get install -y postgresql-18 ./postgresql-18-pg-stream_1.0.0_amd64.deb &&
  pg_ctlcluster 18 main start &&
  psql -U postgres -c 'CREATE EXTENSION pg_stream; SELECT pgstream.version();'
"

# RPM
docker run --rm -it rockylinux:9 bash -c "
  dnf install -y postgresql18-server pg_stream_18-1.0.0-1.el9.x86_64.rpm &&
  postgresql-setup --initdb && systemctl start postgresql &&
  psql -U postgres -c 'CREATE EXTENSION pg_stream; SELECT pgstream.version();'
"
```

---

## 7. GitHub Actions Cost Note

See [GITHUB_ACTIONS_COST.md](GITHUB_ACTIONS_COST.md) for runner cost analysis.
Packaging jobs should only run on release tags (`v*`) to minimise spend.

---

## References

- [INSTALL.md](../../INSTALL.md)
- [Cargo.toml](../../Cargo.toml)
- [pg_stream.control](../../pg_stream.control)
- [PLAN_VERSIONING.md](PLAN_VERSIONING.md)
- [PLAN_DOCKER_IMAGE.md](PLAN_DOCKER_IMAGE.md)
- [GITHUB_ACTIONS_COST.md](GITHUB_ACTIONS_COST.md)
