# Plan: Circuit Breaker — Anomalous Change Volume Protection

Date: 2026-03-02
Status: EXPLORATION
Last Updated: 2026-03-02

---

## 1. Problem Statement

### Catastrophic Data Changes from External Failures

An external ETL process fails and blanks out a source table — issuing a
`DELETE FROM` or `TRUNCATE` followed by an incomplete reload. The change
buffer records hundreds of thousands of deletions. On the next scheduler
tick, pg_trickle faithfully propagates these deletions through the DAG,
wiping out all downstream stream tables.

By the time the ETL team notices the bug, every dependent view is empty or
severely degraded. Restoring the correct state requires a full re-run of
the ETL pipeline plus manual refreshes of every affected stream table.

### The Problem is Not Limited to ETL

Similar patterns occur with:

- **Schema migrations gone wrong** — a poorly written migration deletes/
  updates a large fraction of rows.
- **Runaway application bugs** — a micro-service enters a loop that
  mass-deletes records.
- **Accidental bulk operations** — `UPDATE orders SET status = 'cancelled'`
  without a `WHERE` clause.
- **Faulty CDC replay** — a logical replication slot replays old events,
  causing a spike of phantom changes.

In all cases, the change volume is grossly abnormal compared to the table's
historical pattern. A human looking at the change buffer would say "this
doesn't look right" — but the current system blindly applies the delta.

### What Exists Today

pg_trickle already has a **change ratio check** that triggers an adaptive
fallback from DIFFERENTIAL to FULL refresh when the delta exceeds a
configurable fraction of the table size (default 15%). This is a performance
optimization — not a safety mechanism. The FULL refresh still applies the
changes; it just does so via TRUNCATE + INSERT rather than MERGE.

The system also has **auto-suspension** after consecutive errors (default 3),
but this only fires on refresh *failures* — not on successful refreshes that
happen to apply anomalous deltas.

**Neither mechanism prevents the propagation of anomalous changes.**

### Relationship to Watermark Gating

[PLAN_WATERMARK_GATING.md](PLAN_WATERMARK_GATING.md) addresses the case
where external data is *incomplete* — the circuit breaker addresses the case
where external data is *wrong*. The two features are complementary:

- **Watermark gating** answers: "Has enough data arrived?"
- **Circuit breaker** answers: "Does this data look right?"

A stream table can have both. Watermark alignment may pass (both sources
have advanced their watermarks) but the circuit breaker may trip because one
source had an anomalous volume of deletions. Conversely, the circuit breaker
may be happy but watermark gating may block because one source hasn't
reported completeness yet.

---

## 2. Proposed Mechanism: Circuit Breaker

### 2.1 Core Concept

A **circuit breaker** is a per-ST protective mechanism that monitors change
volume and halts refresh when it detects an anomalous spike. The circuit
breaker has three states:

```
                  ┌──────────┐
                  │  CLOSED  │  ← normal operation
                  └────┬─────┘
                       │ anomalous change detected
                       ▼
                  ┌──────────┐
                  │   OPEN   │  ← refresh halted
                  └────┬─────┘
                       │ user resets via SQL function
                       ▼
                  ┌──────────┐
                  │  CLOSED  │  ← normal operation resumes
                  └──────────┘
```

When OPEN, the stream table's refresh is **skipped** on every scheduler tick.
The change buffer continues to accumulate (no data is lost), but no delta is
applied to the stream table's storage. The ST retains its last known-good
state.

The user must explicitly reset the circuit breaker after investigating and
resolving the root cause:

```sql
SELECT pgtrickle.reset_circuit_breaker('order_summary');
```

### 2.2 Why Manual Reset?

Unlike a traditional circuit breaker (which auto-resets after a cooldown),
a data circuit breaker requires human judgment:

- The anomalous changes may still be in the change buffer — auto-resetting
  would just re-trip the breaker.
- The user needs to decide: should the system apply the buffered changes
  (they were legitimate), discard them (they were erroneous), or
  reinitialize the ST from scratch?
