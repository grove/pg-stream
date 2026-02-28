# PLAN_PG_BACKCOMPAT.md — Supporting Older PostgreSQL Versions (13–17)

> **Status:** Research / Draft  
> **Date:** 2026-02-28  
> **Author:** pg_trickle project

---

## 1. Executive Summary

pg_trickle currently targets **PostgreSQL 18 only** via pgrx 0.17.0 with a
single `pg18` feature flag. This document analyzes what it would take to
backport support to PostgreSQL 13–17, modelled after pg_ivm which supports
PG 13–18 from a single codebase.

**Key finding:** pgrx 0.17.0 already supports PG 13–18 via feature flags.
The framework-level support is there. The challenge is entirely in our own
code — specifically the **1,036 `unsafe` blocks in the parse-tree walker**
(`src/dvm/parser.rs`) and the parser API signatures that changed across PG
major versions.

### Feasibility verdict by PG version

| Target PG | Feasibility | Effort | Key Blockers |
|-----------|-------------|--------|-------------|
| **PG 17** | **High** | Low (days) | Virtual generated column handling (minor) |
| **PG 16** | **Medium-High** | Medium (1–2 weeks) | JSON_TABLE nodes absent; ~250 lines gated |
| **PG 15** | **Medium** | Medium-High (2–3 weeks) | All SQL/JSON constructor nodes absent; ~400 lines gated |
| **PG 14** | **Medium-Low** | High (3–5 weeks) | PG 14 was the version where parser APIs stabilized to current form; some struct field changes |
| **PG 13** | **Low** | Very High (5–8 weeks) | `raw_parser()` and `parse_analyze_fixedparams()` have incompatible signatures; NodeTag values shifted |

---

## 2. pgrx Multi-Version Support Model

pgrx 0.17.0 supports PG 13–18. From pgrx's README:

> "Support from Postgres 13 to Postgres 18 from the same codebase.
> Use Rust feature gating to use version-specific APIs."

### 2.1 How it works

Each PG version is a Cargo feature flag:

```toml
[features]
default = ["pg18"]
pg13 = ["pgrx/pg13", "pgrx-tests/pg13"]
pg14 = ["pgrx/pg14", "pgrx-tests/pg14"]
pg15 = ["pgrx/pg15", "pgrx-tests/pg15"]
pg16 = ["pgrx/pg16", "pgrx-tests/pg16"]
pg17 = ["pgrx/pg17", "pgrx-tests/pg17"]
pg18 = ["pgrx/pg18", "pgrx-tests/pg18"]
```

The `pg_sys::*` module is **regenerated via bindgen** for each PG version.
When you compile with `--features pg16`, all `pg_sys::` types reflect PG 16's
header files — different struct layouts, different NodeTag values, different
function signatures are all handled transparently by pgrx's build system.

### 2.2 Version-conditional compilation

```rust
#[cfg(any(feature = "pg16", feature = "pg17", feature = "pg18"))]
fn handle_json_table(node: *mut pg_sys::JsonTable) -> Result<...> { ... }

#[cfg(any(feature = "pg13", feature = "pg14", feature = "pg15"))]
fn handle_json_table(_node: *mut pg_sys::Node) -> Result<...> {
    // JSON_TABLE not supported on PG < 16
    Err(PgTrickleError::UnsupportedFeature("JSON_TABLE requires PG 16+"))
}
```

---

## 3. Code Audit: What Needs to Change

### 3.1 Parser API Signatures (Highest Impact)

| API | PG 13 | PG 14+ (current) |
|-----|-------|-------------------|
| `raw_parser()` | 1 arg: `raw_parser(query)` | 2 args: `raw_parser(query, RawParseMode)` |
| `parse_analyze_fixedparams()` | Does not exist; uses `parse_analyze()` with different signature | 5 args (current form) |
| `makeFuncCall()` | 3 args | 4 args (added `COERCE_EXPLICIT_CALL`) |

