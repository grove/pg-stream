# SQL_GAPS_7 — Open Questions

**Date:** 2026-02-25
**Source:** [SQL_GAPS_7.md](SQL_GAPS_7.md) Prioritized Implementation Roadmap

---

## Tier 0 — Critical Correctness

### Q1. F1 (G4.1) — DELETE+INSERT Strategy Fix

The DELETE+INSERT merge strategy evaluates the delta query twice (once for
DELETE, once for INSERT). The DELETE mutates the stream table before the INSERT
phase reads it, causing stale reads for aggregate/DISTINCT queries.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Reject at selection time** | Block `delete_insert` strategy for queries where `needs_pgs_count() == true` | Simplest fix. Prevents the bug by disallowing the dangerous combination. Zero risk of regression. |
| **(b) Single-evaluation CTE** | `WITH delta AS MATERIALIZED (...)` then DELETE and INSERT both read from the same materialized result | More robust — preserves the strategy as an option. Both phases see identical data. Moderate complexity. |
| **(c) Remove strategy entirely** | Drop the `delete_insert` GUC option; always use MERGE | Eliminates the entire bug class. Simplest long-term. But removes a fallback for edge cases where MERGE has issues (e.g., PG versions with MERGE bugs). |

**Preferred:** **(a) Reject at selection time.** It's the smallest, safest change.
The strategy is already gated behind an explicit GUC that defaults to `auto`
(MERGE), so very few users are affected. If `delete_insert` is later needed
for non-aggregate queries, it remains available.

---

### Q2. F5 (G1.1) — JOIN Key Change with Simultaneous Right-Side Delete

When a row's join key is updated in the same refresh cycle as the old join
partner is deleted, the delta query reads `current_right` (post-change state)
and fails to produce the correct DELETE for the stale join result.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(A) CTE snapshot** | Read the right side at the frontier LSN using `pg_snapshot_xmin()` | Highest correctness. But requires SERIALIZABLE or snapshot export — complex and may reduce concurrency. |
| **(B) Dual-phase delta** | Compute DELETE delta using old state, INSERT delta using new state | Architecturally invasive. Doubles query cost. Would require deep changes to all join diff operators. |
| **(C) Compensating anti-join** | After MERGE, detect orphaned rows whose join partner no longer exists and emit corrective DELETEs | Moderate complexity. Adds a post-MERGE cleanup pass. Correct but increases refresh time slightly. |
| **(D) Document + FULL fallback** | Document the edge case. Rely on the existing adaptive threshold to trigger FULL refresh when large batches of key-modifying UPDATEs occur | Pragmatic. No code change. Relies on existing fallback mechanism. Risk: small batches of key changes may not trigger the threshold. |

**Preferred:** **(D) Document + FULL fallback** for now. The scenario requires
simultaneous key change + related row deletion in the same refresh cycle, which
is uncommon. The adaptive FULL fallback already handles large mutation batches.
Option C is the right follow-up if customer reports surface real-world impact.

---

### Q3. F2/F3 (G2.1, G2.2) — WAL Decoder Requirements

The WAL decoder has two P1 issues: keyless tables get `pk_hash = 0` (instead
of content hash), and UPDATE events have `old_*` columns always NULL.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Content hashing in WAL decoder** | Implement all-column content hashing matching trigger behavior | Full compatibility with trigger-based CDC. Higher implementation effort (~8–12h for old_* columns). |
| **(b) Require PRIMARY KEY for WAL mode** | Reject keyless tables when WAL CDC is active | Simpler. Avoids the hash mismatch problem entirely. Limits WAL mode to well-designed schemas. |
| **(c) Require REPLICA IDENTITY FULL** | For old_* column support, require `ALTER TABLE ... REPLICA IDENTITY FULL` on source tables | PostgreSQL's native solution. Ensures old tuple data is available in pgoutput. Storage/performance overhead on the source table. |