- Automatic recovery could mask the original problem, leading to silent
  data quality issues.

---

## 3. Trip Conditions

The key design question: **how does the circuit breaker decide that change
volume is "anomalous"?**

### 3.1 Option A — Fixed Threshold (Absolute)

The user configures a hard limit on the number or fraction of changes per
refresh cycle.

```sql
SELECT pgtrickle.alter_stream_table('order_summary',
    circuit_breaker_max_changes => 10000    -- absolute row count
);
-- or
SELECT pgtrickle.alter_stream_table('order_summary',
    circuit_breaker_max_ratio => 0.50       -- 50% of table size
);
```

| Pros | Cons |
|------|------|
| Simple to understand and configure | Requires manual tuning per ST |
| Predictable — no surprises | Doesn't adapt to seasonal patterns or organic growth |
| Easy to implement | Too-tight thresholds cause false trips; too-loose miss real anomalies |
| | User must know what "normal" looks like before setting the threshold |

### 3.2 Option B — Adaptive (Statistical)

The system maintains a rolling baseline of historical change volumes and
trips when the current delta deviates significantly from the baseline.

The circuit breaker keeps a short history of recent refresh delta sizes
and computes:

$$\text{trip if } \Delta_{\text{current}} > \mu + k \cdot \sigma$$

Where:
- $\mu$ = mean delta size over the last $N$ refreshes
- $\sigma$ = standard deviation
- $k$ = sensitivity multiplier (configurable, default e.g. 3.0)
- $N$ = window size (configurable, default e.g. 20 refreshes)

**Cold-start behavior:** Until $N$ refreshes have been recorded, the circuit
breaker is either disabled or uses a conservative fixed threshold as
a backstop.

| Pros | Cons |
|------|------|
| Self-tuning — adapts to organic growth and seasonal patterns | More complex to implement and explain |
| No per-ST manual tuning needed | Can be fooled by gradual drift (boiling frog) |
| Statistical foundation — well-understood anomaly detection | Cold-start period where the breaker is blind |
| Catches anomalies relative to *this* ST's normal behavior | The distribution of delta sizes may not be Gaussian — outliers may need different models |
| | Window size is a hidden tuning parameter |

### 3.3 Option C — Combined (Adaptive with Hard Ceiling)

Use the adaptive model (Option B) for normal operation, but add a hard
ceiling that always trips regardless of the statistical baseline. This
handles the "gradual drift followed by catastrophe" case.

```sql
SELECT pgtrickle.alter_stream_table('order_summary',
    circuit_breaker          => 'adaptive',
    circuit_breaker_ceiling  => 100000,    -- absolute hard limit
    circuit_breaker_sensitivity => 3.0     -- std dev multiplier
);
```

| Pros | Cons |
|------|------|
| Best of both worlds — adaptive for subtle anomalies, ceiling for catastrophes | Most complex configuration surface |
| The ceiling acts as a safety net against gradual baseline drift | Three parameters to understand |
| Adaptive handles normal variation without false positives | |

### 3.4 Option D — Per-Source Monitoring

Instead of (or in addition to) monitoring the delta *applied* to the ST,
monitor the change buffer growth per source table. Trip the breaker on
any ST that depends on a source with anomalous change buffer activity.

This catches the problem earlier — before the delta query is even planned.

| Pros | Cons |
|------|------|
| Catches problems at the source, protecting all downstream STs | A source anomaly affects all STs, even those that might handle it fine |
| No per-ST configuration needed for the monitoring itself | Harder to tie to specific STs — requires DAG traversal |
| Reduces wasted work (no planning/executing a delta that will be rejected) | The change buffer count isn't the same as the delta size (CDC may compress changes) |

### 3.5 Discussion

Options B and C (adaptive, optionally with a ceiling) are the most appealing
for a "set it and forget it" user experience. Option A is the simplest MVP.

Key open questions:

1. **Which metric to monitor?** Candidates:
   - `delta_row_count` (from change buffer — number of raw CDC events)
   - `rows_inserted + rows_deleted` (from MERGE — actual applied changes)
   - Change buffer row count per source (pre-delta)
   - Each tells a slightly different story. Which is the right signal?

2. **Should the check happen before or after the delta query runs?**
   - **Before (pre-check):** Count change buffer rows in the LSN range.
     Cheaper — avoids running an expensive delta query only to discard it.
     But the change buffer count may not correspond 1:1 to the final delta
     (due to offsetting inserts/deletes, PK-level compression, etc.).
   - **After (post-check):** Run the delta, examine the result size before
     applying it. More accurate but wastes compute on the delta query.
   - A hybrid is possible: pre-check for a cheap early-out, post-check for
     precision.

3. **Interaction with the adaptive DIFFERENTIAL→FULL fallback:** The
   existing adaptive fallback already counts change buffer rows and compares
   to a ratio threshold. The circuit breaker check could share this
   infrastructure — but the semantics differ: the fallback switches to FULL
   (still applies changes), while the circuit breaker *blocks* changes.
   The fallback ratio threshold (default 15%) is lower than a typical
   circuit breaker threshold (e.g. 50% or statistical outlier). These are
   two distinct safety layers operating in sequence:

   ```
   Change buffer rows counted
          │
          ▼
   ┌─────────────────────┐     trip    ┌────────────────────────┐
   │  Circuit breaker     │────────────▶│  OPEN — refresh halted │
   │  (e.g. μ + 3σ)      │             └────────────────────────┘
   └──────────┬──────────┘
              │ pass
              ▼
   ┌─────────────────────┐   exceeds   ┌────────────────────────┐
   │  Adaptive fallback   │────────────▶│  Switch DIFF → FULL    │
   │  (e.g. 15% of rows)  │            └────────────────────────┘
   └──────────┬──────────┘
              │ pass
              ▼
       Normal DIFFERENTIAL refresh
   ```

---

## 4. Circuit Breaker State

### 4.1 Catalog Storage

A circuit breaker is per-ST. The state needs to survive server restarts.

**Option: Columns on `pgt_stream_tables`**

```sql
ALTER TABLE pgtrickle.pgt_stream_tables ADD COLUMN
    cb_mode         TEXT NOT NULL DEFAULT 'none'
                    CHECK (cb_mode IN ('none', 'fixed', 'adaptive')),
    cb_state        TEXT NOT NULL DEFAULT 'closed'
                    CHECK (cb_state IN ('closed', 'open')),
    cb_tripped_at   TIMESTAMPTZ,
    cb_trip_reason  TEXT,
    cb_ceiling      BIGINT,          -- hard limit (rows)
    cb_sensitivity  FLOAT8,          -- std dev multiplier (adaptive mode)
    cb_window_size  INT;             -- number of refreshes for rolling baseline
```

**Option: Separate table**

```sql
CREATE TABLE pgtrickle.pgt_circuit_breakers (
    pgt_id         BIGINT PRIMARY KEY REFERENCES pgt_stream_tables,
    mode           TEXT NOT NULL DEFAULT 'none',
    state          TEXT NOT NULL DEFAULT 'closed',
    tripped_at     TIMESTAMPTZ,
    trip_reason    TEXT,
    ceiling        BIGINT,
    sensitivity    FLOAT8 DEFAULT 3.0,
    window_size    INT DEFAULT 20
);
```

A separate table keeps `pgt_stream_tables` from growing wider with every
new feature. But it adds a JOIN in the scheduler hot path.

**No decision made.** The separate table is cleaner long-term; inline
columns are faster at runtime. This should be decided alongside the broader
catalog width discussion in
[REPORT_DB_SCHEMA_STABILITY.md](REPORT_DB_SCHEMA_STABILITY.md).

### 4.2 Rolling Baseline Storage

The adaptive model needs historical delta sizes. Three candidates:

**A. Derived from `pgt_refresh_history`** — query the last $N$ successful
refreshes and compute $\mu$ and $\sigma$ from `delta_row_count`. No
additional storage needed.

| Pros | Cons |
|------|------|
| Zero new schema — data already exists | Requires a SQL query per ST per tick |
| History is already pruned/retained per existing policy | If refresh history is pruned aggressively, the window shrinks |

**B. Pre-computed summary in catalog** — maintain `cb_mean` and
`cb_stddev` columns, updated incrementally after each successful refresh
using Welford's online algorithm.

| Pros | Cons |
|------|------|
| O(1) lookup in scheduler — no extra query | Loses individual data points — can't recompute on window size change |
| Survives history pruning | Another incremental update in the refresh path |

**C. Ring buffer in JSONB** — store the last $N$ `delta_row_count` values
as a JSONB array on the circuit breaker row.

| Pros | Cons |
|------|------|
| Full data for recomputation | JSONB array manipulation per refresh |
| Configurable window without data loss | Grows with window size (bounded) |

Option A (derive from history) is simplest and sufficient if refresh history
retention is at least as large as the window size. Option B is the most
efficient at runtime.

---

## 5. SQL API

### 5.1 Configuration

```sql
-- Enable adaptive circuit breaker with defaults
SELECT pgtrickle.alter_stream_table('order_summary',
    circuit_breaker => 'adaptive'
);

-- Adaptive with custom sensitivity and hard ceiling
SELECT pgtrickle.alter_stream_table('order_summary',
    circuit_breaker             => 'adaptive',
    circuit_breaker_sensitivity => 4.0,       -- 4σ before tripping
    circuit_breaker_ceiling     => 100000     -- absolute hard limit
);

-- Fixed threshold only
SELECT pgtrickle.alter_stream_table('order_summary',
    circuit_breaker             => 'fixed',
    circuit_breaker_ceiling     => 50000      -- trip at 50k changes
);

-- Disable
SELECT pgtrickle.alter_stream_table('order_summary',
    circuit_breaker => 'none'
);
```

### 5.2 Manual Reset

```sql
-- Reset the circuit breaker and resume normal refreshes.
-- Pending changes in the buffer will be applied on the next tick.
SELECT pgtrickle.reset_circuit_breaker('order_summary');
```

**Options on reset — open question:**

The user may want to control what happens to buffered changes when resetting.

```sql
-- Option A: Apply buffered changes normally (default)
SELECT pgtrickle.reset_circuit_breaker('order_summary');

-- Option B: Discard buffered changes and reinitialize from scratch
SELECT pgtrickle.reset_circuit_breaker('order_summary',
    action => 'reinitialize'
);

-- Option C: Discard changes up to the current LSN without applying them,
-- then resume differential from the new baseline
SELECT pgtrickle.reset_circuit_breaker('order_summary',
    action => 'skip_changes'
);
```

| Action | Behavior |
|--------|----------|
| `'apply'` (default) | Buffered changes are applied on next tick. Use when the changes were legitimate but you wanted to verify first. |
| `'reinitialize'` | ST is truncated and fully repopulated from the defining query. Safest when the source data has been corrected. |
| `'skip_changes'` | Frontier is advanced past the buffered changes without applying them. Use when the anomalous changes have been rolled back at the source and you want to "forget" what happened. |

### 5.3 Introspection

```sql
-- Circuit breaker state for all STs
SELECT * FROM pgtrickle.circuit_breaker_status();
```

Returns:

| Column | Type | Description |
|--------|------|-------------|
| `st_name` | `TEXT` | Stream table name |
| `mode` | `TEXT` | `'none'`, `'fixed'`, `'adaptive'` |
| `state` | `TEXT` | `'closed'`, `'open'` |
| `tripped_at` | `TIMESTAMPTZ` | When the breaker tripped (NULL if closed) |
| `trip_reason` | `TEXT` | Human-readable explanation |
| `baseline_mean` | `FLOAT8` | Rolling mean delta size (adaptive mode) |
| `baseline_stddev` | `FLOAT8` | Rolling std dev (adaptive mode) |
| `last_delta` | `BIGINT` | Delta that tripped the breaker |
| `ceiling` | `BIGINT` | Hard ceiling (if set) |
| `sensitivity` | `FLOAT8` | σ multiplier (adaptive mode) |