**Impact:** `raw_parser()` and `parse_analyze_fixedparams()` are called in
`src/api.rs` (~10 sites) and `src/dvm/parser.rs` (~8 sites). These would need
`#[cfg]` guards for PG 13 vs PG 14+.

**Recommendation:** If we target PG 14+ (dropping PG 13), the parser API is
identical and this problem disappears entirely. PG 13 EOL is **November 2025**
(already past). **Targeting PG 14–18 is the pragmatic choice.**

### 3.2 Parse Tree Node Availability

| Feature / Node | Added In | Lines Affected | Approach |
|---------------|----------|---------------|----------|
| `T_JsonIsPredicate` | PG 16 | ~20 lines | `#[cfg]` gate |
| `T_JsonObjectConstructor`, `T_JsonArrayConstructor`, `T_JsonParseExpr`, `T_JsonScalarExpr`, `T_JsonSerializeExpr`, `T_JsonObjectAgg`, `T_JsonArrayAgg` | PG 16 | ~150 lines | `#[cfg]` gate |
| `T_JsonTable`, `T_JsonTableColumn`, `JsonBehaviorType`, `JsonWrapper`, `JsonQuotes` | PG 17 | ~250 lines | `#[cfg]` gate |
| `AggFunc::JsonObjectAggStd`, `AggFunc::JsonArrayAggStd` | PG 16+ (app-level) | ~10 lines | `#[cfg]` gate |
| Virtual generated columns (`attgenerated` values) | PG 18 | ~5 lines | No change needed (STORED exists since PG 12) |

**Total lines requiring `#[cfg]` gating:** ~435 lines in `src/dvm/parser.rs`,
concentrated in the JSON/SQL standard section (lines ~8170–8660).

### 3.3 Catalog Query Compatibility

| Column / Function | Min PG | Used In | Risk |
|------------------|--------|---------|------|
| `pg_proc.prokind` | PG 11 | `parser.rs` | Safe for PG 13+ |
| `pg_replication_slots.wal_status` | PG 13 | `wal_decoder.rs`, `monitor.rs` | Safe for PG 13+ |
| `pg_class.relreplident` | PG 9.4 | `cdc.rs` | Safe |
| `pg_get_viewdef()` trailing-semicolon behavior | Changed in PG 18 | `catalog.rs` | Needs `#[cfg]` or runtime detection |
| `RangeTblEntry.perminfoindex` | PG 16 | Not directly used | Safe (pgrx handles) |
| `MAINTAIN` privilege | PG 17 | Not used | N/A |
| `CheckIndexCompatible()` | PG 18 added extra arg | Not used | N/A |
| `flatten_join_alias_vars()` | PG 16 changed signature | Not directly called | N/A |

### 3.4 FRAMEOPTION Constants

Stable since PG 11. No changes needed.

### 3.5 Shared Memory & Background Workers

`PgLwLock`, `PgAtomic`, `pg_shmem_init!`, `BackgroundWorkerBuilder` — all
stable across PG 13–18 via pgrx abstractions. No changes needed.

### 3.6 SPI & Trigger APIs

Stable across all target versions. The trigger-based CDC approach (AFTER
triggers with transition tables) works identically on PG 13–18.

### 3.7 DDL Event Triggers

`event_trigger` functions work on PG 13+. No changes needed.

---

## 4. How pg_ivm Handles Multi-Version Support

pg_ivm (written in C) uses `#if defined(PG_VERSION_NUM)` preprocessor guards
throughout its codebase. Key patterns observed:

### 4.1 Version-Specific Source Files

pg_ivm ships **separate source files** for code that varies significantly:
- `ruleutils.c` — main version
- `ruleutils_13.c` — PG 13-specific variant
- `ruleutils_14.c` — PG 14-specific variant

Their Makefile includes the appropriate file based on `pg_config --version`.

### 4.2 Inline `#if` Guards

For smaller differences, pg_ivm uses inline guards:

