# Relay Gap Analysis — Lessons from Feldera & RisingWave

> **Status:** Backlog (identified 2026-04-19)
> **Source:** Comparative analysis of Feldera connector docs and RisingWave
> ingestion/delivery docs against the Phase 1 and Phase 2 relay plans.
> **Action required:** None before v0.28.0. All items are additive and
> bolt-on. See individual items for suggested release slots.

---

## Summary

Nine ideas emerged from studying Feldera's connector/fault-tolerance design
and RisingWave's subscription/delivery model. None require changes to the
Phase 1 architecture (Source/Sink traits, coordinator, `RelayMessage` envelope,
config JSONB schema). All are additive — new config fields, new transform
passes, new HTTP endpoints, or new SQL functions in the pg-trickle core.

**Exception:** GAP-008 (pg-native subscription cursor) lives in the
pg-trickle extension itself, not the relay binary. It is the highest-leverage
item: it makes the relay optional for single-consumer use cases.

**Ship order recommendation:**
1. GAP-001 (secret interpolation) — before v0.28.0 ships to any real user
2. GAP-005 (drained health endpoint) — trivial, part of v0.28.0
3. Everything else — Phase 2 or post-v1.0

---

## GAP-001 — Secret Reference Interpolation in Config JSONB

**Inspired by:** Feldera `${secret:kubernetes:name/key}` + `${env:VAR_NAME}`

