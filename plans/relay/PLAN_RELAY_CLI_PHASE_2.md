# Relay CLI — Phase 2 Plan

> **Status:** Proposal (DRAFT)
> **Created:** 2026-04-19
> **Category:** Tooling — Relay CLI Extension
> **Depends on:** [PLAN_RELAY_CLI.md](./PLAN_RELAY_CLI.md) (v0.24.0 Phase 1)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Part A — Additional Backends](#part-a--additional-backends)
  - [A.1 Google Cloud Pub/Sub](#a1-google-cloud-pubsub)
  - [A.2 Amazon Kinesis Data Streams](#a2-amazon-kinesis-data-streams)
  - [A.3 Azure Service Bus](#a3-azure-service-bus)
  - [A.4 Azure Event Hubs](#a4-azure-event-hubs)
  - [A.5 Apache Pulsar](#a5-apache-pulsar)
  - [A.6 MQTT (v5)](#a6-mqtt-v5)
  - [A.7 Elasticsearch / OpenSearch](#a7-elasticsearch--opensearch)
  - [A.8 Object Storage (S3 / GCS / Azure Blob)](#a8-object-storage-s3--gcs--azure-blob)
  - [A.9 ClickHouse](#a9-clickhouse)
  - [A.10 Apache Arrow Flight / gRPC](#a10-apache-arrow-flight--grpc)
  - [Backend Priority Matrix](#backend-priority-matrix)
- [Part B — CLI Extensions & Improvements](#part-b--cli-extensions--improvements)
  - [B.1 Dead-Letter Queue (DLQ)](#b1-dead-letter-queue-dlq)
  - [B.2 Schema Registry Integration (Avro / Protobuf)](#b2-schema-registry-integration-avro--protobuf)
  - [B.3 Message Transforms (JMESPath)](#b3-message-transforms-jmespath)
  - [B.4 Content-Based Routing](#b4-content-based-routing)
  - [B.5 Rate Limiting & Back-Pressure](#b5-rate-limiting--back-pressure)
  - [B.6 Circuit Breaker](#b6-circuit-breaker)
  - [B.7 Multi-Pipeline Mode](#b7-multi-pipeline-mode)
  - [B.8 Config Hot-Reload](#b8-config-hot-reload)
  - [B.9 Dry-Run & Replay Mode](#b9-dry-run--replay-mode)
  - [B.10 OpenTelemetry Tracing](#b10-opentelemetry-tracing)
  - [B.11 Relay TUI Dashboard](#b11-relay-tui-dashboard)
  - [B.12 Plugin System (Dynamic Backends)](#b12-plugin-system-dynamic-backends)
  - [B.13 Encryption Envelope](#b13-encryption-envelope)
  - [B.14 Webhook Signature Verification](#b14-webhook-signature-verification)
- [Part C — Testing Strategy](#part-c--testing-strategy)
- [Part D — Implementation Roadmap](#part-d--implementation-roadmap)
- [Open Questions](#open-questions)

---

## Executive Summary

Phase 2 of the `pgtrickle-relay` CLI extends the v0.24.0 foundation with
additional backends for major cloud platforms and analytics systems, plus
operational improvements that make the relay production-grade at scale.

**Phase 1 (v0.24.0)** ships with 8 sink backends (NATS, Kafka, HTTP webhook,
Redis Streams, SQS, RabbitMQ, PostgreSQL inbox, stdout/file) and 7 source
backends (same minus stdout, plus stdin). Phase 2 adds **10 new backends**
covering all three major cloud providers, the IoT ecosystem, analytics
databases, and data lake storage.

Beyond backends, Phase 2 introduces **14 operational improvements**: dead-letter
queues, schema registry integration, message transforms, content-based routing,
rate limiting, circuit breakers, multi-pipeline mode, config hot-reload,
dry-run/replay, OpenTelemetry tracing, a TUI dashboard, a plugin system,
payload encryption, and webhook signature verification.

---

## Part A — Additional Backends

Each backend is implemented as both a Source and Sink where the system
supports bidirectional communication. Read-only or write-only systems
implement only the relevant trait.

### A.1 Google Cloud Pub/Sub

**Why:** Second-largest cloud messaging platform after AWS. Every GCP-native
organisation needs this. Pub/Sub is Google's equivalent of SQS + SNS combined.

**Crate:** `google-cloud-pubsub` or `gcloud-sdk`

**Direction:** Source + Sink (bidirectional)

#### Sink Configuration

```toml
[sink.gcp-pubsub]
project_id = "my-project"
topic = "pgtrickle-events"
# topic_template = "pgtrickle.{stream_table}"
# ordering_key_template = "{outbox_id}"    # for ordered delivery
# credentials_file = "/etc/gcp/sa.json"    # default: GOOGLE_APPLICATION_CREDENTIALS
# endpoint = "pubsub.googleapis.com:443"   # for emulator: localhost:8085
# batch_max_messages = 1000
# batch_max_bytes = 10485760               # 10 MiB
# publish_timeout_ms = 60000
```

**Dedup:** Uses Pub/Sub's message attributes with `pgt_dedup_key` attribute.
Consumers implement dedup via Dataflow or Cloud Functions idempotency.

**Ordering:** Supports ordering keys for FIFO semantics within a partition.

#### Source Configuration

```toml
[source.gcp-pubsub]
project_id = "my-project"
subscription = "pgtrickle-inbox-sub"
# credentials_file = "/etc/gcp/sa.json"
# max_messages = 100
# ack_deadline_seconds = 30
# endpoint = "pubsub.googleapis.com:443"
```

**Consumption model:** Pull subscription. Messages are acknowledged only
after successful inbox write. Nacked messages are redelivered after the
ack deadline.

**Dedup key:** Uses `pgt_dedup_key` message attribute if present; otherwise
uses Pub/Sub `message_id`.

### A.2 Amazon Kinesis Data Streams

**Why:** AWS's streaming platform for high-throughput, ordered event streams.
Different trade-offs from SQS: ordered within shards, higher throughput,
longer retention (up to 365 days).

**Crate:** `aws-sdk-kinesis`

**Direction:** Source + Sink (bidirectional)

#### Sink Configuration

```toml
[sink.kinesis]
stream_name = "pgtrickle-events"
# region = "us-east-1"
# partition_key_template = "{stream_table}"
# explicit_hash_key = ""                    # optional shard targeting
# batch_size = 500                          # records per PutRecords call (max 500)
# aggregation = true                        # KPL-compatible aggregation
```

**Dedup:** Kinesis does not deduplicate natively. The relay sets a
`pgt_dedup_key` field in the record data. Consumer-side dedup is required.

**Ordering:** Records are ordered within a shard by partition key.

#### Source Configuration

```toml
[source.kinesis]
stream_name = "external-events"
# region = "us-east-1"
# iterator_type = "TRIM_HORIZON"           # TRIM_HORIZON | LATEST | AT_TIMESTAMP
# at_timestamp = "2026-01-01T00:00:00Z"    # when iterator_type = AT_TIMESTAMP
# checkpoint_table = "pgtrickle_kinesis_checkpoints"  # DynamoDB or PG table
# poll_interval_ms = 1000
```

**Consumption model:** Uses `GetShardIterator` + `GetRecords`. Checkpoints
shard positions in a PostgreSQL table (or DynamoDB). Handles shard splits
and merges.

### A.3 Azure Service Bus

**Why:** Primary enterprise messaging platform for Azure. Equivalent of
SQS + RabbitMQ for Microsoft ecosystem customers. Supports queues, topics,
and subscriptions with advanced features (sessions, dead-letter, scheduling).

**Crate:** `azure_messaging_servicebus`

**Direction:** Source + Sink (bidirectional)

#### Sink Configuration

```toml
[sink.azure-servicebus]
connection_string = "Endpoint=sb://mybus.servicebus.windows.net/;SharedAccessKeyName=...;SharedAccessKey=..."
# Or use managed identity:
# namespace = "mybus.servicebus.windows.net"
queue_or_topic = "pgtrickle-events"
# session_id = ""                           # for session-enabled queues
# message_ttl_seconds = 86400              # 24 hours
# schedule_enqueue_time = ""               # delayed delivery
# batch_size = 100                          # messages per send batch
```

**Dedup:** Uses `MessageId` property set to the dedup key. Service Bus
supports native dedup on queues with `RequiresDuplicateDetection` enabled.

#### Source Configuration

```toml
[source.azure-servicebus]
connection_string = "Endpoint=sb://..."
queue_or_topic = "inbox-events"
# subscription = "pgtrickle-relay"         # for topic subscriptions
# receive_mode = "PeekLock"               # PeekLock | ReceiveAndDelete
# max_messages = 10
# max_wait_seconds = 30
# prefetch_count = 100
```

**Consumption model:** Uses `PeekLock` mode. Messages are completed only
after successful inbox write. Abandoned messages are returned to the queue.

### A.4 Azure Event Hubs

**Why:** Azure's Kafka-compatible streaming platform. Many Azure customers
use Event Hubs instead of self-managed Kafka. Supports the Kafka protocol
natively, but also has its own SDK with better Azure integration.

**Crate:** `azure_messaging_eventhubs` or use `rdkafka` with Kafka protocol

**Direction:** Source + Sink (bidirectional)

#### Sink Configuration

```toml
[sink.azure-eventhubs]
connection_string = "Endpoint=sb://myhub.servicebus.windows.net/;SharedAccessKeyName=...;SharedAccessKey=..."
event_hub_name = "pgtrickle-events"
# partition_key_template = "{stream_table}"
# batch_max_bytes = 1048576                # 1 MiB per batch
# Or use Kafka protocol (reuse existing Kafka sink):
# [sink.kafka]
# brokers = "myhub.servicebus.windows.net:9093"
# security_protocol = "SASL_SSL"
# sasl_mechanism = "PLAIN"
# sasl_username = "$ConnectionString"
# sasl_password = "Endpoint=sb://..."
```

**Note:** Since Event Hubs supports the Kafka protocol, users can also use the
existing Kafka sink/source with Event Hubs connection details. The native SDK
provides better integration with Azure identity, diagnostics, and checkpointing.

#### Source Configuration

```toml
[source.azure-eventhubs]
connection_string = "Endpoint=sb://..."
event_hub_name = "external-events"
consumer_group = "$Default"
# checkpoint_store = "postgres"            # postgres | azure-blob
# starting_position = "earliest"           # earliest | latest
```

**Consumption model:** Event processor with partition ownership and
checkpointing. Checkpoints stored in PostgreSQL or Azure Blob Storage.

### A.5 Apache Pulsar

**Why:** Growing alternative to Kafka with superior multi-tenancy,
geo-replication, and tiered storage. Adopted by Splunk, Yahoo, Tencent,
and Verizon Media. Offers both streaming and queuing semantics.

**Crate:** `pulsar` (official Rust client)

**Direction:** Source + Sink (bidirectional)

#### Sink Configuration

```toml
[sink.pulsar]
url = "pulsar://localhost:6650"
topic = "persistent://public/default/pgtrickle-events"
# topic_template = "persistent://public/default/pgtrickle.{stream_table}"
# producer_name = "pgtrickle-relay"
# send_timeout_ms = 30000
# batch_enabled = true
# batch_max_messages = 1000
# compression = "lz4"                      # none | lz4 | zlib | zstd | snappy
# auth_token = "eyJhbGci..."
# tls_cert_file = "/etc/pulsar/cert.pem"
# tls_key_file = "/etc/pulsar/key.pem"
```

**Dedup:** Uses Pulsar's built-in message deduplication (producer-side).
The dedup key is set as the `sequence_id` on the producer.

#### Source Configuration

```toml
[source.pulsar]
url = "pulsar://localhost:6650"
topic = "persistent://public/default/external-events"
subscription = "pgtrickle-inbox"
# subscription_type = "Shared"             # Exclusive | Shared | Failover | Key_Shared
# initial_position = "Earliest"            # Earliest | Latest
# ack_timeout_ms = 30000
# negative_ack_redelivery_delay_ms = 1000
# dead_letter_topic = "persistent://public/default/pgtrickle-dlq"
# max_redeliver_count = 5
```

**Consumption model:** Creates a Pulsar consumer with the specified
subscription type. Messages are acknowledged individually after successful
inbox write. Supports automatic DLQ routing.

### A.6 MQTT (v5)

**Why:** The dominant IoT messaging protocol. MQTT v5 is supported by
Mosquitto, HiveMQ, EMQX, VerneMQ, and every cloud IoT platform (AWS IoT
Core, Azure IoT Hub, GCP IoT Core). Enables pg-trickle to bridge device
telemetry into stream tables.

**Crate:** `rumqttc`

**Direction:** Source + Sink (bidirectional)

#### Sink Configuration

```toml
[sink.mqtt]
url = "mqtt://localhost:1883"
# url = "mqtts://broker.hivemq.com:8883"   # TLS
topic_template = "pgtrickle/{stream_table}/{op}"
# client_id = "pgtrickle-relay"
# qos = 1                                  # 0 = at most once, 1 = at least once, 2 = exactly once
# retain = false
# username = "user"
# password = "pass"
# keep_alive_seconds = 60
# clean_start = true
# tls_ca = "/etc/mqtt/ca.pem"
# tls_cert = "/etc/mqtt/cert.pem"
# tls_key = "/etc/mqtt/key.pem"
# max_packet_size = 268435456              # 256 MiB (MQTT v5)
```

**Dedup:** MQTT v5 does not have native dedup. The relay sets a
`pgt_dedup_key` user property. Consumer-side dedup required.

**QoS levels:**
- QoS 0: Fire-and-forget (fastest, may lose messages)
- QoS 1: At-least-once (default — matches relay semantics)
- QoS 2: Exactly-once (slowest, broker-level guarantee)

#### Source Configuration

```toml
[source.mqtt]
url = "mqtt://localhost:1883"
topic = "devices/+/telemetry"              # MQTT topic filter with wildcards
# client_id = "pgtrickle-inbox"
# qos = 1
# clean_start = false                      # persistent session for offline buffering
# username = "user"
# password = "pass"
```

**Consumption model:** Subscribes to the topic filter. Messages are
processed as they arrive. With QoS 1+, the broker retransmits if the
relay doesn't acknowledge within the keep-alive interval.

**Use-cases:**
- IoT device telemetry → pg-trickle inbox → stream table analytics
- Edge sensor data aggregation
- Smart home / building automation event streams

### A.7 Elasticsearch / OpenSearch

**Why:** Dominant search and analytics engine. Natural fit for CDC: every
data change in pg-trickle can be indexed for full-text search and
real-time dashboards. Used by nearly every mid-to-large engineering team.

**Crate:** `elasticsearch` (official) or `opensearch`

**Direction:** Sink only (write — search engines are not message sources)

#### Sink Configuration

```toml
[sink.elasticsearch]
url = "https://localhost:9200"
index_template = "pgtrickle-{stream_table}"
# index_date_pattern = "%Y.%m"             # monthly indices: pgtrickle-orders-2026.04
# doc_id_template = "{dedup_key}"          # Elasticsearch _id for upserts
# pipeline = "my-ingest-pipeline"          # ingest pipeline
# username = "elastic"
# password = "changeme"
# api_key = "base64-encoded-key"
# tls_ca = "/etc/elasticsearch/ca.pem"
# bulk_max_actions = 1000                  # actions per _bulk request
# bulk_max_bytes = 5242880                 # 5 MiB per _bulk request
# bulk_flush_interval_ms = 1000
# refresh_policy = "false"                 # true | false | wait_for
# op_type = "index"                        # index | create
# routing_template = "{stream_table}"
```

**Operations mapping:**
- `op = "insert"` → Elasticsearch `index` (upsert by `_id`)
- `op = "delete"` → Elasticsearch `delete` (by `_id`)
- `is_full_refresh = true` → Delete-by-query + bulk index (atomic swap
  via index alias rotation)

**Bulk API:** Uses the `_bulk` API for efficient batching. Each batch
from the relay loop is translated to a single `_bulk` request.

**Full-refresh handling:** For full-refresh batches, the sink:
1. Creates a new index with a timestamped suffix
2. Bulk-indexes all rows
3. Swaps the index alias atomically
4. Deletes the old index

**OpenSearch compatibility:** Set `flavor = "opensearch"` in config. The
same code path works for both — the bulk API is compatible.

```toml
[sink.elasticsearch]
url = "https://localhost:9200"
flavor = "opensearch"                      # elasticsearch (default) | opensearch
```

### A.8 Object Storage (S3 / GCS / Azure Blob)

**Why:** Data lake integration. Every modern data stack stores analytical
data in object storage (S3, GCS, Azure Blob). Enables pg-trickle deltas
to flow into Parquet/JSON files for consumption by Spark, dbt, Trino,
DuckDB, and other analytics tools.

**Crate:** `aws-sdk-s3`, `google-cloud-storage`, `azure_storage_blobs`

**Direction:** Sink only (write — not a real-time message source)

#### Sink Configuration

```toml
[sink.object-storage]
provider = "s3"                            # s3 | gcs | azure-blob
bucket = "my-data-lake"
prefix = "pgtrickle/{stream_table}"
# region = "us-east-1"                     # S3/GCS
# account_name = "mystorageaccount"        # Azure Blob
# container_name = "pgtrickle"             # Azure Blob (instead of bucket)

# Output format
format = "jsonl"                           # jsonl | parquet | csv
# compression = "gzip"                     # none | gzip | zstd | snappy (jsonl/csv)
# parquet_compression = "zstd"             # none | snappy | gzip | lz4 | zstd

# Partitioning
partition_by = "date"                      # none | date | hour | custom
# partition_template = "year={%Y}/month={%m}/day={%d}"
# file_prefix = "pgtrickle"
# file_suffix_format = "{timestamp}_{batch_id}"

# Batching (buffer locally, flush periodically)
buffer_max_rows = 100000                   # rows per file
buffer_max_bytes = 268435456               # 256 MiB per file
buffer_max_seconds = 300                   # flush every 5 minutes

# Credentials (default: from environment/instance profile)
# aws_access_key_id = "..."
# aws_secret_access_key = "..."
# gcs_credentials_file = "/etc/gcp/sa.json"
# azure_connection_string = "..."
```

**Partitioning:** Files are organised into date-based or custom partition
layouts compatible with Hive-style partitioning (`year=2026/month=04/...`).

**Parquet support:** Uses the `parquet` crate to write columnar files.
Schema is inferred from the first batch or a user-provided schema file.

**Exactly-once file writes:** Each file is written to a temporary prefix,
then renamed atomically (S3 CopyObject + DeleteObject; GCS compose; Azure
staged upload). On relay crash, orphaned temp files are cleaned up on
restart.

**Full-refresh handling:** Writes a complete snapshot file with a
`_full_refresh` suffix. Downstream consumers can detect and handle
accordingly.

### A.9 ClickHouse

**Why:** Fastest-growing OLAP database. Ideal destination for pg-trickle
change data — enables real-time analytics dashboards over streaming data.
ClickHouse is deployed by Cloudflare, Uber, eBay, Spotify, and thousands
of companies for real-time analytics.

**Crate:** `clickhouse` (official Rust client)

**Direction:** Sink only (write — analytics database, not a message source)

#### Sink Configuration

```toml
[sink.clickhouse]
url = "http://localhost:8123"
database = "default"
table = "pgtrickle_events"
# table_template = "pgtrickle_{stream_table}"
# username = "default"
# password = ""
# insert_batch_size = 10000                # rows per INSERT
# insert_timeout_ms = 30000
# compression = "lz4"                      # none | lz4
# async_insert = true                      # ClickHouse async inserts
# wait_for_async_insert = false
# tls_ca = "/etc/clickhouse/ca.pem"
```

**Operations mapping:**
- `op = "insert"` → ClickHouse `INSERT` into a `ReplacingMergeTree` or
  `CollapsingMergeTree` (depending on user's table engine)
- `op = "delete"` → Insert with sign=-1 for `CollapsingMergeTree`, or
  use `ALTER TABLE ... DELETE` for `MergeTree`
- `is_full_refresh = true` → `TRUNCATE TABLE` + bulk `INSERT`

**Engine recommendations (documented, not enforced):**
- `ReplacingMergeTree` for upsert semantics (most common)
- `CollapsingMergeTree` for efficient deletes with insert+delete pairs
- `MergeTree` for append-only event logs

**Async inserts:** ClickHouse can buffer inserts server-side for higher
throughput. The relay supports this via the `async_insert` setting.

### A.10 Apache Arrow Flight / gRPC

**Why:** Language-agnostic, high-performance columnar data exchange.
Emerging standard for data movement between systems. Used by Dremio,
Databricks, DuckDB, and Ballista. Enables pg-trickle to feed any
Arrow Flight-compatible consumer without serialisation overhead.

**Crate:** `arrow-flight` + `tonic`

**Direction:** Sink (server or client mode) + Source (client mode)

#### Sink Configuration (Client Mode — Push to Flight Server)

```toml
[sink.arrow-flight]
url = "grpc://localhost:50051"
# tls = false
# auth_token = "Bearer ..."
# metadata:
#   x-custom-header = "value"

# Batching
batch_size = 10000                         # rows per RecordBatch
# compression = "zstd"                     # none | lz4 | zstd
```

#### Sink Configuration (Server Mode — Serve to Flight Clients)

```toml
[sink.arrow-flight-server]
listen_addr = "0.0.0.0:50051"
# tls_cert = "/etc/flight/cert.pem"
# tls_key = "/etc/flight/key.pem"
# max_batch_age_seconds = 5               # buffer window before serving
```

**Server mode** turns the relay into an Arrow Flight server that downstream
consumers connect to. Useful for feeding Spark, DuckDB, or custom analytics
pipelines without intermediate storage.

#### Source Configuration

```toml
[source.arrow-flight]
url = "grpc://upstream:50051"
# ticket = "my-stream-ticket"
# auth_token = "Bearer ..."
```

**Schema handling:** Arrow schemas are derived from the JSON payload
structure. For stable schemas, a user-provided `.arrow` schema file is
supported.

---

### Backend Priority Matrix

Backends ranked by estimated demand, ecosystem coverage, and implementation
effort.

| Backend | Direction | Cloud Parity | Popularity | Effort | Priority |
|---------|-----------|-------------|------------|--------|----------|
| **Google Cloud Pub/Sub** | Source + Sink | GCP | ★★★★★ | 2d | **P1** |
| **Amazon Kinesis** | Source + Sink | AWS | ★★★★☆ | 2.5d | **P1** |
| **Azure Service Bus** | Source + Sink | Azure | ★★★★☆ | 2d | **P1** |
| **Elasticsearch / OpenSearch** | Sink | Analytics | ★★★★★ | 2.5d | **P1** |
| **MQTT (v5)** | Source + Sink | IoT | ★★★★☆ | 1.5d | **P2** |
| **Azure Event Hubs** | Source + Sink | Azure | ★★★☆☆ | 1.5d | **P2** |
| **Object Storage (S3/GCS/Blob)** | Sink | Data Lake | ★★★★☆ | 3d | **P2** |
| **ClickHouse** | Sink | Analytics | ★★★★☆ | 1.5d | **P2** |
| **Apache Pulsar** | Source + Sink | Streaming | ★★★☆☆ | 2d | **P3** |
| **Arrow Flight / gRPC** | Source + Sink | Emerging | ★★☆☆☆ | 2.5d | **P3** |

**Priority key:**
- **P1** — Must-have. Covers cloud platform parity (GCP, AWS streaming,
  Azure) and the most-requested analytics use-case (Elasticsearch).
- **P2** — High-value. Covers IoT, data lake, and OLAP analytics.
- **P3** — Forward-looking. Emerging standards with growing adoption.

---

## Part B — CLI Extensions & Improvements

### B.1 Dead-Letter Queue (DLQ)

**Problem:** Phase 1 logs and skips poison messages. Production deployments
need a way to inspect, retry, and manage failed messages.

**Design:** A DLQ table in PostgreSQL stores failed messages with error
context. Failed messages are moved to the DLQ instead of being silently
skipped.

```sql
CREATE TABLE pgtrickle.relay_dlq (
    id              BIGSERIAL PRIMARY KEY,
    relay_mode      TEXT NOT NULL,         -- 'forward' | 'reverse'
    source_name     TEXT NOT NULL,         -- e.g. 'outbox:order_events'
    sink_name       TEXT NOT NULL,         -- e.g. 'nats'
    dedup_key       TEXT NOT NULL,
    subject         TEXT,
    payload         JSONB NOT NULL,
    error_message   TEXT NOT NULL,
    error_kind      TEXT NOT NULL,         -- 'decode' | 'sink_permanent' | 'inbox_permanent'
    attempt_count   INT NOT NULL DEFAULT 1,
    first_failed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_failed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    retried_at      TIMESTAMPTZ,          -- set when retried
    resolved        BOOLEAN NOT NULL DEFAULT false
);
```

**CLI commands:**

```
pgtrickle-relay dlq list          # Show pending DLQ entries
pgtrickle-relay dlq retry <id>    # Retry a specific message
pgtrickle-relay dlq retry-all     # Retry all pending messages
pgtrickle-relay dlq purge         # Delete resolved entries older than N days
pgtrickle-relay dlq stats         # DLQ summary (counts by error_kind)
```

**Configuration:**

```toml
[dlq]
enabled = true                            # default: false in Phase 1
max_retries = 5                           # max automatic retries before DLQ
retry_delay_seconds = 60                  # delay between automatic retries
retention_days = 30                       # auto-purge resolved entries
```

**Effort:** 2d

### B.2 Schema Registry Integration (Avro / Protobuf)

**Problem:** Phase 1 passes JSON payloads through as-is. Many organisations
use Avro or Protobuf with a schema registry (Confluent, Apicurio, AWS Glue)
for type safety, schema evolution, and compact wire formats.

**Design:** The relay can serialise outgoing messages to Avro/Protobuf
using a schema registry, and deserialise incoming messages from Avro/Protobuf
to JSON for inbox insertion.

```toml
[schema_registry]
url = "http://localhost:8081"             # Confluent Schema Registry
# type = "confluent"                      # confluent | apicurio | aws-glue
# username = "user"
# password = "pass"

[forward.serialization]
format = "avro"                           # json (default) | avro | protobuf
# subject_name_strategy = "TopicName"     # TopicName | RecordName | TopicRecordName
# schema_id = 1                           # pin to a specific schema version
# auto_register = true                    # register schema if not found

[reverse.deserialization]
format = "avro"                           # json (default) | avro | protobuf
# Convert to JSON before inbox insert (always — inbox stores JSONB)
```

**Crates:** `apache-avro`, `schema_registry_converter`, `prost`

**Effort:** 3d

### B.3 Message Transforms (JMESPath)

**Problem:** Users sometimes need lightweight payload transformations:
field selection, renaming, or simple computations — without deploying a
separate stream processor.

**Design:** Optional JMESPath expressions that transform the payload
before publishing to the sink (forward) or writing to the inbox (reverse).

```toml
[transform]
# Select specific fields
payload = "{ order_id: id, customer: customer_name, total: amount }"

# Filter: only relay messages matching a condition
filter = "status == 'completed'"

# Add computed fields
enrich = "merge(@, { relayed_at: `now()` })"
```

**Scope limitation:** Transforms are intentionally lightweight. For complex
ETL, users should use the stream table query itself or a downstream
processor (Kafka Streams, Flink, etc.).

**Crate:** `jmespath`

**Effort:** 1.5d

### B.4 Content-Based Routing

**Problem:** Phase 1 uses static subject/topic templates. Production
deployments often need to route messages to different topics based on
payload content (e.g. different Kafka topics per event type).

**Design:** Routing rules that evaluate payload fields and map to
different subjects/topics.

```toml
[routing]
# Default template (Phase 1 — still supported)
subject_template = "pgtrickle.{stream_table}"

# Content-based routing rules (evaluated in order, first match wins)
[[routing.rules]]
match = "event_type == 'order.created'"
subject = "orders.created"

[[routing.rules]]
match = "event_type == 'order.shipped'"
subject = "orders.shipped"

[[routing.rules]]
match = "priority > 5"
subject = "high-priority.{stream_table}"

# Fallback if no rule matches
[routing.default]
subject = "pgtrickle.unmatched"
```

**Expression language:** Reuses JMESPath (same dependency as B.3) for
match conditions.

**Effort:** 1d

### B.5 Rate Limiting & Back-Pressure

**Problem:** A fast outbox poller can overwhelm a slow sink or exceed
API rate limits (e.g. webhook endpoints, SQS quotas).

**Design:** Configurable rate limiter applied between the relay loop
and the sink.

```toml
[rate_limit]
enabled = true
max_messages_per_second = 1000            # 0 = unlimited
max_bytes_per_second = 10485760           # 10 MiB/s; 0 = unlimited
burst_size = 2000                         # allow short bursts above the limit
# strategy = "token_bucket"               # token_bucket | sliding_window
```

**Back-pressure propagation:** When the rate limiter is full, the relay
pauses polling from the source. This propagates back-pressure upstream:

- **Forward:** Outbox poller sleeps, leaving rows in the outbox for
  other consumers or future polls.
- **Reverse:** Source broker retains messages until the relay is ready.

**Crate:** `governor`

**Effort:** 1d

### B.6 Circuit Breaker

**Problem:** When a sink is down, the relay retries indefinitely with
exponential backoff. In a multi-pipeline deployment, a single broken
sink can cause cascading issues.

**Design:** Circuit breaker pattern with configurable thresholds.

```toml
[circuit_breaker]
enabled = true
failure_threshold = 5                     # open circuit after 5 consecutive failures
success_threshold = 3                     # close circuit after 3 consecutive successes
half_open_timeout_seconds = 30            # try one request after this delay
reset_timeout_seconds = 300               # force reset after 5 minutes
```

**States:**
- **Closed:** Normal operation. Failures increment the counter.
- **Open:** All publish attempts fail immediately. Emit
  `relay_circuit_breaker_state{state="open"}` metric.
- **Half-open:** Allow one request through. On success, close; on failure,
  re-open.

**Integration with DLQ:** When the circuit is open, messages are routed
to the DLQ (if enabled) rather than blocking the relay loop.

**Effort:** 1d

### B.7 Multi-Pipeline Mode

**Problem:** Phase 1 supports one source → one sink per process. Some
deployments need multiple pipelines (e.g. forward order_events to Kafka
AND webhook simultaneously, or relay three outboxes to different sinks).

**Design:** A `pipelines` config array that defines multiple independent
relay loops in a single process.

```toml
[[pipelines]]
name = "orders-to-kafka"
mode = "forward"
outbox = "order_events"
sink = "kafka"
group = "orders-kafka"

[[pipelines]]
name = "orders-to-webhook"
mode = "forward"
outbox = "order_events"
sink = "webhook"
group = "orders-webhook"

[[pipelines]]
name = "iot-to-inbox"
mode = "reverse"
source = "mqtt"
inbox = "device_telemetry"

[postgres]
url = "postgres://..."

[sink.kafka]
brokers = "localhost:9092"
topic = "orders"

[sink.webhook]
url = "https://api.example.com/orders"

[source.mqtt]
url = "mqtt://broker:1883"
topic = "devices/+/telemetry"
```

Each pipeline runs as an independent tokio task with its own metrics
labels (`pipeline=<name>`). Shared connections (PostgreSQL, metrics
endpoint) are pooled.

**Effort:** 2d

### B.8 Config Hot-Reload

**Problem:** Changing relay configuration requires a process restart.
In Kubernetes, this means a rolling restart of the relay pods.

**Design:** Watch the config file for changes (via `inotify` / `kqueue`)
and apply non-breaking changes without restart.

**Hot-reloadable settings:**
- Routing rules and subject templates
- Rate limit thresholds
- Log level
- Batch size and poll interval
- Transform expressions
- Circuit breaker thresholds

**Not hot-reloadable** (require restart):
- PostgreSQL connection string
- Sink/source backend type
- Pipeline additions/removals
- Metrics endpoint address
- TLS certificates

```toml
[config]
hot_reload = true                         # default: false
watch_interval_seconds = 5                # polling fallback if inotify unavailable
```

**Signal-based reload:** Also supports `SIGHUP` to trigger a manual
config reload, matching common Unix daemon conventions.

**Effort:** 1.5d

### B.9 Dry-Run & Replay Mode

**Problem:** Operators need to test relay configuration without actually
publishing messages. They also need to replay historical outbox entries
(e.g. after adding a new sink).

#### Dry-Run Mode

```bash
pgtrickle-relay forward --dry-run
```

- Polls the outbox and logs what would be published (subject, dedup key,
  payload size) without actually publishing.
- Validates all config, connections, and permissions.
- Useful for testing routing rules, transforms, and new sink configs.

#### Replay Mode

```bash
pgtrickle-relay forward --replay --from-offset 1000 --to-offset 5000
```

- Re-reads outbox entries from a specific offset range.
- Does **not** use consumer groups (no offset commits).
- Combined with `--dry-run`, shows what would have been published.
- Useful for backfilling a new sink with historical data.

```bash
pgtrickle-relay reverse --replay --input events-2026-04.jsonl
```

- Re-reads messages from a file and writes them to the inbox.
- Uses `ON CONFLICT DO NOTHING` to skip duplicates safely.

**Effort:** 1d

### B.10 OpenTelemetry Tracing

**Problem:** Phase 1 uses structured logging and Prometheus metrics.
Production deployments with distributed tracing (Jaeger, Tempo, Datadog)
need OpenTelemetry spans to correlate relay activity with upstream and
downstream systems.

**Design:** Optional OpenTelemetry integration using the `opentelemetry`
crate ecosystem.

```toml
[telemetry]
enabled = true
exporter = "otlp"                         # otlp | jaeger | stdout
endpoint = "http://localhost:4317"        # OTLP gRPC endpoint
# service_name = "pgtrickle-relay"
# sample_rate = 1.0                       # 1.0 = trace everything
# propagation = "w3c"                     # w3c | b3 | jaeger
```

**Trace structure:**
- Root span: `relay.poll` (one per poll cycle)
  - Child span: `source.poll` (time spent polling source)
  - Child span: `sink.publish` (time spent publishing batch)
  - Child span: `source.acknowledge` (time spent acking)

**Context propagation:** For HTTP webhook sink/source, propagates trace
context via W3C `traceparent` header. For Kafka, uses `traceparent` header
on Kafka records. For NATS, uses `traceparent` NATS header.

**Crates:** `opentelemetry`, `opentelemetry-otlp`, `tracing-opentelemetry`

**Effort:** 1.5d

### B.11 Relay TUI Dashboard

**Problem:** The `pgtrickle-tui` provides a dashboard for stream tables.
A similar dashboard for the relay would help operators monitor pipeline
health, throughput, and errors in real-time.

**Design:** Extend `pgtrickle-tui` with a relay dashboard tab, or add a
`pgtrickle-relay dashboard` subcommand.

**Dashboard panels:**
- Pipeline overview (mode, source, sink, status)
- Throughput graph (messages/sec, bytes/sec)
- Latency graph (p50, p95, p99 poll-to-publish)
- Consumer lag gauge
- Error rate and recent errors
- DLQ status (if enabled)
- Circuit breaker state
- Active connections health

**Implementation:** Reuse the `ratatui` framework from `pgtrickle-tui`.
Read metrics from the relay's Prometheus endpoint (scrape `/metrics`).

**Effort:** 2d

### B.12 Plugin System (Dynamic Backends)

**Problem:** The compiled-in backend approach requires users to rebuild
the relay binary to add custom backends. Some organisations have
proprietary messaging systems or custom protocols.

**Design:** A WASM-based plugin system using `wasmtime` for dynamic
backend loading.

```toml
[plugins]
[[plugins.sinks]]
name = "custom-crm"
path = "/opt/plugins/crm-sink.wasm"
config = { api_url = "https://crm.internal/events", api_key = "..." }

[[plugins.sources]]
name = "proprietary-mq"
path = "/opt/plugins/pmq-source.wasm"
config = { broker = "pmq://internal:9999" }
```

**Plugin interface:** A WASM component model interface that mirrors the
Source/Sink traits:

```wit
interface sink {
    record relay-message {
        dedup-key: string,
        subject: string,
        payload: string,
        op: string,
    }

    resource sink-instance {
        constructor(config: string);
        connect: func() -> result<_, string>;
        publish: func(batch: list<relay-message>) -> result<u32, string>;
        is-healthy: func() -> bool;
        close: func() -> result<_, string>;
    }
}
```

**Trade-off:** This is the most complex feature. Consider deferring to
Phase 3 unless there is clear demand.

**Effort:** 5d

### B.13 Encryption Envelope

**Problem:** Some compliance regimes (HIPAA, PCI-DSS, GDPR) require
payload encryption in transit even when TLS is in use (defence in depth).

**Design:** Optional envelope encryption before publishing to sink.
Messages are encrypted with a data encryption key (DEK), and the DEK
is encrypted with a key encryption key (KEK) from a KMS.

```toml
[encryption]
enabled = true
provider = "aws-kms"                      # aws-kms | gcp-kms | azure-keyvault | local
# key_id = "arn:aws:kms:us-east-1:123456789012:key/..."
# local_key_file = "/etc/relay/encryption.key"   # 256-bit AES key
algorithm = "aes-256-gcm"
# encrypt_fields = ["payload"]            # default: encrypt entire message
# key_rotation_interval_hours = 24
```

**Envelope format:**

```json
{
  "v": 1,
  "enc": "aes-256-gcm",
  "dek": "<base64-encrypted-DEK>",
  "iv": "<base64-IV>",
  "ct": "<base64-ciphertext>",
  "tag": "<base64-auth-tag>"
}
```

Consumers decrypt using the KMS to unwrap the DEK, then AES-GCM decrypt
the ciphertext.

**Effort:** 2d

### B.14 Webhook Signature Verification

**Problem:** Phase 1's webhook source accepts any POST to the configured
path. Production webhooks need cryptographic signature verification to
prevent spoofing.

**Design:** Support common webhook signature schemes.

```toml
[source.webhook]
listen_addr = "0.0.0.0:8080"
path = "/inbox"

# Signature verification (one of):
[source.webhook.signature]
scheme = "hmac-sha256"                    # hmac-sha256 | github | stripe | svix
secret = "whsec_..."
header = "X-Webhook-Signature"            # header containing the signature
# tolerance_seconds = 300                 # reject messages older than 5 min
```

**Supported schemes:**
- **HMAC-SHA256:** Generic `HMAC(secret, body)` → compare to header value
- **GitHub:** `sha256=HMAC(secret, body)` in `X-Hub-Signature-256`
- **Stripe:** Stripe-Signature header with timestamp + HMAC
- **Svix:** Standard webhook signature scheme (used by many SaaS platforms)

**Effort:** 1d

---

## Part C — Testing Strategy

### C.1 New Backend Tests (Testcontainers)

| Test | Containers | Validates |
|------|-----------|-----------|
| GCP Pub/Sub E2E | postgres + pubsub-emulator | Forward: outbox → Pub/Sub topic; Reverse: subscription → inbox |
| Kinesis E2E | postgres + localstack | Forward: outbox → stream; Reverse: stream → inbox |
| Azure Service Bus E2E | postgres + azurite (limited) | Forward: outbox → queue; Basic smoke test |
| Elasticsearch E2E | postgres + elasticsearch | Forward: outbox → index; verify _bulk API; full-refresh alias swap |
| MQTT E2E | postgres + mosquitto | Forward: outbox → topic; Reverse: subscribe → inbox |
| ClickHouse E2E | postgres + clickhouse | Forward: outbox → table; verify insert + delete semantics |
| Object Storage E2E | postgres + minio (S3-compat) | Forward: outbox → Parquet/JSONL files; verify partitioning |
| Pulsar E2E | postgres + pulsar-standalone | Forward: outbox → topic; Reverse: subscription → inbox |
| Arrow Flight E2E | postgres + test flight server | Forward: outbox → Flight stream; verify RecordBatch |

### C.2 Extension Tests

| Test | Validates |
|------|-----------|
| DLQ E2E | Poison message → DLQ table → retry → success |
| DLQ purge | Resolved entries older than retention_days are deleted |
| Schema Registry E2E | Forward: JSON → Avro → Kafka; Reverse: Avro → JSON → inbox |
| Transform E2E | JMESPath field selection + filter + enrichment |
| Content routing E2E | Match rules → correct topic; fallback → default topic |
| Rate limiter E2E | Verify throughput stays within configured limit |
| Circuit breaker E2E | Failure threshold → open → half-open → close |
| Multi-pipeline E2E | Two pipelines in one process, both relaying correctly |
| Config hot-reload E2E | Change routing rules → verify new routes without restart |
| Dry-run E2E | Verify no messages published; verify output logs |
| Replay E2E | Replay from offset range → verify correct messages relayed |
| OTel tracing E2E | Verify spans exported to OTLP collector |
| Encryption E2E | Encrypt → publish → decrypt → verify payload |
| Webhook signature E2E | Signed POST accepted; unsigned POST rejected with 401 |

### C.3 Benchmarks

| Benchmark | Target |
|-----------|--------|
| GCP Pub/Sub throughput | 50K+ events/sec forward |
| Elasticsearch bulk indexing | 20K+ docs/sec via _bulk API |
| ClickHouse insert throughput | 100K+ rows/sec via native protocol |
| Object Storage write | 10K+ events/sec with Parquet batching |
| Multi-pipeline overhead | <5% throughput degradation vs single pipeline |
| Rate limiter accuracy | ±5% of configured limit |
| Transform overhead | <1ms per message for simple JMESPath |

---

## Part D — Implementation Roadmap

### Phase 2a — Cloud Provider Parity (9 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P2-1 | Sink + Source: Google Cloud Pub/Sub | 2d |
| RELAY-P2-2 | Sink + Source: Amazon Kinesis Data Streams | 2.5d |
| RELAY-P2-3 | Sink + Source: Azure Service Bus | 2d |
| RELAY-P2-4 | Sink: Elasticsearch / OpenSearch (bulk API + full-refresh alias swap) | 2.5d |

### Phase 2b — IoT, Analytics & Data Lake (8 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P2-5 | Sink + Source: MQTT v5 | 1.5d |
| RELAY-P2-6 | Sink + Source: Azure Event Hubs (native SDK) | 1.5d |
| RELAY-P2-7 | Sink: Object Storage (S3/GCS/Azure Blob) with JSONL + Parquet | 3d |
| RELAY-P2-8 | Sink: ClickHouse (native protocol, ReplacingMergeTree support) | 1.5d |
| RELAY-P2-9 | Sink + Source: Apache Pulsar | *deferred — P3* |
| RELAY-P2-10 | Sink + Source: Arrow Flight / gRPC | *deferred — P3* |

### Phase 2c — Operational Excellence (12.5 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P2-11 | Dead-letter queue (DLQ table + CLI commands + auto-retry) | 2d |
| RELAY-P2-12 | Schema Registry integration (Avro + Protobuf, Confluent SR) | 3d |
| RELAY-P2-13 | Message transforms (JMESPath payload transforms + filter) | 1.5d |
| RELAY-P2-14 | Content-based routing (match rules + fallback) | 1d |
| RELAY-P2-15 | Rate limiting & back-pressure (token bucket + back-pressure) | 1d |
| RELAY-P2-16 | Circuit breaker (failure/success thresholds, DLQ integration) | 1d |
| RELAY-P2-17 | Multi-pipeline mode (multiple source→sink pairs in one process) | 2d |
| RELAY-P2-18 | Config hot-reload (file watch + SIGHUP) | 1.5d |
| RELAY-P2-19 | Dry-run & replay mode (--dry-run, --replay, --from-offset) | 1d |
| RELAY-P2-20 | OpenTelemetry tracing (OTLP export + context propagation) | 1.5d |
| RELAY-P2-21 | Webhook signature verification (HMAC, GitHub, Stripe, Svix) | 1d |

### Phase 2d — Testing & Polish (6 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P2-22 | Backend integration tests (Pub/Sub, Kinesis, Service Bus, ES, MQTT, ClickHouse, S3) | 3d |
| RELAY-P2-23 | Extension integration tests (DLQ, schema registry, transforms, routing, rate limit, circuit breaker, multi-pipeline, hot-reload, dry-run, replay, OTel, encryption, webhook sig) | 2d |
| RELAY-P2-24 | Benchmarks (new backends + extensions overhead) | 1d |

### Phase 2e — Documentation & Distribution (2 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P2-25 | Documentation updates (new backends, extensions, config reference) | 1d |
| RELAY-P2-26 | Docker image with all features + updated CI matrix | 1d |

### Deferred to Phase 3

| Item | Description | Rationale |
|------|-------------|-----------|
| RELAY-P3-1 | Apache Pulsar backend | Lower demand than cloud-native alternatives |
| RELAY-P3-2 | Arrow Flight / gRPC backend | Emerging standard, not yet mainstream |
| RELAY-P3-3 | Relay TUI dashboard | Nice-to-have, operators can use Grafana |
| RELAY-P3-4 | Plugin system (WASM backends) | High complexity, unclear demand |
| RELAY-P3-5 | Encryption envelope (KMS integration) | Niche compliance requirement |
| RELAY-P3-6 | MongoDB sink | Lower priority than streaming/analytics backends |
| RELAY-P3-7 | Snowflake / BigQuery sink | Cloud data warehouse integration |

---

### Effort Summary

| Phase | Effort |
|-------|--------|
| Phase 2a — Cloud Provider Parity | 9d |
| Phase 2b — IoT, Analytics & Data Lake | 7.5d |
| Phase 2c — Operational Excellence | 16.5d |
| Phase 2d — Testing & Polish | 6d |
| Phase 2e — Documentation & Distribution | 2d |
| **Total** | **~41 days solo / ~26 days with two developers** |

Phases 2a and 2b (backends) can be parallelised with Phase 2c (extensions).
With two developers, one focuses on backends while the other builds
operational features.

### Dependencies

- **Requires Phase 1 (v0.24.0)** — all Phase 2 work builds on the
  Source/Sink trait framework, relay loop, config system, and metrics
  from Phase 1.
- **Schema Registry (B.2)** requires access to a Confluent Schema Registry
  or Apicurio instance for integration testing.
- **Cloud backends (A.1–A.4)** require emulators or test accounts:
  - GCP Pub/Sub: `google/cloud-sdk` emulator
  - AWS Kinesis: LocalStack
  - Azure Service Bus: Limited emulator support — may need a test instance
- **Object Storage (A.8)** uses MinIO for S3-compatible testing.
- **ClickHouse (A.9)** uses the official Docker image.

---

## Open Questions

| # | Question | Options | Recommendation |
|---|----------|---------|----------------|
| 1 | Should Phase 2 be a single release or split across versions? | (a) Single v0.25.0, (b) Split: v0.25.0 (backends) + v0.26.0 (extensions) | **(b)** — split reduces risk. Ship backends first, extensions second. |
| 2 | Should Event Hubs use the native SDK or just document Kafka protocol? | (a) Native SDK, (b) Document Kafka config only | **(a)** — native SDK for better Azure identity integration. Document Kafka as alternative. |
| 3 | Should transforms support languages beyond JMESPath? | (a) JMESPath only, (b) Add CEL (Common Expression Language) | **(a)** for Phase 2. JMESPath is well-known and sufficient. |
| 4 | Should the plugin system use WASM or shared libraries (.so/.dylib)? | (a) WASM (sandboxed, portable), (b) Shared libs (faster, easier) | **(a)** — WASM is safer and more portable. But defer to Phase 3. |
| 5 | Should multi-pipeline share PostgreSQL connections via a pool? | (a) Shared pool (fewer connections), (b) Separate connections per pipeline | **(a)** — use `deadpool-postgres` for connection pooling across pipelines. |
| 6 | Priority: Schema Registry or DLQ first? | (a) DLQ, (b) Schema Registry | **(a)** — DLQ is universally needed. Schema Registry is use-case dependent. |
| 7 | Should Elasticsearch full-refresh use alias rotation or delete+reindex? | (a) Alias rotation (zero-downtime), (b) Delete + reindex | **(a)** — alias rotation is the standard Elasticsearch practice. |
| 8 | Should we support ClickHouse via HTTP or native protocol? | (a) HTTP (simpler), (b) Native (faster) | **(a)** for Phase 2 — HTTP is simpler and well-supported by the `clickhouse` crate. |