```c
// PG 14+ changed CreateTableAsStmt field name
#if defined(PG_VERSION_NUM) && (PG_VERSION_NUM >= 140000)
    ctas->objtype = OBJECT_MATVIEW;
#else
    ctas->relkind = OBJECT_MATVIEW;
#endif

// PG 16 changed flatten_join_alias_vars() signature
#if defined(PG_VERSION_NUM) && (PG_VERSION_NUM >= 160000)
    op = (OpExpr *) flatten_join_alias_vars(NULL, qry, (Node *) op);
#else
    op = (OpExpr *) flatten_join_alias_vars(qry, (Node *) op);
#endif

// PG 16 restructured permission checking fields on RTE
#if defined(PG_VERSION_NUM) && (PG_VERSION_NUM >= 160000)
    rte->perminfoindex = 0;
#else
    rte->requiredPerms = 0;
    rte->checkAsUser = InvalidOid;
    rte->selectedCols = NULL;
    rte->insertedCols = NULL;
    rte->updatedCols = NULL;
    rte->extraUpdatedCols = NULL;
#endif

// PG 14+ changed makeFuncCall() signature
#if defined(PG_VERSION_NUM) && (PG_VERSION_NUM >= 140000)
    fn = makeFuncCall(SystemFuncName("count"), NIL, COERCE_EXPLICIT_CALL, -1);
#else
    fn = makeFuncCall(SystemFuncName("count"), NIL, -1);
#endif

// PG 18 changed CheckIndexCompatible() signature
#if defined(PG_VERSION_NUM) && (PG_VERSION_NUM >= 180000)
    if (CheckIndexCompatible(indexRel->rd_id, ..., false))
#else
    if (CheckIndexCompatible(indexRel->rd_id, ...))
#endif
```

### 4.3 Rust/pgrx Equivalent

In our Rust/pgrx codebase, the equivalent is:

```rust
// Use Cargo feature flags instead of C preprocessor
#[cfg(any(feature = "pg14", feature = "pg15", feature = "pg16",
          feature = "pg17", feature = "pg18"))]
fn raw_parse(sql: &CStr) -> *mut pg_sys::List {
    unsafe { pg_sys::raw_parser(sql.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) }
}

#[cfg(feature = "pg13")]
fn raw_parse(sql: &CStr) -> *mut pg_sys::List {
    unsafe { pg_sys::raw_parser(sql.as_ptr()) }
}
```

---

## 5. Recommended Strategy

### 5.1 Target PG 14–18 (Recommended)

**Drop PG 13 support.** PG 13 reached end-of-life in November 2025. The
`raw_parser()` / `parse_analyze_fixedparams()` API break between PG 13 and
PG 14 is substantial and affects the most critical code path.

**Rationale:**
- PG 13 is EOL
- Parser API signatures changed between PG 13 → 14
- pg_ivm supports PG 13 but has to maintain separate `ruleutils_13.c`
- pgrx 0.17.0 supports PG 14–18 with feature flags; PG 13 is technically
  supported but adds significant maintenance burden

### 5.2 Implementation Phases

#### Phase 1: Cargo.toml Feature Flags (1 day)

```toml
[features]
default = ["pg18"]
pg14 = ["pgrx/pg14", "pgrx-tests/pg14"]
pg15 = ["pgrx/pg15", "pgrx-tests/pg15"]
pg16 = ["pgrx/pg16", "pgrx-tests/pg16"]
pg17 = ["pgrx/pg17", "pgrx-tests/pg17"]
pg18 = ["pgrx/pg18", "pgrx-tests/pg18"]
pg_test = []
```

Update `check-cfg` lint to include pg14–pg17 feature names.

#### Phase 2: Gate JSON/SQL Standard Nodes (3–5 days)

The ~435 lines of JSON parse-tree handling in `parser.rs` need conditional
compilation:

```rust
// JSON_TABLE (PG 17+)
#[cfg(any(feature = "pg17", feature = "pg18"))]
fn deparse_json_table(...) { ... }  // ~250 lines

// SQL/JSON constructors (PG 16+)
#[cfg(any(feature = "pg16", feature = "pg17", feature = "pg18"))]
fn deparse_json_constructor(...) { ... }  // ~150 lines

// For older PG versions, these NodeTag values simply won't appear in parse
// trees, so we can use a catch-all error/skip in the match arm.
```

