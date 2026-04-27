[← Back to Blog Index](README.md)

# The Relay Deep Dive: NATS, Redis Streams, and RabbitMQ

## Beyond Kafka: five broker backends for pgtrickle-relay

---

The [Kafka blog post](streaming-to-kafka-without-kafka.md) covered the most common relay use case: streaming pg_trickle deltas to Kafka. But pgtrickle-relay supports six backends, and the non-Kafka ones are often a better fit depending on your infrastructure.

This post covers the other five: NATS JetStream, Redis Streams, RabbitMQ (AMQP), AWS SQS, and HTTP webhooks. Each has different semantics, different performance characteristics, and different operational profiles.

---

## The Relay Architecture (Quick Recap)

pgtrickle-relay is a standalone binary that acts as a bridge:

```
pg_trickle outbox → relay binary → external broker
external broker → relay binary → pg_trickle inbox
```

It runs as a sidecar — not inside PostgreSQL, not inside the broker. One relay binary can manage multiple pipelines, each with its own source (outbox) and sink (broker).

Configuration is TOML:

```toml
[source]
connection = "postgres://user:pass@localhost/mydb"

[[pipelines]]
stream_table = "order_summary"
sink = "nats"  # or redis, rabbitmq, sqs, webhook
```

---

## NATS JetStream

NATS is a lightweight messaging system. JetStream adds persistence, replay, and consumer groups on top of NATS's fire-and-forget core.

### When to Use NATS

- You want low-latency pub/sub with durable delivery.
- Your infrastructure is Kubernetes-native (NATS runs well as a StatefulSet).
- You need subject-based routing with wildcards.
- You're already running NATS for service-to-service messaging.

### Configuration

```toml
[[pipelines]]
stream_table = "order_events"
sink = "nats"

[pipelines.nats]
url = "nats://nats-server:4222"
stream = "ORDERS"
subject = "orders.{op}.{outbox_id}"
```

**Subject templates:** The `{op}` placeholder expands to `INSERT`, `UPDATE`, or `DELETE`. The `{outbox_id}` is the monotonic sequence number. You can also use `{stream_table}` and `{refresh_id}`.

A subscriber can listen to:
- `orders.>` — all order events
- `orders.INSERT.>` — only inserts
- `orders.DELETE.>` — only deletes

### JetStream Consumer Groups

NATS JetStream supports consumer groups natively. Multiple relay consumers (for HA) can share a durable consumer group:

```toml
[pipelines.nats]
url = "nats://nats-server:4222"
stream = "ORDERS"
subject = "orders.>"
consumer_group = "relay-primary"
```

If the primary relay fails, the secondary picks up from the last acknowledged message.

### Performance

NATS is fast. Expect:
- Publish latency: ~0.5ms per message (local NATS server)
- Throughput: 50,000+ messages/second sustained
- Replay: Full stream replay from any sequence number

---

## Redis Streams

Redis Streams (XADD/XREAD) provide an append-only log similar to Kafka, but with Redis's operational simplicity.

### When to Use Redis Streams

- You're already running Redis.
- You need a simple, low-ops message queue.
- Consumers are Redis-native (most languages have good Redis client libraries).
- You don't need cross-datacenter replication (Redis Cluster handles this, but it's more complex).

### Configuration

```toml
[[pipelines]]
stream_table = "inventory_changes"
sink = "redis"

[pipelines.redis]
url = "redis://redis-server:6379"
stream_key = "pgtrickle:inventory"
maxlen = 100000  # optional: cap stream length
```

**MAXLEN:** Redis Streams can grow unbounded. Set `maxlen` to automatically trim old entries. With approximate trimming (`~` prefix), Redis keeps roughly this many entries.

### Consumer Groups

Redis Streams have native consumer groups (XGROUP):

```toml
[pipelines.redis]
url = "redis://redis-server:6379"
stream_key = "pgtrickle:inventory"
consumer_group = "indexer"
```

Multiple consumers in the group share the workload. Each message is delivered to exactly one consumer in the group.

### Performance

- XADD latency: ~0.2ms per message
- Throughput: 100,000+ messages/second (single Redis instance)
- Memory usage: ~100 bytes per stream entry overhead

**Caveat:** Redis is memory-bound. If your stream tables produce large deltas (wide rows, many changes per cycle), the Redis memory footprint grows quickly. Monitor with `XLEN` and set `maxlen` to prevent OOM.

---

## RabbitMQ (AMQP)

RabbitMQ uses exchanges and queues with routing keys. It's the most flexible in terms of message routing — fanout, direct, topic, and headers exchanges.

### When to Use RabbitMQ

- You need complex routing (one message to multiple queues based on content).
- Your organization standardizes on AMQP.
- You need message-level TTL and dead-letter queues.
- You want per-message acknowledgments with redelivery on failure.

### Configuration

```toml
[[pipelines]]
stream_table = "user_events"
sink = "rabbitmq"

[pipelines.rabbitmq]
url = "amqp://user:pass@rabbitmq-server:5672/vhost"
exchange = "pgtrickle.events"
exchange_type = "topic"
routing_key = "users.{op}"
```

**Routing keys:** With a `topic` exchange, consumers bind queues to patterns:
- `users.*` — all user events
- `users.INSERT` — only inserts
- `*.DELETE` — deletes from any stream table