**Preferred:** **(b) Require PRIMARY KEY for WAL mode** for pk_hash, combined
with **(c) Require REPLICA IDENTITY FULL** for old_* columns. WAL mode is an
opt-in performance optimization — requiring PK + REPLICA IDENTITY FULL is
a reasonable trade-off. Document clearly in the WAL mode prerequisites.

---

## Tier 1 — Verification

### Q4. F8 (G1.2) — Window Partition Key Change Verification

When a row's PARTITION BY column is updated, the row moves between partitions.
The scan emits DELETE(old) + INSERT(new), which should trigger recomputation
of both the old and new partitions.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Test-first, then decide** | Write targeted E2E test. If it passes → downgrade to P4. If it fails → implement fix. | Efficient — avoids spending 4–6h on a fix that may not be needed. The partition-based recomputation may already handle this correctly. |
| **(b) Fix proactively** | Implement explicit dual-partition recomputation without waiting for test results | Guarantees correctness but may be unnecessary work if it already works. |

**Preferred:** **(a) Test-first, then decide.** The existing partition
recomputation logic may already handle this correctly through the DELETE+INSERT
split. A 2-hour E2E test resolves the question definitively.

---

### Q5. F9 (G1.3) — Recursive CTE Monotonicity Fallback

Semi-naive evaluation is incorrect for non-monotone recursive terms (those
containing EXCEPT, NOT EXISTS, or aggregation). The system must detect
non-monotone terms and choose an appropriate strategy.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Always recomputation for non-monotone** | Detect non-monotone operators in the recursive term and force full recomputation | Safe and simple. Recomputation is always correct. May be slower for large recursive results but guarantees correctness. |
| **(b) Use DRed for some non-monotone cases** | Attempt DRed (delete-and-rederive) for stratifiable non-monotone terms | DRed can handle some non-monotone cases correctly (e.g., stratified negation). More efficient when applicable but complex to implement the stratification check. |

**Preferred:** **(a) Always recomputation for non-monotone.** Non-monotone
recursive CTEs are rare in practice. The correctness guarantee outweighs
the performance cost. DRed optimization can be added later if profiling
shows recursive CTEs are a bottleneck.

---

### Q6. F11 (G7.1) — Keyless Table Duplicate Row Handling

Tables without a PRIMARY KEY use content hashing for row identity. Two
identical rows produce the same hash, causing incorrect delta behavior.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Document only** | Add prominent documentation about the limitation | Minimal effort. Users with keyless tables and duplicate rows are warned. |
| **(b) Document + WARNING at creation** | Emit a `WARNING` when `create_stream_table()` detects a source table with no PK/UNIQUE constraint | Proactive user notification. Low effort (1 extra SPI check). Prevents surprise failures. |
| **(c) Document + WARNING + ctid tiebreaker** | Use `ctid` as a secondary hash input to distinguish identical rows | Most correct, but `ctid` changes on VACUUM and UPDATE, so the tiebreaker itself is unstable. Creates a different class of bugs. |

**Preferred:** **(b) Document + WARNING at creation.** A WARNING at stream table
creation time is low-cost and catches the problem early. Users can then add a
PK or UNIQUE constraint. Avoid ctid — its instability creates worse problems
than it solves.

---

### Q7. F12 (G8.1) — PgBouncer Compatibility

PgBouncer in transaction-mode pooling doesn't support session-level advisory
locks, prepared statements, or LISTEN/NOTIFY. The scheduler uses advisory
locks for concurrency control.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Transaction-scoped advisory locks** | Replace `pg_advisory_lock()` with `pg_advisory_xact_lock()` | PgBouncer-safe. Lock is held only for the transaction duration. Simple drop-in replacement. Risk: lock is released between transactions within a refresh cycle. |
| **(b) Row-level locking on catalog** | Use `SELECT ... FOR UPDATE SKIP LOCKED` on `pgs_stream_tables` rows | PgBouncer-safe. No advisory locks needed. Natural fit — each ST "claims" its own catalog row. Concurrent refreshes skip already-locked STs. |
| **(c) Both + documentation** | Implement (b) as primary, keep (a) as fallback, document PgBouncer guidance | Most robust. Two independent concurrency mechanisms. More code to maintain. |

