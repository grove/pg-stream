# Relay Service

`pgtrickle-relay` is a lightweight sidecar process that bridges pg_trickle
outbox and inbox tables with external messaging systems — NATS, Kafka, Redis
Streams, HTTP webhooks, AWS SQS, and RabbitMQ.

Rather than writing custom polling loops in each of your services, you run one
(or a few) relay instances and let them handle all the I/O with external
brokers. Your application only deals with PostgreSQL.

> **Available since v0.29.0**

---

## When to use the relay

| Scenario | Use relay? |
|----------|-----------|
| Your services need to publish events to Kafka / NATS | Yes |
| External services send you events via Kafka / NATS | Yes |
| You want exactly-once delivery without managing broker offsets yourself | Yes |
| Your services already talk directly to Kafka / NATS | Optional |
| Your entire stack is PostgreSQL with no external broker | No — use outbox/inbox directly |

The relay is optional. The [Transactional Outbox](OUTBOX.md) and
[Transactional Inbox](INBOX.md) work perfectly without it — the relay just
handles the transport layer.

---

## How it works

```
┌──────────────────────────────────────────────────────────┐
│ PostgreSQL (pg_trickle extension)                        │
│                                                          │
│  stream table → outbox table ──→ [Relay: forward]  ──→ Kafka / NATS / …
│                                                          │
│  inbox table  ←─────────────── [Relay: reverse]  ←── Kafka / NATS / …
└──────────────────────────────────────────────────────────┘
                   pgtrickle-relay binary
```

**Forward pipeline**: polls an outbox consumer group and publishes each delta
batch to an external sink (Kafka topic, NATS subject, SQS queue, webhook URL,
…).

**Reverse pipeline**: consumes messages from an external source (Kafka topic,
NATS consumer, …) and inserts them into a pg_trickle inbox table with
`ON CONFLICT DO NOTHING` for idempotency.

Both directions are configured entirely via SQL — no YAML config files on disk.

---

## Installation

### Docker (recommended)

```bash
docker pull ghcr.io/grove/pgtrickle-relay:0.29.0
```

### Build from source

```bash
# Default features (NATS + webhook + stdout)
cargo install --path pgtrickle-relay

# All backends
cargo install --path pgtrickle-relay \
  --features nats,webhook,kafka,redis,sqs,rabbitmq,pg-inbox,stdout
```

---

## Quick start (5 minutes)

This example publishes order changes to NATS JetStream.

### Step 1: Set up the stream table and outbox

```sql
-- Create a stream table (skip if you already have one)
SELECT pgtrickle.create_stream_table(
    'public.order_summary',
    $$SELECT customer_id, COUNT(*) AS order_count, SUM(amount) AS total
      FROM orders GROUP BY customer_id$$
);

-- Enable the outbox
SELECT pgtrickle.enable_outbox('public.order_summary');
```

### Step 2: Register the relay pipeline

```sql
SELECT pgtrickle.set_relay_outbox(
    'orders-to-nats',
    config => '{
        "stream_table": "order_summary",
        "sink_type": "nats",
        "nats_url": "nats://localhost:4222",
        "nats_stream": "pgtrickle",
        "subject_template": "pgtrickle.{stream_table}.{op}"
    }'::jsonb
);

SELECT pgtrickle.enable_relay('orders-to-nats');
```

### Step 3: Start the relay

```bash
pgtrickle-relay \
  --postgres-url "postgres://relay_user:password@localhost:5432/mydb" \
  --relay-group-id "my-datacenter"
```

That's it. Order changes now flow from PostgreSQL to NATS automatically.

---

## Forward pipeline (outbox → external broker)

A forward pipeline polls an outbox consumer group and publishes events to an
external system.

### Registering a forward pipeline

```sql
SELECT pgtrickle.set_relay_outbox(
    'pipeline-name',
    config => '{...}'::jsonb
);
SELECT pgtrickle.enable_relay('pipeline-name');
```

### Common config fields

