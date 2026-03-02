# Plan: Cross-Source Snapshot Consistency

Date: 2026-03-02
Status: PROPOSED
Last Updated: 2026-03-02

---

## 1. Problem Statement

### Topology

[PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md)
addresses the **single-source diamond** problem — where two intermediate
stream tables (B, C) share a common upstream source (A) and converge at a
downstream stream table (D). The epoch-based atomic group solution (Option 1)
guarantees that D never observes a split version of A.

This plan addresses a **different, structurally undetectable problem**: two
intermediate stream tables that share *no* common upstream ancestor but
converge at the same downstream node.

```
B1 (base table)    C1 (base table)
      │                   │
      ▼                   ▼
     B2                  C2
      │                   │
      └─────────┬─────────┘
                ▼
                D
```

- B1 and C1 are **independent** base tables (or independent stream tables
  with no common source).
- B2 depends solely on B1: `B2 = f(B1)`.
- C2 depends solely on C1: `C2 = g(C1)`.
- D joins B2 and C2: `D = h(B2, C2)`.

### Why the Diamond Plan Does Not Apply

The diamond detection algorithm in
[PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md)
detects shared ancestors:

```
ancestors(path B1→B2→D) ∩ ancestors(path C1→C2→D) = {B1} ∩ {C1} = ∅
```

No shared ancestor ⇒ no consistency group is formed ⇒ B2 and C2 refresh as
independent singletons. When D joins them, it may see:

- B2 reflecting B1 at wall-clock time T₁.
- C2 reflecting C1 at wall-clock time T₂, where T₂ ≠ T₁.

This is not a data corruption issue, and it does not violate the per-ST
Delayed View Semantics (DVS) guarantee. But it means D reflects a
**temporally incoherent join**: the B1 "world" and the C1 "world" seen by D
correspond to different moments in time.

### Severity

The impact depends on the use case:

- **Loosely coupled reporting** (e.g. D joins a product catalogue and a
  sales ledger that change independently): Usually acceptable. Minor
  staleness across independent dimensions is expected and tolerable.
- **Tightly coupled cross-domain joins** (e.g. D joins a positions table
  with a prices table for mark-to-market, or joins an orders table with a
  FX rates table): Potentially significant. A 10-second window where prices
  and positions reflect different points in time can produce materially
  incorrect P&L figures.
- **Audit / compliance views**: May require documented temporal coherence
  guarantees.

---

## 2. What Can Be Guaranteed Without Changes

Even without any of the approaches below, pg_trickle already provides:

1. **Per-ST DVS**: B2 is a correct and consistent snapshot of B1; C2 is a
   correct and consistent snapshot of C1. Neither is corrupted.
2. **Topological ordering**: when D refreshes in a given tick, both B2 and
   C2 have already been refreshed in that tick. D never joins a version of
   B2 that is *newer* than what the tick computed.
3. **Single-tick refresh skew bounded by tick duration**: The gap between
   B2's snapshot of B1 and C2's snapshot of C1 is at most one tick's
   duration apart in wall-clock time.

What cannot be guaranteed without additional mechanism: that B2 and C2 were
computed from the *same* consistent snapshot of the database at the same
logical instant.

---

## 3. Proposed Approaches

### 3.1 Approach A — Shared REPEATABLE READ Snapshot

**Core idea:** Run B2, C2, and D's refreshes inside a single `REPEATABLE
READ` transaction. PostgreSQL's snapshot isolation guarantees that all reads
within a `REPEATABLE READ` transaction see the database as it was at the
moment the transaction started — regardless of concurrent writes that commit
during the transaction.

#### 3.1.1 How It Works

```
Scheduler tick:
  BEGIN ISOLATION LEVEL REPEATABLE READ;
    refresh B2  →  reads B1 at snapshot T
    refresh C2  →  reads C1 at snapshot T  (same T, guaranteed)
    refresh D   →  joins B2@T and C2@T
  COMMIT;
