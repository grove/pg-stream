# PLAN_RELAY_WIRE_FORMATS — Pluggable Wire Formats for pgtrickle-relay

> **Status:** Proposed
> **Created:** 2026-04-23
> **Priority:** Medium (unblocks Debezium / long-tail CDC sources for the
> appliance plan)
> **Targets:** `pgtrickle-relay` 0.30 – 0.34
> **Related:**
> [pgtrickle-relay/README.md](../../pgtrickle-relay/README.md) ·
> [PLAN_RELAY_CLI.md](PLAN_RELAY_CLI.md) ·
> [../ecosystem/PLAN_APPLIANCE.md](../ecosystem/PLAN_APPLIANCE.md)

---

## 1  Goal

Decouple **transport** (Kafka, NATS, Redis, RabbitMQ, HTTP, SQS) from
**envelope format** in `pgtrickle-relay`'s pipelines — both reverse
(consume) and forward (produce) — and ship **Debezium support in both
directions** as the first non-native format. Today the relay
hard-codes the pg_trickle native envelope; this plan adds a
`WireFormat` axis so a Kafka pipeline can speak Debezium JSON, Avro,
Maxwell, Canal, or any future envelope without touching the transport
code.

The strategic motivation is twofold:

1. **Reverse direction** unlocks the long-tail CDC sources for the
   appliance ([../ecosystem/PLAN_APPLIANCE.md](../ecosystem/PLAN_APPLIANCE.md)):
   Oracle, Db2, MongoDB, Cassandra, Vitess, Spanner, Yugabyte — every
   source Debezium covers becomes usable via Debezium Server → relay
   → inbox table.
2. **Forward direction** makes pg_trickle a first-class CDC source for
   the entire Debezium-shaped ecosystem: Apache Iceberg, Apache Pinot,
   Apache Doris, StarRocks, ClickHouse, Apache Druid, Snowflake Kafka
   Connector, Materialize, ksqlDB, Flink CDC sinks — none of these
   need to learn the pg_trickle native envelope; they consume
   Debezium-shaped messages today.

Symmetry is the design property that pays for itself. The same
envelope flows in and out, the same Schema Registry is reused, the
same DLQ + monitoring story applies, and the relay slots into any
Debezium-shaped pipeline regardless of direction.

---

## 2  Why the Relay, Not the Extension

- The relay is **already a separate Rust binary** with sidecar
  deployment. Adding wire formats is purely additive — no JVM, no new
  process model, no impact on the appliance's "just a Postgres" story.
- The relay **already speaks four of Debezium Server's sinks** (Kafka,
  NATS, Redis Streams, RabbitMQ) plus HTTP. Both directions reuse the
  same transports; only the envelope encoder/decoder is new.
- The relay **already has the inbox/outbox contract** and SQL config
  surface. A new envelope just changes the bytes on the wire;
  downstream pg_trickle treatment is unchanged.
- Embedding Debezium Engine (the JVM library) in pg_trickle itself is
  rejected: it would force a JVM into the appliance deployment.

---

## 3  Architecture

### 3.1  New trait (symmetric)

```rust
trait WireFormat: Send + Sync {
    fn name(&self) -> &'static str;

    // ------------ reverse path (consume) ------------

    /// Decode a single raw message from the transport into the inbox
    /// row format pg_trickle expects. Returns None for messages the
    /// format chooses to skip (e.g. Debezium tombstones with
    /// tombstone_handling = drop, or schema-change events).
    fn decode(&self, raw: &RawMessage) -> Result<Option<InboxRow>, WireError>;

    /// Optional schema-evolution hook on the consume side. Default
    /// no-op. Debezium impl uses this to detect envelope-version
    /// changes and halt the consumer rather than silently corrupt
    /// rows.
    fn observe_schema(&mut self, _raw: &RawMessage) -> Result<(), WireError> {
        Ok(())
    }

    // ------------ forward path (produce) ------------

    /// Encode a pg_trickle outbox row into transport bytes plus a
    /// routing key. Implementations may emit multiple messages per
    /// row (e.g. Debezium emits a tombstone after a delete for log
    /// compaction); the EncodedBatch carries them in order.
    fn encode(&self, row: &OutboxRow, ctx: &EncodeContext)
        -> Result<EncodedBatch, WireError>;

    /// Optional registration hook on the produce side. Called once
    /// per (topic, schema) pair before the first emit. Debezium-Avro
    /// impl registers schemas with Confluent Schema Registry here.
    fn register_schema(&mut self, _topic: &str, _schema: &OutboxSchema)
        -> Result<(), WireError> {
        Ok(())
    }
}
```