**Key insight:** These nodes don't need "alternative implementations" — they
simply don't exist in older PGs' parse trees. If a user writes SQL with
`JSON_TABLE()` on PG 15, PostgreSQL itself will reject it before our extension
ever sees it. So the `#[cfg]` gates just need to exclude code that won't
compile, not provide fallback behavior.

#### Phase 3: `pg_get_viewdef()` Behavior Difference (1 day)

PG 18 changed `pg_get_viewdef()` to include a trailing semicolon. The
`strip_view_definition_suffix()` function in `catalog.rs` already handles
this. Verify it works on older PGs where the semicolon is absent.

#### Phase 4: CI Matrix Expansion (2–3 days)

```yaml
strategy:
  matrix:
    pg-version: ['14', '15', '16', '17', '18']
```

For each PG version:
- Update `tests/Dockerfile.e2e` to be parameterizable
- Update `tests/common/mod.rs` Testcontainers image tags
- Update `justfile` to accept `pg` variable (already done)

#### Phase 5: Dockerfiles & Packaging (2 days)

- Parameterize `Dockerfile.e2e` with `ARG PG_VERSION`
- Build/test for each supported PG version
- Update CNPG images

#### Phase 6: WAL Decoder Compatibility Validation (3–5 days)

The WAL decoder (`src/wal_decoder.rs`) was initially assessed as high-risk,
but deeper analysis revealed it is **not inherently PG 18-specific** at the
API level. See **Section 6A** below for the full analysis. This phase involves:
- Verifying `pgoutput` text format parsing against each target PG version
- Testing logical replication slot lifecycle on PG 14–17
- Validating `pg_replication_slots` catalog view columns
- Potentially disabling WAL-based CDC for PG versions where testing reveals
  incompatibilities, with trigger-based CDC as the universal fallback

### 5.3 Effort Estimation

| Phase | Effort | Risk |
|-------|--------|------|
| Phase 1: Feature flags | 1 day | Low |
| Phase 2: JSON node gating | 3–5 days | Medium (large unsafe code surface) |
| Phase 3: Behavior differences | 1 day | Low |
| Phase 4: CI matrix | 2–3 days | Low-Medium |
| Phase 5: Docker/packaging | 2 days | Low |
| Phase 6: WAL decoder validation | 3–5 days | Medium (testing, not rewriting) |
| **Total** | **~2.5–3 weeks** | |

---

## 6. Macro/Helper Pattern for Version Gating

To reduce boilerplate, define version range macros:

```rust
/// True when compiling for PG >= 16 (SQL/JSON constructors available)
macro_rules! pg_since_16 {
    () => {
        cfg!(any(feature = "pg16", feature = "pg17", feature = "pg18"))
    };
}

/// Conditional compilation attribute for PG >= 16
/// Usage: #[pg_gte_16]
#[cfg(any(feature = "pg16", feature = "pg17", feature = "pg18"))]
macro_rules! if_pg_gte_16 { ($($tt:tt)*) => { $($tt)* } }
#[cfg(not(any(feature = "pg16", feature = "pg17", feature = "pg18")))]
macro_rules! if_pg_gte_16 { ($($tt:tt)*) => {} }
```

Or more idiomatically, use the `cfg_aliases` crate:

```toml
[build-dependencies]
cfg_aliases = "0.2"
```

```rust
// build.rs
fn main() {
    cfg_aliases::cfg_aliases! {
        pg_gte_14: { any(feature = "pg14", feature = "pg15", feature = "pg16",
                         feature = "pg17", feature = "pg18") },
        pg_gte_16: { any(feature = "pg16", feature = "pg17", feature = "pg18") },
        pg_gte_17: { any(feature = "pg17", feature = "pg18") },
        pg_gte_18: { feature = "pg18" },
    }
}
```

