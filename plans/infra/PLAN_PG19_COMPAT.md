# PLAN_PG19_COMPAT.md — PostgreSQL 19 Forward-Compatibility

> **Status:** Draft  
> **Target version:** Post-1.0 (Advanced SQL A3)  
> **Author:** pg_trickle project

---

## 1. Overview

pg_trickle currently targets PostgreSQL 18.x exclusively. This document covers
the strategy to add PostgreSQL 19 support without regressing PG18 support,
targeting the period after PostgreSQL 19 beta availability (estimated late 2026).

---

## 2. pgrx Version Dependency

pgrx tracks PostgreSQL major versions. Each pgrx minor release adds support
for a new PG major:

| pgrx version | PG support |
|-------------|-----------|
| 0.17.x | PG 14, 15, 16, 17, 18 |
| 0.18.x (expected) | PG 15, 16, 17, 18, 19 |

**Action:** When pgrx 0.18.x is released with PG19 support, bump `Cargo.toml`:

```toml
[dependencies]
pgrx = "0.18"
```

Run `cargo pgrx init --pg19 /path/to/pg19/pg_config` and rebuild.

---

## 3. `pg_sys::*` API Audit

The following pg_sys APIs are used in the codebase and may change across
major PG versions. An audit should be run before the 0.18.x pgrx bump:

```bash
# Find all pg_sys usages
grep -r 'pg_sys::' src/ | grep -v '//' | awk -F'pg_sys::' '{print $2}' | \
  grep -oE '^[A-Za-z_]+' | sort -u
```

Known risk areas based on current codebase:

| API category | Risk level | Notes |
|-------------|-----------|-------|
| `pg_sys::SPI_*` | Low | SPI API stable since PG9 |
| `pg_sys::BackgroundWorker*` | Low | Stable since PG9.4 |
| `pg_sys::LWLock*` | Low | Stable |
| `pg_sys::heap_form_tuple` | Medium | Heap access APIs restructured in PG17 |
| `pg_sys::RelationData` / `pg_sys::Form_pg_class` | Medium | Catalog struct layout can shift |
| `pg_sys::EventTriggerData` | Low | DDL triggers stable |
| WAL decoder `pg_sys::LogicalDecodingContext` | High | WAL API frequently changes across majors |

Run the full test suite against PG19 beta and record which pg_sys calls fail
to compile — those require conditional compilation.

---

## 4. Conditional Compilation Strategy

pgrx provides feature flags for each PG version:

```rust
#[cfg(feature = "pg18")]
fn foo_pg18() { ... }

#[cfg(feature = "pg19")]
fn foo_pg19() { ... }

#[cfg(any(feature = "pg18", feature = "pg19"))]
fn foo_both() { ... }
```

Where pg_sys structs have changed fields:

```rust
#[cfg(feature = "pg18")]
let nkeys = index_info.ii_NumIndexKeyAttrs;

#[cfg(feature = "pg19")]
let nkeys = index_info.ii_IndexAttrNumbers.len() as i16; // hypothetical
```

Keep version-divergent blocks small and clearly commented. Do not duplicate
entire functions — use helper closures or trait dispatch.

---

## 5. CI Matrix

Current:
```yaml
pg-version: ['18']
```

After PG19 beta is available:
```yaml
pg-version: ['18', '19']
```

The E2E test suite and integration tests both use Testcontainers with
`postgres:XX` images. Adding `19` to the matrix is the primary deliverable.

```yaml
# .github/workflows/ci.yml  (future)
strategy:
  matrix:
    pg-version: ['18', '19']
    os: [ubuntu-24.04]
```

Cost impact: doubles the CI matrix size. See
[PLAN_GITHUB_ACTIONS_COST.md](PLAN_GITHUB_ACTIONS_COST.md) for budget analysis.

---

## 6. WAL Decoder Specific Concerns

The WAL decoding path (`src/wal_decoder.rs`) uses logical decoding APIs that
are the most volatile across PG major versions. Known changes in PG17+ already
required code updates. For PG19:

1. Review the PostgreSQL 19 release notes for logical decoding changes.
2. Run `cargo check --features pg19` as soon as pgrx 0.18 is available.
3. WAL decoder feature may need to be disabled for PG19 initially with a
   `#[cfg(not(feature = "pg19"))]` guard until ported.

---

## 7. Timeline

| Event | Estimated Date |
|-------|---------------|
| PostgreSQL 19 alpha 1 | May 2026 |
| PostgreSQL 19 beta 1 | June 2026 |
| pgrx 0.18.x with PG19 support | July–August 2026 |
| pg_trickle PG19 CI green | August–September 2026 |
| pg_trickle 1.1.0 with PG19 support | September–October 2026 |

---

## 8. PG19 Compatibility Checklist

- [ ] pgrx 0.18.x released and Cargo.toml bumped
- [ ] `cargo check --features pg19` passes (no compile errors)
- [ ] `just test-unit` passes on PG19
- [ ] `just test-integration` passes on PG19 (Testcontainers `postgres:19`)
- [ ] `just test-e2e` passes on PG19
- [ ] WAL decoder compiles and passes WAL tests on PG19
- [ ] CI matrix updated with `pg-version: ['18', '19']`
- [ ] CHANGELOG.md and README updated with PG19 support
- [ ] Compatibility matrix in PLAN_VERSIONING.md updated

---

## References

- [Cargo.toml](../../Cargo.toml)
- [plans/infra/PLAN_GITHUB_ACTIONS_COST.md](PLAN_GITHUB_ACTIONS_COST.md)
- [plans/infra/PLAN_VERSIONING.md](PLAN_VERSIONING.md)
- [src/wal_decoder.rs](../../src/wal_decoder.rs)
- [pgrx releases](https://github.com/pgcentralfoundation/pgrx/releases)
- [PostgreSQL versioning policy](https://www.postgresql.org/support/versioning/)