### 3.2  Composition

Reverse (consume) pipeline:

```
Source (Kafka / NATS / Redis / …)
    └─▶ RawMessage
        └─▶ WireFormat::decode  ────▶ Option<InboxRow>
                                          └─▶ inbox writer (SPI)
```

Forward (produce) pipeline:

```
outbox poller (SPI)
    └─▶ OutboxRow
        └─▶ WireFormat::encode  ────▶ EncodedBatch
                                          └─▶ Sink (Kafka / NATS / Redis / …)
```

`Source` and `Sink` impls stay unchanged; the wire format is the only
new dimension.

### 3.3  Config surface

One new optional field in every pipeline config (forward and reverse):

```jsonc
// reverse (consume) — Debezium → inbox
{
  "source_type": "kafka",
  // … existing transport fields …
  "wire_format": "debezium",          // default = "pgtrickle_native"
  "wire_config": {
    "envelope": "json",               // json | avro | protobuf
    "schema_registry": "http://schema-registry:8081",
    "tombstone_handling": "delete",   // delete | drop
    "key_strategy": "primary_key",    // primary_key | message_key
    "snapshot_op_treatment": "insert" // insert | upsert
  }
}

// forward (produce) — outbox → Debezium
{
  "sink_type": "kafka",
  // … existing transport fields …
  "wire_format": "debezium",
  "wire_config": {
    "envelope": "json",
    "schema_registry": "http://schema-registry:8081",
    "server_name": "pgtrickle-prod",  // becomes Debezium source.name
    "topic_template": "{server}.{schema}.{stream_table}",
    "emit_tombstones": true,          // null-value after delete
    "heartbeat_interval_ms": 10000    // emit Debezium heartbeats
  }
}
```

When `wire_format` is omitted, behaviour is identical to today
(`pgtrickle_native`).

### 3.4  Built-in implementations

| Name                  | Decode | Encode | Status      | Notes                                            |
|-----------------------|--------|--------|-------------|--------------------------------------------------|
| `pgtrickle_native`    | ✅     | ✅     | Default     | Current behaviour, refactored behind the trait   |
| `debezium`            | ✅     | ✅     | New (0.31+) | JSON first, Avro/Confluent SR in 0.32, Protobuf in 0.34 |
| `maxwell`             | ✅     | —      | Optional    | Decode-only (no real outbound use case)          |
| `canal`               | ✅     | —      | Optional    | Decode-only                                       |
| `cdc_json` (custom)   | ✅     | ✅     | Optional    | User-supplied JSON path expressions               |

Maxwell, Canal, and the custom format are gated behind cargo features
to keep the default binary small. Debezium support is symmetric
because both directions have real consumers in the ecosystem.

---

## 4  Debezium Specifics — Both Directions

### 4.0  Symmetry table