### Fanout Example

```toml
[pipelines.rabbitmq]
exchange = "pgtrickle.broadcast"
exchange_type = "fanout"
```

Every consumer queue bound to the exchange gets every message. Useful for broadcasting changes to multiple downstream services simultaneously.

### Performance

- Publish latency: ~1–2ms per message
- Throughput: 10,000–30,000 messages/second (depends on persistence settings)
- Persistence: Durable exchanges and queues survive broker restart

**Note:** RabbitMQ's throughput is lower than NATS or Redis because it provides stronger delivery guarantees (persistent messages with acknowledgments). The trade-off is reliability vs. speed.

---

## AWS SQS

SQS is AWS's managed message queue. No infrastructure to manage — it's a service.

### When to Use SQS

- Your workloads run on AWS.
- You want zero message queue operations (no servers, no patching, no monitoring).
- Consumers are Lambda functions or ECS tasks.
- You need FIFO ordering within a message group.

### Configuration

```toml
[[pipelines]]
stream_table = "order_summary"
sink = "sqs"

[pipelines.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789/pgtrickle-orders"
region = "us-east-1"
# Credentials from environment (AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY)
# or IAM role
```

**FIFO queues:** For ordered delivery:

```toml
[pipelines.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789/pgtrickle-orders.fifo"
message_group_id = "{stream_table}"
message_dedup_id = "{outbox_id}"
```

The `message_dedup_id` ensures exactly-once delivery within SQS's 5-minute deduplication window.

### Performance

- Publish latency: 5–20ms per message (network round-trip to SQS API)
- Throughput: ~3,000 messages/second (standard queue), ~300/second (FIFO)
- Cost: ~$0.40 per million messages

**Batching:** The relay batches up to 10 messages per SQS `SendMessageBatch` call, reducing API calls and latency.

---

## HTTP Webhooks

The simplest sink: POST each message to an HTTP endpoint.

### When to Use Webhooks

- You're integrating with a third-party service that accepts webhooks.
- You don't have a message broker and don't want one.
- The consumer is a serverless function (AWS Lambda, Cloudflare Workers).
- Volume is low enough that per-message HTTP calls are acceptable.

### Configuration

```toml
[[pipelines]]
stream_table = "alert_triggers"
sink = "webhook"

[pipelines.webhook]
url = "https://api.example.com/webhooks/pgtrickle"
headers = { "Authorization" = "Bearer ${ENV:WEBHOOK_TOKEN}", "Content-Type" = "application/json" }
timeout_ms = 5000
retry_max = 3
retry_backoff_ms = 1000
```

**Headers:** Support environment variable interpolation (`${ENV:VAR_NAME}`) for secrets.

**Retry:** Failed POSTs are retried with exponential backoff: 1s, 2s, 4s. After `retry_max` attempts, the message is logged as failed and the relay continues.

### Performance

- Latency: depends on the webhook endpoint (typically 50–200ms per request)
- Throughput: limited by endpoint response time and concurrency
- Reliability: best-effort (retries, but no persistent queue)

**Caveat:** Webhooks are inherently at-least-once. The endpoint must be idempotent. Use the `outbox_id` in the payload for deduplication.

---

## Choosing a Backend

| Backend | Latency | Throughput | Ops Overhead | Best For |
|---------|---------|-----------|-------------|----------|
| Kafka | 2–10ms | 100K+/s | High (ZK/KRaft, brokers) | Enterprise event streaming |
| NATS | 0.5ms | 50K+/s | Low (single binary) | Kubernetes-native, low-latency |
| Redis | 0.2ms | 100K+/s | Low (existing Redis) | Simple queues, existing Redis |
| RabbitMQ | 1–2ms | 10–30K/s | Medium | Complex routing, AMQP |
| SQS | 5–20ms | 3K/s | Zero | AWS-native, serverless |
| Webhook | 50–200ms | 10–100/s | Zero | Third-party integrations |

**Decision tree:**
1. Already running Kafka? → Kafka.
2. On AWS with no broker preference? → SQS.
3. Want low-latency and run Kubernetes? → NATS.
4. Already running Redis? → Redis Streams.
5. Need complex routing? → RabbitMQ.
6. Low volume, third-party integration? → Webhook.

---

## Multi-Sink Pipelines

A single relay instance can run multiple pipelines to different backends:

```toml
[[pipelines]]
stream_table = "order_events"
sink = "kafka"
[pipelines.kafka]
brokers = "kafka:9092"
topic = "orders"

[[pipelines]]
stream_table = "order_events"
sink = "webhook"
[pipelines.webhook]
url = "https://slack-webhook.example.com/notify"
```

The same stream table's outbox feeds both Kafka and a Slack webhook. Each pipeline has its own consumer group and offset — they're independent.

---

## Summary

pgtrickle-relay supports six backends: Kafka, NATS, Redis Streams, RabbitMQ, SQS, and HTTP webhooks. Each has different trade-offs in latency, throughput, operational complexity, and delivery semantics.

The relay binary is the same regardless of backend. Switch backends by changing the TOML config. Run multiple pipelines to different backends simultaneously. Use environment variables for secrets.

Pick the backend that matches your existing infrastructure. If you have no preference, NATS (for self-hosted) or SQS (for AWS) are the lowest-friction options.
