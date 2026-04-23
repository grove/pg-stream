# pgtrickle-relay

Standalone Rust CLI binary that bridges [pg_trickle](https://github.com/grove/pg-trickle) outbox and inbox tables with external messaging systems.

## Overview

`pgtrickle-relay` runs as a sidecar or standalone process alongside your PostgreSQL instance. It supports two directions:

- **Forward mode**: Polls pg-trickle outbox tables and publishes deltas to external sinks.
- **Reverse mode**: Consumes messages from external sources and writes them into pg-trickle inbox tables.

Both directions share the same source/sink trait abstractions, config system, observability stack, and error handling.

## Supported Backends

| Backend | Feature flag | Forward (sink) | Reverse (source) |
|---------|-------------|----------------|-----------------|
| NATS JetStream | `nats` (default) | ✅ | ✅ |
| HTTP webhook | `webhook` (default) | ✅ | ✅ |
| stdout / file | `stdout` (default) | ✅ | — |
| Apache Kafka | `kafka` | ✅ | ✅ |
| Redis Streams | `redis` | ✅ | ✅ |
| AWS SQS | `sqs` | ✅ | ✅ |
| RabbitMQ | `rabbitmq` | ✅ | ✅ |
| PostgreSQL inbox | `pg-inbox` | ✅ | — |

## Installation

### From source

```bash
cargo install --path pgtrickle-relay --features default
```

### From Docker

```bash
docker pull ghcr.io/grove/pgtrickle-relay:0.29.0
```

## Configuration

All relay pipelines are configured via SQL — no YAML files required.

### Register a forward pipeline (outbox → NATS)

```sql
SELECT pgtrickle.set_relay_outbox(
    'orders-to-nats',
    config => '{
        "stream_table": "orders_stream",
        "sink_type": "nats",
        "nats_url": "nats://localhost:4222",
        "nats_stream": "pgtrickle",
        "subject_template": "pgtrickle.{stream_table}.{op}"
    }'::jsonb
);
SELECT pgtrickle.enable_relay('orders-to-nats');
```

### Register a reverse pipeline (NATS → inbox)

```sql
SELECT pgtrickle.set_relay_inbox(
    'nats-to-orders-inbox',
    config => '{
        "source_type": "nats",
        "nats_url": "nats://localhost:4222",
        "nats_stream": "commands",
        "nats_consumer": "relay-consumer",
        "inbox_table": "orders_inbox"
    }'::jsonb
);
SELECT pgtrickle.enable_relay('nats-to-orders-inbox');
```

### Manage pipelines

```sql
-- List all pipelines
SELECT * FROM pgtrickle.list_relay_configs();

-- Disable a pipeline
SELECT pgtrickle.disable_relay('orders-to-nats');

-- Delete a pipeline
SELECT pgtrickle.delete_relay('orders-to-nats');
```

## Running the relay

```bash
pgtrickle-relay \
  --postgres-url "postgres://relay:password@localhost:5432/mydb" \
  --metrics-addr "0.0.0.0:9090" \
  --log-format json \
  --relay-group-id "dc1-relay"
```

### Environment variables

| Variable | CLI flag | Default |
|----------|----------|---------|
| `PGTRICKLE_RELAY_POSTGRES_URL` | `--postgres-url` | (required) |
| `PGTRICKLE_RELAY_METRICS_ADDR` | `--metrics-addr` | `0.0.0.0:9090` |
| `PGTRICKLE_RELAY_LOG_FORMAT` | `--log-format` | `text` |
| `PGTRICKLE_RELAY_LOG_LEVEL` | `--log-level` | `info` |
| `PGTRICKLE_RELAY_GROUP_ID` | `--relay-group-id` | `default` |

## Observability

The relay exposes:
- `GET /metrics` — Prometheus metrics (messages published, consumed, errors, lag)
- `GET /health` — JSON health check

```bash
curl http://localhost:9090/health
# {"status":"healthy","pipelines_owned":3,"uptime_seconds":42}
```

## High Availability

Multiple relay instances can run against the same PostgreSQL database. Pipeline ownership is distributed via PostgreSQL advisory locks — each pipeline is owned by exactly one relay instance at a time. If an instance dies, another acquires the lock on the next discovery interval.

No external coordinator (ZooKeeper, etcd, etc.) is required.

## Hot Reload

Config changes take effect without restart. The relay listens on the `pgtrickle_relay_config` PostgreSQL notification channel. When you run `set_relay_outbox`, `enable_relay`, `disable_relay`, or `delete_relay`, the relay reloads the affected pipeline within seconds.

## Building with specific backends only

```bash
# Kafka only (no NATS, no webhook)
cargo build --no-default-features --features kafka,stdout

# All backends
cargo build --features nats,webhook,kafka,redis,sqs,rabbitmq,pg-inbox,stdout
```

## Subject / topic templates

Subject templates support these variables:

| Variable | Description |
|----------|-------------|
| `{stream_table}` | Name of the source stream table |
| `{op}` | Operation: `insert`, `update`, `delete` |
| `{outbox_id}` | Sequential outbox batch ID |
| `{refresh_id}` | pg-trickle refresh ID |

Example: `pgtrickle.{stream_table}.{op}` → `pgtrickle.orders.insert`

## License

Apache-2.0. See [LICENSE](../LICENSE).