| Concern                  | Decode (consume)                              | Encode (produce)                              |
|--------------------------|-----------------------------------------------|-----------------------------------------------|
| `op` field               | `c/u/d/r` → INSERT/UPDATE/DELETE/INSERT       | INSERT/UPDATE/DELETE → `c/u/d` (no `r`)       |
| `before` / `after`       | Read both, populate inbox row                 | Emit both from outbox row                     |
| `source` block           | Parse `ts_ms`, `lsn`, `db`, `table`           | Emit `{name, db, schema, table, ts_ms, lsn, version: "pgtrickle/x.y"}` |
| Tombstone (null value)   | Treat as DELETE for previous key (configurable) | Emit after every DELETE if `emit_tombstones`  |
| Topic / key              | Subscribed topic; key from `wire_config.key_strategy` | `topic_template` + key = primary-key columns |
| Schema (JSON)            | Optional embedded `schema` block ignored       | Emit embedded `schema` block per first message per topic, then cache |
| Schema (Avro)            | Fetch from Schema Registry by ID               | Register `<topic>-key` and `<topic>-value` subjects on first emit |
| Heartbeats               | Recognise heartbeat topic, advance offsets     | Emit heartbeats every `heartbeat_interval_ms` so consumers can advance offsets when no data flows |
| Schema-change topic      | Out of scope (separate consumer concern)       | Out of scope (we have no analogue to Debezium's DDL events) |
| Transaction topic        | Out of scope                                   | Out of scope                                  |

### 4.1  Decoder specifics

#### Envelope shape (JSON example)

```json
{
  "schema": { … },
  "payload": {
    "before": { "id": 7, "name": "alice" },
    "after":  { "id": 7, "name": "alice2" },
    "source": { "ts_ms": 1714029482000, "db": "app", "table": "users", "lsn": 12345 },
    "op": "u",
    "ts_ms": 1714029482123,
    "transaction": null
  }
}
```

The decoder maps:

| Debezium field     | InboxRow field                                |
|--------------------|-----------------------------------------------|
| `op = c`           | `INSERT`                                      |
| `op = u`           | `UPDATE` (uses `before` + `after`)            |
| `op = d`           | `DELETE` (uses `before`)                      |
| `op = r` (snapshot)| Configurable: `INSERT` (default) or `UPSERT`  |
| `payload.after`    | `new_row`                                     |
| `payload.before`   | `old_row`                                     |
| `payload.source.ts_ms` | `commit_ts` (UTC ms → microseconds)       |
| `payload.source.lsn` (PG) / `pos` (MySQL) / `change_lsn` (MSSQL) | `source_position` |
| Tombstone (null value) | Drop, or treat as `DELETE` for previous key (configurable) |

#### Type coercions (decode side)

Debezium represents types in several non-obvious ways. Decoder ships
with explicit handlers for the common ones:

| Debezium logical type             | Inbox column type      | Notes                                |
|-----------------------------------|------------------------|--------------------------------------|
| `io.debezium.time.Date`           | `date`                 | int days since epoch                 |
| `io.debezium.time.Timestamp`      | `timestamp`            | int64 ms                             |
| `io.debezium.time.MicroTimestamp` | `timestamp`            | int64 µs                             |
| `io.debezium.time.NanoTimestamp`  | `timestamp`            | int64 ns (truncated to µs)           |
| `io.debezium.time.ZonedTimestamp` | `timestamptz`          | ISO-8601 string                      |
| `io.debezium.data.Decimal`        | `numeric`              | base64 bytes + scale → numeric       |
| `io.debezium.data.VariableScaleDecimal` | `numeric`        | scale per row                        |
| `io.debezium.data.Json`           | `jsonb`                | string                               |
| `io.debezium.data.Uuid`           | `uuid`                 | string                               |
| `io.debezium.data.geometry.*`     | `text` (WKT)           | First cut: stringify; PostGIS deferred |
| MySQL `enum`                      | `text`                 | Debezium emits the label             |
| MySQL `set`                       | `text[]`               | Debezium comma-delimited → array     |

Unknown logical types fall through as `text`; we log a warning the
first time we see one per topic.

### 4.2  Encoder specifics

#### What we emit per pg_trickle outbox event

- **INSERT:** one Debezium message with `op: "c"`, `before: null`,
  `after: <row>`. Key = primary-key columns.
- **UPDATE:** one message with `op: "u"`, `before: <old row>`,
  `after: <new row>`. pg_trickle's outbox already tracks both.
- **DELETE:** one message with `op: "d"`, `before: <old row>`,
  `after: null`. Followed by a tombstone (null-value, same key) when
  `emit_tombstones = true` — required for Kafka log-compacted topics.
- **No `r` (snapshot) events.** pg_trickle outboxes are never
  snapshots; this is documented as a difference vs. real Debezium.

#### Source block we emit

```json
"source": {
  "version": "pgtrickle/0.31",
  "connector": "pgtrickle",
  "name": "<server_name from wire_config>",
  "ts_ms": <commit_ts in ms>,
  "db": "<database>",
  "schema": "<schema>",
  "table": "<stream_table or source_table>",
  "lsn": <pg_lsn or op_seq>,
  "snapshot": "false"
}
```

The `connector: "pgtrickle"` value is the honest signal that we are
**not** Debezium proper. Downstream tools that key off the connector
name (e.g. some sink connectors) need to be configured to accept
our value or treat it generically. Documented prominently.

#### Type coercions (encode side)

Mostly the inverses of the decode mappings. Specifically:

| PG type         | Debezium logical type emitted          |
|-----------------|----------------------------------------|
| `numeric`       | `io.debezium.data.VariableScaleDecimal` |
| `timestamp`     | `io.debezium.time.MicroTimestamp`      |
| `timestamptz`   | `io.debezium.time.ZonedTimestamp`      |
| `date`          | `io.debezium.time.Date`                |
| `uuid`          | `io.debezium.data.Uuid`                |
| `jsonb` / `json`| `io.debezium.data.Json`                |
| `bytea`         | base64 string with `bytes` schema      |
| arrays          | Debezium `array` schema                |
| user types / domains | underlying base type, with a warning the first time |

Geometry / geography are emitted as WKT text in the first cut; PostGIS
binary handling is deferred.

#### Heartbeats

When `heartbeat_interval_ms > 0`, the encoder emits a Debezium
heartbeat message (matching `__debezium-heartbeat.<server_name>`
topic conventions) every interval. This prevents downstream Kafka
Consumer Groups from failing offset commits when a low-traffic
stream table has no events for a while. Cheap; on by default with a
10-second interval.

### 4.3  Schema evolution

Debezium emits schema-change events on a separate topic
(`<server>.schema-changes` for MySQL, in the value's `schema` block for
others).

**Decode side:** `observe_schema()` checks the envelope version and
field set on every message; on incompatible change it returns
`WireError::SchemaIncompatible`, which the relay surfaces as a
pipeline-level alert and halts the consumer. The user resolves by
re-creating or updating the inbox table. We do **not** subscribe to
Debezium schema-change topics.

**Encode side:** when the outbox row's column set changes (a stream
table was altered), the encoder emits a fresh embedded JSON schema on
the next message and, for Avro, registers a new subject version with
the Schema Registry. We do **not** emit on a separate schema-changes
topic — that is a documented gap vs. real Debezium.

