# pgtrickle-relay — Architecture & Operations Guide

`pgtrickle-relay` is a standalone Rust CLI binary that bridges pg-trickle
outbox/inbox tables with external messaging systems.

## Architecture

```
PostgreSQL (pg-trickle extension)
  ├─ outbox tables ──→ [OutboxPollerSource] ──→ Sink (NATS / Kafka / webhook / …)
  └─ inbox tables  ←── [InboxSink]         ←── Source (NATS / Kafka / webhook / …)
```

The relay is a workspace member of the pg-trickle repository at `pgtrickle-relay/`.

### Key modules

| Module | Purpose |
|--------|---------|
| `main.rs` | CLI entry point, tracing init, coordinator startup |
| `cli.rs` | clap argument definitions |
| `config.rs` | `RelayConfig` (global) and `PipelineConfig` (per-pipeline) |
| `coordinator.rs` | Advisory lock acquisition, pipeline hot-reload |
| `envelope.rs` | `RelayMessage` envelope, `AckToken`, subject templates |
| `transforms.rs` | Subject routing helpers, dedup key extraction |
| `metrics.rs` | Prometheus metrics, `/metrics` + `/health` axum server |
| `error.rs` | `RelayError` enum |
| `source/` | Source trait + implementations |
| `sink/` | Sink trait + implementations |

## Source implementations

| File | Backend | Feature |
|------|---------|---------|
| `source/outbox.rs` | pg-trickle outbox poller | always |
| `source/stdin.rs` | stdin / JSONL file | always |
| `source/nats.rs` | NATS JetStream pull consumer | `nats` |
| `source/webhook.rs` | axum HTTP webhook receiver | `webhook` |
| `source/kafka.rs` | Kafka consumer (rdkafka) | `kafka` |
| `source/redis.rs` | Redis Streams XREADGROUP | `redis` |
| `source/sqs.rs` | AWS SQS consumer | `sqs` |
| `source/rabbitmq.rs` | RabbitMQ consumer (lapin) | `rabbitmq` |

## Sink implementations

| File | Backend | Feature |
|------|---------|---------|
| `sink/stdout.rs` | stdout / JSONL file | `stdout` |
| `sink/inbox.rs` | pg-trickle inbox table | always |
| `sink/pg_outbox.rs` | Remote PostgreSQL inbox | `pg-inbox` |
| `sink/nats.rs` | NATS JetStream publish | `nats` |
| `sink/webhook.rs` | HTTP webhook (reqwest) | `webhook` |
| `sink/kafka.rs` | Kafka producer (rdkafka) | `kafka` |
| `sink/redis.rs` | Redis Streams XADD | `redis` |
| `sink/sqs.rs` | AWS SQS SendMessageBatch | `sqs` |
| `sink/rabbitmq.rs` | RabbitMQ publish (lapin) | `rabbitmq` |

## Pipeline catalog

Relay pipeline configuration is stored in PostgreSQL:

```sql
-- Forward pipelines (outbox → external sink)
SELECT * FROM pgtrickle.relay_outbox_config;

-- Reverse pipelines (external source → inbox)
SELECT * FROM pgtrickle.relay_inbox_config;

-- Consumer offsets (for forward pipelines with simple mode)
SELECT * FROM pgtrickle.relay_consumer_offsets;
```

## Coordinator and advisory locks

Each relay binary instance runs a coordinator loop:

1. Load all enabled pipelines from the catalog.
2. For each pipeline, try `pg_try_advisory_lock(hashtext(relay_group_id), hashtext(pipeline_name))`.
3. If the lock is acquired, start the pipeline worker task.
4. If the lock is not acquired (another instance owns it), skip.
5. Listen on `pgtrickle_relay_config` for config changes and hot-reload.

This ensures each pipeline runs on exactly one instance at a time, with
automatic failover if an instance dies.

## Idempotency

Every backend implements idempotent delivery:

| Backend | Dedup mechanism |
|---------|----------------|
| NATS | `Nats-Msg-Id` header → JetStream server-side dedup |
| Kafka | Idempotent producer + record key |
| HTTP webhook | `Idempotency-Key` header |
| Redis Streams | XADD with `*` ID (server-assigned, monotonic) |
| SQS FIFO | `MessageDeduplicationId` |
| RabbitMQ | `message-id` AMQP property |
| PostgreSQL inbox | `ON CONFLICT (event_id) DO NOTHING` |

## Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `pgtrickle_relay_messages_published_total` | Counter | Messages successfully published |
| `pgtrickle_relay_messages_consumed_total` | Counter | Messages consumed from sources |
| `pgtrickle_relay_publish_errors_total` | Counter | Publish failures |
| `pgtrickle_relay_source_errors_total` | Counter | Source poll failures |
| `pgtrickle_relay_pipelines_owned` | Gauge | Pipelines currently owned by this instance |
| `pgtrickle_relay_lag_seconds` | Gauge | Estimated relay lag per pipeline |

## Deployment

### Docker

```dockerfile
FROM ghcr.io/grove/pgtrickle-relay:0.29.0
ENV PGTRICKLE_RELAY_POSTGRES_URL=postgres://relay:pass@db:5432/app
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
```

### Multiple instances (HA)

Run 2–3 relay instances pointing at the same PostgreSQL. Each instance acquires
advisory locks for a subset of pipelines. If one fails, others reacquire its
locks on the next discovery interval.

All instances should use the same `--relay-group-id` so advisory locks are
scoped correctly.

## Troubleshooting

### Relay is not consuming messages

1. Check `GET /health` — is status healthy?
2. Check `pgtrickle.relay_outbox_config` — is the pipeline `enabled = true`?
3. Check `pgtrickle.relay_consumer_offsets` — is the offset advancing?
4. Check relay logs for advisory lock acquisition messages.

### Relay keeps re-processing old messages

The outbox offset may not be persisting. Check that the relay has `UPDATE`
permission on `pgtrickle.relay_consumer_offsets`, or that you're using consumer
group mode with `commit_offset()`.

### High lag

Decrease `poll_interval_ms` in the pipeline config, or increase batch size.

## See also

- [pgtrickle-relay README](../pgtrickle-relay/README.md) — quickstart and config reference
- [PLAN_RELAY_CLI.md](../plans/relay/PLAN_RELAY_CLI.md) — full design document
- [SQL_REFERENCE.md](SQL_REFERENCE.md) — complete SQL API reference
