[← Back to Blog Index](README.md)

# IVM Without Primary Keys

## How content hashing lets pg_trickle track changes in keyless tables

---

You have a table with no primary key. Maybe it's a log table, an external feed you don't control, or a legacy table that predates your team. You want to create a stream table over it. Can you?

Yes. pg_trickle doesn't require primary keys on source tables. It uses **content-based hashing** to generate a synthetic row identity — `__pgt_row_id` — that serves the same purpose for change tracking.

This post explains how it works, what the edge cases are, and when you should add a primary key anyway.

---

## Why Row Identity Matters

Incremental view maintenance needs to answer a question for every source row: "Is this row new, changed, or deleted?"

With a primary key, the answer is straightforward:
- If the PK exists in the change buffer but not in the previous result → INSERT.
- If the PK exists in both but values differ → UPDATE.
- If the PK was in the previous result but not in the current data → DELETE.

Without a primary key, there's no stable identifier to track across refresh cycles. Two rows with identical values are indistinguishable. A row that appears twice might be a duplicate or a re-insert.

---

## The Content-Hash Approach

When a source table has no primary key, pg_trickle generates a row identity by hashing the row's content:

```
__pgt_row_id = xxHash64(col1 || col2 || col3 || ...)
```

The hash is computed over all columns in the row (or all columns referenced by the stream table's defining query, when column-level change tracking is active). pg_trickle uses xxHash64 — a fast, non-cryptographic hash function — because the goal is deduplication, not security.

This hash is stored in the change buffer alongside the row data:

```sql
-- What a change buffer row looks like internally
SELECT __pgt_row_id, __pgt_op, old_col1, old_col2, new_col1, new_col2
FROM pgtrickle_changes.changes_12345;
```

```
   __pgt_row_id    | __pgt_op | old_col1 | old_col2 | new_col1 | new_col2
-------------------+----------+----------+----------+----------+----------
 a1b2c3d4e5f67890  | I        | NULL     | NULL     | 42       | 'hello'
 f0e1d2c3b4a59678  | D        | 17       | 'world'  | NULL     | NULL
```

---

## How It Handles Duplicates

Content hashing means that two rows with identical values produce the same `__pgt_row_id`. This is by design — from the perspective of the stream table query, two identical rows are interchangeable.

But it creates a counting problem. If a table has three identical rows and one is deleted, the result should still include two copies. pg_trickle tracks this with **multiplicity counting** in the Z-set:

- Insert identical row → weight +1
- Delete identical row → weight -1
- Net weight = number of copies remaining

The hash identifies the *value*, and the weight tracks the *count*. This is the same multiset arithmetic described in the [Z-set post](z-set-data-structure.md), applied at the change-tracking level.

---

## When Primary Keys Exist

When a source table *does* have a primary key, pg_trickle uses it directly:

```
__pgt_row_id = xxHash64(primary_key_columns)
```

The hash is over the PK columns only, not the entire row. This is faster (fewer bytes to hash) and more stable (the PK doesn't change on UPDATE, so the row identity is preserved even when non-key columns change).

For composite primary keys:

```
__pgt_row_id = xxHash64(pk_col1 || pk_col2 || pk_col3)
```

The practical difference: with a PK, an UPDATE is tracked as a single row with old and new values. Without a PK, an UPDATE looks like a DELETE of the old row (full content hash) plus an INSERT of the new row (different full content hash). Both are correct; the PK version is slightly more efficient because it avoids recomputing the hash.

---

## The Hash Collision Question

xxHash64 produces a 64-bit hash. With $2^{64}$ possible values, the probability of a collision is:

$$P(\text{collision}) \approx \frac{n^2}{2^{65}}$$

For 1 billion rows ($n = 10^9$):

$$P \approx \frac{10^{18}}{3.7 \times 10^{19}} \approx 2.7\%$$

That's not zero. For tables with billions of rows, hash collisions are a real possibility.

What happens if two different rows hash to the same `__pgt_row_id`?

pg_trickle treats them as the same row. This can cause:
- A DELETE of one row being interpreted as a DELETE of the other.
- Change buffer deduplication merging two distinct changes.

In practice, this manifests as a minor count discrepancy in the stream table. pg_trickle's periodic FULL refresh (triggered by AUTO mode or manual intervention) corrects any accumulated drift.

**If collision risk is unacceptable:** Add a primary key. Even a synthetic `BIGSERIAL` column eliminates the problem entirely.

---

## Foreign Tables and Keyless Sources

Foreign tables — `postgres_fdw`, `file_fdw`, `parquet_fdw` — often lack primary keys. pg_trickle handles them with content hashing, but with a caveat:

Foreign tables can't have triggers (in most FDW implementations). pg_trickle falls back to **polling-based change detection**: it periodically compares the current foreign table contents with the last known state, using the content hash to identify what changed.

This is more expensive than trigger-based CDC (it requires a full scan of the foreign table), but it works. For small-to-medium foreign tables (under 1M rows), the scan is fast enough. For larger tables, consider materializing the foreign data into a local table first.

---

## Log Tables and Append-Only Sources

Keyless tables are especially common for log-style data: event tables, audit logs, sensor readings. These are naturally append-only — rows are inserted and never updated or deleted.

For these, pg_trickle's `append_only` flag is the right combination with content hashing:

```sql
SELECT pgtrickle.create_stream_table(
  name        => 'hourly_event_counts',
  query       => $$
    SELECT
      event_type,
      date_trunc('hour', created_at) AS hour,
      COUNT(*) AS count
    FROM events
    GROUP BY event_type, date_trunc('hour', created_at)
  $$,
  schedule    => '5s',
  append_only => true
);
```

With `append_only => true`, pg_trickle skips DELETE/UPDATE tracking entirely. The content hash is still generated for deduplication, but the absence of deletes means the multiplicity counting is simplified — every hash entry is weight +1.

---

## Checking Row Identity Mode

You can see how pg_trickle is tracking row identity for each source:

```sql
SELECT
  source_table,
  row_id_mode,
  row_id_columns
FROM pgtrickle.pgt_dependencies
WHERE pgt_name = 'hourly_event_counts';
```

```
 source_table | row_id_mode    | row_id_columns
--------------+----------------+-----------------
 events       | content_hash   | {event_type,created_at,user_id,payload}
 customers    | primary_key    | {id}
```

`row_id_mode` is either `primary_key` (using the PK) or `content_hash` (hashing all referenced columns).

---

## Performance Impact

Content hashing is fast — xxHash64 processes data at ~10 GB/s on modern hardware. For a row with 500 bytes of data, the hash takes ~50 nanoseconds. Even at 100,000 rows/second, hashing adds <5ms of overhead per second.

The real cost isn't the hash computation. It's the wider change buffer rows. With a primary key, the change buffer stores only the PK columns plus the changed columns. With content hashing, it stores all referenced columns (to reconstruct the hash for deduplication).

For narrow tables (5–10 columns), the difference is negligible. For wide tables (50+ columns), the change buffer can be 2–5× larger without a PK. If storage or I/O is a concern, add a primary key.

---

## Summary

pg_trickle doesn't require primary keys. It uses xxHash64 content hashing to generate synthetic row identities, with multiplicity counting to handle duplicates correctly.

The trade-offs:
- **No PK:** Works everywhere. Wider change buffers. Theoretical collision risk at billion-row scale.
- **With PK:** More efficient. Stable identity across UPDATEs. Zero collision risk.

For log tables, foreign tables, and legacy tables without keys, content hashing just works. For everything else, a primary key is still the better choice — not because pg_trickle requires it, but because your database does.
