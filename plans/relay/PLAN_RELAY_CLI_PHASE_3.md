# Relay CLI — Phase 3 Plan

> **Status:** Proposal (DRAFT)
> **Created:** 2026-05-03
> **Category:** Tooling — Relay CLI Extension
> **Depends on:** [PLAN_RELAY_CLI_PHASE_2.md](./PLAN_RELAY_CLI_PHASE_2.md)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Part A — Connector Ecosystem Integrations](#part-a--connector-ecosystem-integrations)
  - [A.1 Airbyte Protocol Adapter](#a1-airbyte-protocol-adapter)
  - [A.2 dlt Integration](#a2-dlt-integration)
  - [A.3 Redpanda Connect / Benthos](#a3-redpanda-connect--benthos)
  - [A.4 Fivetran HVR Endpoint](#a4-fivetran-hvr-endpoint)
  - [Connector Priority Matrix](#connector-priority-matrix)
- [Part B — Additional Backends](#part-b--additional-backends)
  - [B.1 Apache Pulsar](#b1-apache-pulsar)
  - [B.2 Apache Arrow Flight / gRPC](#b2-apache-arrow-flight--grpc)
  - [B.3 AMQP 1.0 (Azure Service Bus, Qpid)](#b3-amqp-10)
  - [B.4 MongoDB Sink](#b4-mongodb-sink)
  - [B.5 Snowflake / BigQuery Sink](#b5-snowflake--bigquery-sink)
- [Part C — Advanced Features](#part-c--advanced-features)
  - [C.1 Relay Dashboard (ratatui)](#c1-relay-dashboard)
  - [C.2 Plugin System (WASM Backends)](#c2-plugin-system-wasm-backends)
  - [C.3 Encryption Envelope (KMS)](#c3-encryption-envelope-kms)
- [Part D — Testing Strategy](#part-d--testing-strategy)
- [Part E — Implementation Roadmap](#part-e--implementation-roadmap)
- [Open Questions](#open-questions)

---

## Executive Summary

Phase 3 of the `pgtrickle-relay` extends the relay into a universal connector
hub by integrating with the major data engineering connector ecosystems:
**Airbyte**, **dlt**, **Redpanda Connect (Benthos)**, and **Fivetran**.

Phase 2 ships the **Singer protocol adapter** — the highest-value single
integration — which establishes the subprocess I/O and JSON-lines parsing
infrastructure that Phase 3 builds on. Airbyte's protocol is ~95% identical
to Singer, making it a thin translation layer. dlt and Redpanda Connect use
HTTP/REST and NDJSON respectively, both lightweight to integrate.

Phase 3 also picks up backends and features deferred from Phase 2: Apache
Pulsar, Arrow Flight, AMQP 1.0, MongoDB, Snowflake/BigQuery, the relay
dashboard, the WASM plugin system, and encryption envelopes.

**Combined ecosystem reach:**

| Ecosystem | Connectors | Coverage |
|-----------|------------|----------|
| Singer / Meltano Hub | ~500 taps + targets | SaaS APIs, databases, warehouses, file formats |
| Airbyte | ~400 connectors | SaaS APIs, databases, warehouses (Docker-based) |
| dlt | ~100+ verified sources | SaaS APIs, REST APIs, databases (Python-native) |
| Redpanda Connect | ~200 inputs/outputs | Message brokers, databases, cloud services, APIs |
| Fivetran | ~300+ connectors | SaaS APIs, databases (managed, proprietary) |
| **Total unique reach** | **~1000+ unique connectors** | Virtually any data source or destination |

---

## Part A — Connector Ecosystem Integrations

All connector ecosystem adapters share common infrastructure established
by the Singer adapter in Phase 2:
- Subprocess lifecycle management (spawn, monitor, restart)
- JSON-lines stdin/stdout I/O
- State checkpointing in PostgreSQL
- DLQ integration for malformed/failed messages
- Crash recovery from last checkpoint

### A.1 Airbyte Protocol Adapter

**Why:** Airbyte is the dominant open-source EL platform with ~400 connectors
(sources and destinations). Its protocol is nearly identical to Singer — both
use newline-delimited JSON messages over stdin/stdout — but with different
message types and richer metadata. Since Phase 2 ships the Singer adapter,
Airbyte support is a thin translation layer.

**Protocol:** [Airbyte Protocol](https://docs.airbyte.com/understanding-airbyte/airbyte-protocol)

**Key differences from Singer:**

| Concept | Singer | Airbyte |
|---------|--------|---------|
| Schema message | `SCHEMA` | `AirbyteMessage.CATALOG` |
| Data message | `RECORD` | `AirbyteMessage.RECORD` (`AirbyteRecordMessage`) |
| State message | `STATE` | `AirbyteMessage.STATE` (`AirbyteStateMessage`) |
| Control message | — | `AirbyteMessage.CONTROL` (connector lifecycle) |
| Log message | — | `AirbyteMessage.LOG` (structured logging) |
| Trace message | — | `AirbyteMessage.TRACE` (error/estimate reporting) |
| Connector packaging | Python executable | Docker container |
| Config format | JSON file | JSON file (with spec validation) |

**Direction:** Source + Sink (bidirectional)

#### Sink Configuration (Forward: outbox → Airbyte Destination)

```json
{
  "sink_type": "airbyte",
  "sink": {
    "destination_image": "airbyte/destination-bigquery:latest",
    "destination_config": {
      "project_id": "my-project",
      "dataset_id": "pgtrickle",
      "credentials_json": "..."
    },
    "stream_name_template": "{stream_table}",
    "namespace": "pgtrickle",
    "sync_mode": "append",
    "batch_size": 1000
  }
}
```

**Message mapping:**
- Delta batch start → `AirbyteMessage.CATALOG` with `AirbyteCatalog`
  containing stream schema (JSON Schema from delta columns).
- Each delta row → `AirbyteMessage.RECORD` with `AirbyteRecordMessage`:
  ```json
  {
    "type": "RECORD",
    "record": {
      "stream": "orders_stream",
      "namespace": "pgtrickle",
      "data": { /* delta columns */ },
      "emitted_at": 1714700000000
    }
  }
  ```
- After batch → `AirbyteMessage.STATE` with consumer group offset.

**Operations mapping:**
- `op = "insert"` → RECORD with `_airbyte_emitted_at`
- `op = "delete"` → RECORD with `_ab_cdc_deleted_at` set (Airbyte CDC
  convention for soft deletes)
- `is_full_refresh = true` → Sync mode set to `overwrite` in catalog

**Docker integration:** Unlike Singer (bare executables), Airbyte connectors
are Docker images. The relay:
1. Pulls the destination image if not present.
2. Runs it with `docker run --rm -i` and pipes stdin/stdout.
3. Passes config via a mounted temp file.
4. Captures stdout (STATE/LOG/TRACE messages) and stderr (connector logs).

**Non-Docker mode:** For environments without Docker (e.g., Kubernetes
sidecars), the relay also supports bare Airbyte Python connectors via
`destination_command` (same as Singer's `target_command`).

#### Source Configuration (Reverse: Airbyte Source → inbox)

```json
{
  "source_type": "airbyte",
  "source": {
    "source_image": "airbyte/source-stripe:latest",
    "source_config": {
      "account_id": "acct_...",
      "client_secret": "..."
    },
    "configured_catalog": {
      "streams": [
        {
          "stream": { "name": "charges", "namespace": "stripe" },
          "sync_mode": "incremental",
          "cursor_field": ["created"]
        }
      ]
    }
  }
}
```

**Consumption model:**
1. Relay spawns the Airbyte source container.
2. Reads stdout for `AirbyteMessage` JSON lines.
3. `CATALOG` → stored for schema validation.
4. `RECORD` → converted to inbox rows, batch-inserted.
5. `STATE` → checkpointed to `pgtrickle.relay_airbyte_state`.
6. `LOG` → forwarded to relay structured logging.
7. `TRACE` → error traces logged; estimate traces used for progress metrics.

**State management:** Same pattern as Singer — state stored in PostgreSQL,
passed to the source on restart via `--state` (or mounted file for Docker).

**Effort:** 2.5d (leverages Singer subprocess infrastructure)

### A.2 dlt Integration

**Why:** [dlt](https://dlthub.com) is the fastest-growing Python-first EL
tool in the modern data stack. It has strong synergy with dbt (which
pg-trickle already supports via `dbt-pgtrickle`), a clean REST API source
pattern, and ~100+ verified sources. Unlike Singer/Airbyte (subprocess
protocols), dlt is best integrated via its REST API or as a library.

**Two integration modes:**

#### Mode 1: dlt REST API Source (dlt → relay → inbox)

dlt exposes data via its [REST API source](https://dlthub.com/docs/dlt-ecosystem/verified-sources/rest_api)
pattern. The relay can act as a dlt destination by accepting HTTP POSTs
from a dlt pipeline.

```json
{
  "source_type": "dlt",
  "source": {
    "mode": "http_receiver",
    "listen_addr": "0.0.0.0:8090",
    "path": "/dlt/inbox",
    "auth_token": "Bearer dlt_...",
    "inbox_table": "dlt_events"
  }
}
```

The dlt pipeline pushes data to the relay's HTTP endpoint:

```python
import dlt

pipeline = dlt.pipeline(
    pipeline_name="salesforce_to_pgtrickle",
    destination=dlt.destinations.http(
        url="http://relay:8090/dlt/inbox",
        headers={"Authorization": "Bearer dlt_..."}
    )
)

# Any dlt source → relay inbox → stream table
pipeline.run(salesforce_source())
```

#### Mode 2: dlt File Export Sink (relay → dlt-compatible files)

The relay writes delta batches as dlt-compatible JSONL or Parquet files
that a dlt pipeline can pick up for loading into any dlt destination.

```json
{
  "sink_type": "dlt",
  "sink": {
    "mode": "file_export",
    "output_dir": "/var/lib/pgtrickle-relay/dlt-export",
    "format": "jsonl",
    "schema_name": "pgtrickle",
    "table_name_template": "{stream_table}",
    "include_dlt_metadata": true
  }
}
```

**dlt metadata columns added automatically:**
- `_dlt_load_id` — unique batch identifier
- `_dlt_id` — row-level unique ID (from dedup key)
- `_dlt_extracted_at` — extraction timestamp

A companion dlt pipeline then loads these files:

```python
import dlt
from dlt.sources.filesystem import filesystem

pipeline = dlt.pipeline(destination="bigquery")
pipeline.run(
    filesystem(bucket_url="/var/lib/pgtrickle-relay/dlt-export")
)
```

**Effort:** 2d

### A.3 Redpanda Connect / Benthos

**Why:** [Redpanda Connect](https://www.redpanda.com/connect) (formerly
Benthos) is a stream processing tool with ~200 built-in inputs, outputs,
and processors. It's widely used as a data plumbing layer alongside Kafka
and Redpanda. Integration allows pg-trickle deltas to flow through any
Benthos pipeline and vice versa.

**Two integration modes:**

#### Mode 1: HTTP Bridge (relay ↔ Benthos HTTP input/output)

The relay acts as an HTTP source/sink for Benthos pipelines. This is the
simplest integration — no special protocol, just NDJSON over HTTP.

**Sink (relay → Benthos):**

```json
{
  "sink_type": "benthos",
  "sink": {
    "mode": "http_push",
    "url": "http://benthos:4195/post",
    "format": "ndjson",
    "batch_size": 1000,
    "headers": {
      "Content-Type": "application/x-ndjson",
      "X-PgTrickle-Stream": "{stream_table}"
    }
  }
}
```

**Source (Benthos → relay):**

```json
{
  "source_type": "benthos",
  "source": {
    "mode": "http_receive",
    "listen_addr": "0.0.0.0:8091",
    "path": "/benthos/inbox",
    "format": "ndjson",
    "inbox_table": "benthos_events"
  }
}
```

#### Mode 2: stdin/stdout Bridge (relay spawns Benthos subprocess)

For tighter integration, the relay can spawn a Benthos process with
`stdin`/`stdout` input/output, piping delta messages through a Benthos
processing pipeline.

```json
{
  "sink_type": "benthos",
  "sink": {
    "mode": "subprocess",
    "command": "redpanda-connect",
    "args": ["run", "/etc/benthos/pipeline.yaml"],
    "format": "ndjson"
  }
}
```

Example Benthos pipeline config (`pipeline.yaml`):
```yaml
input:
  stdin:
    codec: lines

pipeline:
  processors:
    - mapping: |
        root = this
        root.processed_at = now()

output:
  aws_s3:
    bucket: my-data-lake
    path: "pgtrickle/${!json("stream_table")}/${!count("files")}.jsonl"
```

This enables any Benthos processor (filtering, enrichment, branching) and
any Benthos output (S3, GCS, Snowflake, DynamoDB, etc.) without building
native relay backends for each.

**Effort:** 1.5d

### A.4 Fivetran HVR Endpoint

**Why:** Fivetran is the most widely used managed EL platform (~300+
connectors). While fully managed (no self-hosted components), Fivetran's
[Hybrid Deployment](https://fivetran.com/docs/getting-started/hybrid-deployment)
model uses HTTP callbacks that the relay's webhook source can serve.

**Implementation:** The relay's existing webhook source backend acts as a
Fivetran-compatible connector endpoint. This is a thin formatting layer
(similar to the n8n/Zapier webhook flavors in Phase 2).

#### Source Configuration (Fivetran → relay → inbox)

```json
{
  "source_type": "webhook",
  "source": {
    "listen_addr": "0.0.0.0:8080",
    "path": "/fivetran/webhook",
    "flavor": "fivetran",
    "fivetran_api_key": "...",
    "fivetran_api_secret": "...",
    "inbox_table": "fivetran_events"
  }
}
```

**Fivetran webhook flow:**
1. Fivetran's connector sends HTTP POST with CDC payload.
2. Relay verifies the request signature using the API secret.
3. Payload is transformed from Fivetran's format to inbox rows:
   ```json
   {
     "event": "insert",
     "schema": "public",
     "table": "orders",
     "data": { "id": 42, "amount": 99.95 },
     "before": null
   }
   ```
4. Rows are batch-inserted into the inbox table.

#### Sink Configuration (relay → Fivetran API)

For pushing deltas to Fivetran-managed destinations, use Fivetran's
[Webhook destination](https://fivetran.com/docs/destinations/webhooks):

```json
{
  "sink_type": "webhook",
  "sink": {
    "url": "https://webhooks.fivetran.com/...",
    "flavor": "fivetran",
    "fivetran_api_key": "..."
  }
}
```

**Note:** Fivetran's proprietary managed connectors are not directly
callable — this integration covers the webhook/HVR bridge patterns.
For full Fivetran connector access, users pair Fivetran with pg-trickle
via Fivetran's PostgreSQL connector (reads from stream tables directly).

**Effort:** 1d

---

### Connector Priority Matrix

| Ecosystem | Effort | Depends On | Value | Priority |
|-----------|--------|------------|-------|----------|
| **Airbyte** | 2.5d | Singer adapter (Phase 2) | ★★★★★ | **P1** |
| **dlt** | 2d | Webhook source (Phase 1) | ★★★★☆ | **P1** |
| **Redpanda Connect** | 1.5d | HTTP + subprocess infra | ★★★★☆ | **P2** |
| **Fivetran** | 1d | Webhook source (Phase 1) | ★★★☆☆ | **P2** |

**Priority rationale:**
- **Airbyte (P1)** — second-largest open-source EL ecosystem; ~95% code
  reuse from Singer adapter; the two together cover ~900 connectors.
- **dlt (P1)** — fastest-growing EL tool; natural pairing with
  `dbt-pgtrickle`; completes the modern analytics stack story.
- **Redpanda Connect (P2)** — useful for teams already using Benthos
  as streaming infrastructure; subprocess mode reuses Singer infra.
- **Fivetran (P2)** — large user base but mostly managed; webhook
  integration is a thin layer over existing backend.

---

## Part B — Additional Backends

> These backends were deferred from Phase 2 due to lower demand or higher
> complexity. See [Phase 2](./PLAN_RELAY_CLI_PHASE_2.md) for full design
> details on Pulsar and Arrow Flight.

### B.1 Apache Pulsar

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

**Effort:** 2d

### B.2 Apache Arrow Flight / gRPC

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

**Effort:** 2.5d

### B.3 AMQP 1.0

Source + Sink. Standard protocol that unlocks Azure Service Bus (native),
Apache Qpid, ActiveMQ Artemis, and other AMQP 1.0 brokers. Separate from
RabbitMQ's AMQP 0-9-1 implementation in Phase 1.

**Crate:** `fe2o3-amqp`

```json
{
  "sink_type": "amqp1",
  "sink": {
    "url": "amqps://mybus.servicebus.windows.net",
    "address": "pgtrickle-events",
    "sasl_mechanism": "PLAIN",
    "username": "...",
    "password": "..."
  }
}
```

**Effort:** 2d

### B.4 MongoDB Sink

Sink only. Writes pg-trickle deltas as MongoDB documents. Useful for teams
using MongoDB as a read-optimised query store alongside PostgreSQL.

**Crate:** `mongodb`

```json
{
  "sink_type": "mongodb",
  "sink": {
    "connection_string": "mongodb://localhost:27017",
    "database": "pgtrickle",
    "collection_template": "{stream_table}",
    "doc_id_field": "dedup_key",
    "write_concern": "majority"
  }
}
```

**Operations mapping:**
- `op = "insert"` → `updateOne` with `upsert: true`
- `op = "delete"` → `deleteOne`
- `is_full_refresh = true` → `drop` + `insertMany`

**Effort:** 1.5d

### B.5 Snowflake / BigQuery Sink

Sink only. Cloud data warehouse integration for teams running analytics
on Snowflake or BigQuery. Uses bulk loading APIs for efficiency.

**Snowflake:** Stage files (S3/GCS/Azure) → `COPY INTO` via Snowpipe.  
**BigQuery:** Storage Write API for streaming inserts or GCS staging +
`LOAD DATA` for batch.

```json
{
  "sink_type": "snowflake",
  "sink": {
    "account": "myorg-myaccount",
    "database": "ANALYTICS",
    "schema": "PGTRICKLE",
    "table_template": "{stream_table}",
    "warehouse": "RELAY_WH",
    "role": "RELAY_ROLE",
    "stage": "@PGTRICKLE_STAGE",
    "private_key_file": "/etc/snowflake/rsa_key.p8"
  }
}
```

```json
{
  "sink_type": "bigquery",
  "sink": {
    "project_id": "my-project",
    "dataset_id": "pgtrickle",
    "table_template": "{stream_table}",
    "write_mode": "streaming",
    "credentials_file": "/etc/gcp/sa.json"
  }
}
```

**Effort:** 3d (1.5d each)

---

## Part C — Advanced Features

### C.1 Relay Dashboard

**Problem:** A dashboard for the relay would help operators monitor pipeline
health, throughput, and errors in real-time. The `pgtrickle-tui` crate that
previously provided a stream-table dashboard has been removed from the project.

**Design:** Add a `pgtrickle-relay dashboard` subcommand backed by ratatui.

**Dashboard panels:**
- Pipeline overview (mode, source, sink, status)
- Throughput graph (messages/sec, bytes/sec)
- Latency graph (p50, p95, p99 poll-to-publish)
- Consumer lag gauge
- Error rate and recent errors
- DLQ status (if enabled)
- Circuit breaker state
- Active connections health

**Implementation:** Use the `ratatui` crate directly in `pgtrickle-relay`.
Read metrics from the relay's Prometheus endpoint (scrape `/metrics`).

**Effort:** 2d

### C.2 Plugin System (WASM Backends)

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

**Effort:** 5d

### C.3 Encryption Envelope (KMS)

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

---

## Part D — Testing Strategy

### D.1 Connector Ecosystem Tests

| Test | Setup | Validates |
|------|-------|-----------|
| Airbyte sink E2E | postgres + mock Airbyte destination (Docker) | Forward: outbox → AirbyteRecordMessage; verify STATE checkpointing |
| Airbyte source E2E | postgres + mock Airbyte source (Docker) | Reverse: AirbyteRecordMessage → inbox; verify CATALOG handling |
| Airbyte CDC delete | postgres + mock destination | `op = delete` → `_ab_cdc_deleted_at` soft delete convention |
| dlt HTTP receiver E2E | postgres + dlt pipeline (Python) | dlt REST API POST → inbox rows; verify `_dlt_*` metadata |
| dlt file export E2E | postgres + filesystem check | Forward: outbox → JSONL files with dlt schema; verify partitioning |
| Benthos HTTP bridge E2E | postgres + benthos container | Forward: outbox → NDJSON POST → Benthos; verify processing |
| Benthos subprocess E2E | postgres + benthos binary | Forward: outbox → stdin → Benthos pipeline → stdout; verify output |
| Fivetran webhook E2E | postgres + mock Fivetran POST | Webhook POST → inbox; verify signature verification |
| Singer → Airbyte compat | postgres + both adapters | Same outbox, same data → identical inbox results via Singer and Airbyte |

### D.2 Additional Backend Tests

| Test | Containers | Validates |
|------|-----------|-----------|
| Pulsar E2E | postgres + pulsar-standalone | Forward + Reverse; verify dedup + subscription types |
| Arrow Flight E2E | postgres + test Flight server | Forward: RecordBatch emission; verify schema |
| AMQP 1.0 E2E | postgres + ActiveMQ Artemis | Forward + Reverse; verify AMQP 1.0 message properties |
| MongoDB E2E | postgres + mongodb | Forward: upsert + delete; verify full-refresh drop+insert |
| Snowflake E2E | postgres + mock stage (S3/MinIO) | Forward: stage files + verify COPY INTO SQL generation |
| BigQuery E2E | postgres + BQ emulator | Forward: verify Storage Write API message format |

### D.3 Advanced Feature Tests

| Test | Validates |
|------|-----------|
| Dashboard smoke test | ratatui renders without crash; metrics refresh |
| WASM plugin load | Custom sink plugin loads and processes messages |
| WASM plugin crash | Plugin crash does not crash relay; error reported |
| Encryption roundtrip | Encrypt envelope → publish → decrypt → verify |
| KMS key rotation | New DEK after rotation interval; old messages still decryptable |

### D.4 Benchmarks

| Benchmark | Target |
|-----------|--------|
| Airbyte sink throughput | 15K+ records/sec (Docker overhead vs Singer) |
| Benthos subprocess throughput | 20K+ records/sec (NDJSON pipe) |
| Pulsar sink throughput | 50K+ events/sec |
| Arrow Flight sink throughput | 100K+ rows/sec (columnar, zero-copy) |
| MongoDB sink throughput | 30K+ docs/sec (bulk upsert) |
| Snowflake bulk load | 10K+ rows/sec (staged COPY INTO) |
| WASM plugin overhead | <10% vs native backend |

---

## Part E — Implementation Roadmap

### Phase 3a — Connector Ecosystems (7 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P3-9 | Airbyte protocol adapter (Source + Sink, Docker + non-Docker) | 2.5d |
| RELAY-P3-10 | dlt integration (HTTP receiver source + file export sink) | 2d |
| RELAY-P3-11 | Redpanda Connect / Benthos (HTTP bridge + subprocess modes) | 1.5d |
| RELAY-P3-12 | Fivetran HVR endpoint (webhook flavor + signature verification) | 1d |

### Phase 3b — Additional Backends (11 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P3-1 | Sink + Source: Apache Pulsar | 2d |
| RELAY-P3-2 | Sink + Source: Arrow Flight / gRPC | 2.5d |
| RELAY-P3-6 | Sink + Source: AMQP 1.0 | 2d |
| RELAY-P3-7 | Sink: MongoDB | 1.5d |
| RELAY-P3-8 | Sink: Snowflake + BigQuery | 3d |

### Phase 3c — Advanced Features (9 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P3-3 | Relay dashboard (ratatui) | 2d |
| RELAY-P3-4 | Plugin system (WASM backends via wasmtime) | 5d |
| RELAY-P3-5 | Encryption envelope (KMS integration) | 2d |

### Phase 3d — Testing & Documentation (5 days)

| Item | Description | Effort |
|------|-------------|--------|
| RELAY-P3-13 | Connector ecosystem integration tests | 2d |
| RELAY-P3-14 | Additional backend + advanced feature tests | 2d |
| RELAY-P3-15 | Documentation, examples, and Docker image updates | 1d |

---

### Effort Summary

| Phase | Effort |
|-------|--------|
| Phase 3a — Connector Ecosystems | 7d |
| Phase 3b — Additional Backends | 11d |
| Phase 3c — Advanced Features | 9d |
| Phase 3d — Testing & Documentation | 5d |
| **Total** | **~32 days solo / ~20 days with two developers** |

Phase 3a (connectors) can be parallelised with Phase 3b (backends). Phase
3c (advanced features) depends on 3a/3b for the WASM plugin interface but
the dashboard and encryption work can start earlier.

### Dependencies

- **Requires Phase 2** — Singer adapter infrastructure (subprocess I/O,
  JSON-lines parsing, state checkpointing) is the foundation for Airbyte
  and Redpanda Connect subprocess modes.
- **Airbyte** requires Docker for the standard connector packaging. The
  relay also supports bare Python connectors for Docker-free environments.
- **dlt** requires a Python environment for integration testing.
- **Snowflake/BigQuery** require test accounts or emulators.
- **WASM plugin system** requires `wasmtime` and the Component Model.

---

## Open Questions

| # | Question | Options | Recommendation |
|---|----------|---------|----------------|
| 1 | Should Airbyte support Docker-only or also bare Python connectors? | (a) Docker only, (b) Both Docker and bare Python | **(b)** — Kubernetes sidecars can't run Docker-in-Docker easily. |
| 2 | Should dlt integration use HTTP receiver or subprocess mode? | (a) HTTP only, (b) Subprocess only, (c) Both | **(c)** — HTTP for production, subprocess for local dev. Start with HTTP (a) and add subprocess later if demanded. |
| 3 | Should Redpanda Connect use HTTP bridge or subprocess mode? | (a) HTTP only, (b) Subprocess only, (c) Both | **(c)** — HTTP for external Benthos, subprocess for embedded processing. |
| 4 | Should we combine Singer + Airbyte into a unified "EL Protocol" backend? | (a) Separate backends, (b) Unified with `protocol: singer \| airbyte` | **(a)** — cleaner separation, simpler config. Shared infra is internal. |
| 5 | Priority: Connector ecosystems or additional backends first? | (a) Connectors first, (b) Backends first, (c) Parallel | **(a)** — connectors unlock the most value per effort day. |
| 6 | Should the WASM plugin system use Component Model or Core WASM? | (a) Component Model (richer, newer), (b) Core WASM (simpler, stable) | **(a)** — Component Model is production-ready in wasmtime and provides richer interfaces. |
| 7 | Fivetran: full HVR integration or just webhook flavoring? | (a) Full HVR, (b) Webhook flavor only | **(b)** — Fivetran is proprietary; webhook covers the practical use-case. |