Then in source code:

```rust
#[cfg(pg_gte_17)]
fn deparse_json_table(node: *mut pg_sys::JsonTable) -> Result<String, PgTrickleError> {
    // ... existing code
}
```

---

## 6A. WAL CDC Compatibility Analysis (Deep Dive)

Initial assessment rated the WAL decoder as "High risk" for backporting.
Deep analysis of `src/wal_decoder.rs` (1,406 lines) revealed this was
overly conservative.

### 6A.1 Key Finding: Pure SPI, No `pg_sys` Unsafe Calls

Unlike the parse-tree walker, the WAL decoder contains **zero `unsafe` blocks**
and **zero `pg_sys::` calls**. All PostgreSQL interaction is via SPI SQL
queries. This means there are no struct layout or function signature
compatibility concerns — the primary risk vectors for the DVM parser do not
apply here.

### 6A.2 API Availability by PG Version

| API / Feature | Introduced | Used In | Status |
|--------------|-----------|---------|--------|
| `pg_create_logical_replication_slot($1, 'pgoutput')` | PG 10 | `ensure_replication_slot()` | Safe for PG 14+ |
| `pg_logical_slot_get_changes()` | PG 10 | `poll_wal_changes()` | Safe for PG 14+ |
| `pgoutput` output plugin | PG 10 | All WAL decoding | Safe for PG 14+ |
| `pg_replication_slots.confirmed_flush_lsn` | PG 10 | `get_slot_lag()` | Safe for PG 14+ |
| `pg_replication_slots.wal_status` | PG 13 | `monitor.rs` | Safe for PG 14+ |
| `wal_level = logical` GUC | PG 10 | Prerequisite check | Safe for PG 14+ |
| `ALTER PUBLICATION` / `CREATE PUBLICATION` | PG 10 | Publication management | Safe for PG 14+ |

All core APIs have been available since PG 10. The entire WAL decoder
should compile and run without code changes on PG 14+.

### 6A.3 Actual Risks (Nuanced)

The risks are **testing and behavioral**, not API-level:

1. **`pgoutput` text format stability:** The WAL decoder parses the text
   representation returned by `pg_logical_slot_get_changes()` with the
   `pgoutput` plugin. This text format has only been tested against PG 18.
   While the format is part of the logical replication protocol and should
   be stable, minor formatting differences (e.g., type representation,
   NULL handling, TOAST behavior) could exist across versions.

2. **Logical replication reliability improvements:** Each PG release has
   improved logical replication reliability. PG 15 added `two_phase`
   support and streaming improvements. PG 16 improved parallel apply.
   PG 18 added further slot management improvements. The WAL decoder
   may encounter edge cases on older versions that have been fixed in
   newer releases.

3. **Slot behavior differences:** Replication slot lifecycle, conflict
   handling, and invalidation rules have evolved. The transition
   orchestration (TRIGGER → TRANSITIONING → WAL) assumes PG 18 slot
   behavior. Older versions may handle slot conflicts differently.

4. **Publication scope:** `FOR TABLE` publications work since PG 10,
   but some publication options (e.g., `publish_via_partition_root`)
   were added in PG 13+. Our publication management code should be
   audited for option availability.

### 6A.4 Recommendation: Progressive WAL CDC Rollout

**Ship trigger-based CDC on all supported PG versions immediately.** The
trigger CDC pipeline (`src/cdc.rs`) is pure SQL/SPI and works identically
on PG 14–18 with zero changes. This is the default CDC mode and covers
the primary use case.

For WAL-based CDC:
- **PG 18:** Fully supported (current, tested)
- **PG 16–17:** Enable after targeted testing of `pgoutput` format parsing
  and slot lifecycle. Likely works with no code changes — just needs
  validation.
- **PG 14–15:** Enable after extended testing. Lower priority given
  approaching EOL dates.

The `pg_trickle.cdc_mode` GUC already controls CDC mode selection. On
older PG versions where WAL CDC hasn't been validated, the extension
can default to trigger mode and return a clear error if WAL mode is
explicitly requested:

```rust
#[cfg(not(pg_gte_18))]
if config.cdc_mode == CdcMode::Wal {
    pgrx::warning!(
        "WAL-based CDC has not been validated on PG {}; using trigger CDC",
        pg_sys::PG_MAJORVERSION
    );
}
```

Alternatively, since the code is identical, WAL CDC can be enabled on all
versions behind a `pg_trickle.experimental_wal_cdc` GUC for pre-PG 18
versions, allowing users to opt in while we gather feedback.

---

## 7. Feature Degradation Matrix

Not all pg_trickle features need to be available on all PG versions.
Graceful degradation:

| Feature | PG 14 | PG 15 | PG 16 | PG 17 | PG 18 |
|---------|:-----:|:-----:|:-----:|:-----:|:-----:|
| Basic streaming tables | Yes | Yes | Yes | Yes | Yes |
| Trigger-based CDC | Yes | Yes | Yes | Yes | Yes |
| Differential refresh | Yes | Yes | Yes | Yes | Yes |
| Background worker scheduling | Yes | Yes | Yes | Yes | Yes |
| DAG dependency tracking | Yes | Yes | Yes | Yes | Yes |
| SQL/JSON constructors in views | -- | -- | Yes | Yes | Yes |
| JSON_TABLE in views | -- | -- | -- | Yes | Yes |
| WAL-based CDC (trigger fallback) | Trigger | Trigger | Likely* | Likely* | Yes |
| WAL-based CDC (native) | Needs testing | Needs testing | Needs testing | Needs testing | Yes |
| Virtual generated columns | -- | -- | -- | -- | Yes |

\* WAL CDC is expected to work on PG 16–17 with no code changes based on
API analysis (see Section 6A). Requires validation testing before enabling.

Clear error messages should be returned when a user tries to use an
unsupported feature on an older PG version.

---

## 8. Comparison: pg_trickle vs pg_ivm Multi-Version Approach

| Aspect | pg_ivm (C) | pg_trickle (Rust/pgrx) |
|--------|-----------|----------------------|
| Language | C with `#if PG_VERSION_NUM` | Rust with `#[cfg(feature = "pgXX")]` |
| Build system | PGXS `make install` | Cargo + pgrx |
| Version detection | Runtime `PG_VERSION_NUM` macro | Compile-time feature flags |
| Separate files per version | Yes (`ruleutils_13.c`, `ruleutils_14.c`) | Not needed (same file, cfg-gated) |
| Parse tree walking | Uses C `nodeTag()` directly | Uses `pg_sys::NodeTag` via `unsafe` casts |
| Testing | `pg_regress` | Testcontainers + pgrx-tests |
| Parse tree scope | Limited (SELECT, JOINs, aggs, subqueries, CTEs) | Full SQL coverage (window funcs, JSON, CTEs, all expressions) |

**Key difference:** pg_ivm operates on the **analyzed** Query tree (post-
rewrite), while pg_trickle's parser works on both raw parse trees and analyzed
Query trees. The raw parse tree is more version-sensitive because struct
layouts change more frequently at that level.

---

## 9. Risk Analysis

### 9.1 NodeTag Value Shifts

PostgreSQL assigns integer values to `NodeTag` enum entries. These values can
shift when new node types are added between versions. pgrx handles this
transparently — when compiling with `--features pg16`, the `NodeTag` enum has
PG 16's values. **No manual adjustment needed** as long as we use symbolic
constants (`pg_sys::NodeTag::T_SelectStmt`) rather than raw integers.

### 9.2 Struct Field Layout Changes

Parse tree structs occasionally gain, lose, or rename fields across PG majors.
Examples:
- `CreateTableAsStmt`: `.relkind` (PG 13) vs `.objtype` (PG 14+)
- `RangeTblEntry`: different permission fields in PG 16+
- `SelectStmt`: potential field additions

pgrx's bindgen regenerates all struct definitions for each PG version, so
**compilation will fail immediately** if we reference a non-existent field.
This is actually an advantage — we get compile-time safety rather than
runtime crashes.