**Preferred:** **(b) Row-level locking on catalog.** `FOR UPDATE SKIP LOCKED`
is the cleanest solution — it's PgBouncer-safe, doesn't require advisory lock
IDs, and naturally maps to the per-ST refresh model. It also enables future
parallel refresh (multiple workers each lock different ST rows).

---

## Tier 2 — Robustness

### Q8. F13 (G4.2) — LIMIT in Subquery Without ORDER BY

`LIMIT` in a FROM subquery or lateral subquery without `ORDER BY` produces
non-deterministic results — full and differential refresh may pick different rows.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) WARNING** | Allow but emit a WARNING about non-determinism | Permissive. `LIMIT` + `ORDER BY` in laterals is a common, valid pattern. Only the missing `ORDER BY` case is problematic. |
| **(b) ERROR** | Reject `LIMIT` in subqueries without `ORDER BY` entirely | Strictest. Prevents non-determinism. May block valid use cases where the user doesn't care about which row is picked. |
| **(c) ERROR for DIFF, allow for FULL-only** | Reject in DIFFERENTIAL mode (where non-determinism causes drift), allow in FULL-only mode | Most nuanced. Correctly identifies that the issue is differential divergence, not the query itself. More complex logic. |

**Preferred:** **(a) WARNING.** Most PostgreSQL users expect `LIMIT` in laterals
to work. A WARNING surfaces the risk without blocking legitimate patterns.
Users who need determinism will add `ORDER BY`; users doing `LATERAL (... LIMIT 1)`
for "any row" semantics can proceed with awareness.

---

### Q9. F14 (G5.2) — CUBE Combinatorial Explosion Limit

`CUBE(a, b, c, ..., n)` generates $2^n$ grouping sets, each becoming a UNION ALL
branch. Large CUBEs can exhaust memory.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Hardcoded limit (64)** | Reject if $2^n > 64$ | Simple. 64 branches covers CUBE on up to 6 columns, which handles virtually all real-world cases. |
| **(b) Hardcoded limit (128)** | Reject if $2^n > 128$ | More permissive. CUBE on 7 columns. Still safe from OOM. |
| **(c) GUC-configurable limit** | `pg_stream.max_grouping_set_branches` with default 64 | Most flexible — power users can raise it. Adds a GUC. |

**Preferred:** **(a) Hardcoded limit of 64.** CUBE on 6+ columns is exceedingly
rare and likely a mistake. A hardcoded limit avoids GUC proliferation. The error
message should suggest explicit `GROUPING SETS(...)` as an alternative.

---

## Tier 3 — Test Coverage

### Q10. F17–F26 — Test File Organization

10 test tasks cover untested operators, auto-rewrites, GUC variations, and
multi-cycle refresh patterns.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Single new file** | All new tests in `e2e_aggregate_coverage_tests.rs` (aggregates) + additions to existing files for other operators | Simple organization. Aggregates are the bulk of the work (21 variants). Other tests fit in existing files (`e2e_create_tests.rs`, `e2e_refresh_tests.rs`). |
| **(b) Per-operator new files** | New files: `e2e_full_join_tests.rs`, `e2e_set_operation_tests.rs`, `e2e_scalar_subquery_tests.rs`, etc. | Cleaner separation. Each file is focused. Easier to run individual operator suites. Adds 5-6 new test files. |
| **(c) Hybrid** | One new file for aggregate batch (`e2e_aggregate_coverage_tests.rs`), add other tests to existing files where they fit, new files only for completely unrepresented operators | Balanced. Keeps the test directory manageable while maintaining logical grouping. |

**Preferred:** **(c) Hybrid.** The 21 aggregate tests warrant their own file.
FULL JOIN tests fit in `e2e_create_tests.rs` or a new `e2e_join_tests.rs`.
INTERSECT/EXCEPT fit in a new `e2e_set_operation_tests.rs`. GUC and multi-cycle
tests fit in `e2e_refresh_tests.rs`. This avoids both a monolithic test file
and excessive file proliferation.