**Priority: High — DESIGNED AND SCHEDULED. Implemented as RELAY-SEC in
Phase 1. See [§A.16 in PLAN_RELAY_CLI.md](./PLAN_RELAY_CLI.md#a16-secret-reference-interpolation).**

### Problem

`relay_outbox_config.config` and `relay_inbox_config.config` store connector
credentials (Kafka SASL passwords, NATS tokens, SQS secret keys, etc.) as
plaintext JSON. Anyone with `SELECT` access to these tables can read every
credential. Rotating a credential means an `UPDATE` to the config row.

### Proposed solution

At config-load time (startup + every hot-reload), walk the resolved JSONB tree
and substitute two reference types before handing the config to the Source/Sink
constructors:

| Syntax | Resolved from | Use-case |
|--------|--------------|----------|
| `${env:VAR_NAME}` | Process environment variable | 12-factor deployments, CI pipelines |
| `${file:/path/to/secret}` | Contents of a file on the relay host | Docker secrets (`/run/secrets/`), K8s mounted secrets (`/etc/secrets/`) |

Phase 2 can add `${secret:vault:path/key}` (HashiCorp Vault) and
`${secret:aws-ssm:param-name}` (AWS Parameter Store) once the core mechanism
is in place.

### Example config rows (no secrets in the DB)

```sql
INSERT INTO pgtrickle.relay_outbox_config (name, config) VALUES (
    'orders-to-kafka',
    '{
        "source_type": "outbox",
        "source": {"outbox": "order_events", "group": "order-publisher"},
        "sink_type": "kafka",
        "sink": {
            "brokers": "${env:KAFKA_BROKERS}",
            "sasl_username": "${env:KAFKA_SASL_USERNAME}",
            "sasl_password": "${file:/run/secrets/kafka_sasl_password}",
            "topic": "order-events"
        }
    }'
);
```

### Implementation sketch

New `src/secrets.rs` in the relay crate:

```rust
/// Walk a serde_json::Value tree and resolve ${env:X} and ${file:/path}
/// references. Returns an error if any reference cannot be resolved.
/// IMPORTANT: never log the returned value — it contains resolved secrets.
pub fn resolve_secret_refs(value: &serde_json::Value) -> Result<serde_json::Value, RelayError> {
    match value {
        Value::String(s) => resolve_string(s),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), resolve_secret_refs(v)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(arr) => {
            Ok(Value::Array(arr.iter().map(resolve_secret_refs)
                .collect::<Result<Vec<_>, _>>()?))
        }
        other => Ok(other.clone()),
    }
}

fn resolve_string(s: &str) -> Result<serde_json::Value, RelayError> {
    if let Some(var) = s.strip_prefix("${env:").and_then(|s| s.strip_suffix('}')) {
        validate_env_var_name(var)?;
        let val = std::env::var(var)
            .map_err(|_| RelayError::SecretNotFound(format!("env var {var} not set")))?;
        return Ok(Value::String(val));
    }
    if let Some(path) = s.strip_prefix("${file:").and_then(|s| s.strip_suffix('}')) {
        let val = std::fs::read_to_string(path)
            .map_err(|e| RelayError::SecretNotFound(format!("file {path}: {e}")))?;
        return Ok(Value::String(val.trim_end_matches('\n').to_owned()));
    }
    Ok(Value::String(s.to_owned()))
}
```

`resolve_secret_refs` is called in `coordinator.rs` after reading each pipeline
row from the DB, before constructing the Source/Sink. The resolved config is
held in memory only — never written back, never logged.

On hot-reload (NOTIFY), secrets are re-resolved. This enables zero-downtime
credential rotation: mount the new secret file / update the env var, then
`UPDATE relay_outbox_config SET updated_at = now()` to trigger a reload.

### Error handling

If a reference cannot be resolved, the pipeline is disabled and a
`pgtrickle_relay_alert` (or structured log entry) is emitted. The coordinator
continues running all other pipelines — a missing secret for one pipeline must
not take down the whole relay.

### Security checklist

- [ ] Never log resolved config values (`tracing::debug!("loaded pipeline config: {:?}", raw_config)` — log the *raw* (unreferenced) form only)
- [ ] `validate_env_var_name` enforces `[A-Za-z_][A-Za-z0-9_]*` to prevent traversal
- [ ] File path is not validated for traversal (the relay runs with operator-provided config; restricting paths is an operator responsibility, same as Feldera's approach)
- [ ] Resolved secrets do not appear in Prometheus metrics labels or health endpoints
- [ ] `relay_outbox_config.config` column should have a `pgtrickle_relay` role that can INSERT/UPDATE but not SELECT after the secret fields are stored as references — or, better, store *only* references and grant no SELECT at all; document this in SQL_REFERENCE.md

### Resolution

**Accepted and designed.** Implemented as `RELAY-SEC` in Phase 1. Design
details (JSONB walker, `${env:}` + `${file:}` tokens, per-pipeline failure
isolation, hot-reload re-resolution, security checklist) are fully specified
in [PLAN_RELAY_CLI.md §A.16](./PLAN_RELAY_CLI.md#a16-secret-reference-interpolation).

Phase 2 extensions: `${secret:vault:path}` and `${secret:aws-ssm:param}`
(requires `vaultrs` / `aws-sdk-ssm` optional features).

---

## GAP-002 — Completion Token / Drain-Wait Endpoint

**Inspired by:** Feldera `GET /completion_status?token=<token>`

**Priority: Medium — useful for tests and synchronous workflows.**

### Problem

Integration tests and application code currently have no way to know when a
specific outbox batch has been acknowledged to the sink. Tests must either
sleep, poll the sink, or use a Kafka consumer to verify delivery. This is
fragile and slow.

### Proposed solution

A `GET /wait-for-outbox-id` endpoint on the relay's existing axum metrics
server:

```
GET /wait-for-outbox-id?pipeline=<name>&id=<outbox_id>&timeout_ms=<ms>

200 OK  {"status": "complete", "acked_at": "2026-04-19T12:34:56Z"}
408     {"status": "timeout",  "last_acked_id": 42}
404     {"status": "unknown_pipeline"}
```

The relay tracks `last_acked_outbox_id` per pipeline in memory (already needed
for the `consumer_lag` metric). The endpoint long-polls against that value with
the specified timeout using a `tokio::sync::watch` channel.

### Architecture fit

`last_acked_outbox_id` is already written to `relay_consumer_offsets` after
each batch commit. The endpoint reads the in-memory copy (no DB query) and
returns immediately if already past the requested `id`. If not, it waits on
the `watch::Receiver` until the worker advances past that id or the timeout
fires.

No changes to Source/Sink traits, coordinator, or DB schema.

### Suggested release

v0.28.0 Phase 2 or as a Phase 1 bonus item (implementation is ~50 lines).

---

## GAP-003 — Per-Pipeline Output Buffer / Micro-Batching

**Inspired by:** Feldera `enable_output_buffer` + `max_output_buffer_time_millis`
+ `max_output_buffer_size_records`

**Priority: Medium — required before adding Delta Lake / Iceberg / BigQuery sinks.**

### Problem

The relay currently forwards each poll batch to the sink immediately. Sinks
with high per-write overhead (Iceberg, Delta Lake, BigQuery, ClickHouse batch
API) produce thousands of tiny files or many small API calls when the stream
table refresh rate is high. Feldera solved this with an output buffer that
accumulates changes before flushing.

### Proposed solution

Two new optional fields in the pipeline `config` JSONB:

```json
{
  "output_buffer_time_ms": 5000,
  "output_buffer_max_records": 100000
}
```

In the worker loop, instead of `sink.publish(&batch)` immediately, push each
batch into a `VecDeque<RelayMessage>`. Flush to the sink when either:
- accumulated `len() >= output_buffer_max_records`, or
- `Instant::now() - buffer_started_at >= output_buffer_time_ms`

The `tokio::time::interval` already exists in the worker loop for the poll
tick. The buffer flush just piggybacks on that timer.

### Architecture fit

Sits entirely inside the existing worker loop between `source.poll()` and
`sink.publish()`. No trait changes, no schema changes.

### Suggested release

Phase 2, alongside the first analytics sink (Delta Lake, Iceberg, ClickHouse).

---

## GAP-004 — Backfill → Live Handoff (`startup_mode`)

**Inspired by:** Feldera connector orchestration with `labels` + `start_after`

**Priority: Medium — important for cold-start correctness when full-refresh is pending.**

### Problem

When a relay restarts with a pending full-refresh outbox row (e.g. after a
manual `refresh_stream_table('orders', 'FULL')` or an AUTO fallback), it
forwards a mix of snapshot rows and incremental rows to the sink in a single
poll batch. Upsert sinks (PostgreSQL inbox, Redis, ClickHouse) handle this
correctly, but append-only sinks (Kafka, NATS without dedup) receive
duplicates and out-of-order snapshots.

### Proposed solution

Optional `startup_mode` field in pipeline config:

| Value | Behaviour |
|-------|-----------|
| `"latest"` (default) | Start from the current committed offset. Skip any pending rows (no change from current behaviour). |
| `"snapshot_first"` | Drain all pending full-refresh outbox rows completely before forwarding any incremental rows. Within the snapshot, apply upsert semantics. |
| `"incremental_only"` | Skip any pending full-refresh rows; forward only differential rows from the current offset. |
| `"since:<outbox_id>"` | Resume from a specific `outbox_id` (replay). Equivalent to `seek_offset()`. |

`"snapshot_first"` is the Feldera `start_after` / `backfill` pattern: drain
the batch-load connector before activating the streaming connector.

### Architecture fit

Logic lives in the coordinator's pipeline-start path, before the worker loop
begins. No trait changes. Reads `payload["full_refresh"]` flag from outbox
rows already decoded by `decode_payload()`.

### Suggested release

Phase 2.

---

## GAP-005 — `/health/drained` Endpoint

**Inspired by:** Feldera `end_of_input: true` + `buffered_records: 0` status

**Priority: Low-medium — DESIGNED AND SCHEDULED. Included in RELAY-9 (Phase 1). See [§A.12 in PLAN_RELAY_CLI.md](./PLAN_RELAY_CLI.md#a12-observability).**

### Problem

Kubernetes `preStop` hooks need a way to block pod termination until all in-flight
batches have been acked to sinks. Without this, a rolling deployment can drop
messages that were polled but not yet committed during the shutdown window.

The existing `/health` endpoint returns `200 OK` / `503 Service Unavailable`
based on PG connection health, but says nothing about consumer lag.

### Proposed solution

Add `/health/drained` to the axum metrics server:

```
GET /health/drained

200 OK   {"drained": true,  "max_lag_rows": 0}
202      {"drained": false, "max_lag_rows": 1247, "lagging_pipelines": ["orders-to-kafka"]}
```

Returns 200 only when `last_acked_outbox_id >= max(id)` across all enabled
pipelines (i.e. consumer lag = 0 on every pipeline).

Kubernetes preStop hook:
```yaml
lifecycle:
  preStop:
    exec:
      command: ["sh", "-c", "until curl -sf http://localhost:9090/health/drained; do sleep 1; done"]
```

### Architecture fit

Reads in-memory `last_acked_outbox_id` per pipeline (already tracked for
GAP-002). No DB queries, no trait changes.

### Resolution

**Accepted and designed.** Folded into `RELAY-9` (metrics + health endpoints)
in Phase 1. The `/health/drained` endpoint returns HTTP 200 when consumer lag
= 0 across all pipelines, 202 while draining. Reads in-memory state only.
Documented in [PLAN_RELAY_CLI.md §A.12](./PLAN_RELAY_CLI.md#a12-observability).

---

## GAP-006 — pg-Native Subscription Cursor SQL API

**Inspired by:** RisingWave `CREATE SUBSCRIPTION` + `DECLARE cur SUBSCRIPTION CURSOR FOR sub`

**Priority: High (long-term) — makes the relay optional for simple use cases.**

**Note: This is a pg-trickle core extension feature, not a relay binary feature.**

### Problem

The relay binary requires Rust, a deployment slot, and operational overhead.
For simple single-consumer use cases (a backend service, a worker, a cron job),
the overhead of running `pgtrickle-relay` alongside the application just to
read outbox rows is disproportionate.

RisingWave solves this elegantly: `CREATE SUBSCRIPTION` backed by the system's
change log, consumed via the standard PostgreSQL cursor protocol over any Postgres
client library. No broker, no agent, no sidecar.

### Proposed solution

Two new SQL functions in the pg-trickle extension:

```sql
-- Open a named server-side cursor over the outbox for a stream table.
-- since_outbox_id = 0 means "from the beginning" (full snapshot).
-- since_outbox_id = NULL means "from current position" (latest mode).
SELECT pgtrickle.open_outbox_cursor(
    stream_table    TEXT,
    cursor_name     TEXT,
    since_outbox_id BIGINT DEFAULT NULL
);

-- Fetch up to batch_size rows from the cursor.
-- Each row: (outbox_id, op TEXT, payload JSONB, is_full_refresh BOOL, rw_timestamp TIMESTAMPTZ)
SELECT * FROM pgtrickle.fetch_outbox_cursor(
    cursor_name  TEXT,
    batch_size   INT DEFAULT 100
);

-- Commit progress. The retention drain will not delete rows with
-- id <= committed_outbox_id for cursors registered with this name.
SELECT pgtrickle.commit_outbox_cursor(
    cursor_name     TEXT,
    committed_outbox_id BIGINT
);

-- Close the cursor and release the progress registration.
SELECT pgtrickle.close_outbox_cursor(cursor_name TEXT);
```

Backed by a server-side PG cursor over `outbox_<st>` + `outbox_delta_rows_<st>`,
with progress tracked in a `pgtrickle.outbox_cursor_progress` catalog table
(so retention drain honours the cursor's position, same as consumer groups).

**Consumer-side Python example (no relay binary needed):**

```python
conn = psycopg2.connect(dsn)
conn.autocommit = False

cur = conn.cursor()
cur.execute("SELECT pgtrickle.open_outbox_cursor('orders', 'my-cursor')")

while True:
    cur.execute("SELECT * FROM pgtrickle.fetch_outbox_cursor('my-cursor', 100)")
    rows = cur.fetchall()
    if not rows:
        time.sleep(1)
        continue
    for (outbox_id, op, payload, is_full_refresh, ts) in rows:
        publish_to_downstream(op, payload)
    last_id = rows[-1][0]
    cur.execute("SELECT pgtrickle.commit_outbox_cursor('my-cursor', %s)", [last_id])
    conn.commit()
```

This is the "zero infrastructure" path: pure SQL, any Postgres client, no relay
binary, no Kafka, no NATS. The relay binary remains the right choice for
multi-consumer fan-out, broker integration, and complex routing.

### Suggested release

v0.29.0 or v0.30.0 — after the relay binary is stable and adoption patterns
are understood.

---

## GAP-007 — Force Key Compaction within a Poll Batch

**Inspired by:** RisingWave `force_compaction = true` sink option

**Priority: Low-medium — reduces write amplification for upsert sinks.**

### Problem

When a stream table refresh rate is high (sub-second) and the relay poll
interval is coarser (e.g. 500 ms), a single poll batch may contain multiple
changes to the same primary key: `insert → update → update`. An upsert sink
(PostgreSQL inbox, Redis, ClickHouse) would execute three writes where one
suffices.

### Proposed solution

Optional `compaction_key` field in pipeline config:

```json
{
    "sink_type": "pg-inbox",
    "sink": {"inbox_table": "order_inbox"},
    "compaction_key": ["order_id"]
}
```

A `compaction_pass(batch, key_columns)` function in `transforms.rs` that folds
the batch before calling `sink.publish()`:

1. Group `RelayMessage` items by the values of `payload[key_columns]`.
2. Within each group, apply last-write-wins: keep only the final state.
3. If the final state is a `delete`, emit a `delete`. Otherwise, emit a single
   `insert` (upsert semantics for the downstream).
4. Preserve original order within each group (for append-only sinks that don't
   set `compaction_key`, behaviour is unchanged).

### Architecture fit

A pure transform over `Vec<RelayMessage>` between `source.poll()` and
`sink.publish()`. No trait changes, no schema changes.

### Suggested release

Phase 2, alongside upsert sink backends (ClickHouse, Delta Lake).

---

## GAP-008 — Snapshot-Only Pipeline Mode

**Inspired by:** RisingWave CDC `snapshot.mode = initial_only`

**Priority: Low — useful for one-time migrations and initial data exports.**

### Problem

There is no clean way to do a single, orchestrated full export of a stream
table to an external system (e.g. initial load of a ClickHouse table, one-time
S3 export). Users currently have to: manually trigger a FULL refresh, run the
relay to drain it, then manually disable the pipeline. Error-prone and hard to
automate.

### Proposed solution

Optional `mode: "snapshot_only"` in pipeline config:

```json
{
    "mode": "snapshot_only",
    "source_type": "outbox",
    "source": {"outbox": "orders"},
    "sink_type": "s3",
    "sink": {"bucket": "my-data-lake", "prefix": "orders/"}
}
```

Relay behaviour:
1. Coordinator detects `mode = snapshot_only` at pipeline startup.
2. Calls `pgtrickle.refresh_stream_table('orders', 'FULL')` if no full-refresh
   outbox row is already pending.
3. Worker drains the resulting outbox row (handles claim-check path).
4. After `outbox_rows_consumed()` is called, sets `enabled = false` on the
   pipeline row via `pgtrickle.disable_relay('orders-to-s3')`.
5. Emits a structured log: `pipeline="orders-to-s3" mode=snapshot_only status=complete rows_exported=142857`.

### Architecture note (mildest of the nine items)

The coordinator needs to distinguish one-shot pipelines from continuous ones
and handle the `enabled = false` self-termination. The worker loop's existing
drain-to-empty logic already exists in the shutdown path — this just hooks it
up differently. No Source/Sink trait changes.

### Suggested release

Phase 2.

---

## GAP-009 — SINCE / FULL / LATEST Startup Position as First-Class Config

**Inspired by:** RisingWave subscription cursor `SINCE <ts>` / `FULL` / `since now()`

**Priority: Low — documentation and UX improvement over `seek_offset()` SQL calls.**

### Problem

Replaying a pipeline from a specific point, or starting fresh from the current
position, currently requires knowing about `pgtrickle.seek_offset()` and calling
it manually before starting the relay. This is undiscoverable from the relay
config alone. RisingWave surfaces this as a first-class cursor declaration that
is self-documenting.

### Proposed solution

This overlaps significantly with GAP-004 (`startup_mode`). Merge them: the
`startup_mode` field proposed in GAP-004 already covers the `since:<id>`,
`latest`, and `snapshot_first` (≈`FULL`) modes.

The additional UX improvement from this gap analysis: document the mapping
between RisingWave's cursor modes and pg-trickle's `startup_mode` values in
SQL_REFERENCE.md and the relay Getting Started guide, so users familiar with
RisingWave can orient immediately.

| RisingWave cursor | pg-trickle relay `startup_mode` |
|-------------------|---------------------------------|
| `SINCE now()` | `"latest"` (default) |
| `FULL` | `"snapshot_first"` |
| `SINCE begin()` | `"since:0"` |
| `SINCE <unix_ms>` | `"since:<outbox_id>"` (closest outbox_id ≥ timestamp) |

**No separate implementation required** — this is a documentation task once
GAP-004 is implemented.

### Suggested release

Alongside GAP-004 in Phase 2.

---

## Summary Table

| ID | Feature | Inspired by | Arch change? | Priority | Suggested release |
|----|---------|-------------|-------------|----------|-------------------|
| GAP-001 | Secret reference interpolation | Feldera | No | **High** | ✅ v0.28.0 Phase 1 (RELAY-SEC, §A.16) |
| GAP-002 | Completion token / drain-wait endpoint | Feldera | No | Medium | v0.28.0 Phase 1 bonus or Phase 2 |
| GAP-003 | Per-pipeline output buffer | Feldera | No | Medium | Phase 2 (before analytics sinks) |
| GAP-004 | Backfill → live handoff (`startup_mode`) | Feldera | No | Medium | Phase 2 |
| GAP-005 | `/health/drained` endpoint | Feldera | No | Low-medium | ✅ v0.28.0 Phase 1 (RELAY-9, §A.12) |
| GAP-006 | pg-native subscription cursor SQL API | RisingWave | No (core feature) | **High** (long-term) | v0.29–0.30 |
| GAP-007 | Force key compaction | RisingWave | No | Low-medium | Phase 2 |
| GAP-008 | Snapshot-only pipeline mode | RisingWave | Minimal | Low | Phase 2 |
| GAP-009 | SINCE/FULL/LATEST startup position | RisingWave | No (doc only) | Low | Alongside GAP-004 |