Both sides are conservative — we'd rather halt or notify than
corrupt — matching the pg_trickle drift-detection philosophy.

### 4.4  Schema Registry (Avro) — both directions

When `envelope = avro`:

**Decode:**
1. Read the magic byte + 4-byte schema ID from the value.
2. Fetch the schema from the configured Confluent Schema Registry
   (cached by ID, no per-message HTTP).
3. Decode via the `apache-avro` crate.
4. Apply the logical-type mapping.

**Encode:**
1. On first emit per (topic, schema) pair, register `<topic>-key` and
   `<topic>-value` subjects with the Schema Registry; cache the
   returned IDs.
2. Prepend magic byte + 4-byte schema ID to each Avro-encoded value.
3. Re-register on schema change (new column, type widening, etc.).

Schema Registry auth supports basic + bearer; mTLS deferred. Apicurio
compatibility tracked separately for 0.34+.

### 4.5  Transactions

Debezium's transaction-metadata topic (`<server>.transaction`) is
out of scope in both directions for the first cut. The decoder
applies row-by-row; the encoder does not emit transaction begin/end
messages. Users who need atomic multi-row semantics must use the
native pg_trickle envelope or wait for a future
`debezium_transactional` mode.

### 4.6  What we honestly are not

The encoder is a **Debezium-shaped emitter**, not a Debezium
connector. Specifically we do not:

- Implement the Kafka Connect framework (REST API, offsets topic,
  status topic, JMX metrics).
- Subscribe to or emit on the schema-change topic.
- Emit on the transaction-metadata topic.
- Implement Debezium's snapshot lifecycle (no `r` events).
- Set `source.connector` to anything other than `pgtrickle`.

Downstream consumers that depend on those features will not see them.
Documented prominently in the user-facing docs so nobody is surprised.

---

## 5  Phased Roadmap

| Phase | Relay version | Deliverable                                                                 |
|-------|---------------|----------------------------------------------------------------------------|
| 1     | 0.30          | Symmetric `WireFormat` trait (`decode` + `encode`) + `NativePgtrickleEnvelope` impl. Refactor only, no behaviour change. Full test parity with current forward and reverse pipelines. |
| 2     | 0.31          | `Debezium` JSON in **both directions**: decoder (consume) and encoder (produce). Common type mappings (numeric, text, bool, ts, date, jsonb, uuid). Tombstones. Heartbeats. Snapshot-op handling on consume side. |
| 3     | 0.32          | Avro envelope + Confluent Schema Registry client, **both directions**. Decimal/Date/Timestamp logical-type plumbing. Schema registration on encode, schema fetching on decode. |
| 4     | 0.33          | Schema-evolution detection + `WireError::SchemaIncompatible` alerting (decode); schema re-registration on outbox column change (encode). Per-pipeline DLQ for un-encodable / un-decodable messages. |
| 5     | 0.34          | Optional formats behind features: `maxwell` (decode), `canal` (decode), `cdc_json` (both, user-supplied JSONPath). Protobuf envelope for Debezium (both). |

Each phase is independently shippable and gated behind a Cargo
feature so the default binary size stays bounded. Phases 2 and 3
deliver both directions in the same release because the type-mapping
work is the bulk of the cost and is largely shared between encode
and decode.

---

## 6  Compatibility & Versioning

- **Pin Debezium version range per relay release.** Document tested
  range (e.g. "Debezium 2.5–2.7" for relay 0.31). Newer Debezium
  envelopes that introduce breaking field changes are tracked in a
  CHANGELOG entry, with a pinned-version test fixture per supported
  source DB.
- **No breaking changes to the inbox row shape.** The new decoders
  emit the same `InboxRow` the native envelope already produces. All
  downstream pg_trickle code is unaffected.
- **Default behaviour unchanged.** Pipelines without `wire_format`
  config continue to use `pgtrickle_native`.

---

## 7  Testing Strategy

- **Trait-level unit tests.** Each `WireFormat` impl has its own
  fixture-driven test suite (raw bytes → expected `InboxRow`).
- **Debezium golden fixtures.** Capture real envelope samples from
  Debezium connectors against testcontainers (PG, MySQL, MSSQL,
  MongoDB) into `tests/fixtures/debezium/`. Re-run on every relay
  release. Tracks envelope drift between Debezium versions.
- **End-to-end Kafka test.** `testcontainers` spins up Kafka +
  Debezium PG connector + relay; assert that a row inserted on the
  source PG ends up in the relay's inbox table.
- **Schema-evolution tests.** Inject envelope-version mismatches and
  assert the consumer halts with the expected `SchemaIncompatible`
  error rather than writing garbage.
- **Type-coercion property tests.** For each Debezium logical type,
  generate values with `proptest` and assert round-trip into the
  expected PG type.

---

## 8  Risks