```

Both B2 and C2 see B1 and C1 as of the exact same transaction snapshot T.
D then joins two views that are temporally coherent with each other.

#### 3.1.2 Relationship to Option 1 (Atomic Groups)

This is a **natural extension** of the epoch-based atomic group model.
Option 1 uses a `SAVEPOINT` inside an outer transaction to achieve
atomicity. Approach A replaces (or wraps) that with `REPEATABLE READ`
isolation, which adds snapshot coherence on top of atomicity.

The `ConsistencyGroup` struct from Option 1 gains an additional field:

```rust
pub struct ConsistencyGroup {
    pub members: Vec<NodeId>,
    pub convergence_points: Vec<NodeId>,
    pub epoch: u64,
    /// Whether this group requires a shared REPEATABLE READ snapshot.
    pub isolation_level: IsolationLevel,
}

pub enum IsolationLevel {
    /// Default PostgreSQL READ COMMITTED. Used for single-source diamonds.
    ReadCommitted,
    /// REPEATABLE READ. Used when members span multiple independent sources.
    RepeatableRead,
}
```

#### 3.1.3 Trigger Conditions

The `REPEATABLE READ` flag is set on a group when:

- The group was **user-declared** (see Approach B), or
- *(future auto-detection)* the group spans members with no common
  transitive source (i.e., independent base table lineages converge).

Auto-detection of cross-source groups is strictly optional — the user
declaration (Approach B) is sufficient for correctness and simpler to
implement.

#### 3.1.4 Costs and Considerations

- **Lock duration:** A `REPEATABLE READ` transaction holds a snapshot slot
  for its entire duration. For fast refreshes (milliseconds) this is
  negligible. For slow refreshes (seconds), it may delay autovacuum's
  ability to reclaim dead tuples.
- **Write conflicts:** `REPEATABLE READ` does not block concurrent writes to
  B1 or C1. The snapshot is read-only from the perspective of B1/C1; writes
  by other transactions are invisible to this tick but will be picked up on
  the next tick.
- **Serialization failures:** `REPEATABLE READ` can in theory cause
  serialization errors if the refresh queries write back to tables that are
  also being concurrently modified in a way that creates a read-write
  conflict. In practice, stream table refreshes write to storage tables
  (e.g. `order_totals_by_region`) that are not concurrently written by user
  transactions, so this risk is very low.
- **Backward compatibility:** Groups with `IsolationLevel::ReadCommitted`
  behave identically to the current scheduler. Only explicitly opted-in
  groups use `RepeatableRead`.

#### 3.1.5 Pros & Cons

| Pros | Cons |
|------|------|
| Strongest possible guarantee — exact same DB snapshot for all members | Longer snapshot held — affects autovacuum horizon |
| No schema changes to source tables | Cannot span multiple PostgreSQL backends (single-backend only, which pg_trickle already is) |
| Extends naturally from Option 1's group model | Adds an isolation level knob that users must understand |
| Composable with SAVEPOINT rollback semantics | Refresh of unrelated STs in the same tick may be delayed |

---

### 3.2 Approach B — User-Declared Co-Refresh Groups

**Core idea:** Because cross-source temporal coherence is a **semantic**
property (not structurally detectable from the DAG), allow users to
explicitly declare that a set of stream tables must be refreshed together
with a shared snapshot.

#### 3.2.1 SQL Interface

```sql
-- Declare a co-refresh group
SELECT pgtrickle.create_refresh_group(
    'b2_c2_group',
    members    => ARRAY['public.B2', 'public.C2'],
    isolation  => 'repeatable_read'   -- or 'read_committed'
);

-- Inspect groups
SELECT * FROM pgtrickle.refresh_groups();