### 5.4 Alerts

When the circuit breaker trips, emit a `NOTIFY pg_trickle_alert` event:

```json
{
  "event": "circuit_breaker_tripped",
  "st_name": "order_summary",
  "schema": "public",
  "delta_row_count": 85432,
  "baseline_mean": 1200.5,
  "baseline_stddev": 340.2,
  "ceiling": null,
  "computed_threshold": 2221.1,
  "trip_reason": "delta 85432 exceeds adaptive threshold 2221 (μ=1201, 3.0σ=1020)"
}
```

This integrates with the existing `pg_trickle_alert` NOTIFY channel and
follows the same JSON event format used by `stale_data`, `auto_suspended`,
and other existing alert types.

---

## 6. Scheduler Integration

### 6.1 Where the Check Runs

The circuit breaker check runs in the scheduler's per-ST refresh loop,
**before** the delta query is planned and executed:

```
for each ST in topological_order:
    ... existing checks (status, schedule, advisory lock, upstream changes) ...

    // ── Circuit breaker gate ──────────────────────────────
    if st.cb_state == 'open':
        log!("circuit breaker open for {}", st.name)
        skip this ST
        continue

    if st.cb_mode != 'none':
        change_count = count_change_buffer_rows(st, prev_frontier, new_frontier)
        if should_trip(st, change_count):
            trip_circuit_breaker(st, change_count, reason)
            emit NOTIFY circuit_breaker_tripped
            skip this ST
            continue
    // ──────────────────────────────────────────────────────

    ... adaptive fallback check (existing) ...
    ... proceed with refresh ...
```

### 6.2 Relationship to Existing Adaptive Fallback

Both the circuit breaker and the adaptive DIFFERENTIAL→FULL fallback examine
change buffer row counts, but they serve different purposes and operate at
different thresholds:

| Mechanism | Typical threshold | Effect | Purpose |
|-----------|-------------------|--------|---------|
| **Circuit breaker** | Statistical outlier or hard ceiling | **Block refresh entirely** | **Data safety** |
| Adaptive fallback | ~15% of table (auto-tuned) | Switch DIFF → FULL | Performance optimization |

The circuit breaker runs **first**. If it passes, the adaptive fallback runs
as usual. Both can share the change buffer count query to avoid duplicate
SPI calls.

### 6.3 Interaction with Consistency Groups

If a circuit-breaker-tripped ST is a member of a diamond consistency group
or a watermark-gated group:

- **Diamond atomic group:** The tripped ST cannot refresh, so the entire
  group is skipped. This is consistent with the atomic guarantee —
  all-or-nothing. The group retries next tick (and will skip again until
  the breaker is reset).
- **Watermark group:** The circuit breaker adds an additional gate on top
  of watermark alignment. Even if all watermarks align, a tripped breaker
  on any member blocks that member (and its atomic group, if any).
- **Cascade STs:** Downstream STs that depend on the tripped ST will see
  no upstream changes and naturally produce empty deltas. They may still
  refresh (applying zero changes) or be skipped if the scheduler's
  "upstream changes" pre-check short-circuits first.

---

## 7. Concrete Example

### Scenario: ETL Failure Blanks Out `orders`

