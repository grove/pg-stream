[← Back to Blog Index](README.md)

# Soft Deletes and Tombstone Management in Differential IVM

## How `deleted_at` patterns interact with delta propagation, and the right way to model soft deletion for stream tables

---

Soft deletes are everywhere. Instead of `DELETE FROM users WHERE id = 42`, you write `UPDATE users SET deleted_at = now() WHERE id = 42`. The row stays in the table, invisible to the application but available for audit, recovery, and compliance. It's a sensible pattern. It's also one that creates subtle correctness issues when combined with incremental view maintenance.

The problem is that a soft-deleted row is still physically present in the table. If your stream table's query doesn't filter on `deleted_at`, it will include "deleted" rows in its aggregates. If it does filter on `deleted_at`, then soft-deleting a row is semantically an UPDATE to the source table but functionally a DELETE from the stream table's perspective. Getting the delta propagation right for this case requires understanding how pg_trickle processes updates and what happens when a row transitions from "visible" to "invisible" in a filtered query.

---

## The Naive Approach and Its Problems

Consider a stream table that counts active users per plan:

```sql
-- Source table with soft deletes
CREATE TABLE users (
    id         serial PRIMARY KEY,
    plan       text NOT NULL,
    email      text NOT NULL,
    deleted_at timestamptz  -- NULL means active
);

-- Stream table: count users per plan
SELECT pgtrickle.create_stream_table(
    'plan_counts',
    $$
    SELECT plan, COUNT(*) AS user_count
    FROM users
    WHERE deleted_at IS NULL
    GROUP BY plan
    $$
);
```

When you soft-delete a user (`UPDATE users SET deleted_at = now() WHERE id = 42`), the following happens:

1. The CDC trigger fires, recording the change: old row (deleted_at = NULL) → new row (deleted_at = now())
2. pg_trickle processes the delta: the old row satisfied the `WHERE deleted_at IS NULL` filter, but the new row does not
3. From the stream table's perspective, this is a row removal — the user is no longer counted in `plan_counts`
4. The count for that user's plan decreases by 1

This works correctly. pg_trickle's differential engine handles it because it sees the update as a simultaneous removal of the old row (weight -1) and insertion of the new row (weight +1). Since the new row doesn't pass the filter, only the removal propagates. The aggregate decreases.

---

## Where It Gets Tricky: Un-Deleting

The soft delete pattern implies the ability to un-delete: `UPDATE users SET deleted_at = NULL WHERE id = 42`. This reverses the transition — a row that was invisible becomes visible again.

pg_trickle handles this correctly too: the old row (deleted_at = now()) doesn't pass the filter, but the new row (deleted_at = NULL) does. The stream table sees a new row appearing, and the count increases.

The issue isn't correctness — it's performance. Every update to any column in the `users` table triggers the CDC mechanism, which records the before and after image. If you frequently update non-relevant columns (last_login_at, session_count, etc.), the trigger fires for every one of those updates, even though they don't affect the stream table's result.

pg_trickle's differential engine will correctly determine that these updates don't change the stream table output (both old and new rows pass the filter with the same projected values, so the net delta is zero). But the trigger still fires, the change buffer still receives the event, and the refresh still processes it — only to discard it.

---

## Optimizing With Column-Level Filtering

For tables with frequent updates to non-relevant columns, pg_trickle's column-level change detection ensures that only updates to columns referenced in the stream table's query generate meaningful deltas. The `users` table might see thousands of updates per second to `last_login_at`, but if the stream table only references `plan` and `deleted_at`, updates to other columns produce zero-deltas that are discarded early in the refresh pipeline.

The practical advice: when designing tables with soft deletes that back stream tables, keep the `deleted_at` column in the same table as the columns you're aggregating. Don't put it in a separate "metadata" table that requires a join — that would force the stream table to maintain a join, which is more expensive than filtering a column.

---

## Ghost Rows in Aggregates

The most dangerous pattern with soft deletes is forgetting to filter:

```sql
-- WRONG: includes soft-deleted users in the count
SELECT pgtrickle.create_stream_table(
    'plan_counts_buggy',
    'SELECT plan, COUNT(*) AS user_count FROM users GROUP BY plan'
);
```

This stream table counts all users, including soft-deleted ones. It's technically correct (it reflects the table's physical state), but it's almost certainly not what the application intends. The dashboard shows inflated numbers. The billing system charges for inactive users. The capacity planning is wrong.

Worse, because the stream table is incrementally maintained, the bug is invisible during normal operation. New users appear, soft-deleted users stay counted, and the numbers only grow. You won't notice until someone asks why the "active users" metric never decreases despite daily churn.

The fix is simple but must be deliberate:

```sql
-- CORRECT: always filter soft-deleted rows in stream table definitions
SELECT pgtrickle.create_stream_table(
    'plan_counts',
    $$
    SELECT plan, COUNT(*) AS user_count
    FROM users
    WHERE deleted_at IS NULL
    GROUP BY plan
    $$
);
```

Make it a code review rule: every stream table over a table with a `deleted_at` column must have `WHERE deleted_at IS NULL` unless there's an explicit reason to include soft-deleted rows.

---

## Tombstone Accumulation and Performance

Soft deletes create a long-term storage problem. Rows accumulate in the table forever unless you periodically purge them. For the source table, this means bloat and slower index scans. For stream tables, it means the change buffer processes deletions when you eventually hard-delete old tombstones.

When you run a cleanup job:

```sql
-- Monthly tombstone purge: hard-delete rows soft-deleted more than 90 days ago
DELETE FROM users WHERE deleted_at < now() - interval '90 days';
```

This generates DELETE events in the CDC buffer. But because the stream table's query filters `WHERE deleted_at IS NULL`, and these rows already had `deleted_at` set (they were already invisible to the stream table), the deletes produce zero-deltas. The stream table's aggregates don't change.

pg_trickle handles this efficiently: the differential engine evaluates the deleted rows against the stream table's filter, determines they were already excluded, and skips them. The refresh processes the events but produces no output changes.

However, if you have many stream tables that join against the soft-deleted table, each one evaluates the purged rows independently. For a bulk purge of 1 million tombstones with 10 stream tables referencing that table, that's 10 million delta evaluations (each producing zero output). Not catastrophic, but worth scheduling during low-traffic windows.

---

## The Temporal Soft Delete Pattern

A more sophisticated approach uses a validity range instead of a single timestamp:

```sql
CREATE TABLE users (
    id         serial PRIMARY KEY,
    plan       text NOT NULL,
    email      text NOT NULL,
    valid_from timestamptz NOT NULL DEFAULT now(),
    valid_to   timestamptz  -- NULL means currently active
);
```

This supports time-travel queries ("what was the user's plan on March 15th?") and makes the soft-delete semantic explicit. A user is "active" when `valid_to IS NULL` or `valid_to > now()`.

For stream tables, this pattern works identically to `deleted_at IS NULL` filtering:

```sql
SELECT pgtrickle.create_stream_table(
    'active_users_by_plan',
    $$
    SELECT plan, COUNT(*) AS user_count
    FROM users
    WHERE valid_to IS NULL
    GROUP BY plan
    $$
);
```

The advantage is clarity: the semantics are explicit in the schema, and the stream table filter is self-documenting.

---

## Multi-Table Soft Deletes and Cascading

Real applications have related tables that all use soft deletes. An organization is soft-deleted, and all its users should become invisible:

```sql
-- Organizations and users both have soft deletes
CREATE TABLE organizations (
    id serial PRIMARY KEY,
    name text,
    deleted_at timestamptz
);

CREATE TABLE users (
    id serial PRIMARY KEY,
    org_id integer REFERENCES organizations(id),
    email text,
    deleted_at timestamptz
);
```

A stream table that should only show users from active organizations:

```sql
SELECT pgtrickle.create_stream_table(
    'active_user_directory',
    $$
    SELECT u.id, u.email, o.name AS org_name
    FROM users u
    JOIN organizations o ON o.id = u.org_id
    WHERE u.deleted_at IS NULL
      AND o.deleted_at IS NULL
    $$
);
```

When an organization is soft-deleted, all its users disappear from the stream table — even though the users themselves weren't modified. pg_trickle detects the organization's `deleted_at` change, re-evaluates the join condition for all users in that organization, and removes them from the result.

This cascading visibility is handled correctly and incrementally. Only the users belonging to the soft-deleted organization are processed. Users in other organizations are untouched.

---

## Best Practices Summary

1. **Always filter `deleted_at IS NULL`** in stream table definitions over soft-deletable tables. Make this a linting rule.

2. **Keep `deleted_at` in the same table** as the columns your stream tables aggregate. Avoid needing a join just to check deletion status.

3. **Schedule tombstone purges** during low-traffic periods. They generate CDC events that produce zero-deltas but still consume processing resources.

4. **Use column-level change detection** (pg_trickle's default behavior) to avoid processing irrelevant updates to non-referenced columns.

5. **Test the un-delete path** — verify that restoring a soft-deleted row correctly re-includes it in dependent stream tables.

6. **Consider validity ranges** (`valid_from`/`valid_to`) for temporal data that needs time-travel semantics. Stream tables work equally well with this pattern.

---

*Soft deletes are a schema pattern. Incremental view maintenance is an engine feature. Understanding how they interact prevents subtle correctness bugs and ensures your stream tables always reflect the intended application semantics — not the physical table contents.*