---

## Cross-Cutting / Scope

### Q11. WAL Mode for 1.0

The WAL decoder has three P1 issues (F2, F3, F4). The trigger-based CDC is
production-ready.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) Trigger-only for 1.0, WAL experimental** | Ship 1.0 with trigger CDC only. Mark WAL mode as experimental/unsupported. Fix F4 (parsing, 2-3h) as hygiene. Defer F2+F3 (~14h). | Fastest to 1.0. WAL mode is opt-in behind a GUC already. No user expects WAL mode in a first release. |
| **(b) WAL production-ready for 1.0** | F2+F3+F4 (~14–21h) are 1.0 blockers. WAL mode fully supported at launch. | Higher effort. Gives users a migration path to lower-overhead CDC from day one. |
| **(c) Remove WAL code for 1.0** | Strip WAL decoder from the release binary. Reintroduce when ready. | Cleanest release. No dead code. But loses the existing implementation work and makes re-integration harder. |

**Preferred:** **(a) Trigger-only for 1.0, WAL experimental.** The trigger-based
CDC is solid and sufficient for 1.0. WAL mode can be promoted in a 1.1 release
after F2+F3 are implemented and battle-tested. Fix F4 (parsing) regardless — it's
a 2-3h hygiene fix that prevents misclassification even in experimental mode.

---

### Q12. 1.0 Release Scope

How much of the roadmap must be completed before 1.0?

| Alternative | Tier Coverage | Effort | What It Includes |
|-------------|--------------|--------|-----------------|
| **(a) Tiers 0+1 only** | P0/P1 fixes | ~39–57h | All correctness issues closed. Test gaps remain — some operators are unverified under real PG execution. |
| **(b) Tiers 0+1+3** | P0/P1 + test coverage | ~70–95h | Correctness + comprehensive E2E validation. High confidence in release quality. |
| **(c) Tiers 0+1+2+3** | Through robustness | ~75–104h | Adds P2 fixes (LIMIT warning, CUBE limit, RANGE_AGG, replica detection). Marginal effort over (b). |
| **(d) Tiers 0–4** | Through operational | ~100–140h | Full production hardening. Monitoring, retry logic, PgBouncer docs, upgrade paths. |

**Preferred:** **(c) Tiers 0+1+2+3.** The P2 items (F13, F14, F15, F16) add
only ~7–9h over the test coverage scope but prevent crashes and confusing errors.
Tier 4 (operational hardening) can follow in a 1.0.x patch cycle. This gives a
release that's correct, well-tested, and robust — ~75–104h across ~7 sessions.

---

### Q13. Session Execution Order

The recommended plan runs 9 sessions, front-loading non-WAL fixes (Sessions 1–6)
and deferring WAL decoder work to Session 7.

| Alternative | Description | Rationale |
|-------------|-------------|-----------|
| **(a) As proposed** | Sessions 1–6 focus on trigger-based correctness and tests. Session 7 tackles WAL. Sessions 8–9 are operational hardening. | Maximizes value for the default (trigger) path first. WAL fixes only matter when WAL mode is enabled. |
| **(b) WAL earlier (Session 3–4)** | Move F2+F3+F4 to Session 3, shift test coverage to Sessions 5–6 | Gets WAL mode production-ready sooner. Useful if WAL is a 1.0 requirement. |
| **(c) Test coverage earlier** | Move F17–F26 (test coverage) to Sessions 3–4, before DDL tracking fixes | Tests may surface unknown bugs in existing operators, informing whether DDL/WAL fixes need reprioritization. |

**Preferred:** **(a) As proposed**, with one modification: move a small batch of
high-value tests (F18: FULL JOIN, F19: INTERSECT/EXCEPT) into Session 2 alongside
the DDL tracking work. These operators have 400+ lines of untested delta SQL and
testing them early could surface P1 bugs before the codebase grows further.
