# Plan: Connection Pooler (PgBouncer) Compatibility

**Status:** PROPOSED
**Target:** v0.9.0

## Context

`pg_trickle` currently relies on session-scoped PostgreSQL features. Specifically:

1. **Advisory Locks** (`pg_advisory_lock`): Used for concurrency control in the background scheduler.
2. **Prepared Statements** (`PREPARE` / `EXECUTE`): Used heavily in the refresh engine for optimization and query performance.
3. **LISTEN / NOTIFY**: Used to broadcast events and alerts.

These features are incompatible with **transaction-mode connection poolers** (like PgBouncer operating in transaction mode). This is a critical issue because transaction-mode pooling is the default configuration on many managed cloud platforms (e.g., Supabase, Neon, RDS Proxy) and changing it to session mode is either highly discouraged, explicitly unsupported, or impossible.

This roadmap item details how we will tackle connection pooler compatibility.

## The Tradeoffs of Migration

Historically, "just fixing it" under the hood has been delayed due to several substantial tradeoffs that touch on performance, architecture, and complexity.

### 1. Large Refactoring Effort
Switching from session locks to transaction-scoped locks (`pg_advisory_xact_lock()`) or row-level catalog locks (`SELECT ... FOR UPDATE SKIP LOCKED`) requires significant changes across the refresh engine, the CDC pipeline, and the scheduler coordination layer.

### 2. Lock and Concurrency Risks
If we migrate to `pg_advisory_xact_lock()`, the lock is dropped the moment the immediate transaction commits. Given that a single stream table refresh cycle can span multiple transactions internally, this lock would be released prematurely, creating small windows for race conditions and concurrent overlap. Mitigating this with catalog row-level locking (`FOR UPDATE SKIP LOCKED`) introduces more robust transaction-safe concurrency but requires complex architectural changes in how workers claim stream tables.

### 3. The Performance Hit of Losing Prepared Statements
A transaction-mode pooler loses connection context between queries, meaning prepared statements created with `PREPARE` or Rust's `pg_sys::SPI_prepare` would result in "prepared statement does not exist" errors on subsequent executions. Eliminating prepared statements from the diff and merge engines means falling back to inline SQL parsing and planning on every cycle, which will introduce overhead for highly frequent, low-latency refresh operations.

### 4. Loss of the Publisher/Subscriber Pattern (`LISTEN`/`NOTIFY`)
Transaction mode also drops the continuity required for `LISTEN`/`NOTIFY`. If we disable `NOTIFY`, any alerts currently fired by the background workers immediately vanish from external monitoring clients unless we build a stateful polling fallback (which inherently puts more continuous load on the database).

## Evaluated Approaches

### Option A: The "Topology / Routing" Approach (Current Workaround)
Keep the session-scoped locks, prepared statements, and `LISTEN`/`NOTIFY` intact. Instead of changing the extension, we push the requirement to the infrastructure layer. Users configure their connection pooler to allow a dedicated *direct* connection to PostgreSQL strictly for `pg_trickle`'s background workers and administrative actions, while the rest of the application goes through the pooler.
- **Pros:** Zero implementation risk, keeps the current architectural simplicity, retains maximum performance.
- **Cons:** Puts the burden heavily on the user. Unworkable for developers bounded by strict Platform-as-a-Service requirements.

### Option B: The Full Rewrite (Zero Session State)
Eradicate all session state. Replace `pg_advisory_lock` with `FOR UPDATE SKIP LOCKED`. Strip out all prepared statements. Remove `LISTEN`/`NOTIFY`.
- **Pros:** Truly, seamlessly cloud-native without caveats.
- **Cons:** Requires a massive refactoring effort. Harms the runtime efficiency for users who run self-hosted setups and *don't* require transaction mode pooling. 

### Option C: Graceful Degradation / Opt-In Mode (Recommended)
Instead of a global GUC, we introduce this as a stream table property (e.g. `WITH (pooler_compatibility_mode = true)` in native syntax, or via `alter_stream_table()`).
- When `false` (default): Behavior remains exactly as it is today — performing fast, utilizing session-level state, and emitting notifications for that specific table.
- When `true`: The system gracefully degrades. It shifts concurrency logic to `FOR UPDATE SKIP LOCKED` universally, disables prepared statements within the refresh engine and generates inline SQL for that specific table, and completely bypasses `LISTEN`/`NOTIFY` emissions.
- **Pros:** Cloud users get a robust engine without breaking existing performance expectations for dedicated-server users. Enables a hybrid setup where internal tables are fast and external-facing tables operate nicely through standard poolers.
- **Cons:** Expanded testing surface (the CI needs to run the E2E matrix with both variations).

## Detailed Implementation Plan (v0.9.0)

We will proceed with **Option C: Graceful Degradation / Opt-In Mode**.

### Phase 1: Catalog Concurrency Modernization
Before handling prepared statements, we must solve concurrency. Instead of relying on `pg_advisory_lock()` across the scheduler, we will transition to `SELECT ... FOR UPDATE SKIP LOCKED` on the `pg_catalog.pgt_stream_tables` rows. This naturally guarantees safe cross-transaction boundaries as long as the worker keeps the master transaction open, or we use transaction boundaries strategically. This is functionally better regardless of PgBouncer and will apply to ALL stream tables safely.

### Phase 2: Introduce Opt-In Compatibility Toggle
We will add a new catalog column `pooler_compatibility_mode` to `pgt_stream_tables`.
- Modify `create_stream_table` / `alter_stream_table` to pass this option.
- Modify `refresh.rs` and `diff.rs` to check this flag on the target stream table. If enabled, skip the `SPI_prepare` flows and use inline strings for DML execution.
- Modify the event bus to skip `NOTIFY` emissions when the flag is true.

### Phase 3: Hardware and Dependency Testing
Update the E2E infrastructure to include a Docker Compose configuration that injects PgBouncer in transaction mode natively. We will run the entire light E2E suite through this port to validate the absence of session states and prove mathematical integrity remains under connection multiplexing.