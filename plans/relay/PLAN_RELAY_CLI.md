# Relay CLI — Implementation Plan

> **Status:** Implementation Plan (DESIGN REVIEW COMPLETE — 2026-04-19)
> **Created:** 2026-04-18
> **Category:** Tooling — Bidirectional Relay
> **Related:** [PLAN_TRANSACTIONAL_OUTBOX_HELPER.md](../patterns/PLAN_TRANSACTIONAL_OUTBOX_HELPER.md) ·
> [ROADMAP v0.25.0](../../ROADMAP.md#v0250--relay-cli-pgtrickle-relay)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Goals & Non-Goals](#goals--non-goals)
- [Architecture Overview](#architecture-overview)
- [Part A — Core Framework](#part-a--core-framework)
  - [A.1 Crate Structure](#a1-crate-structure)
  - [A.2 CLI Interface](#a2-cli-interface)
  - [A.3 Configuration](#a3-configuration)
  - [A.4 Relay Modes (Forward & Reverse)](#a4-relay-modes-forward--reverse)
  - [A.5 Source Trait](#a5-source-trait)
  - [A.6 Sink Trait](#a6-sink-trait)
  - [A.7 Message Envelope](#a7-message-envelope)
  - [A.8 Outbox Poller (Forward Mode Source)](#a8-outbox-poller-forward-mode-source)
  - [A.9 Payload Handling (Inline + Claim-Check)](#a9-payload-handling-inline--claim-check)
  - [A.10 Consumer Group Integration](#a10-consumer-group-integration)
  - [A.11 Graceful Shutdown & Signal Handling](#a11-graceful-shutdown--signal-handling)
  - [A.12 Observability](#a12-observability)
  - [A.13 Error Handling & Retries](#a13-error-handling--retries)
  - [A.14 Catalog Schema (Config Tables)](#a14-catalog-schema-config-tables)
  - [A.15 Horizontal Scaling & Work Distribution](#a15-horizontal-scaling--work-distribution)
- [Part B — Sink Backends (Forward Mode)](#part-b--sink-backends-forward-mode)
  - [B.1 NATS JetStream](#b1-nats-jetstream)
  - [B.2 HTTP Webhook](#b2-http-webhook)
  - [B.3 Apache Kafka](#b3-apache-kafka)
  - [B.4 stdout / File](#b4-stdout--file)
  - [B.5 Redis Streams](#b5-redis-streams)
  - [B.6 Amazon SQS](#b6-amazon-sqs)
  - [B.7 PostgreSQL (Inbox)](#b7-postgresql-inbox)
  - [B.8 RabbitMQ (AMQP 0-9-1)](#b8-rabbitmq-amqp-0-9-1)
- [Part C — Source Backends (Reverse Mode)](#part-c--source-backends-reverse-mode)
  - [C.1 NATS JetStream Source](#c1-nats-jetstream-source)
  - [C.2 HTTP Webhook Receiver](#c2-http-webhook-receiver)
  - [C.3 Apache Kafka Consumer](#c3-apache-kafka-consumer)
  - [C.4 stdin / File Source](#c4-stdin--file-source)
  - [C.5 Redis Streams Consumer](#c5-redis-streams-consumer)
  - [C.6 Amazon SQS Consumer](#c6-amazon-sqs-consumer)
  - [C.7 RabbitMQ Consumer](#c7-rabbitmq-consumer)
- [Part D — PostgreSQL Inbox Sink (Reverse Mode)](#part-d--postgresql-inbox-sink-reverse-mode)
- [Part E — Testing Strategy](#part-e--testing-strategy)
- [Part F — Documentation & Distribution](#part-f--documentation--distribution)
- [Part G — Implementation Roadmap](#part-g--implementation-roadmap)
- [Open Questions](#open-questions)

---

## Executive Summary

`pgtrickle-relay` is a standalone Rust CLI binary that bridges pg-trickle
outboxes and inboxes with popular messaging systems and destinations. It
ships as a separate crate in the pg-trickle workspace (`pgtrickle-relay/`),
alongside the existing `pgtrickle-tui` crate.

The relay operates in two modes:

- **Forward mode** (`relay forward`): Polls pg-trickle outbox tables and
  publishes deltas to external sinks (NATS, Kafka, webhooks, Redis, SQS,
  RabbitMQ, PostgreSQL inbox, stdout/file).
- **Reverse mode** (`relay reverse`): Consumes messages from external sources
  (NATS, Kafka, webhooks, Redis, SQS, RabbitMQ, stdin/file) and writes them
  into pg-trickle inbox tables.

Both directions share the same Source/Sink trait abstractions, config system,
observability, shutdown logic, and error handling. Each backend implements
both the Source and Sink traits where it makes sense, so the same NATS/Kafka/
Redis/etc. code serves both directions.

**Primary use-cases:**
1. Operators enable an outbox on a stream table, point `pgtrickle-relay
   forward` at it, and deltas flow to Kafka / NATS / webhooks / etc.
   without writing any relay code.
2. Operators point `pgtrickle-relay reverse` at a Kafka topic (or NATS
   subject, or Redis stream, etc.) and messages arrive in a pg-trickle
   inbox table — ready for stream table processing — without writing any
   consumer code.

---

## Goals & Non-Goals

### Goals

- **Bidirectional:** Forward (outbox → sinks) and reverse (sources → inbox)
  in a single binary.
- **Zero custom code required** to relay events in either direction.
- **Symmetric abstractions:** Source trait + Sink trait compose freely;
  any Source can feed any Sink (though the primary use-cases are
  outbox → external and external → inbox).
- **Correct handling of all outbox payload modes:** inline, claim-check,
  and full-refresh fallback — including the combined `full_refresh` +
  `claim_check` case.
- **At-least-once delivery** via consumer groups + broker-side dedup keys
  (forward) and inbox `ON CONFLICT DO NOTHING` (reverse).
- **Multi-instance safe** — multiple relay instances share a consumer group
  (forward) or a broker consumer group (reverse).
- **Observable** — structured logging, Prometheus metrics endpoint, health
  check endpoint.
- **Configurable via file, env vars, and CLI flags** — 12-factor friendly.
- **Small binary, fast startup** — suitable for sidecar containers.

### Non-Goals

- The relay is **not a message broker**. It is a poll-and-forward /
  consume-and-write bridge.
- **Exactly-once delivery is not guaranteed by the relay alone**. Exactly-once
  is achieved by composition (relay dedup keys + broker dedup + inbox
  `ON CONFLICT DO NOTHING`), per the pg-trickle outbox design.
- **Complex routing / transformation / filtering** beyond subject/topic
  mapping is not in scope. Users needing ETL should compose the relay with
  downstream stream processors.
- **No embedded PostgreSQL client library** — uses `tokio-postgres` (same as
  `pgtrickle-tui`).
- **Message format conversion** (e.g. Avro → JSON) is not in scope for
  v0.25.0. The relay passes JSON payloads through as-is.

---

## Architecture Overview

```
                        ┌─────────────────────────────────────────┐
                        │              pgtrickle-relay             │
                        │                                         │
   ┌─────────────┐      │  ┌──────────┐       ┌──────────┐       │      ┌─────────────┐
   │  pg-trickle │      │  │  Source   │       │   Sink   │       │      │  pg-trickle │
   │   outbox    │─────▶│  │  trait    │──────▶│  trait   │──────▶│─────▶│   (ext.)    │
   │             │      │  └──────────┘       └──────────┘       │      │  Kafka/NATS │
   └─────────────┘      │                                         │      │  Redis/etc. │
                        │       FORWARD MODE                      │      └─────────────┘
                        │                                         │
                        ├─────────────────────────────────────────┤
                        │                                         │
   ┌─────────────┐      │  ┌──────────┐       ┌──────────┐       │      ┌─────────────┐
   │  Kafka/NATS │      │  │  Source   │       │   Sink   │       │      │  pg-trickle │
   │  Redis/SQS  │─────▶│  │  trait    │──────▶│  trait   │──────▶│─────▶│   inbox     │
   │  Webhooks   │      │  └──────────┘       └──────────┘       │      │             │
   └─────────────┘      │                                         │      └─────────────┘
                        │       REVERSE MODE                      │
                        │                                         │
                        │  ┌──────────────────────────────────┐   │
                        │  │  Coordinator (1 pinned PG conn)  │   │
                        │  │  advisory locks, hot-reload      │   │
                        │  └────────────────┬─────────────────┘   │
                        │     ┌─────────────▼─────────────────┐   │
                        │     │  Worker pool (N tokio tasks)   │   │
                        │     │  each with its own PG conn     │   │
                        │     └───────────────────────────────┘   │
                        │  ┌──────────────────────────────────┐   │
                        │  │  Shared: metrics, health, config, │   │
                        │  │  shutdown, error handling, retries │   │
                        │  └──────────────────────────────────┘   │
                        └─────────────────────────────────────────┘
```

**Key insight:** The outbox poller is just another Source implementation.
The inbox writer is just another Sink implementation. External systems
(NATS, Kafka, etc.) implement *both* Source and Sink. This lets users
compose any source with any sink, even though the two primary modes are
the most common.

**Horizontal scaling:** Multiple relay pods can run simultaneously as a
Kubernetes Deployment (not StatefulSet). Each pod runs one coordinator
that races for pipeline ownership using PostgreSQL advisory locks. Work
is automatically distributed across pods with no external coordinator or
ZooKeeper-style consensus.

---

## Part A — Core Framework

### A.1 Crate Structure

A new workspace member at `pgtrickle-relay/`:

```
pgtrickle-relay/
├── Cargo.toml
├── src/
│   ├── main.rs           # Entry point, CLI parsing, signal handling
│   ├── cli.rs            # clap derive definitions
│   ├── config.rs         # TOML / YAML / JSON + env + CLI merging
│   ├── coordinator.rs    # Advisory-lock coordinator, worker dispatch, hot-reload
│   ├── error.rs          # RelayError enum
│   ├── envelope.rs       # RelayMessage envelope type
│   ├── metrics.rs        # Prometheus metrics + health endpoint
│   ├── source/
│   │   ├── mod.rs        # Source trait definition
│   │   ├── outbox.rs     # pg-trickle outbox poller (forward mode)
│   │   ├── nats.rs       # NATS JetStream consumer
│   │   ├── webhook.rs    # HTTP webhook receiver (axum listener)
│   │   ├── kafka.rs      # Apache Kafka consumer
│   │   ├── stdin.rs      # stdin / file reader
│   │   ├── redis.rs      # Redis Streams consumer
│   │   ├── sqs.rs        # Amazon SQS consumer
│   │   └── rabbitmq.rs   # RabbitMQ AMQP consumer
│   ├── sink/
│   │   ├── mod.rs        # Sink trait definition
│   │   ├── inbox.rs      # pg-trickle inbox writer (reverse mode)
│   │   ├── nats.rs       # NATS JetStream publisher
│   │   ├── webhook.rs    # HTTP webhook POST client
│   │   ├── kafka.rs      # Apache Kafka producer
│   │   ├── stdout.rs     # stdout / file writer
│   │   ├── redis.rs      # Redis Streams XADD
│   │   ├── sqs.rs        # Amazon SQS sender
│   │   ├── pg_outbox.rs  # PostgreSQL outbox/inbox on remote PG
│   │   └── rabbitmq.rs   # RabbitMQ AMQP publisher
│   └── transforms.rs     # Subject/topic templating, key extraction
└── tests/
    ├── common/
    │   └── mod.rs         # Test helpers, Testcontainers setup
    ├── forward/           # Forward-mode tests
    │   ├── poller_tests.rs
    │   ├── nats_tests.rs
    │   ├── webhook_tests.rs
    │   └── kafka_tests.rs
    └── reverse/           # Reverse-mode tests
        ├── nats_inbox_tests.rs
        ├── kafka_inbox_tests.rs
        ├── webhook_inbox_tests.rs
        └── redis_inbox_tests.rs
```

#### Cargo.toml — Feature Flags

Each backend is gated behind a Cargo feature so users compile only what
they need. A default feature set covers the most common backends.

```toml
[package]
name = "pgtrickle-relay"
version = "0.25.0"
edition = "2024"

[[bin]]
name = "pgtrickle-relay"
path = "src/main.rs"

[features]
default = ["nats", "webhook", "kafka", "stdout"]
nats     = ["dep:async-nats"]
webhook  = ["dep:reqwest"]           # also enables axum webhook receiver
kafka    = ["dep:rdkafka"]
stdout   = []                        # no extra deps
redis    = ["dep:redis"]
sqs      = ["dep:aws-sdk-sqs"]
pg-inbox = []                        # uses tokio-postgres (already a dep)
rabbitmq = ["dep:lapin"]

[dependencies]
# Core
clap = { version = "4", features = ["derive", "env"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "signal", "time", "sync"] }
tokio-postgres = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
toml = "1.1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4", "serde"] }

# Metrics / health + webhook receiver
axum = "0.8"
prometheus = "0.14"

# Sinks + Sources (optional)
async-nats = { version = "0.40", optional = true }
reqwest = { version = "0.12", features = ["json"], optional = true }
rdkafka = { version = "0.37", features = ["cmake-build"], optional = true }
redis = { version = "0.28", features = ["tokio-comp", "streams"], optional = true }
aws-sdk-sqs = { version = "1", optional = true }
lapin = { version = "2", optional = true }
```

### A.2 CLI Interface

```
pgtrickle-relay [OPTIONS]

OPTIONS:
      --postgres-url <URL>      PostgreSQL connection string (required)
                                [env: PGTRICKLE_RELAY_POSTGRES_URL]
      --metrics-addr <ADDR>     Prometheus metrics + health endpoint
                                (default: 0.0.0.0:9090)
      --log-format <FMT>        Log format: text, json (default: text)
      --log-level <LEVEL>       Log level (default: info)
  -V, --version                 Print version
  -h, --help                    Print help
```

Pipeline management is done entirely via SQL — there are no CLI subcommands
for managing pipelines. See [A.3 Configuration](#a3-configuration) for SQL
examples.

### A.3 Configuration

The relay has no config file and no environment variables for pipeline
definitions. All pipeline config lives in the database (see [A.14](#a14-catalog-schema-config-tables)).
The only required input at startup is the PostgreSQL connection URL:

> **Note on the TOML blocks in Parts B and C:** The `[sink.nats]`,
> `[source.kafka]`, etc. blocks shown throughout this document are **examples
> of the `config` JSONB column format** displayed in human-readable TOML
> syntax for clarity. The relay binary does not read a TOML config file.
> All pipeline configuration is stored in and read from the
> `pgtrickle.relay_outbox_config` and `pgtrickle.relay_inbox_config`
> database tables as plain JSON objects.

```bash
# Minimal startup
pgtrickle-relay --postgres-url postgres://relay:password@localhost/mydb

# Or via the single supported env var (bootstrap only)
export PGTRICKLE_RELAY_POSTGRES_URL=postgres://relay:password@localhost/mydb
pgtrickle-relay
```

On startup the relay:
1. Connects to PostgreSQL
2. Queries `pgtrickle.relay_outbox_config` and `pgtrickle.relay_inbox_config`
   for all `enabled = true` rows
3. Spawns one tokio task per pipeline
4. Subscribes to `LISTEN pgtrickle_relay_config` for hot-reload

If either table does not exist the relay exits with a clear error.

#### Example Pipeline Inserts

Pipelines are managed entirely via SQL:

```sql
-- Forward: outbox → NATS
INSERT INTO pgtrickle.relay_outbox_config (name, config) VALUES (
    'orders-to-nats',
    '{"source_type": "outbox", "source": {"outbox": "order_events", "group": "order-publisher"},
      "sink_type":   "nats",   "sink":   {"url": "nats://localhost:4222"}}'
);

-- Reverse: Kafka → inbox
INSERT INTO pgtrickle.relay_inbox_config (name, config) VALUES (
    'kafka-to-orders',
    '{"source_type": "kafka",    "source": {"brokers": "localhost:9092", "topic": "orders"},
      "sink_type":   "pg-inbox", "sink":   {"inbox_table": "order_inbox"}}'
);
```

### A.4 Relay Modes (Forward & Reverse)

The relay has two primary modes that compose Sources and Sinks:

| Mode | Source | Sink | Primary use-case |
|------|--------|------|------------------|
| **Forward** | pg-trickle outbox | NATS / Kafka / webhook / Redis / SQS / RabbitMQ / PG / stdout | Publish outbox deltas to external systems |
| **Reverse** | NATS / Kafka / webhook / Redis / SQS / RabbitMQ / stdin | pg-trickle inbox | Consume external events into inbox for stream processing |

#### Process topology

Each relay process contains a **coordinator** and a **worker pool**:

```
Process (pod)
├── Coordinator task  (1 long-lived, pinned PG connection)
│   ├── On startup: load enabled pipelines from relay_*_config
│   ├── Every tick: pg_try_advisory_lock(hashtext(group_id), hashtext(pipeline_id))
│   │   for each unowned pipeline
│   ├── On lock acquired: read consumer_offsets, dispatch to worker pool
│   ├── On lock lost (coordinator conn drop or another pod stole it):
│   │   cancel the corresponding worker task
│   └── LISTEN pgtrickle_relay_config for hot-reload (enable/disable/update)
│
└── Worker pool (N tokio tasks, bounded channel from coordinator)
    ├── Worker 1 → pipeline A  (own PG conn for data reads + offset writes)
    ├── Worker 2 → pipeline B  (own PG conn for data reads + offset writes)
    └── Worker N → idle / available
```

The coordinator's PG connection **must not be pooled or returned between
operations** — it is the lease heartbeat for all advisory locks held by this
process. If the connection drops, every lock is released and other pods
immediately race to acquire them.

Lock key uses the two-argument form so different relay groups use isolated
namespaces and never collide with each other:

```sql
SELECT pg_try_advisory_lock(
    hashtext(relay_group_id),   -- namespace per group/deployment
    hashtext(pipeline_id)       -- individual pipeline within the group
                                -- (pipeline_id is TEXT — cannot cast to int)
)
```

> **Hash collision risk:** `hashtext()` maps unbounded TEXT to int4 (2.1 billion
> values). For deployments with up to a few thousand pipelines the birthday-paradox
> collision probability is negligible (< 0.0001%). For larger deployments use the
> single-argument `pg_try_advisory_lock(bigint)` form with a collision-resistant
> key: `('x' || left(md5(relay_group_id || ':' || pipeline_id), 16))::bit(64)::bigint`.
> Pipeline names are validated to be unique in `relay_outbox_config` and
> `relay_inbox_config`, so combining group_id and pipeline_id into a single
> MD5-derived int8 eliminates the namespace collision risk entirely.

#### Core worker loop (per pipeline)

```rust
async fn worker_loop(
    pipeline: PipelineConfig,
    source: &dyn Source,
    sink: &dyn Sink,
    db: &Client,
    shutdown: CancellationToken,
) -> Result<(), RelayError> {
    // Exponential backoff on empty polls to avoid busy-spinning against a
    // quiet outbox. Resets to min_sleep on any non-empty batch.
    let mut empty_poll_sleep = Duration::from_millis(pipeline.poll_interval_ms);
    const MAX_SLEEP: Duration = Duration::from_secs(5);

    loop {
        tokio::select! {
            batch = source.poll() => {
                let batch = batch?;
                if batch.is_empty() {
                    // Back off rather than tight-loop against an empty outbox.
                    tokio::time::sleep(empty_poll_sleep).await;
                    empty_poll_sleep = (empty_poll_sleep * 2).min(MAX_SLEEP);
                    continue;
                }
                // Non-empty batch — reset backoff.
                empty_poll_sleep = Duration::from_millis(pipeline.poll_interval_ms);
                sink.publish(&batch).await?;
                source.acknowledge(&batch).await?;
                // Write durable offset atomically after each batch
                update_consumer_offset(&db, &pipeline, batch.last_id()).await?;
            }
            _ = shutdown.cancelled() => {
                break;
            }
        }
    }
    Ok(())
}
```

### A.5 Source Trait

```rust
#[async_trait]
pub trait Source: Send + Sync {
    /// Human-readable source name for logs and metrics.
    fn name(&self) -> &str;

    /// Establish connection to the source. Called on startup and reconnect.
    async fn connect(&mut self) -> Result<(), RelayError>;

    /// Poll for the next batch of messages. Returns empty vec if none
    /// available (caller should sleep before retrying).
    async fn poll(&self) -> Result<Vec<RelayMessage>, RelayError>;

    /// Acknowledge successful processing of a batch. For outbox sources
    /// this commits the offset; for broker sources this acks the messages.
    async fn acknowledge(&self, batch: &[RelayMessage]) -> Result<(), RelayError>;

    /// Check if the source connection is healthy.
    async fn is_healthy(&self) -> bool;

    /// Graceful close.
    async fn close(&mut self) -> Result<(), RelayError>;
}
```

### A.6 Sink Trait

```rust
#[async_trait]
pub trait Sink: Send + Sync {
    /// Human-readable sink name for logs and metrics.
    fn name(&self) -> &str;

    /// Establish connection to the sink. Called on startup and reconnect.
    async fn connect(&mut self) -> Result<(), RelayError>;

    /// Publish a batch of messages. The sink must handle dedup keys.
    /// Returns the number of messages successfully published.
    async fn publish(&self, batch: &[RelayMessage]) -> Result<usize, RelayError>;

    /// Check if the sink connection is healthy.
    async fn is_healthy(&self) -> bool;

    /// Graceful close.
    async fn close(&mut self) -> Result<(), RelayError>;
}
```

### A.7 Message Envelope

Both directions use a common message envelope:

```rust
/// Unified message envelope used by both forward and reverse relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayMessage {
    /// Dedup key for idempotent delivery.
    /// Forward: "{outbox}:{outbox_id}:{row_index}"
    /// Reverse: source-specific (e.g. Kafka offset, NATS msg ID)
    pub dedup_key: String,

    /// Resolved subject/topic (forward) or inbox event_type (reverse).
    pub subject: String,

    /// The row/event payload as JSON.
    pub payload: serde_json::Value,

    /// Operation: "insert", "delete", or "event" (reverse generic).
    pub op: String,

    /// Whether this batch is a full-refresh snapshot (forward only).
    pub is_full_refresh: bool,

    /// pg-trickle outbox metadata (forward only, None in reverse).
    pub outbox_id: Option<i64>,
    pub refresh_id: Option<Uuid>,

    /// Source-specific metadata for acknowledgement (not serialized).
    #[serde(skip)]
    pub ack_token: Option<AckToken>,
}

/// Opaque token that the Source uses to acknowledge a message.
/// Each source backend stores whatever it needs here.
#[derive(Debug, Clone)]
pub enum AckToken {
    OutboxOffset(i64),
    KafkaOffset { partition: i32, offset: i64 },
    NatsAckHandle(async_nats::Message),
    SqsReceiptHandle(String),
    RabbitMqDeliveryTag(u64),
    RedisStreamId(String),
    None,
}
```

### A.8 Outbox Poller (Forward Mode Source)

The outbox poller implements the `Source` trait. It operates in two modes:

#### Simple Mode (no consumer group)

Used when `--group` is not specified. On startup the worker reads the last
committed offset from `relay_consumer_offsets` (durable across restarts and
pod failures) and polls from that position forward.

```rust
async fn poll_simple(
    db: &Client,
    pipeline: &PipelineConfig,
    batch_size: i64,
) -> Result<Vec<RelayMessage>, RelayError> {
    // Read durable offset — survives pod restarts and coordinator failover
    let last_offset: i64 = db.query_opt(
        "SELECT last_change_id FROM pgtrickle.relay_consumer_offsets
         WHERE relay_group_id = $1 AND pipeline_id = $2",
        &[&pipeline.group_id, &pipeline.id],
    ).await?.map(|r| r.get(0)).unwrap_or(0i64);

    let rows = db.query(
        &format!(
            "SELECT id, payload FROM pgtrickle.{} WHERE id > $1 ORDER BY id LIMIT $2",
            pipeline.outbox_name  // validated at startup against catalog
        ),
        &[&last_offset, &batch_size],
    ).await?;

    let mut messages = Vec::new();
    for row in &rows {
        let id: i64 = row.get("id");
        let payload: serde_json::Value = row.get("payload");
        let batch = decode_payload(&payload, db, &pipeline.outbox_name, id).await?;
        messages.extend(batch_to_messages(&batch));
    }
    Ok(messages)
}
```

After each batch is published and acknowledged, the worker writes the offset
atomically:

```rust
async fn update_consumer_offset(
    db: &Client,
    pipeline: &PipelineConfig,
    last_id: i64,
) -> Result<(), RelayError> {
    db.execute(
        "INSERT INTO pgtrickle.relay_consumer_offsets
             (relay_group_id, pipeline_id, last_change_id, worker_id, updated_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (relay_group_id, pipeline_id)
         DO UPDATE SET last_change_id = EXCLUDED.last_change_id,
                       worker_id      = EXCLUDED.worker_id,
                       updated_at     = EXCLUDED.updated_at",
        &[&pipeline.group_id, &pipeline.id, &last_id, &pipeline.worker_id],
    ).await?;
    Ok(())
}
```

`worker_id` is set to `"<pod_name>:<thread_index>"` at startup, where `<pod_name>` is read from the `HOSTNAME` environment variable (or `os::hostname()` if unset); `<thread_index>` is the worker thread index (e.g. `"relay-6b7f9-0:3"`). Useful for debugging stalls in multi-pod deployments.

#### Consumer Group Mode

> **Simple mode vs. consumer group mode:** The relay supports two offset-tracking
> strategies for forward mode. **Simple mode** (default) uses the relay's own
> `pgtrickle.relay_consumer_offsets` table — a lightweight `(relay_group_id,
> pipeline_id, last_change_id)` row updated atomically after each batch. This
> is sufficient for single-relay deployments and requires no v0.24.0 consumer
> group infrastructure; the extension's retention drain checks `relay_consumer_offsets`
> to avoid data loss. **Consumer group mode** delegates offset tracking to
> the v0.24.0 `poll_outbox()` / `commit_offset()` API, which uses the extension's
> `pgt_consumer_offsets` table and adds visibility timeouts, lease management,
> multi-relay coordination, and lag monitoring via `consumer_lag()`. Use consumer
> group mode when multiple relay instances share a single outbox, or when you need
> the operational tooling (heartbeats, dead consumer reaping, `seek_offset()` replay).
> The pipeline's `config` JSONB controls which mode is used: include a `"group"`
> key in the source config to activate consumer group mode.

Used when `--group` is specified. Delegates coordination to pg-trickle's
built-in consumer group SQL functions.

```rust
async fn poll_group(
    db: &Client,
    group: &str,
    consumer_id: &str,
    stream_table_name: &str,  // stream table name for decode_payload / outbox_rows_consumed
    batch_size: i32,
    visibility_seconds: i32,
) -> Result<Vec<RelayMessage>, RelayError> {
    let rows = db.query(
        "SELECT * FROM pgtrickle.poll_outbox($1, $2, $3, $4)",
        &[&group, &consumer_id, &batch_size, &visibility_seconds],
    ).await?;

    if rows.is_empty() {
        // Heartbeat even when idle
        db.execute(
            "SELECT pgtrickle.consumer_heartbeat($1, $2)",
            &[&group, &consumer_id],
        ).await?;
        return Ok(vec![]);
    }

    let mut messages = Vec::new();
    for row in &rows {
        let id: i64 = row.get("id");
        let payload: serde_json::Value = row.get("payload");
        let batch = decode_payload(&payload, db, stream_table_name, id).await?;
        messages.extend(batch_to_messages(&batch));
    }
    Ok(messages)
}
```

#### Lease Renewal for Slow Sinks

If a batch takes longer to publish than `visibility_seconds`, the poller
spawns a background task that periodically calls `extend_lease()`:

```rust
let lease_guard = spawn_lease_renewer(db, group, consumer_id, visibility_seconds);
// ... process batch ...
lease_guard.cancel(); // stop renewing after commit
```

The renewer calls `extend_lease()` at `visibility_seconds / 2` intervals.

### A.9 Payload Handling (Inline + Claim-Check)

The payload decoder handles all four outbox modes:

| `claim_check` | `full_refresh` | Path |
|----------------|----------------|------|
| `false`        | `false`        | Inline differential — rows in `payload.inserted` / `payload.deleted` |
| `false`        | `true`         | Inline full refresh — all current rows in `payload.inserted`, apply upsert semantics |
| `true`         | `false`        | Claim-check differential — cursor-fetch from `outbox_delta_rows_<st>` |
| `true`         | `true`         | Claim-check full refresh — cursor-fetch + upsert semantics |

```rust
#[derive(Debug)]
struct OutboxBatch {
    outbox_id: i64,
    refresh_id: Uuid,
    is_full_refresh: bool,
    inserted: Vec<serde_json::Value>,
    deleted: Vec<serde_json::Value>,
}

async fn decode_payload(
    payload: &serde_json::Value,
    db: &Client,
    stream_table_name: &str,   // pg_trickle stream table name (NOT the outbox table name)
    outbox_id: i64,
) -> Result<OutboxBatch, RelayError> {
    let v = payload["v"].as_i64().unwrap_or(0);
    if v != 1 {
        return Err(RelayError::UnsupportedPayloadVersion(v));
    }

    let is_full_refresh = payload.get("full_refresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_claim_check = payload.get("claim_check")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_claim_check {
        // Cursor-fetch from companion table in bounded batches.
        // outbox_name is derived from stream_table_name ("outbox_" + stream_table_name).
        let outbox_name = format!("outbox_{}", stream_table_name);
        let (inserted, deleted) = fetch_claim_check_rows(
            db, &outbox_name, outbox_id
        ).await?;

        // Signal consumption complete.
        // outbox_rows_consumed() takes the STREAM TABLE name, not the outbox table name.
        db.execute(
            "SELECT pgtrickle.outbox_rows_consumed($1, $2)",
            &[&stream_table_name, &outbox_id],
        ).await?;

        Ok(OutboxBatch {
            outbox_id,
            refresh_id: parse_uuid(payload),
            is_full_refresh,
            inserted,
            deleted,
        })
    } else {
        // Inline — rows are in the payload itself
        Ok(OutboxBatch {
            outbox_id,
            refresh_id: parse_uuid(payload),
            is_full_refresh,
            inserted: extract_array(payload, "inserted"),
            deleted: extract_array(payload, "deleted"),
        })
    }
}

async fn fetch_claim_check_rows(
    db: &Client,
    outbox_name: &str,
    outbox_id: i64,
) -> Result<(Vec<Value>, Vec<Value>), RelayError> {
    // Use a server-side cursor in bounded batches to keep relay heap bounded
    // regardless of claim-check delta size. This is the key memory-safety
    // guarantee of the claim-check path — never buffer the full delta in RAM.
    const FETCH_BATCH: i64 = 1000;
    let delta_table = format!("pgtrickle.outbox_delta_rows_{}", outbox_name);

    let cursor_name = format!("relay_cc_{}_{}", outbox_id, uuid::Uuid::new_v4().simple());
    // Embed outbox_id as a literal — batch_execute does not support parameter
    // binding, so $1 would not be substituted. outbox_id is an i64 from the
    // database, not user input, so embedding it directly is safe.
    db.batch_execute(&format!(
        "DECLARE {cursor} NO SCROLL CURSOR FOR \
         SELECT op, payload FROM {table} WHERE outbox_id = {oid} ORDER BY row_num",
        cursor = cursor_name,
        table = delta_table,  // validated at startup against catalog
        oid = outbox_id,
    )).await?;

    let mut inserted = Vec::new();
    let mut deleted = Vec::new();
    loop {
        let rows = db.query(
            &format!("FETCH {n} FROM {cursor}", n = FETCH_BATCH, cursor = cursor_name),
            &[],
        ).await?;
        let done = rows.len() < FETCH_BATCH as usize;
        for row in rows {
            let op: &str = row.get("op");
            let payload: serde_json::Value = row.get("payload");
            match op {
                "I" => inserted.push(payload),
                "D" => deleted.push(payload),
                _ => tracing::warn!(op, "unknown delta op"),
            }
        }
        if done { break; }
    }
    db.batch_execute(&format!("CLOSE {cursor}", cursor = cursor_name)).await?;
    Ok((inserted, deleted))
}
```

### A.10 Consumer Group Integration

When `--group` is specified in forward mode, the relay:

1. **Startup:** Calls `create_consumer_group()` (idempotent) to ensure the
   group exists.
2. **Poll loop:** Uses `poll_outbox()` + `commit_offset()` as described in A.8.
3. **Heartbeat:** Sends `consumer_heartbeat()` every 10 seconds via a
   background task, independent of the poll loop.
4. **Lease renewal:** Calls `extend_lease()` if batch processing takes
   longer than `visibility_seconds / 2`.
5. **Shutdown:** On SIGTERM/SIGINT, finishes the current batch, commits the
   offset, then exits.

### A.11 Graceful Shutdown & Signal Handling

```rust
let shutdown = tokio::signal::ctrl_c();
let sigterm = async {
    tokio::signal::unix::signal(SignalKind::terminate())
        .expect("register SIGTERM")
        .recv()
        .await;
};

tokio::select! {
    _ = relay_loop => {},
    _ = shutdown => {
        tracing::info!("SIGINT received, finishing current batch...");
    }
    _ = sigterm => {
        tracing::info!("SIGTERM received, finishing current batch...");
    }
}
// Drain in-flight batch, acknowledge, close connections
```

The shutdown sequence:

1. Stop polling/consuming.
2. Wait for the in-flight batch to finish publishing (up to a configurable
   `shutdown_timeout_seconds`, default 30).
3. Acknowledge the completed batch (commit outbox offset or ack broker messages).
4. Close sink, source, and database connections.
5. Exit with code 0.

If the in-flight batch doesn't complete within the timeout, the relay exits
with code 1. Unacknowledged messages will be redelivered by the source.

### A.12 Observability

#### Structured Logging

- `tracing` + `tracing-subscriber` with `env-filter`.
- `--log-format json` for structured JSON logs (default in container mode).
- Key span fields: `mode`, `source`, `sink`, `outbox`, `inbox`, `group`.

#### Prometheus Metrics

Exposed via an HTTP endpoint (default `:9090/metrics`):

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `relay_polls_total` | counter | `mode`, `source` | Total poll attempts |
| `relay_messages_published_total` | counter | `mode`, `sink` | Messages published to sink |
| `relay_messages_received_total` | counter | `mode`, `source` | Messages received from source |
| `relay_batches_committed_total` | counter | `mode` | Batches acknowledged |
| `relay_batch_publish_duration_seconds` | histogram | `mode`, `sink` | Time to publish a batch |
| `relay_poll_duration_seconds` | histogram | `mode`, `source` | Time for a poll round-trip |
| `relay_claim_check_fetches_total` | counter | — | Claim-check cursor fetches (forward) |
| `relay_full_refresh_batches_total` | counter | — | Full-refresh batches received (forward) |
| `relay_inbox_writes_total` | counter | `inbox` | Rows written to inbox (reverse) |
| `relay_inbox_dedup_skips_total` | counter | `inbox` | Rows skipped via ON CONFLICT (reverse) |
| `relay_errors_total` | counter | `mode`, `kind` | Errors by category |
| `relay_last_committed_offset` | gauge | `mode` | Last committed offset |
| `relay_consumer_lag_rows` | gauge | `mode` | Rows behind latest |
| `relay_source_connected` | gauge | — | 1 if source connection is healthy |
| `relay_sink_connected` | gauge | — | 1 if sink connection is healthy |

#### Health Endpoint

`GET :9090/health` returns:

```json
{
  "status": "healthy",
  "mode": "forward",
  "source": "outbox",
  "sink": "nats",
  "postgres": "connected",
  "source_healthy": true,
  "sink_healthy": true,
  "last_poll_at": "2026-04-18T12:00:00Z",
  "last_commit_at": "2026-04-18T12:00:00Z",
  "lag_rows": 42
}
```

Returns HTTP 200 if healthy, 503 if degraded (no poll in last 60 s or
source/sink disconnected). Suitable for Kubernetes liveness/readiness probes.

### A.13 Error Handling & Retries

| Error class | Behaviour |
|-------------|-----------|
| PostgreSQL connection lost | Reconnect with exponential backoff (1 s → 2 s → 4 s → … → 60 s cap). |
| Source connection lost | Reconnect with exponential backoff. Pause relay until reconnected. |
| Sink connection lost | Reconnect with exponential backoff. Pause relay until reconnected. |
| Sink publish failure (transient) | Retry the batch up to 3 times with backoff. Do not acknowledge. |
| Sink publish failure (permanent, e.g. 400) | Log error, skip the message, emit `relay_errors_total{kind="sink_permanent"}`. Advance offset to avoid poison-pill blocking. |
| Payload decode error | Log error, skip the message. Emit `relay_errors_total{kind="decode"}`. |
| Claim-check fetch failure | Retry up to 3 times. If permanent, log + skip + emit error metric. |
| Inbox write failure (transient) | Retry with backoff. Do not ack source message. |
| Inbox write failure (permanent) | Log error, skip message, emit `relay_errors_total{kind="inbox_permanent"}`. |

All retries use jittered exponential backoff to avoid thundering herds.

---

### A.14 Catalog Schema (Config Tables)

Pipeline definitions live in two tables created by the pg-trickle extension
migration (same schema as other catalog tables). All backend-specific settings
go in the `config` JSONB column; validation happens in Rust at load time.

```sql
-- Forward pipelines: outbox → sink
CREATE TABLE pgtrickle.relay_outbox_config (
    name     TEXT PRIMARY KEY,
    enabled  BOOLEAN NOT NULL DEFAULT true,
    config   JSONB NOT NULL  -- {source_type, source, sink_type, sink}
);

-- Reverse pipelines: source → inbox
CREATE TABLE pgtrickle.relay_inbox_config (
    name     TEXT PRIMARY KEY,
    enabled  BOOLEAN NOT NULL DEFAULT true,
    config   JSONB NOT NULL  -- {source_type, source, sink_type, sink}
);

-- Durable per-pipeline offset tracking.
-- Written atomically after each committed batch so that any pod can take
-- over from exactly the right position when the coordinator lock moves.
-- relay_group_id  = namespaces separate relay deployments (e.g. "orders-kafka").
-- pipeline_id     = unique pipeline row PK (name from relay_*_config).
-- last_change_id  = last outbox row id successfully published (simple mode)
--                   or last broker offset (reverse mode).
-- worker_id       = "<pod-name>:<thread>" for operational diagnostics.
CREATE TABLE pgtrickle.relay_consumer_offsets (
    relay_group_id  TEXT        NOT NULL,
    pipeline_id     TEXT        NOT NULL,
    last_change_id  BIGINT      NOT NULL DEFAULT 0,
    worker_id       TEXT,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (relay_group_id, pipeline_id)
);

-- One shared trigger function; TG_TABLE_NAME identifies the direction
CREATE OR REPLACE FUNCTION pgtrickle.relay_config_notify()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify(
        'pgtrickle_relay_config',
        json_build_object(
            'direction', TG_TABLE_NAME,
            'event',     TG_OP,
            'name',      COALESCE(NEW.name, OLD.name),
            'enabled',   COALESCE(NEW.enabled, OLD.enabled)
        )::text
    );
    RETURN NULL;
END;
$$;

CREATE TRIGGER relay_outbox_config_notify
    AFTER INSERT OR UPDATE OR DELETE ON pgtrickle.relay_outbox_config
    FOR EACH ROW EXECUTE FUNCTION pgtrickle.relay_config_notify();

CREATE TRIGGER relay_inbox_config_notify
    AFTER INSERT OR UPDATE OR DELETE ON pgtrickle.relay_inbox_config
    FOR EACH ROW EXECUTE FUNCTION pgtrickle.relay_config_notify();
```

The relay subscribes to `LISTEN pgtrickle_relay_config` after startup:
- `INSERT` with `enabled = true` → start a new pipeline task
- `UPDATE` → gracefully restart the named pipeline with the new config
- `DELETE` or `UPDATE` with `enabled = false` → gracefully stop the pipeline

Credentials must not be stored as plaintext in `config` JSONB. Use env var
references (e.g. `{"password_env": "KAFKA_PASSWORD"}`) resolved at runtime.

#### SQL API Functions

Seven PL/pgSQL wrapper functions provide a public API for managing relay
pipelines without requiring direct table access. Validation of required JSONB
keys and direction constraints happens inside the functions, not in application
code. Each function validates that the required top-level keys (`source_type`,
`sink_type`, `source`, `sink`) are present in the JSONB config and raises
`relay.invalid_config` if not — preventing silent misconfigurations that would
only surface at relay startup.

```sql
-- Upsert a forward pipeline (outbox → sink).
-- source_type must be 'outbox'. Raises relay.invalid_config on bad JSON shape.
-- enabled defaults to true; pass false to insert in disabled state.
SELECT pgtrickle.set_relay_outbox(
    'orders-to-nats',
    '{"source_type":"outbox","source":{"outbox":"orders","group":"order-relay"},
      "sink_type":"nats","sink":{"url":"nats://localhost:4222"}}'
);

-- Upsert a reverse pipeline (source → inbox).
-- sink_type must be 'pg-inbox'. Raises relay.invalid_config on bad JSON shape.
-- enabled defaults to true; pass false to insert in disabled state.
SELECT pgtrickle.set_relay_inbox(
    'kafka-to-orders',
    '{"source_type":"kafka","source":{"brokers":"localhost:9092","topic":"orders"},
      "sink_type":"pg-inbox","sink":{"inbox_table":"order_inbox"}}'
);

-- Enable / disable a pipeline by name (searches both tables).
SELECT pgtrickle.enable_relay('orders-to-nats');
SELECT pgtrickle.disable_relay('kafka-to-orders');

-- Delete a pipeline by name (searches both tables).
SELECT pgtrickle.delete_relay('orders-to-nats');

-- Fetch config for a single named pipeline.
SELECT * FROM pgtrickle.get_relay_config('orders-to-nats');
-- Returns: name TEXT, direction TEXT, enabled BOOLEAN, config JSONB

-- List all pipelines (both directions).
SELECT * FROM pgtrickle.list_relay_configs();
-- Returns rows for every entry in relay_outbox_config and relay_inbox_config.
```

All functions are schema-qualified and raise exceptions with clear messages on
missing pipelines or invalid configs. The underlying table names are not part
of the public API.

#### Access Control

Direct table access is revoked; all mutations go through the API functions.
The migration includes:

```sql
-- Revoke direct table access from the relay role
REVOKE ALL ON pgtrickle.relay_outbox_config    FROM pgtrickle_relay;
REVOKE ALL ON pgtrickle.relay_inbox_config     FROM pgtrickle_relay;
REVOKE ALL ON pgtrickle.relay_consumer_offsets FROM pgtrickle_relay;

-- Grant execute on the API functions only
GRANT EXECUTE ON FUNCTION pgtrickle.set_relay_outbox(TEXT, JSONB)  TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.set_relay_inbox(TEXT, JSONB)   TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.enable_relay(TEXT)                      TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.disable_relay(TEXT)                     TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.delete_relay(TEXT)                      TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.get_relay_config(TEXT)                  TO pgtrickle_relay;
GRANT EXECUTE ON FUNCTION pgtrickle.list_relay_configs()                    TO pgtrickle_relay;
```

The functions run with `SECURITY DEFINER` so they can access the underlying
tables on behalf of any caller granted `EXECUTE`, without exposing the tables
directly. Superusers and the `pgtrickle` role (extension owner) retain full
table access for administrative purposes.

---

### A.15 Horizontal Scaling & Work Distribution

The relay is designed to scale horizontally on Kubernetes with **zero external
coordination**, using PostgreSQL advisory locks as the exclusive work lease.

#### Distributed ownership via advisory locks

Each pod runs a **coordinator task** that continuously races to acquire
advisory locks for all enabled pipelines:

```sql
SELECT pg_try_advisory_lock(
    hashtext(relay_group_id),   -- namespace per relay group/deployment
    hashtext(pipeline_id)       -- individual pipeline within group
                                -- (pipeline_id is TEXT — cannot cast to int)
);
```

> **Hash collision note:** See [A.4](#a4-relay-modes-forward--reverse) for
> the recommended int8 key alternative for large deployments.

- If the lock is acquired → this pod owns that pipeline; spawn a worker task.
- If the lock is already held → another pod owns it; skip.
- If the pod crashes or its coordinator connection drops → all locks are
  automatically released, and other pods immediately race to acquire them.

Lock acquisition is **serialized by PostgreSQL**, so split-brain (two pods
processing the same pipeline simultaneously) is impossible.

**Design rationale:** Session-level advisory locks are:
- Atomic and scoped to a PG connection (no external etcd or ZK).
- Automatically released on disconnect or connection failure.
- Idiomatic for PostgreSQL HA patterns (pgBouncer, Patroni, logical replication).
- Zero operational overhead — no new infrastructure to deploy.

#### Multi-pod failover

When Pod A crashes:

1. Pod A's coordinator connection (which held all locks) is severed by PostgreSQL.
2. All advisory locks held by Pod A are instantly released.
3. Pod B's coordinator (running in the background on a timer) immediately
   detects and acquires the released locks.
4. Pod B dispatches workers to the affected pipelines.
5. Workers read `relay_consumer_offsets` to resume from the last committed offset
   (written atomically after each batch).

**Result:** Zero data loss, no manual intervention, no wait for heartbeat timeout.

#### Stateless pod design

Each pod stores **no pipeline state** — all state lives in the database:

| State | Location | Durability |
|-------|----------|------------|
| Pipeline config | `relay_*_config` tables | Durable |
| Ownership | `pg_locks` (advisory locks) | Session-scoped |
| Offsets | `relay_consumer_offsets` | Durable (atomically updated per batch) |
| Metrics | In-process | Ephemeral (regenerated on pod restart) |

Pods are identical and can be shut down / restarted / scaled in/out at any time.

#### Configuration

On process startup:
1. Connect to PostgreSQL.
2. Load all rows from `relay_outbox_config` and `relay_inbox_config` where
   `enabled = true`.
3. Assign a unique `relay_group_id` (derived from hostname + deploy version).
4. Start the coordinator task.

To enable/disable a pipeline without restarting the pod:
1. Update the config row: `UPDATE relay_outbox_config SET enabled = false ...`
2. The database trigger fires `NOTIFY pgtrickle_relay_config`.
3. All pods' coordinators receive the notification and reload the config.
4. Pods automatically stop or start workers as needed.

#### Performance implications

- **Poll overhead:** Advisory lock acquisition is ~1ms per tick per pod (negligible).
- **Offset writes:** `INSERT ... ON CONFLICT DO UPDATE` (1-2ms per batch).
- **Concurrency:** Multiple pods polling the same source is **safe** because
  the outbox source tracks offsets per relay group, not globally.

#### Connection Pool Sizing

Each relay pod consumes exactly:
- **1 coordinator connection** (pinned, holds all advisory locks for this pod).
- **1 connection per active worker** (up to `max_workers_per_pod` pipelines).

With N pods and M pipelines distributed across them:

```
total_PG_connections = N_pods × (1 + min(M_pipelines / N_pods, max_workers_per_pod))
```

Example: 3 pods × (1 coordinator + 4 worker pipelines each) = **15 connections**.

The startup config option `max_workers_per_pod` (default: `10`) caps the
worker count per pod. Tune it so `N_pods × (1 + max_workers_per_pod) +
other_app_connections` stays well below `max_connections` on the PostgreSQL
server. Leave headroom for admin connections (at least 5 via
`superuser_reserved_connections`).

If `max_connections` is approached, reduce `max_workers_per_pod`, increase
replicas, or front the relay with PgBouncer in transaction mode (the relay's
worker connections are compatible with transaction pooling; the coordinator
connection must **not** be pooled — it holds session-level advisory locks).

```yaml
# Environment-variable tuning knobs for the relay process:
env:
  - name: PGTRICKLE_RELAY_MAX_WORKERS_PER_POD
    value: "10"         # caps per-pod worker connections (default: 10)
  - name: PGTRICKLE_RELAY_COORDINATOR_TICK_MS
    value: "500"        # advisory-lock retry interval (default: 500 ms)
```

#### Example Kubernetes Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: pgtrickle-relay
spec:
  replicas: 3                           # horizontal scale
  selector:
    matchLabels:
      app: pgtrickle-relay
  template:
    metadata:
      labels:
        app: pgtrickle-relay
    spec:
      containers:
      - name: relay
        image: grove/pgtrickle-relay:0.25.0
        env:
        - name: PGTRICKLE_RELAY_POSTGRES_URL
          valueFrom:
            secretKeyRef:
              name: pg-credentials
              key: relay-url
        ports:
        - name: metrics
          containerPort: 9090
        livenessProbe:
          httpGet:
            path: /health
            port: metrics
          initialDelaySeconds: 5
          periodSeconds: 10
        readinessProbe:
          httpGet:
            path: /health
            port: metrics
          initialDelaySeconds: 5
          periodSeconds: 10
        resources:
          requests:
            cpu: 100m
            memory: 256Mi
          limits:
            cpu: 500m
            memory: 512Mi
```

Scaling up → new pods join and acquire available locks.
Scaling down → evicted pods release locks; remaining pods acquire them.
Rolling updates → replace pods one at a time; work rebalances automatically.

---

## Part B — Sink Backends (Forward Mode)

These backends implement the `Sink` trait and are used primarily in forward
mode (outbox → external). They can also be used in reverse mode when
composing arbitrary source → sink pipelines.

### B.1 NATS JetStream

**Crate:** `async-nats`

```toml
[sink.nats]
url = "nats://localhost:4222"
# credentials_file = "/etc/nats/creds"
# tls_ca = "/etc/nats/ca.pem"
subject_template = "pgtrickle.{stream_table}.{op}"
# Publish options:
# ack_timeout_ms = 5000
# max_in_flight = 256          # concurrent unacked publishes
```

**Dedup:** Uses `Nats-Msg-Id` header set to `{dedup_key}`.
JetStream deduplicates within the configured dedup window.

**Full-refresh handling:** Publishes with header `Pgtrickle-Full-Refresh: true`
so consumers can detect snapshot events.

### B.2 HTTP Webhook

**Crate:** `reqwest`

```toml
[sink.webhook]
url = "https://api.example.com/events"
method = "POST"                             # POST (default) or PUT
# headers:
#   Authorization = "Bearer token123"
#   X-Custom = "value"
# timeout_ms = 10000
# batch_mode = true                         # send entire batch as one POST
# retry_on_status = [429, 500, 502, 503, 504]
# tls_ca = "/etc/ssl/custom-ca.pem"
```

**Batch mode (default: true):** Sends the entire batch as a single POST body.

**Per-event mode (`batch_mode = false`):** One HTTP request per event row.
Slower but compatible with endpoints that expect single-event payloads.

**Dedup:** Sends `Idempotency-Key` header with the dedup key.

**Full-refresh handling:** Sends `X-Pgtrickle-Full-Refresh: true` header.

### B.3 Apache Kafka

**Crate:** `rdkafka` (librdkafka wrapper)

```toml
[sink.kafka]
brokers = "localhost:9092"
topic = "pgtrickle-events"
# topic_template = "pgtrickle.{stream_table}"
# key_template = "{op}:{outbox_id}"          # Kafka record key
# acks = "all"                               # all | 1 | 0
# compression = "lz4"                        # none | gzip | snappy | lz4 | zstd
# linger_ms = 5
# batch_num_messages = 1000
# security_protocol = "SASL_SSL"
# sasl_mechanism = "PLAIN"
# sasl_username = "user"
# sasl_password = "password"
# Additional librdkafka config:
# [sink.kafka.rdkafka]
# "message.max.bytes" = "10485760"
```

**Dedup:** Uses the dedup key as the Kafka record key + enables idempotent
producer (`enable.idempotence = true`).

**Full-refresh:** Sets Kafka header `pgtrickle-full-refresh: true`.

### B.4 stdout / File

No external dependencies.

```toml
[sink.stdout]
format = "jsonl"            # jsonl (default) | json_pretty | csv
# output = "/var/log/relay/events.jsonl"   # file path; omit for stdout
# rotate_bytes = 104857600  # 100 MiB log rotation (file mode only)
```

Useful for:
- Debugging and development
- Piping to `jq`, `tee`, log collectors
- Integration testing

### B.5 Redis Streams

**Crate:** `redis` with `streams` feature

```toml
[sink.redis]
url = "redis://localhost:6379"
stream_key = "pgtrickle:{stream_table}"
# maxlen = 100000             # MAXLEN ~ approximate trimming
# password = "secret"
# tls = false
```

**Dedup:** Uses the dedup key as the Redis Stream entry ID field
(`pgt_dedup_key`). Consumers use consumer groups on the Redis side for
exactly-once processing.

### B.6 Amazon SQS

**Crate:** `aws-sdk-sqs`

```toml
[sink.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/my-queue"
# region = "us-east-1"       # default: from env/config
# message_group_id = "pgtrickle"   # for FIFO queues
# batch_send = true          # use SendMessageBatch (up to 10 per call)
```

**Dedup:** For FIFO queues, uses `MessageDeduplicationId` set to the dedup
key. For standard queues, dedup is the consumer's responsibility.

### B.7 PostgreSQL (Inbox)

Uses `tokio-postgres` (already a dependency).

```toml
[sink.pg-inbox]
url = "postgres://user:password@other-host/other_db"
inbox_table = "my_inbox"
# schema = "public"
# on_conflict = "DO NOTHING"   # default: idempotent via event_id PK
```

Inserts events into a pg-trickle inbox (or any table with compatible
schema) on a different PostgreSQL instance. Enables cross-database /
cross-service event propagation using pg-trickle on both sides.

```sql
INSERT INTO my_inbox (event_id, event_type, payload, received_at)
VALUES ($1, $2, $3, now())
ON CONFLICT (event_id) DO NOTHING;
```

### B.8 RabbitMQ (AMQP 0-9-1)

**Crate:** `lapin`

**Protocol note:** Targets AMQP 0-9-1, the native RabbitMQ protocol and
production standard. AMQP 1.0 support can be added in Phase 2 (see Phase 2
plan) to reach Azure Service Bus, Apache Qpid, and other AMQP 1.0 brokers.

```toml
[sink.rabbitmq]
url = "amqp://guest:guest@localhost:5672"
exchange = "pgtrickle"
routing_key_template = "{stream_table}.{op}"
# exchange_type = "topic"     # topic | direct | fanout
# declare_exchange = true     # auto-declare on connect
# mandatory = true            # require at least one queue binding
# tls = false
```

**Dedup:** Sets `message-id` AMQP property to the dedup key. Consumer-side
dedup required (RabbitMQ does not deduplicate natively).

---

## Part C — Source Backends (Reverse Mode)

These backends implement the `Source` trait and are used in reverse mode
(external system → inbox). They consume messages from external brokers /
transports and feed them into the relay loop for writing to a pg-trickle
inbox table.

### C.1 NATS JetStream Source

**Crate:** `async-nats`

```toml
[source.nats]
url = "nats://localhost:4222"
subject = "external.events.>"       # NATS subject to subscribe to
# durable_name = "pgtrickle-inbox"  # durable consumer for persistence
# deliver_policy = "all"            # all | last | new | by_start_time
# ack_wait_seconds = 30
# max_deliver = 5                   # max redelivery attempts
# credentials_file = "/etc/nats/creds"
# tls_ca = "/etc/nats/ca.pem"
```

**Consumption model:** Creates a JetStream pull consumer (durable if
`durable_name` set). Messages are acked only after successful inbox write.

**Dedup key:** Uses `Nats-Msg-Id` header if present; otherwise generates
`nats:{stream}:{seq}` from the stream sequence number.

**Event type mapping:** Subject is used as the inbox `event_type`.
Configurable via `[routing]` overrides.

### C.2 HTTP Webhook Receiver

**Crate:** `axum` (already a dependency for metrics)

```toml
[source.webhook]
listen_addr = "0.0.0.0:8080"       # separate from metrics endpoint
path = "/inbox"                     # POST path to receive events
# auth_header = "X-Webhook-Secret"
# auth_value = "my-secret-token"
# max_body_bytes = 10485760         # 10 MiB
```

**Consumption model:** Runs an HTTP server that accepts POST requests.
Each request body is parsed as JSON and written to the inbox. Returns 200
only after successful inbox write (synchronous acknowledgement).

**Dedup key:** Uses `Idempotency-Key` header if present; otherwise
generates a UUID v4.

**Event type mapping:** Uses the URL path suffix or a configurable header
(e.g. `X-Event-Type`).

### C.3 Apache Kafka Consumer

**Crate:** `rdkafka`

```toml
[source.kafka]
brokers = "localhost:9092"
topic = "external-events"
group_id = "pgtrickle-inbox-writer"
# auto_offset_reset = "earliest"    # earliest | latest
# security_protocol = "SASL_SSL"
# sasl_mechanism = "PLAIN"
# sasl_username = "user"
# sasl_password = "password"
# Additional librdkafka config:
# [source.kafka.rdkafka]
# "session.timeout.ms" = "30000"
```

**Consumption model:** Uses rdkafka's `StreamConsumer` with manual offset
commits. Offsets are committed only after successful inbox write.

**Dedup key:** Uses Kafka record key if present; otherwise generates
`kafka:{topic}:{partition}:{offset}`.

**Event type mapping:** Uses Kafka header `event-type` if present, or the
topic name.

### C.4 stdin / File Source

No external dependencies.

```toml
[source.stdin]
format = "jsonl"            # jsonl (default) | json_pretty
# input = "/var/spool/events.jsonl"   # file path; omit for stdin
# follow = false             # tail -f mode for files
```

Useful for:
- Replaying captured events into an inbox
- Piping from other tools: `cat events.jsonl | pgtrickle-relay reverse --source stdin`
- Integration testing

**Dedup key:** Uses `dedup_key` field from JSON payload if present;
otherwise generates a UUID v4.

### C.5 Redis Streams Consumer

**Crate:** `redis` with `streams` feature

```toml
[source.redis]
url = "redis://localhost:6379"
stream_key = "external:events"
group_name = "pgtrickle-inbox"
consumer_name = "relay-1"           # default: hostname
# start_id = "0"                    # "0" for all history, "$" for new only
# block_ms = 5000
# count = 100                       # messages per XREADGROUP call
# password = "secret"
# tls = false
```

**Consumption model:** Uses Redis consumer groups (`XREADGROUP`).
Messages are `XACK`ed only after successful inbox write.

**Dedup key:** Uses `pgt_dedup_key` field if present; otherwise uses the
Redis stream entry ID.

### C.6 Amazon SQS Consumer

**Crate:** `aws-sdk-sqs`

```toml
[source.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/my-queue"
# region = "us-east-1"
# max_messages = 10                  # messages per ReceiveMessage call
# wait_time_seconds = 20            # long polling
# visibility_timeout = 30
```

**Consumption model:** Uses `ReceiveMessage` with long polling. Messages
are deleted (`DeleteMessage`) only after successful inbox write. Visibility
timeout provides implicit retry on failure.

**Dedup key:** Uses `MessageDeduplicationId` (FIFO) or `MessageId`
(standard).

### C.7 RabbitMQ Consumer

**Crate:** `lapin`

**Protocol note:** Targets AMQP 0-9-1 (RabbitMQ native). AMQP 1.0 support
for Azure Service Bus and other AMQP 1.0 brokers planned for Phase 2.

```toml
[source.rabbitmq]
url = "amqp://guest:guest@localhost:5672"
queue = "inbox-events"
# consumer_tag = "pgtrickle-relay"
# prefetch_count = 100
# declare_queue = true               # auto-declare on connect
# auto_ack = false                   # manual ack (default)
# tls = false
```

**Consumption model:** Uses `basic_consume` with manual acknowledgement.
Messages are acked only after successful inbox write. `basic_nack` with
requeue on transient failure.

**Dedup key:** Uses `message-id` AMQP property if present; otherwise
generates a UUID v4.

---

## Part D — PostgreSQL Inbox Sink (Reverse Mode)

The inbox sink is the primary Sink for reverse mode. It writes messages
into a pg-trickle inbox table via `tokio-postgres`.

```toml
[reverse]
source = "kafka"
inbox = "external_events"
inbox_schema = "public"

# Optional: map source fields to inbox columns
[reverse.mapping]
event_id = "dedup_key"         # default: dedup_key
event_type = "subject"         # default: subject
payload = "payload"            # default: payload
```

#### Inbox Table Schema

The relay expects the inbox table to have at minimum:

```sql
CREATE TABLE external_events (
    event_id    TEXT PRIMARY KEY,       -- populated from dedup_key
    event_type  TEXT NOT NULL,          -- populated from subject
    payload     JSONB NOT NULL,         -- populated from message payload
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Additional columns are ignored. The relay uses `ON CONFLICT (event_id)
DO NOTHING` for idempotent writes.

#### Batch Insert

For throughput, the inbox sink batches inserts:

```rust
async fn publish(&self, batch: &[RelayMessage]) -> Result<usize, RelayError> {
    let mut written = 0;
    // Use a transaction for atomicity
    let tx = self.db.transaction().await?;

    for msg in batch {
        let result = tx.execute(
            "INSERT INTO $inbox (event_id, event_type, payload, received_at) \
             VALUES ($1, $2, $3, now()) \
             ON CONFLICT (event_id) DO NOTHING",
            &[&msg.dedup_key, &msg.subject, &msg.payload],
        ).await?;
        if result > 0 { written += 1; }
    }

    tx.commit().await?;
    Ok(written)
}
```

#### Dedup Semantics

- `ON CONFLICT (event_id) DO NOTHING` ensures that duplicate messages
  (e.g. from at-least-once source delivery) are silently dropped.
- The `relay_inbox_dedup_skips_total` metric tracks how often this happens.

---

## Part E — Testing Strategy

### E.1 Unit Tests

- Payload decoder: all 4 outbox modes (inline, claim-check, full-refresh, combined).
- Config merging: CLI > env > file (TOML / YAML / JSON) > defaults; all three file formats produce identical results.
- Subject/topic templating with variables.
- Dedup key generation (both forward and reverse).
- Retry backoff calculation.
- RelayMessage envelope serialization round-trip.
- Source → Sink composition (mock source + mock sink).

### E.2 Forward Mode Integration Tests (Testcontainers)

Each test spins up PostgreSQL + the target sink via Testcontainers:

| Test | Containers | Validates |
|------|-----------|-----------|
| NATS relay E2E | postgres + nats | Poll → publish → JetStream consume; verify dedup |
| Kafka relay E2E | postgres + kafka (redpanda) | Poll → produce → consume; verify partition key |
| Webhook relay E2E | postgres + wiremock | Poll → POST → verify request body + headers |
| Redis relay E2E | postgres + redis | Poll → XADD → XRANGE; verify stream entries |
| PG inbox relay E2E | postgres (source) + postgres (target) | Poll → INSERT → verify inbox rows |
| Consumer group E2E | postgres + nats + 2 relay instances | Verify no duplicates, crash recovery |
| Claim-check E2E | postgres + nats | Large delta → cursor fetch → publish all rows |
| Full-refresh E2E | postgres + nats | AUTO fallback → full-refresh header set |

### E.3 Reverse Mode Integration Tests (Testcontainers)

| Test | Containers | Validates |
|------|-----------|-----------|
| NATS → inbox E2E | nats + postgres | Publish to subject → relay consumes → verify inbox row |
| Kafka → inbox E2E | kafka (redpanda) + postgres | Produce record → relay consumes → verify inbox row |
| Webhook → inbox E2E | postgres | POST to relay webhook → verify inbox row; verify 200 response |
| Redis → inbox E2E | redis + postgres | XADD → relay reads → verify inbox row |
| stdin → inbox E2E | postgres | Pipe JSONL → relay reads → verify inbox row |
| SQS → inbox E2E | localstack + postgres | Send message → relay receives → verify inbox row |
| RabbitMQ → inbox E2E | rabbitmq + postgres | Publish to queue → relay consumes → verify inbox row |
| Reverse dedup E2E | nats + postgres | Publish same message twice → verify only 1 inbox row |
| Reverse crash recovery | kafka + postgres | Kill relay mid-batch → restart → verify no lost messages |

### E.4 Benchmark

- Forward throughput: relay 100K inline outbox rows to NATS; measure events/sec.
- Forward throughput: relay 100K claim-check outbox rows; measure events/sec.
- Reverse throughput: consume 100K Kafka messages → inbox; measure events/sec.
- Latency: p50/p95/p99 poll-to-publish for a single event (both modes).
- Memory: verify bounded memory during claim-check fetch (no full delta
  buffering).
- **Consumer group contention:** `commit_offset()` latency with 10 concurrent relay
  workers all committing to the same group simultaneously — must be < 10 ms p99.
- **Relay crash + re-poll:** measure time from visibility timeout expiry to
  second relay picking up and re-publishing the batch; must be < `visibility_seconds + 2 s`.
- **Advisory lock storm:** 10 pods all starting simultaneously; measure lock
  acquisition time until all pipelines are owned; must complete within 5 s.

### E.5 End-to-End Latency Benchmark

Measures the **full path latency** that users cite in production comparisons:

```
source DML commit → CDC trigger → refresh MERGE → outbox INSERT + NOTIFY
  → relay poll (or LISTEN wake) → broker publish → consumer receipt
```

| Relay wake mode | p50 target | p95 target |
|-----------------|-----------|----------|
| Polling only (v0.24.0, `visibility_seconds = 30s`) | < 1.5 s  | < 2.5 s  |
| LISTEN/NOTIFY wake (v0.25.0 relay) | < 100 ms | < 250 ms |

Add as `benches/e2e_relay_latency.rs` (Testcontainers, `tokio`, requires
`--features e2e-bench`). Run manually; not in Criterion regression gate
because results are environment-sensitive.

---

## Part F — Documentation & Distribution

### F.1 Documentation

| Document | Content |
|----------|---------|
| `pgtrickle-relay/README.md` | Quick start, installation, forward & reverse basic usage |
| `docs/RELAY.md` | Comprehensive guide: all backends, config reference, deployment patterns |
| `docs/SQL_REFERENCE.md` | Updated with relay-related SQL functions |
| `docs/PATTERNS.md` | "Relay" section with worked examples per backend (forward + reverse) |

### F.2 Distribution

| Channel | Artifact |
|---------|----------|
| GitHub Releases | Pre-built binaries (Linux amd64/arm64, macOS amd64/arm64) |
| Docker Hub | `grove/pgtrickle-relay:0.25.0` — minimal distroless image |
| Cargo | `cargo install pgtrickle-relay` |
| Homebrew | `brew install grove/tap/pgtrickle-relay` |

#### Docker Image

```dockerfile
FROM rust:1.85-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --bin pgtrickle-relay \
    --features default

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /src/target/release/pgtrickle-relay /usr/local/bin/
ENTRYPOINT ["pgtrickle-relay"]
```

#### Kubernetes Sidecar Pattern (Forward)

All pipeline definitions live in the database (see [A.14](#a14-catalog-schema-config-tables)).
The only env var required at startup is `PGTRICKLE_RELAY_POSTGRES_URL`.
Configure pipelines once via `pgtrickle-relay config set` or SQL;
the running relay picks up changes automatically via `LISTEN/NOTIFY`.

```yaml
containers:
  - name: app
    image: myapp:latest
  - name: relay
    image: grove/pgtrickle-relay:0.25.0
    env:
      - name: PGTRICKLE_RELAY_POSTGRES_URL
        valueFrom:
          secretKeyRef:
            name: pg-credentials
            key: relay-url
    ports:
      - name: metrics
        containerPort: 9090
    livenessProbe:
      httpGet:
        path: /health
        port: 9090
    readinessProbe:
      httpGet:
        path: /health
        port: 9090
```

Pipeline setup (run once, not per-pod):

```bash
# Forward: outbox → NATS
pgtrickle-relay config set outbox orders-to-nats \
  --config '{"source_type":"outbox","source":{"outbox":"order_events","group":"order-publisher"},"sink_type":"nats","sink":{"url":"nats://nats:4222"}}'
```

#### Kubernetes Sidecar Pattern (Reverse)

Same pattern — the single `PGTRICKLE_RELAY_POSTGRES_URL` env var is sufficient.
All source/inbox config lives in `pgtrickle.relay_inbox_config`.

```yaml
containers:
  - name: app
    image: myapp:latest
  - name: relay
    image: grove/pgtrickle-relay:0.25.0
    env:
      - name: PGTRICKLE_RELAY_POSTGRES_URL
        valueFrom:
          secretKeyRef:
            name: pg-credentials
            key: relay-url
    ports:
      - name: metrics
        containerPort: 9090
    livenessProbe:
      httpGet:
        path: /health
        port: 9090
```

Pipeline setup (run once, not per-pod):

```bash
# Reverse: Kafka → inbox
pgtrickle-relay config set inbox kafka-to-orders \
  --config '{"source_type":"kafka","source":{"brokers":"kafka:9092","topic":"orders"},"sink_type":"pg-inbox","sink":{"inbox_table":"order_inbox"}}'
```

---

## Part G — Implementation Roadmap

### Phase 1 — Core Framework + Forward Tier 1 Sinks (12 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-CAT | **Catalog schema + SQL API + offset tracking.** `sql/pg_trickle--0.24.0--0.25.0.sql`: create `relay_outbox_config`, `relay_inbox_config`, and `relay_consumer_offsets` tables; shared `relay_config_notify()` trigger; 7 SQL wrapper functions. | 0.5d |
| RELAY-1 | Crate scaffold, CLI parsing (`--postgres-url`, `--metrics-addr`, `--log-format`, `--log-level`), DB bootstrap (load tables, LISTEN/NOTIFY), coordinator task setup, error types, RelayMessage envelope | 2d |
| RELAY-2 | Source + Sink traits, coordinator loop (advisory locks), worker pool dispatch, cancellation token plumbing | 1.5d |
| RELAY-3 | Outbox poller source (simple mode with durable offsets + consumer group mode) | 2.5d |
| RELAY-4 | Payload decoder (inline + claim-check + full-refresh) | 1d |
| RELAY-5 | Sink: stdout/file backend | 0.5d |
| RELAY-6 | Sink: NATS JetStream | 1d |
| RELAY-7 | Sink: HTTP webhook | 1d |
| RELAY-8 | Sink: Apache Kafka | 1.5d |
| RELAY-9 | Metrics endpoint + health check + signal handling + graceful shutdown | 1d |

### Phase 2 — Forward Tier 2 Sinks (5 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-10 | Sink: Redis Streams | 1d |
| RELAY-11 | Sink: Amazon SQS | 1d |
| RELAY-12 | Sink: PostgreSQL inbox (remote) | 1d |
| RELAY-13 | Sink: RabbitMQ AMQP | 1d |
| RELAY-14 | Subject/topic routing templates + key extraction | 1d |

### Phase 3 — Reverse Mode Sources + Inbox Sink (10 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-22 | Inbox sink (pg-trickle inbox writer with batch insert + ON CONFLICT dedup) | 1.5d |
| RELAY-23 | Source: NATS JetStream consumer (durable pull consumer, ack after inbox write) | 1d |
| RELAY-24 | Source: Apache Kafka consumer (manual offset commit after inbox write) | 1.5d |
| RELAY-25 | Source: HTTP webhook receiver (axum server, synchronous ack) | 1d |
| RELAY-26 | Source: Redis Streams consumer (XREADGROUP + XACK) | 1d |
| RELAY-27 | Source: Amazon SQS consumer (ReceiveMessage + DeleteMessage) | 1d |
| RELAY-28 | Source: RabbitMQ consumer (basic_consume + manual ack) | 1d |
| RELAY-29 | Source: stdin/file reader | 0.5d |
| RELAY-30 | Reverse-mode dedup key mapping + event type extraction config | 0.5d |

### Phase 4 — Testing & Polish (7 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-15 | Unit tests (payload, config, routing, retries, envelope, mock source/sink) | 1d |
| RELAY-16 | Forward integration tests (NATS, Kafka, webhook, Redis, PG inbox) | 2d |
| RELAY-17 | Forward consumer group E2E (multi-instance, crash recovery) | 1d |
| RELAY-31 | Reverse integration tests (NATS→inbox, Kafka→inbox, webhook→inbox, Redis→inbox, SQS→inbox, RabbitMQ→inbox, stdin→inbox) | 2d |
| RELAY-32 | Reverse dedup + crash recovery E2E | 0.5d |
| RELAY-18 | Benchmarks (forward + reverse throughput, latency, memory) | 0.5d |

### Phase 5 — Documentation & Distribution (2.5 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-19 | Documentation (README, docs/RELAY.md, PATTERNS.md — covering both modes) | 1d |
| RELAY-20 | Dockerfile + GitHub Actions CI for binary builds | 1d |
| RELAY-21 | Release automation (GitHub Releases, Docker Hub, Homebrew) | 0.5d |

> **Total: ~36.5 days solo / ~23 days with two developers**
> (Phases 1–2 forward sinks and Phase 3 reverse sources can be parallelised.
> Requires v0.24.0 outbox + consumer groups for full forward E2E testing;
> reverse mode only needs inbox table schema.)

### Dependencies

- **Forward mode requires v0.24.0 outbox (Part A)** — the relay polls
  `pgtrickle.outbox_<st>` and reads claim-check rows from
  `pgtrickle.outbox_delta_rows_<st>`.
- **Forward consumer group mode requires v0.24.0 Part B** — `poll_outbox()`,
  `commit_offset()`, `consumer_heartbeat()`, `extend_lease()`.
- **Reverse mode requires only an inbox table** — no dependency on outbox
  implementation. Can be developed and tested independently.
- **Both modes can be developed in parallel** with the outbox implementation:
  mock the outbox table schema in integration tests until the real extension
  is ready.

---

## Open Questions

| # | Question | Options | Recommendation |
|---|----------|---------|----------------|
| 1 | Should the relay support multiple pipelines in one process? | (a) One pipeline per process (simpler), (b) Multi-pipeline coordinator (more throughput per pod) | **(b)** — one process runs a coordinator that manages all enabled pipelines via advisory locks. Multiple pods scale horizontally with zero external coordination. This is more resource-efficient and operationally simpler. See [A.15](#a15-horizontal-scaling--work-distribution). |
| 2 | Should we support custom transforms (e.g. JMESPath, JSONata)? | (a) No transforms in v0.25.0, (b) JMESPath filter | **(a)** — out of scope. The stream table query itself is the transform layer. |
| 3 | Dead-letter queue for the relay? | (a) Skip poison events + log, (b) DLQ table in PostgreSQL | **(a)** for v0.25.0. Log + metric is sufficient. DLQ can be added later. |
| 4 | Should `pgtrickle-relay` be a workspace member or a separate repo? | (a) Workspace member (shared CI, version lock), (b) Separate repo | **(a)** — workspace member alongside `pgtrickle-tui`. Shared version, single release. |
| 5 | Should NATS Micro integration be included for service discovery? | (a) Yes, (b) No | **(b)** for initial release. Can be added if demand exists. |
| 6 | Google Cloud Pub/Sub as a backend? | (a) v0.25.0, (b) Post-v0.25.0 | **(b)** — add post-launch based on demand. |
| 7 | Azure Service Bus as a backend? | (a) v0.25.0, (b) Post-v0.25.0 | **(b)** — add post-launch based on demand. |
| 8 | Should reverse mode support arbitrary Sink (not just inbox)? | (a) Only inbox sink in v0.25.0, (b) Any sink | **(a)** — primary use-case is source→inbox. Arbitrary source→sink is architecturally possible but not a priority. |
| 9 | Message format conversion (e.g. Avro, Protobuf)? | (a) JSON only in v0.25.0, (b) Schema registry integration | **(a)** — JSON only. Schema-aware deserialization is a post-v0.25.0 feature. |