```sql
-- Setup
CREATE TABLE orders (id INT PRIMARY KEY, customer TEXT, amount NUMERIC);
CREATE TABLE order_lines (order_id INT, product TEXT, qty INT);

SELECT pgtrickle.create_stream_table('order_summary',
    'SELECT customer, SUM(amount) AS total FROM orders GROUP BY customer');

SELECT pgtrickle.create_stream_table('line_summary',
    'SELECT product, SUM(qty) AS total_qty FROM order_lines GROUP BY product');

SELECT pgtrickle.create_stream_table('order_report',
    'SELECT os.customer, os.total, ls.total_qty
     FROM order_summary os
     JOIN line_summary ls ON os.customer = ls.product');

-- Enable circuit breakers
SELECT pgtrickle.alter_stream_table('order_summary',
    circuit_breaker => 'adaptive');
SELECT pgtrickle.alter_stream_table('line_summary',
    circuit_breaker => 'adaptive',
    circuit_breaker_ceiling => 50000);
```

**Normal operation** over 100 refresh cycles:
- `order_summary` typically sees 50–200 changes per cycle (μ=120, σ=45).
- `line_summary` typically sees 100–400 changes per cycle (μ=230, σ=80).

**ETL failure at T=12:05:**
1. External job runs `DELETE FROM orders` (accidentally).
2. Change buffer records 50,000 deletion events.
3. Next scheduler tick:
   - Circuit breaker on `order_summary` evaluates:
     $50{,}000 > 120 + 3 \times 45 = 255$. **Trip.**
   - `order_summary` is skipped. It retains its last-known-good contents.
   - `order_report` (downstream) sees no upstream changes, skips.
4. Alert fires: `circuit_breaker_tripped` with `delta_row_count=50000`.
5. ETL team investigates, fixes the pipeline, reloads `orders`.
6. Team resets the breaker:
   ```sql
   SELECT pgtrickle.reset_circuit_breaker('order_summary',
       action => 'reinitialize');
   ```
7. `order_summary` does a FULL refresh from the corrected data.
8. Normal operation resumes.

---

## 8. Open Questions

1. **Pre-check vs post-check:** Should the circuit breaker examine the
   change buffer count (pre-check, cheap but approximate) or the actual
   delta result (post-check, accurate but expensive)? Or both? The
   pre-check can share the count query with the adaptive fallback. A
   post-check would require materializing the delta to a temp table,
   checking its size, then either applying or discarding it.

2. **Granularity — per-ST or per-source?** If a source table has an
   anomalous spike, should ALL downstream STs trip, or only those whose
   individual delta is anomalous? Per-source tripping is simpler but may
   over-block. Per-ST is more precise but requires per-ST statistics.

