# pg_trickle Error Reference

This document lists all `PgTrickleError` variants with descriptions, common
causes, and suggested fixes. If you encounter an error not listed here, please
[open an issue](https://github.com/grove/pg-trickle/issues).

> **Tip:** Most errors include context (table name, OID, or query fragment) in
> the message text. Use that context to narrow down the root cause.

---

## SQLSTATE Code Reference

Every pg_trickle error includes a PostgreSQL SQLSTATE code for programmatic
error handling. Use `SQLSTATE` in PL/pgSQL `EXCEPTION WHEN` blocks or check
the error code in your client library.

| Error Variant | SQLSTATE | Code Name |
|---------------|----------|-----------|
| QueryParseError | `42000` | SYNTAX_ERROR_OR_ACCESS_RULE_VIOLATION |
| TypeMismatch | `42804` | DATATYPE_MISMATCH |
| UnsupportedOperator | `0A000` | FEATURE_NOT_SUPPORTED |
| CycleDetected | `3F000` | INVALID_SCHEMA_DEFINITION |
| NotFound | `42P01` | UNDEFINED_TABLE |
| AlreadyExists | `42P07` | DUPLICATE_TABLE |
| InvalidArgument | `22023` | INVALID_PARAMETER_VALUE |
| QueryTooComplex | `54000` | PROGRAM_LIMIT_EXCEEDED |
| UpstreamTableDropped | `42P01` | UNDEFINED_TABLE |
| UpstreamSchemaChanged | `42P17` | INVALID_TABLE_DEFINITION |
| LockTimeout | `55P03` | LOCK_NOT_AVAILABLE |
| ReplicationSlotError | `55000` | OBJECT_NOT_IN_PREREQUISITE_STATE |
| WalTransitionError | `55000` | OBJECT_NOT_IN_PREREQUISITE_STATE |
| SpiError | `XX000` | INTERNAL_ERROR |
| SpiPermissionError | `42501` | INSUFFICIENT_PRIVILEGE |
| WatermarkBackwardMovement | `22000` | DATA_EXCEPTION |
| WatermarkGroupNotFound | `42704` | UNDEFINED_OBJECT |
| WatermarkGroupAlreadyExists | `42710` | DUPLICATE_OBJECT |
| RefreshSkipped | `55000` | OBJECT_NOT_IN_PREREQUISITE_STATE |
| InternalError | `XX000` | INTERNAL_ERROR |

---

## Error Categories

pg_trickle classifies errors into four categories that determine retry behavior:

| Category | Retried by scheduler? | Description |
|----------|-----------------------|-------------|
| **User** | No | Invalid queries, type mismatches, DAG cycles. Fix the input. |
| **Schema** | No (triggers reinitialize) | Upstream DDL changes. The stream table is reinitialized automatically. |
| **System** | Yes (with backoff) | Lock timeouts, replication slot problems, transient SPI failures. |
| **Internal** | No | Unexpected bugs. Please report these. |

---

## User Errors

### QueryParseError

**Message:** `query parse error: <details>`

**Description:** The defining query could not be parsed or validated by the
pg_trickle query analyzer.

**Common causes:**
- Syntax error in the defining query
- Use of PostgreSQL syntax not yet supported by pgrx's query parser
- A CTE or subquery that cannot be analyzed

**Suggested fix:** Simplify the query. Check that it runs as a standalone
`SELECT` statement. Review [SQL Reference — Expression Support](SQL_REFERENCE.md#expression-support)
for supported syntax.

---

### TypeMismatch

**Message:** `type mismatch: <details>`

**Description:** A type incompatibility was detected between the defining query
output and the stream table schema, or between source columns and expected types.

**Common causes:**
- Column type changed on a source table after stream table creation
- Explicit cast to an incompatible type in the defining query
- `UNION` branches with mismatched column types

**Suggested fix:** Ensure column types match. Use explicit `CAST()` to align
types if needed. If the source table changed, use
`pgtrickle.repair_stream_table()` to reinitialize.

---

### UnsupportedOperator

**Message:** `unsupported operator for DIFFERENTIAL mode: <operator>`

**Description:** The defining query uses an SQL operator or construct that
pg_trickle cannot maintain incrementally.

**Common causes:**
- `TABLESAMPLE`, `GROUPING SETS` beyond the branch limit, recursive CTEs with
  unsupported patterns, certain window function combinations
- Non-monotonic or volatile functions in positions that prevent differential
  maintenance

**Suggested fix:** Use `refresh_mode => 'FULL'` to fall back to full
recomputation:
```sql
SELECT pgtrickle.alter_stream_table('my_stream_table',
    refresh_mode => 'FULL');
```
Or restructure the query to avoid the unsupported construct. See
[SQL Reference — Expression Support](SQL_REFERENCE.md#expression-support).

---

### CycleDetected

**Message:** `cycle detected in dependency graph: A -> B -> C -> A`

**Description:** Adding or altering this stream table would create a circular
dependency in the refresh DAG.

**Common causes:**
- Stream table A depends on stream table B, which depends on A
- Indirect cycles through chains of stream tables

**Suggested fix:** Restructure the stream table definitions to break the cycle.
Use `pgtrickle.get_dependency_graph()` to visualize the current DAG. If
circular dependencies are intentional, enable `pg_trickle.allow_circular = true`
(see [Configuration](CONFIGURATION.md)).

---

### NotFound

**Message:** `stream table not found: <name>`

**Description:** The specified stream table does not exist in the
`pgtrickle.pgt_stream_tables` catalog.

**Common causes:**
- Typo in the stream table name
- The stream table was already dropped
- Schema-qualified name required but not provided (e.g., `myschema.my_st`)

**Suggested fix:** Check the name with `pgtrickle.list_stream_tables()`. Use
the fully qualified name: `schema.table_name`.

---

### AlreadyExists

**Message:** `stream table already exists: <name>`

**Description:** A `create_stream_table()` call was made for a stream table
name that is already registered.

**Common causes:**
- Re-running a migration or DDL script without `IF NOT EXISTS`

**Suggested fix:** Use `pgtrickle.create_stream_table_if_not_exists()` or
`pgtrickle.create_or_replace_stream_table()` for idempotent creation.

---

### InvalidArgument

**Message:** `invalid argument: <details>`

**Description:** An invalid value was passed to a pg_trickle API function.

**Common causes:**
- Invalid `refresh_mode` value (must be `'DIFFERENTIAL'`, `'FULL'`, or `'AUTO'`)
- Calling `resume_stream_table()` on a table that is not suspended
- Invalid schedule interval or threshold value
- Empty or malformed table name

**Suggested fix:** Check the function signature in the
[SQL Reference](SQL_REFERENCE.md) and correct the argument.

---

### QueryTooComplex

**Message:** `query too complex: <details>`

**Description:** The defining query exceeds the maximum parse depth, which
protects against stack overflow during query analysis.

**Common causes:**
- Deeply nested subqueries (> 64 levels by default)
- Large `UNION ALL` chains
- Complex CTE hierarchies

**Suggested fix:** Simplify the query. If the depth limit is too restrictive,
increase `pg_trickle.max_parse_depth` (default: 64). See
[Configuration](CONFIGURATION.md).

---

## Schema Errors

### UpstreamTableDropped

**Message:** `upstream table dropped: OID <oid>`

**Description:** A source table referenced by the stream table's defining query
was dropped.

**Common causes:**
- `DROP TABLE` on a source table
- Table replaced via `DROP` + `CREATE` (new OID)

**Suggested fix:** Either recreate the source table with the same schema or
drop the stream table and recreate it. If `pg_trickle.block_source_ddl = true`
(default), the DROP would have been blocked in the first place.

---

### UpstreamSchemaChanged

**Message:** `upstream table schema changed: OID <oid>`

**Description:** A source table's schema was altered (e.g., column added,
dropped, or type changed) in a way that affects the defining query.

**Common causes:**
- `ALTER TABLE ... ADD/DROP/ALTER COLUMN` on a source table
- Type change on a column used in the defining query

**Suggested fix:** The stream table will be automatically reinitialized on the
next scheduler tick. If `pg_trickle.block_source_ddl = true` (default), most
schema changes are blocked proactively. Use
`pgtrickle.alter_stream_table(..., query => '...')` to update the defining
query if needed.

---

## System Errors

### LockTimeout

**Message:** `lock timeout: <details>`

**Description:** A lock required for refresh could not be acquired within the
configured timeout.

**Common causes:**
- Long-running transactions holding locks on the stream table or source tables
- Concurrent `ALTER TABLE` or `VACUUM FULL` operations
- High contention on the change buffer tables

**Suggested fix:** This error is automatically retried with exponential backoff.
If persistent, investigate long-running transactions with `pg_stat_activity`.
Consider increasing `lock_timeout` or reducing refresh frequency.

---

### ReplicationSlotError

**Message:** `replication slot error: <details>`

**Description:** An error occurred with the logical replication slot used for
WAL-based CDC.

**Common causes:**
- Replication slot dropped externally
- `wal_level` changed from `logical` to a lower level
- Slot lag exceeded `max_slot_wal_keep_size`

**Suggested fix:** Check replication slot status with
`SELECT * FROM pg_replication_slots`. Ensure `wal_level = logical`. If the slot
was dropped, pg_trickle will recreate it automatically. See
[Configuration — WAL CDC](CONFIGURATION.md).

---

### WalTransitionError

**Message:** `WAL transition error: <details>`

**Description:** An error occurred during the transition from trigger-based CDC
to WAL-based CDC.

**Common causes:**
- `wal_level` is not `logical` when `cdc_mode = 'auto'`
- Transient connection issues during the transition

**Suggested fix:** Ensure `wal_level = logical` in `postgresql.conf` if you
want WAL-based CDC. Otherwise set `pg_trickle.cdc_mode = 'trigger'` to stay
on trigger-based CDC. This error is retried automatically.

---

### SpiError

**Message:** `SPI error: <details>`

**Description:** A PostgreSQL Server Programming Interface (SPI) error occurred
during an internal query.

**Common causes:**
- Transient serialization failures under high concurrency
- Deadlocks between refresh and concurrent DML
- Connection issues in background workers
- Permanent errors: missing columns, syntax errors in generated SQL

**Suggested fix:** Transient SPI errors (deadlocks, serialization failures) are
retried automatically. Permanent errors (permission denied, missing objects)
will suspend the stream table after `max_consecutive_errors` failures. Check
`pgtrickle.check_health()` for details.

---

### SpiPermissionError

**Message:** `SPI permission error: <details>`

**Description:** The background worker's role lacks required permissions.

**Common causes:**
- Missing `SELECT` privilege on a source table
- Missing `INSERT`/`UPDATE`/`DELETE` privilege on the stream table
- Role used by the background worker is not the table owner

**Suggested fix:** Grant the necessary privileges to the role running
pg_trickle's background workers:
```sql
GRANT SELECT ON source_table TO pgtrickle_role;
GRANT ALL ON pgtrickle.my_stream_table TO pgtrickle_role;
```
This error does **not** count toward the consecutive error suspension limit.

---

## Watermark Errors

### WatermarkBackwardMovement

**Message:** `watermark moved backward: <details>`

**Description:** A watermark advancement was rejected because the new value is
older than the current watermark, violating monotonicity.

**Common causes:**
- Clock skew in distributed systems
- Manual watermark manipulation with an incorrect value
- Bug in watermark tracking logic

**Suggested fix:** Ensure watermark values are monotonically increasing. Check
the current watermark with `pgtrickle.get_watermark_groups()`.

---

### WatermarkGroupNotFound

**Message:** `watermark group not found: <details>`

**Description:** The specified watermark group does not exist.

**Common causes:**
- Typo in the watermark group name
- The group was deleted or never created

**Suggested fix:** List existing groups with
`pgtrickle.get_watermark_groups()`.

---

### WatermarkGroupAlreadyExists

**Message:** `watermark group already exists: <details>`

**Description:** A watermark group with this name already exists.

**Common causes:**
- Re-running a setup script without idempotent guards

**Suggested fix:** Use a different name or delete the existing group first.

---

## Transient Errors

### RefreshSkipped

**Message:** `refresh skipped: <details>`

**Description:** A refresh was skipped because a previous refresh for the same
stream table is still running.

**Common causes:**
- Slow refresh (large delta or complex query) overlapping with the next
  scheduled cycle
- Multiple manual `refresh_stream_table()` calls in parallel

**Suggested fix:** No action needed — the scheduler will retry on the next
cycle. If this happens frequently, increase the schedule interval or
investigate why refreshes are slow using `pgtrickle.explain_st()`.

This error does **not** count toward the consecutive error suspension limit.

---

## Internal Errors

### InternalError

**Message:** `internal error: <details>`

**Description:** An unexpected internal error that indicates a bug in
pg_trickle.

**Common causes:**
- This should not happen in normal operation

**Suggested fix:** Please [report the issue](https://github.com/grove/pg-trickle/issues)
with the full error message, your PostgreSQL version, and pg_trickle version.
Include the output of `pgtrickle.check_health()` and the relevant PostgreSQL
log entries.

---

## See Also

- [FAQ — Troubleshooting](FAQ.md#troubleshooting)
- [SQL Reference](SQL_REFERENCE.md)
- [Configuration Reference](CONFIGURATION.md)