| Field | Required | Description |
|-------|----------|-------------|
| `stream_table` | Yes | Name of the stream table whose outbox to consume |
| `sink_type` | Yes | One of: `nats`, `kafka`, `webhook`, `redis`, `sqs`, `rabbitmq`, `pg-inbox`, `stdout` |
| `batch_size` | No | Messages per poll (default: 100) |
| `poll_interval_ms` | No | How often to poll in milliseconds (default: 1000) |
| `subject_template` | No | Subject/topic routing template (see [Templates](#subjecttopic-templates)) |

### NATS JetStream

```sql
SELECT pgtrickle.set_relay_outbox('orders-nats', config => '{
    "stream_table":    "order_summary",
    "sink_type":       "nats",
    "nats_url":        "nats://nats:4222",
    "nats_stream":     "pgtrickle",
    "subject_template": "pgtrickle.{stream_table}.{op}"
}'::jsonb);
```

### Apache Kafka

```sql
SELECT pgtrickle.set_relay_outbox('orders-kafka', config => '{
    "stream_table":    "order_summary",
    "sink_type":       "kafka",
    "kafka_brokers":   "kafka:9092",
    "kafka_topic":     "order-events",
    "kafka_key_field": "customer_id"
}'::jsonb);
```

### HTTP Webhook

```sql
SELECT pgtrickle.set_relay_outbox('orders-webhook', config => '{
    "stream_table":    "order_summary",
    "sink_type":       "webhook",
    "webhook_url":     "https://api.example.com/events",
    "webhook_headers": {"Authorization": "Bearer secret"}
}'::jsonb);
```

### Redis Streams

```sql
SELECT pgtrickle.set_relay_outbox('orders-redis', config => '{
    "stream_table": "order_summary",
    "sink_type":    "redis",
    "redis_url":    "redis://redis:6379",
    "redis_stream": "pgtrickle:order_summary"
}'::jsonb);
```

### AWS SQS

```sql
SELECT pgtrickle.set_relay_outbox('orders-sqs', config => '{
    "stream_table":   "order_summary",
    "sink_type":      "sqs",
    "sqs_queue_url":  "https://sqs.us-east-1.amazonaws.com/123456789/orders",
    "sqs_region":     "us-east-1"
}'::jsonb);
```

---

## Reverse pipeline (external broker → inbox)

A reverse pipeline consumes from an external source and writes into a pg_trickle
inbox, giving you exactly-once delivery semantics backed by PostgreSQL.

### Step 1: Create the inbox

```sql
SELECT pgtrickle.create_inbox('order_commands');
```

### Step 2: Register a reverse pipeline

```sql
SELECT pgtrickle.set_relay_inbox(
    'nats-to-order-commands',
    config => '{
        "source_type":   "nats",
        "nats_url":      "nats://nats:4222",
        "nats_stream":   "commands",
        "nats_consumer": "relay-consumer",
        "inbox_table":   "order_commands"
    }'::jsonb
);
SELECT pgtrickle.enable_relay('nats-to-order-commands');
```

The relay inserts each message with `ON CONFLICT (event_id) DO NOTHING`, so
duplicate deliveries from the broker are silently discarded.

### Kafka → inbox

```sql
SELECT pgtrickle.set_relay_inbox('kafka-to-commands', config => '{
    "source_type":      "kafka",
    "kafka_brokers":    "kafka:9092",
    "kafka_topic":      "order-commands",
    "kafka_group_id":   "pgtrickle-relay",
    "inbox_table":      "order_commands"
}'::jsonb);
```

### Webhook → inbox

```sql
SELECT pgtrickle.set_relay_inbox('webhook-to-commands', config => '{
    "source_type":  "webhook",
    "webhook_bind": "0.0.0.0:8080",
    "webhook_path": "/events",
    "inbox_table":  "order_commands"
}'::jsonb);
```

---

## Subject / topic templates

Subject and topic names can use template variables:

| Variable | Description | Example |
|----------|-------------|---------|
| `{stream_table}` | Source stream table name | `order_summary` |
| `{op}` | Operation: `insert`, `update`, or `delete` | `insert` |
| `{outbox_id}` | Sequential outbox batch ID | `42` |
| `{refresh_id}` | pg_trickle refresh UUID | `3f8a…` |

Example template: `pgtrickle.{stream_table}.{op}`  
Rendered: `pgtrickle.order_summary.insert`

---

## Pipeline management

```sql
-- List all pipelines (enabled and disabled)
SELECT name, direction, enabled, config
FROM pgtrickle.list_relay_configs();

-- Get the config for one pipeline
SELECT pgtrickle.get_relay_config('orders-to-nats');

-- Disable without deleting
SELECT pgtrickle.disable_relay('orders-to-nats');

-- Re-enable
SELECT pgtrickle.enable_relay('orders-to-nats');

-- Delete permanently
SELECT pgtrickle.delete_relay('orders-to-nats');
```

---

## Hot reload

You do not need to restart the relay when pipeline configuration changes.
The relay listens on the `pgtrickle_relay_config` PostgreSQL notification
channel. When you call `set_relay_outbox`, `enable_relay`, `disable_relay`, or
`delete_relay`, the change is picked up within seconds.

This means you can add or modify pipelines in production without any downtime.

---

## High availability

Run two or three relay instances pointing at the same PostgreSQL database.
Pipeline ownership is distributed using PostgreSQL advisory locks:

1. Each instance tries to acquire a lock per pipeline.
2. The instance that wins the lock owns the pipeline and starts processing.
3. If an instance dies, its locks are released and another instance picks up
   the pipeline on the next discovery interval (default: 30 seconds).

```bash
# Instance 1
pgtrickle-relay --postgres-url "postgres://..." --relay-group-id "prod"

# Instance 2 (same group ID, same database)
pgtrickle-relay --postgres-url "postgres://..." --relay-group-id "prod"
```

All instances **must** use the same `--relay-group-id` so advisory locks are
scoped correctly and offsets are shared.

---

## Running the relay

### Command-line flags

```bash
pgtrickle-relay \
  --postgres-url     "postgres://relay:pass@db:5432/mydb"  \
  --metrics-addr     "0.0.0.0:9090"                        \
  --log-format       json                                  \
  --log-level        info                                  \
  --relay-group-id   "dc1"
```

### Environment variables

| Variable | CLI flag | Default |
|----------|----------|---------|
| `PGTRICKLE_RELAY_POSTGRES_URL` | `--postgres-url` | (required) |
| `PGTRICKLE_RELAY_METRICS_ADDR` | `--metrics-addr` | `0.0.0.0:9090` |
| `PGTRICKLE_RELAY_LOG_FORMAT` | `--log-format` | `text` |
| `PGTRICKLE_RELAY_LOG_LEVEL` | `--log-level` | `info` |
| `PGTRICKLE_RELAY_GROUP_ID` | `--relay-group-id` | `default` |

### Docker

```dockerfile
FROM ghcr.io/grove/pgtrickle-relay:0.29.0
ENV PGTRICKLE_RELAY_POSTGRES_URL=postgres://relay:pass@db:5432/mydb
ENV PGTRICKLE_RELAY_LOG_FORMAT=json
CMD ["pgtrickle-relay"]
```

### Kubernetes (sidecar)

```yaml
containers:
- name: relay
  image: ghcr.io/grove/pgtrickle-relay:0.29.0
  env:
  - name: PGTRICKLE_RELAY_POSTGRES_URL
    valueFrom:
      secretKeyRef:
        name: db-credentials
        key: relay-url
  ports:
  - containerPort: 9090
    name: metrics
  livenessProbe:
    httpGet:
      path: /health
      port: 9090
    initialDelaySeconds: 5
    periodSeconds: 10
```

---

## Monitoring

The relay exposes a Prometheus `/metrics` endpoint and a JSON `/health` endpoint
on the configured `--metrics-addr` (default: `0.0.0.0:9090`).

```bash
curl http://localhost:9090/health
# {"status":"healthy","pipelines_owned":3,"uptime_seconds":42}
```

### Key metrics

| Metric | Type | Description |
|--------|------|-------------|
| `pgtrickle_relay_messages_published_total` | Counter | Messages successfully published to the sink |
| `pgtrickle_relay_messages_consumed_total` | Counter | Messages consumed from the source |
| `pgtrickle_relay_publish_errors_total` | Counter | Publish failures |
| `pgtrickle_relay_source_errors_total` | Counter | Source poll failures |
| `pgtrickle_relay_pipelines_owned` | Gauge | Pipelines currently owned by this instance |
| `pgtrickle_relay_lag_seconds` | Gauge | Estimated relay lag per pipeline |

A rising `pgtrickle_relay_lag_seconds` means the relay is not keeping up.
Common causes: slow broker, batch size too small, or the relay is not
running.

---

## Idempotency guarantees

Each backend uses a different mechanism to prevent duplicate processing:

| Backend | Mechanism |
|---------|-----------|
| NATS JetStream | `Nats-Msg-Id` header → server-side dedup |
| Kafka | Idempotent producer + record key |
| HTTP webhook | `Idempotency-Key` request header |
| Redis Streams | `XADD` with server-assigned monotonic ID |
| SQS FIFO | `MessageDeduplicationId` |
| RabbitMQ | `message-id` AMQP property |
| PostgreSQL inbox | `ON CONFLICT (event_id) DO NOTHING` |

---

## Troubleshooting

### Relay is not consuming messages

1. Check `GET /health` — is status `healthy`?
2. Confirm the pipeline is enabled:
   ```sql
   SELECT enabled FROM pgtrickle.list_relay_configs()
   WHERE name = 'orders-to-nats';
   ```
3. Check that the outbox is enabled on the stream table:
   ```sql
   SELECT pgtrickle.outbox_status('public.order_summary');
   ```
4. Look at relay logs for advisory lock acquisition. If another instance owns
   the pipeline, you will see `lock not acquired for pipeline=orders-to-nats`.

### Messages are being re-published

The outbox offset is not persisting. Check that the relay's PostgreSQL user has
`UPDATE` permission on `pgtrickle.pgt_consumer_offsets`:

```sql
GRANT UPDATE ON pgtrickle.pgt_consumer_offsets TO relay_user;
```

### High relay lag

- Decrease `poll_interval_ms` in the pipeline config (e.g. from 1000 to 250)
- Increase `batch_size` (e.g. from 100 to 1000)
- Add more relay instances (each owns a disjoint set of pipelines)

### Pipeline is not picking up config changes

If hot reload is not working, verify the relay user has `LISTEN` permission on
the `pgtrickle_relay_config` channel. Alternatively, restart the relay — it
always loads the latest config at startup.

---

## Supported backends

| Backend | Feature flag | Forward | Reverse |
|---------|-------------|---------|---------|
| NATS JetStream | `nats` (default) | Yes | Yes |
| HTTP webhook | `webhook` (default) | Yes | Yes |
| stdout / file | `stdout` (default) | Yes | — |
| Apache Kafka | `kafka` | Yes | Yes |
| Redis Streams | `redis` | Yes | Yes |
| AWS SQS | `sqs` | Yes | Yes |
| RabbitMQ | `rabbitmq` | Yes | Yes |
| PostgreSQL inbox | `pg-inbox` | Yes | — |

Build with only the backends you need to minimize binary size:

```bash
# NATS + Kafka only
cargo build --no-default-features --features nats,kafka,stdout
```

---

## See also

- [Transactional Outbox](OUTBOX.md) — set up the outbox that the relay reads from
- [Transactional Inbox](INBOX.md) — set up the inbox that the relay writes to
- [Relay Architecture & Operations](RELAY.md) — module internals, coordinator design, deployment detail
- [SQL Reference: Relay Pipeline Catalog](SQL_REFERENCE.md#relay-pipeline-catalog-v0290)
- [Pattern 9: Bidirectional Event Pipeline with Relay](PATTERNS.md#pattern-9-bidirectional-event-pipeline-with-relay-v0290)