| Risk                                             | Mitigation                                                  |
|--------------------------------------------------|-------------------------------------------------------------|
| Debezium envelope changes between minor versions | Pinned-version fixtures + CHANGELOG; supported range stated per release |
| Avro / Schema Registry adds dependency surface   | Gate behind `avro` cargo feature; default build is JSON-only |
| Decimal precision loss (base64 ↔ numeric)        | Use `rust_decimal` or `bigdecimal` on both sides; never f64 |
| Tombstone semantics differ between sources       | Per-source override in `wire_config.tombstone_handling`     |
| Schema-history topic divergence                  | Out of scope; document workaround                           |
| User confusion between native and Debezium modes | Clear docs; `EXPLAIN PIPELINE` SQL function showing decoded/encoded format |
| Downstream tools key off `source.connector`      | Document `pgtrickle` value prominently; provide config knob to spoof `postgresql` if user accepts the dishonesty |
| Encoder schema drift mid-stream                  | Re-register Avro schema on column change; emit fresh JSON schema block; halt with clear error if Schema Registry rejects |
| Forward-direction perf vs native envelope        | Debezium envelope is ~2-4× larger than native; document the cost; users who need throughput stay on native |

---

## 9  Open Questions

1. **Should we ship a `debezium` cargo feature or include in default?**
   Recommendation: separate feature, off by default — keeps minimal
   builds small and avoids pulling in Schema Registry deps.
2. **`source.connector` honesty knob.** Some downstream tools key off
   the value `postgresql` rather than treating any CDC envelope
   uniformly. Do we expose a `wire_config.spoof_connector_name`
   option that lies and says `postgresql`? Pragmatic answer: yes,
   off by default, with a doc warning.
3. **Schema Registry alternatives** (Apicurio, AWS Glue Schema
   Registry)? Defer to 0.34+; protocol is similar but auth differs.
4. **Custom `cdc_json` format scope.** Worth adding for users who
   roll their own? Cheap to ship but invites endless support
   requests. Recommendation: ship it but mark "experimental, no
   stability guarantees."
5. **Forward-direction snapshot events.** Should `CREATE STREAM TABLE`
   followed by initial population emit `op: "r"` snapshot events for
   compatibility with sinks that distinguish snapshot from streaming?
   Defer until a real consumer asks.

---

## 10  Honest Verdict

This is a **clean, contained, high-leverage piece of work** — and
symmetry roughly doubles the leverage for ~30% more cost.

The key insight is that "support Debezium" looked like a giant
feature but factors into "add a wire-format axis + write one
encoder/decoder pair." The transport plumbing is already done. The
inbox/outbox contracts are already done. Observability is already
done. We are bolting one new abstraction onto an existing
well-shaped system.

The strategic payoff is disproportionate **in both directions**:

- **Decode:** every CDC source Debezium supports — Oracle, Db2,
  MongoDB, Cassandra, Vitess, Spanner, Yugabyte, Informix — becomes
  usable with pg_trickle without us writing a single line of
  source-specific code.
- **Encode:** every downstream tool that consumes the Debezium
  envelope — Apache Iceberg sink connectors, Apache Pinot, Apache
  Doris, StarRocks, ClickHouse Kafka engine, Apache Druid, Snowflake
  Kafka Connector, Materialize, ksqlDB, Flink CDC sinks — accepts
  pg_trickle output without learning our native envelope.

The encoder side **quietly resurrects the lakehouse / OLAP
destination story** that was dropped from
[../ecosystem/PLAN_APPLIANCE.md](../ecosystem/PLAN_APPLIANCE.md).
Instead of writing Iceberg, Hudi, Pinot, Doris writers ourselves, we
emit Debezium and let the existing connector ecosystem handle the
destinations.

What we honestly are not: a Debezium connector. We don't speak Kafka
Connect, we don't emit schema-change or transaction topics, our
`source.connector` field says `pgtrickle`. These are documented gaps,
not bugs.

**Recommendation:** ship Phase 1 (symmetric refactor) in relay 0.30
regardless, then commit to Phase 2 (Debezium JSON, both directions)
for 0.31. Re-evaluate after 0.31 ships based on real user demand —
but the symmetry is worth defending against scope-cuts because it is
the whole point of the design.