3. **Cascade behavior:** When a circuit breaker trips on an intermediate
   ST, should all downstream STs also be explicitly blocked? They'll see
   no upstream changes (since the tripped ST didn't update), so they'll
   naturally produce empty deltas. But should this be made explicit in
   the UI / monitoring output?

4. **GUC for global default:** Should there be a
   `pg_trickle.circuit_breaker_default` GUC that applies a default mode
   to all STs (e.g. `'adaptive'`)? This avoids configuring each ST
   individually in large deployments. The per-ST setting would override.

5. **Interaction with manual refresh:** If a user calls
   `pgtrickle.refresh_stream_table('order_summary')` while the circuit
   breaker is open, should it be honored or blocked?
   - **Block:** The breaker is a safety mechanism; bypassing it defeats
     the purpose.
   - **Honor:** The user may be testing after a fix; they know what they're
     doing.
   - **Force flag:** `refresh_stream_table('order_summary', force => true)`
     to explicitly bypass.

6. **Cold-start sensitivity:** During the first $N$ refreshes, the adaptive
   model has no baseline. Options:
   - Breaker is inactive during cold-start (simplest, but unprotected).
   - Use a configurable cold-start ceiling (safe, but adds a parameter).
   - Use a high fixed default (e.g. 10,000 changes) until enough history
     accumulates.

7. **Separate deletions from insertions?** A mass DELETE is usually more
   alarming than a mass INSERT (new data rarely blanks out downstream STs).
   Should the circuit breaker weight deletions differently? For example:
   - Trip on deletions at $\mu + 2\sigma$ but on insertions at $\mu + 4\sigma$.
   - Or: track insertion and deletion baselines separately.
   - Or: monitor net change (inserts − deletes) — a large negative net
     change is the strongest signal of data loss.

8. **Recovery workflow UX:** The three-option reset (`'apply'`,
   `'reinitialize'`, `'skip_changes'`) provides flexibility but may
   overwhelm new users. Should the default be:
   - `'apply'` (least surprising — "just let it through"), or
   - `'reinitialize'` (safest — "start fresh"), or
   - Require the user to explicitly choose (no default — forces deliberate
     decision)?

9. **Windowed vs exponential decay:** The rolling baseline could use:
   - A fixed window of the last $N$ refreshes (simple, bounded memory).
   - An exponentially weighted moving average (EWMA), which gives more
     weight to recent refreshes and adapts faster to legitimate baseline
     shifts.
   - EWMA is better for STs with seasonal or trending change patterns.
     Fixed window is simpler to explain and debug.

10. **Half-open / audit state:** Instead of binary open/closed, should there
    be a "half-open" state where the system runs a single refresh into a
    **staging area** (e.g. a temp table or shadow copy) without applying it
    to the main storage? The user can inspect the staged delta before
    committing. This adds significant complexity but provides a safe
    investigation path.

11. **Notification channels:** Beyond `pg_trickle_alert` NOTIFY, should the
    circuit breaker integrate with external alerting? For example, writing
    to a `pg_trickle_events` table that external monitoring (Prometheus,
    PagerDuty) can poll. This may be better handled by a generic alerting
    plan rather than circuit-breaker-specific.

---

## 9. Relationship to Other Plans

| Plan | Relationship |
|------|--------------|
| [PLAN_WATERMARK_GATING.md](PLAN_WATERMARK_GATING.md) | Complementary. Watermark gating prevents refresh when external data is *incomplete*. Circuit breaker prevents refresh when data changes are *anomalous*. Both gate refresh but for different reasons. A ST can have both: watermark alignment must pass **and** circuit breaker must be closed. |
| [PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md) | A tripped circuit breaker on any member of a diamond atomic group blocks the entire group (consistent with all-or-nothing semantics). |
| [PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md](PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md) | Orthogonal. Snapshot consistency concerns *which* data is visible; the circuit breaker concerns *whether* to apply changes at all. |
| Adaptive DIFF→FULL fallback ([refresh.rs](../../src/refresh.rs)) | The circuit breaker is a stricter safety layer above the adaptive fallback. Both examine change volume but at different thresholds with different effects (block vs. switch strategy). Can share the change buffer count query. |
| Auto-suspension ([scheduler.rs](../../src/scheduler.rs)) | Auto-suspension handles repeated *failures* (errors). Circuit breaker handles anomalous *successes* (unexpected volumes). Complementary — different triggers, same protective intent. |
| [PLAN_HYBRID_CDC.md](PLAN_HYBRID_CDC.md) | The circuit breaker works regardless of CDC mode (trigger or WAL). In WAL mode, the "change buffer count" may be derived from decoded WAL records rather than trigger-written rows, but the trip logic is identical. |

---

## 10. Prior Art

1. **Netflix Hystrix / Resilience4j** — Circuit breaker pattern for
   microservice calls. Trip on failure rate, reset after timeout or manual
   intervention. Inspiration for the state machine model, though the
   "failure" in our case is anomalous data volume rather than service errors.

2. **Apache Flink Backpressure** — Flink operators slow down when
   downstream can't keep up. Not a circuit breaker per se, but the same
   principle of protecting the system from overwhelming data rates.

3. **Great Expectations / Soda** — Data quality tools that validate data
   against expectations (row counts, distributions, null rates) before
   pipeline stages proceed. The circuit breaker's adaptive baseline is
   a simplified version of statistical expectation checking, applied
   inline in the refresh pipeline rather than as a separate validation
   step.

4. **dbt source freshness + alerting** — dbt can test whether source data
   is stale, but doesn't prevent model execution based on anomalous volume.
   The circuit breaker goes further by actually blocking propagation.

5. **PostgreSQL `statement_timeout` / `lock_timeout`** — Built-in safety
   mechanisms that abort operations exceeding time bounds. The circuit
   breaker applies a similar philosophy to data volume bounds.

6. **Kafka Consumer Lag Monitoring** — Kafka consumer groups monitor lag
   (difference between latest offset and consumer position). An anomalous
   spike in lag can indicate producer issues. The circuit breaker's
   change buffer monitoring is analogous: a sudden spike in unconsumed
   changes indicates something unusual at the source.

---

## 11. Sketch Implementation Steps

> Preliminary — subject to change based on the open design decisions.

### Step 1 — Trip logic as pure function

```rust
/// Determines whether the circuit breaker should trip.
///
/// Returns `Some(reason)` if the breaker should trip, `None` otherwise.
pub fn should_trip(
    mode: CircuitBreakerMode,
    change_count: i64,
    baseline_mean: Option<f64>,
    baseline_stddev: Option<f64>,
    ceiling: Option<i64>,
    sensitivity: f64,
) -> Option<String> { ... }
```

Testable without a database. Unit tests cover all mode combinations,
cold-start (no baseline), edge cases (zero stddev, etc.).

### Step 2 — Catalog: circuit breaker configuration and state

Add columns or table for mode, state, tripped_at, trip_reason, ceiling,
sensitivity, window_size. Migration SQL.

### Step 3 — `alter_stream_table()` extension

Accept `circuit_breaker`, `circuit_breaker_ceiling`,
`circuit_breaker_sensitivity` parameters.

### Step 4 — Baseline computation helper

Query `pgt_refresh_history` for the last $N$ successful DIFFERENTIAL
refreshes of a given ST, compute mean and stddev of `delta_row_count`.

### Step 5 — Scheduler integration

Pre-check in the per-ST refresh loop. Trip and skip if needed. Share
change buffer count query with the adaptive fallback to avoid duplicate
SPI calls.

### Step 6 — `reset_circuit_breaker()` SQL function

Set state to `'closed'`. Handle the `action` parameter (`'apply'`,
`'reinitialize'`, `'skip_changes'`).

### Step 7 — `circuit_breaker_status()` introspection function

Return current state, baseline, last delta, ceiling, sensitivity for all
STs.

### Step 8 — NOTIFY alert on trip

Emit `circuit_breaker_tripped` event on `pg_trickle_alert`.

### Step 9 — Tests

| Test | Type | Proves |
|------|------|--------|
| `test_cb_trip_fixed_threshold` | Unit | Fixed mode trips at ceiling |
| `test_cb_trip_adaptive` | Unit | Adaptive trips at μ + kσ |
| `test_cb_no_trip_normal_volume` | Unit | Normal delta does not trip |
| `test_cb_cold_start_no_baseline` | Unit | Behavior with < N historical refreshes |
| `test_cb_combined_ceiling_overrides` | Unit | Hard ceiling trips even when adaptive wouldn't |
| `test_cb_blocks_refresh_when_open` | E2E | Tripped ST skips refresh on tick |
| `test_cb_reset_apply` | E2E | Reset with default action resumes refresh |
| `test_cb_reset_reinitialize` | E2E | Reset triggers FULL reinitialize |
| `test_cb_reset_skip_changes` | E2E | Reset advances frontier, skips buffered changes |
| `test_cb_alert_emitted` | E2E | NOTIFY fires with correct JSON on trip |
| `test_cb_diamond_group_blocked` | E2E | Tripped member blocks entire atomic group |
| `test_cb_downstream_sees_no_changes` | E2E | Downstream STs skip when upstream is tripped |

### Step 10 — Documentation

- `docs/SQL_REFERENCE.md`: `circuit_breaker` parameter, `reset_circuit_breaker()`,
  `circuit_breaker_status()`.
- `docs/CONFIGURATION.md`: GUC if added.
- `CHANGELOG.md`.