### 9.3 Unsafe Block Correctness

The 1,036 unsafe blocks in `parser.rs` cast `*mut pg_sys::Node` to concrete
types based on NodeTag checks. These casts are safe as long as:
1. The NodeTag check is correct (ensured by using `pg_sys::NodeTag::T_*`)
2. The struct layout matches (ensured by pgrx bindgen per-version)
3. The field access is valid (ensured by Rust type system + bindgen)

Point 2 means that **if it compiles, it's correct** for each PG version.

---

## 10. Alternative Approaches Considered

### 10.1 SQL-Only Parsing (No pg_sys Parse Trees)

Replace the `pg_sys::raw_parser()` based approach with pure SQL-based query
analysis (e.g., regex or a Rust SQL parser like `sqlparser-rs`).

**Pros:** Zero PG-version dependency for parsing.  
**Cons:** Would lose access to PostgreSQL's actual parse tree, which is
essential for correct semantic analysis (OID resolution, type checking,
operator resolution). **Not viable** for the DVM engine.

### 10.2 Separate Crates per PG Version

Like pg_ivm's separate source files, maintain separate crate features with
completely independent parser implementations.

**Pros:** Clean separation.  
**Cons:** Massive code duplication. The parser is ~14,000 lines. **Not viable.**

### 10.3 Runtime Version Detection

Check `SHOW server_version_num` at extension load time and branch accordingly.

**Pros:** Single binary.  
**Cons:** Not possible with pgrx — the `pg_sys::` bindings are compile-time
fixed to a specific PG version. A binary compiled for PG 16 cannot run on
PG 18. **Not applicable.**

---

## 11. Recommended Minimum Viable Multi-Version Support

**Target: PG 16–18** as the initial multi-version release.

**Rationale:**
- PG 16+ represents the stable JSON/SQL constructor API
- Only JSON_TABLE (~250 lines) needs PG 17+ gating
- PG 14–15 EOL dates: November 2026 and November 2027 respectively
- Reduces initial scope significantly vs full PG 14–18 support
- Can expand to PG 14–15 in a follow-up release

**Effort for PG 16–18:** ~1.5 weeks total:
- Feature flags: 1 day
- JSON_TABLE gating: 2 days
- CI & Docker: 2–3 days
- Testing: 2–3 days

---

## 12. Open Questions

1. **Do we need PG 14–15 support?** What is the user demand? Check
   PostgreSQL adoption statistics and potential customer requirements.
2. **WAL decoder scope:** ~~Should WAL-based CDC be PG 18-only initially, with
   trigger-based CDC available on all supported versions?~~ **Resolved:** Deep
   analysis (Section 6A) shows WAL CDC uses pure SPI with APIs available since
   PG 10. Trigger CDC ships on all versions as the default. WAL CDC is enabled
   on PG 18 (tested) and can be progressively enabled on PG 16–17 after
   `pgoutput` format validation testing. No code changes needed — only testing.
3. **Release strategy:** Ship multi-version support in 1.0 or as a 1.1 follow-up?
4. **pgrx version pinning:** pgrx 0.17.0 is pinned. Would a newer pgrx
   release improve multi-version support or fix bugs?

---

## References

- [Cargo.toml](../../Cargo.toml) — current feature flags
- [PLAN_PG19_COMPAT.md](PLAN_PG19_COMPAT.md) — forward-compatibility plan
- [PLAN_VERSIONING.md](PLAN_VERSIONING.md) — versioning policy
- [pgrx README](https://github.com/pgcentralfoundation/pgrx) — multi-version support docs
- [pg_ivm](https://github.com/sraoss/pg_ivm) — reference implementation (C, PG 13–18)
- [PostgreSQL Versioning Policy](https://www.postgresql.org/support/versioning/)
- [src/dvm/parser.rs](../../src/dvm/parser.rs) — primary risk area
- [src/api.rs](../../src/api.rs) — parser API call sites