-- Remove a group
SELECT pgtrickle.drop_refresh_group('b2_c2_group');
```

The scheduler discovers declared groups during DAG rebuild and treats them
identically to auto-detected consistency groups, applying the declared
isolation level.

#### 3.2.2 Catalog

A new catalog table:

```sql
CREATE TABLE pgtrickle.pgt_refresh_groups (
    group_id    SERIAL PRIMARY KEY,
    group_name  TEXT NOT NULL UNIQUE,
    member_oids OID[] NOT NULL,
    isolation   TEXT NOT NULL DEFAULT 'read_committed'
                CHECK (isolation IN ('read_committed', 'repeatable_read')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

#### 3.2.3 Validation at Group Creation

When `create_refresh_group()` is called:

1. Verify all named STs exist in `pgt_stream_tables`.
2. Verify no member appears in more than one declared group (or raise an
   informational notice if groups should be merged).
3. Verify the declared members can be ordered topologically (no cycles).
4. Store the group; signal `DAG_REBUILD_SIGNAL` so the scheduler picks it
   up on the next tick.

#### 3.2.4 Pros & Cons

| Pros | Cons |
|------|------|
| User controls exactly which STs need coherence — no false positives | Requires user knowledge of which joins are temporally sensitive |
| Works for any DAG topology — not limited to detectable diamonds | Catalog change required |
| Clear, auditable intent | User must remember to update groups when adding new STs |
| Composable with Approach A (group declares isolation level) | No automatic enforcement — easy to forget |

---

### 3.3 Approach C — Global LSN Watermark per Tick

**Core idea:** At the start of each scheduler tick, record
`pg_current_wal_lsn()` as the **tick watermark** $W$. All refresh queries
in that tick only consume WAL changes up to $W$. No refresh in this tick
will ever see a commit with LSN > $W$.

#### 3.3.1 How It Works

```rust
// At scheduler tick start:
let tick_watermark = Spi::get_one::<String>(
    "SELECT pg_current_wal_lsn()::text"
)?.ok_or(PgTrickleError::SpiError)?;

// Pass watermark to each refresh:
// The refresh query adds WHERE lsn <= $1 to its WAL/CDC consumption.
execute_refresh_with_watermark(&st, &tick_watermark)?;
```

Each ST's change buffer consumption respects `tick_watermark`: only changes
with LSN ≤ W are applied. Changes that arrived after W will be applied in
the *next* tick.

#### 3.3.2 What This Guarantees

All STs refreshed in the same tick have consumed changes up to the same WAL
position W. This means:

- B2 has applied all B1 changes with LSN ≤ W.
- C2 has applied all C1 changes with LSN ≤ W.
- Neither has applied any change with LSN > W.

This does **not** guarantee that B2 and C2 reflect B1 and C1 at the same
*instant* — WAL LSNs for B1 and C1 are independent sequences — but it
guarantees that **no change after the tick started is visible to either**,
bounding the staleness gap to zero for intra-tick commits.

#### 3.3.3 Relationship to Approach A

Approach C is weaker than Approach A:

- Approach A: B2 and C2 see the exact same snapshot (same `xmin` horizon,
  same in-flight transaction visibility).
- Approach C: B2 and C2 have both consumed only changes ≤ W, but their
  `READ COMMITTED` reads may have seen different in-flight transactions
  during their individual refresh executions.

In practice, for tables with low concurrent write rates, the difference is
negligible. For high-throughput tables, Approach A is strictly stronger.

#### 3.3.4 Implementation Note

The existing `Frontier` struct per ST already tracks per-source LSN. The
change needed is to cap the LSN consumed each tick to `min(current_lsn,
tick_watermark)` rather than advancing to `current_lsn`. This is a small
change to `refresh.rs` / `wal_decoder.rs` with no schema impact.

#### 3.3.5 Pros & Cons

| Pros | Cons |
|------|------|
| No schema changes, no user action required — applies globally | Weaker than Approach A (READ COMMITTED, not snapshot isolation) |
| Low implementation risk — small change to LSN consumption cap | Does not prevent in-flight transaction visibility skew |
| Improves cross-source consistency for all STs automatically | Every tick slightly delays refresh to the watermark point |
| Can be implemented independently of and before Approaches A & B | Does not help for sources with very high write rates (many LSN positions per tick) |

---

### 3.4 Approach D — Snapshot Export (Rejected)

**Core idea:** Use PostgreSQL's `pg_export_snapshot()` /
`SET TRANSACTION SNAPSHOT` to share an exact snapshot across multiple
transactions or backends.

**Why rejected for pg_trickle:**

pg_trickle's background worker refreshes all STs within a single backend
(single SPI context). Snapshot export is designed for **cross-connection**
sharing — its primary use case is parallel `pg_dump` across multiple
backends all reading from the same snapshot.

Within a single SPI context, `REPEATABLE READ` (Approach A) already
provides identical semantics at zero additional complexity. Snapshot export
would add protocol overhead and error-handling complexity for no gain.

**Verdict:** Not implemented. Approach A is strictly superior in the
single-backend context.

---

## 4. Comparison

| | **A — Shared REPEATABLE READ** | **B — User-Declared Groups** | **C — LSN Watermark** | **D — Snapshot Export** |
|---|---|---|---|---|
| **Auto-detects cross-source groups?** | No (extends detected groups) | No (user declares) | Yes (applies universally) | N/A |
| **Snapshot coherence guarantee** | Exact — same `xmin` horizon | Exact — same `xmin` horizon | Bounded — same LSN ceiling, not same snapshot | Exact (but same as A) |
| **Schema changes** | No | Yes (`pgt_refresh_groups` table) | No | No |
| **User action required** | No (once groups are formed) | Yes | No | N/A |
| **Implementation complexity** | Low | Medium | Low | High (rejected) |
| **Performance impact** | Holds snapshot slot longer | Same as A | Negligible | N/A |
| **Backward compatible** | Yes (opt-in per group) | Yes (new feature) | Yes (global, always on) | N/A |
| **Feasibility** | High | High | High | Low |

---

## 5. Recommendation

### Phased adoption

**Phase 1 — Approach C (LSN Watermark):** Implement immediately as a
always-on, zero-configuration improvement. It costs nothing in terms of user
education and tightens cross-source staleness for all STs in every tick.
This is the right default.

**Phase 2 — Approach B (User-Declared Groups) + Approach A (REPEATABLE
READ isolation):** Implement together. Approach B provides the user-facing
API; Approach A provides the execution mechanism when
`isolation = 'repeatable_read'` is declared. Users with temporally sensitive
cross-source joins opt in explicitly.

Approach D is not implemented.

### Configuration

```sql
-- Declare a co-refresh group with snapshot isolation
SELECT pgtrickle.create_refresh_group(
    'positions_prices_group',
    members   => ARRAY['public.position_snapshots', 'public.price_snapshots'],
    isolation => 'repeatable_read'
);
```

No GUC is required for Phase 1 (LSN watermark is always on). For Phase 2,
the per-group `isolation` parameter is the sole control surface.

---

## 6. Implementation Plan

### Phase 1: LSN Watermark (Approach C)

**Files:** `scheduler.rs`, `refresh.rs`, `wal_decoder.rs`

1. At the start of `run_scheduler_tick()`, query
   `pg_current_wal_lsn()::text` and store it as `tick_watermark: String`.
2. Thread `tick_watermark` through to `execute_refresh()` (or equivalent)
   as a parameter.
3. In the WAL/CDC change buffer consumption, cap the LSN consumed to
   `min(available_lsn, tick_watermark)`.
4. Changes with LSN > `tick_watermark` remain in the buffer and are
   consumed on the next tick.
5. Log `tick_watermark` in `pgt_refresh_history` for observability.

**Tests:**
- `test_watermark_caps_lsn_consumption` (unit): mock change buffer with
  LSNs above and below watermark; confirm only sub-watermark changes are
  applied.
- `test_watermark_cross_source_bounded` (E2E): concurrent writes to B1 and
  C1 after tick start are not visible to B2/C2 until the next tick.

---

### Phase 2: User-Declared Groups + REPEATABLE READ (Approaches B + A)

**Files:** `api.rs`, `catalog.rs`, `dag.rs`, `scheduler.rs`

#### Step 2.1 — Catalog

1. Add `pgtrickle.pgt_refresh_groups` table (see §3.2.2).
2. Add `create_refresh_group()`, `drop_refresh_group()`,
   `refresh_groups()` SQL functions in `api.rs`.
3. `create_refresh_group()` validates members exist, checks for membership
   conflicts, topologically validates, then inserts and signals
   `DAG_REBUILD_SIGNAL`.

#### Step 2.2 — DAG integration

1. In `StDag::rebuild()`, load declared groups from `pgt_refresh_groups`
   and merge them with auto-detected consistency groups (from the diamond
   plan). A declared group takes precedence over auto-detection for its
   members' isolation level.
2. Set `isolation_level: IsolationLevel::RepeatableRead` on the merged
   group when the declared group specifies `isolation = 'repeatable_read'`.

#### Step 2.3 — Scheduler execution

1. Extend the group execution loop (from Option 1, Step 5 of
   [PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md)):

   ```rust
   match group.isolation_level {
       IsolationLevel::ReadCommitted => {
           // Existing SAVEPOINT-based atomic group execution.
       }
       IsolationLevel::RepeatableRead => {
           // Run all members inside a REPEATABLE READ transaction.
           // On any failure, rollback the entire transaction.
           Spi::run("BEGIN ISOLATION LEVEL REPEATABLE READ")?;
           let mut ok = true;
           for member in &group.members {
               if let Err(e) = execute_single_refresh(member, &mut ctx) {
                   log!("repeatable_read group rollback: {:?}", e);
                   ok = false;
                   break;
               }
           }
           if ok {
               Spi::run("COMMIT")?;
               group.advance_epoch();
           } else {
               Spi::run("ROLLBACK")?;
               record_group_skip(&group, &mut ctx);
           }
       }
   }
   ```

#### Step 2.4 — Tests

| Test | Type | What it proves |
|---|---|---|
| `test_refresh_group_create_drop` | Integration | Round-trip catalog CRUD |
| `test_refresh_group_member_conflict` | Integration | Duplicate member raises error |
| `test_repeatable_read_snapshot_coherence` | E2E | B2 and C2 both see B1/C1 at same snapshot T; concurrent write to C1 after tick start not visible |
| `test_repeatable_read_rollback_on_fail` | E2E | C2 failure rolls back B2; both retry next tick |
| `test_declared_group_overrides_autodetect` | E2E | Declared group with `repeatable_read` takes precedence over diamond auto-detection's `read_committed` |

---

## 7. Relationship to Diamond Consistency Plan

| Plan | Problem | Root cause | Solution mechanism |
|---|---|---|---|
| [Diamond Consistency](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md) | D sees B@v2 and C@v1 of the *same* source A | Sequential refreshes of B and C within the same tick | SAVEPOINT atomic group: B, C, D all-or-nothing |
| This plan | D sees B2 reflecting B1@T₁ and C2 reflecting C1@T₂ where T₁ ≠ T₂ | B1 and C1 are independent — no shared ancestor to detect | Shared REPEATABLE READ snapshot (user-declared) or LSN watermark |

The two plans are **complementary and additive**. A single scheduler group
can be both atomically consistent (Option 1 SAVEPOINT) *and* snapshot
coherent (`REPEATABLE READ` isolation), when a user-declared co-refresh
group is formed over members that also happen to share a common ancestor.

---

## 8. Open Questions

1. **Interaction with immediate IVM mode:** Approach C (LSN watermark) is
   a deferred-mode concept. In immediate IVM (transactional mode), changes
   propagate within a single user transaction, so the source snapshot is
   already shared by definition. No changes needed for that mode.

2. **Group membership validation:** If a user adds a new ST that logically
   belongs to a declared group but forgets to add it to the group, D can
   still see incoherent results. Should pg_trickle warn when a new ST is
   created that joins members of an existing declared group?
   **Proposed answer:** Yes — emit a `WARNING` suggesting the user review
   their declared co-refresh groups.

3. **Long-running refreshes under REPEATABLE READ:** If a member of a
   `repeatable_read` group has a very slow refresh query, the snapshot slot
   is held for the entire duration, potentially blocking autovacuum. Should
   there be a configurable timeout after which the group falls back to
   `read_committed`?
   **Proposed answer:** Add a `snapshot_timeout_ms` parameter to
   `create_refresh_group()`, defaulting to `0` (no timeout). If the timeout
   is hit, log a warning and fall back to `read_committed` for that tick.

4. **Can Approach C (LSN watermark) be disabled?** Some deployments might
   prefer STs to always advance to the latest available LSN regardless of
   when other STs in the tick started. **Proposed answer:** Add a
   `pg_trickle.tick_watermark_enabled` GUC defaulting to `on`.
