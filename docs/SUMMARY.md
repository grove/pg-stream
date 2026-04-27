# Summary

[pg_trickle](introduction.md)
[What is pg_trickle?](ESSENCE.md)

---

# Discover

- [Use Cases](USE_CASES.md)
- [Comparisons](COMPARISONS.md)
- [Glossary](GLOSSARY.md)
- [Playground (Docker sandbox)](PLAYGROUND.md)
- [Real-time Demo](DEMO.md)

---

# Get Started

- [5-Minute Quickstart](QUICKSTART_5MIN.md)
- [Getting Started Tutorial](GETTING_STARTED.md)
- [Installation](installation.md)

---

# Build with Stream Tables

- [Best-Practice Patterns](PATTERNS.md)
- [Performance Cookbook](PERFORMANCE_COOKBOOK.md)
- [SQL Reference](SQL_REFERENCE.md)
- [Configuration](CONFIGURATION.md)
- [Predictive Cost Model](COST_MODEL.md)

---

# Operate

- [Pre-Deployment Checklist](PRE_DEPLOYMENT.md)
- [Scaling Guide](SCALING.md)
- [Capacity Planning](CAPACITY_PLANNING.md)
- [Multi-Database Deployments](MULTI_DATABASE.md)
- [Backup and Restore](BACKUP_AND_RESTORE.md)
- [Snapshots](SNAPSHOTS.md)
- [High Availability and Replication](HA_AND_REPLICATION.md)
- [Upgrading](UPGRADING.md)
- [Security Guide](SECURITY_GUIDE.md)
- [Troubleshooting & Runbook](TROUBLESHOOTING.md)
- [Error Reference](ERRORS.md)
- [TUI Tool](TUI.md)
- [CLI Reference](CLI_REFERENCE.md)

---

# Distributed & Streaming

- [Citus Distributed Tables](CITUS.md)
- [CDC Modes](CDC_MODES.md)
- [SLA-based Smart Scheduling](SLA_SCHEDULING.md)
- [Downstream Publications](PUBLICATIONS.md)
- [Transactional Outbox](OUTBOX.md)
- [Transactional Inbox](INBOX.md)
- [Relay Service](RELAY_GUIDE.md)
- [Relay Architecture & Operations](RELAY.md)

---

# Tutorials

- [What Happens on INSERT](tutorials/WHAT_HAPPENS_ON_INSERT.md)
- [What Happens on UPDATE](tutorials/WHAT_HAPPENS_ON_UPDATE.md)
- [What Happens on DELETE](tutorials/WHAT_HAPPENS_ON_DELETE.md)
- [What Happens on TRUNCATE](tutorials/WHAT_HAPPENS_ON_TRUNCATE.md)
- [Row-Level Security](tutorials/ROW_LEVEL_SECURITY.md)
- [Partitioned Tables](tutorials/PARTITIONED_TABLES.md)
- [Foreign Table Sources](tutorials/FOREIGN_TABLE_SOURCES.md)
- [Tiered Scheduling](tutorials/TIERED_SCHEDULING.md)
- [Fuse Circuit Breaker](tutorials/FUSE_CIRCUIT_BREAKER.md)
- [Circular Dependencies](tutorials/CIRCULAR_DEPENDENCIES.md)
- [Tuning Refresh Mode](tutorials/tuning-refresh-mode.md)
- [Monitoring & Alerting](tutorials/MONITORING_AND_ALERTING.md)
- [ETL & Bulk Load Patterns](tutorials/ETL_BULK_LOAD.md)
- [Migrating from Materialized Views](tutorials/MIGRATING_FROM_MATERIALIZED_VIEWS.md)
- [Migrating from pg_ivm](tutorials/MIGRATING_FROM_PG_IVM.md)

---

# Integrations

- [dbt-pgtrickle](integrations/dbt.md)
- [CloudNativePG / Kubernetes](integrations/cloudnativepg.md)
- [Citus (long-form reference)](integrations/citus.md)
- [Prometheus & Grafana](integrations/prometheus.md)
- [PgBouncer & Connection Poolers](integrations/pgbouncer.md)
- [Flyway & Liquibase](integrations/flyway-liquibase.md)
- [ORM Integration](integrations/orm.md)
- [Multi-Tenant](integrations/multi-tenant.md)
- [dbt Hub Submission](integrations/dbt-hub-submission.md)

---

# Reference

- [Blog](blog/README.md)
  - [Why Your Materialized Views Are Always Stale](blog/stale-materialized-views.md)
  - [Differential Dataflow for the Rest of Us](blog/differential-dataflow-explained.md)
  - [Incremental Aggregates in PostgreSQL: No ETL Required](blog/incremental-aggregates-no-etl.md)
  - [The Z-Set: The Data Structure That Makes IVM Correct](blog/z-set-data-structure.md)
  - [The Cost Model: How pg_trickle Decides Whether to Refresh Differentially](blog/cost-model-auto-mode.md)
  - [Recursive CTEs That Update Themselves](blog/recursive-ctes-that-update-themselves.md)
  - [Window Functions Without the Full Recompute](blog/window-functions-without-full-recompute.md)
  - [GROUPING SETS, ROLLUP, and CUBE — Incrementally](blog/grouping-sets-rollup-cube.md)
  - [EXISTS and NOT EXISTS: The Delta Rules Nobody Talks About](blog/exists-not-exists-delta-rules.md)
  - [DISTINCT That Doesn't Recount](blog/distinct-reference-counting.md)
  - [Scalar Subqueries in the SELECT List — Incrementally](blog/scalar-subqueries.md)
  - [LATERAL Joins in a Stream Table](blog/lateral-joins.md)
  - [Set Operations Done Right: UNION, INTERSECT, EXCEPT](blog/set-operations.md)
  - [IMMEDIATE Mode: When "Good Enough Freshness" Isn't Good Enough](blog/immediate-mode-zero-lag.md)
  - [How pg_trickle Handles Diamond Dependencies](blog/diamond-dependencies.md)
  - [Temporal Stream Tables: Time-Windowed Views That Update Themselves](blog/temporal-stream-tables.md)
  - [Declare Freshness Once: CALCULATED Scheduling](blog/calculated-scheduling.md)
  - [Cycles in Your Dependency Graph? That's Fine.](blog/circular-dependencies.md)
  - [Hot, Warm, Cold, Frozen: Tiered Scheduling at Scale](blog/tiered-scheduling.md)
  - [The CDC Mode You Never Have to Choose](blog/hybrid-cdc-mode.md)
  - [IVM Without Primary Keys](blog/ivm-without-primary-keys.md)
  - [Foreign Tables as Stream Table Sources](blog/foreign-table-sources.md)
  - [The Medallion Architecture Lives Inside PostgreSQL](blog/medallion-architecture-postgresql.md)
  - [CQRS Without a Second Database](blog/cqrs-without-second-database.md)
  - [Slowly Changing Dimensions in Real Time](blog/slowly-changing-dimensions.md)
  - [The Append-Only Fast Path](blog/append-only-fast-path.md)
  - [Real-Time Leaderboards That Don't Lie](blog/real-time-leaderboards.md)
  - [The Hidden Cost of Trigger-Based Denormalization](blog/trigger-denormalization-cost.md)
  - [How We Replaced a Celery Pipeline with 3 SQL Statements](blog/replaced-celery-with-sql.md)
  - [Migrating from pg_ivm to pg_trickle](blog/migrating-from-pg-ivm.md)
  - [Streaming to Kafka Without Kafka Expertise](blog/streaming-to-kafka-without-kafka.md)
  - [The Relay Deep Dive: NATS, Redis Streams, and RabbitMQ](blog/relay-deep-dive.md)
  - [The Inbox Pattern: Receiving Events from Kafka into PostgreSQL](blog/inbox-pattern-kafka.md)
  - [The Outbox You Don't Have to Build](blog/built-in-outbox.md)
  - [dbt + pg_trickle: The Analytics Engineer's Stack](blog/dbt-analytics-stack.md)
  - [Distributed IVM with Citus](blog/distributed-ivm-citus.md)
  - [pg_trickle on CloudNativePG](blog/pg-trickle-cloudnativepg-kubernetes.md)
  - [Making pg_trickle Work Through PgBouncer](blog/pgbouncer-compatibility.md)
  - [Publishing Stream Tables via Logical Replication](blog/logical-replication-publishing.md)
  - [One PostgreSQL, Five Databases, One Worker Pool](blog/multi-database.md)
  - [Your pgvector Index Is Lying to You](blog/incremental-pgvector.md)
  - [Incremental Vector Aggregates: Building Recommendation Engines in Pure SQL](blog/incremental-vector-aggregates.md)
  - [Deploying RAG at Scale: pg_trickle as Your Embedding Infrastructure](blog/deploying-rag-at-scale.md)
  - [HNSW Recall Is a Lie: Distribution Drift Explained](blog/hnsw-recall-distribution-drift.md)
  - [The pgvector Tooling Landscape in 2026](blog/pgvector-tooling-landscape.md)
  - [Multi-Tenant Vector Search with Row-Level Security](blog/multi-tenant-vector-search-rls.md)
  - [Stop Rebuilding Your Search Index at 3am](blog/stop-rebuilding-search-index.md)
  - [pg_trickle Monitors Itself](blog/self-monitoring.md)
  - [How to Change a Stream Table Query Without Taking It Offline](blog/online-schema-evolution.md)
  - [Backup and Restore for Stream Tables](blog/backup-and-restore.md)
  - [Testing Stream Tables: Shadow Mode and Correctness Fuzzing](blog/shadow-mode-correctness-fuzzing.md)
  - [Snapshots: Time Travel for Stream Tables](blog/snapshots-time-travel.md)
  - [Drain Mode: Zero-Downtime Upgrades for Stream Tables](blog/drain-mode.md)
  - [Column-Level Lineage in One Function Call](blog/column-level-lineage.md)
  - [Error Budgets for Stream Tables](blog/error-budgets.md)
  - [Structured Logging and OpenTelemetry for Stream Tables](blog/structured-logging.md)
  - [The 45ms Cold-Start Tax and How L0 Cache Eliminates It](blog/l0-cache-cold-start.md)
  - [Spill-to-Disk and the Auto-Fallback Safety Net](blog/spill-to-disk-fallback.md)
  - [TPC-H at 1GB in 40ms](blog/tpch-benchmarking-ivm.md)
  - [From Nexmark to Production: Benchmarking Stream Processing in PostgreSQL](blog/nexmark-benchmark.md)
  - [Reactive Alerts Without Polling](blog/reactive-alerts-without-polling.md)
  - [The Outbox Pattern, Turbocharged](blog/outbox-pattern-turbocharged.md)
- [FAQ](FAQ.md)
- [What's New](WHATS_NEW.md)
- [Changelog](changelog.md)
- [Roadmap](roadmap.md)
- [Release Process](RELEASE.md)
- [Project History](PROJECT_HISTORY.md)
- [Contributing](contributing.md)
- [Security Policy](security.md)

---

# Internals (for contributors)

- [Architecture Overview](ARCHITECTURE.md)
- [DVM Operators](DVM_OPERATORS.md)
- [DVM Rewrite Rules](DVM_REWRITE_RULES.md)
- [Benchmarks](BENCHMARK.md)

# Research

- [DBSP Comparison](research/DBSP_COMPARISON.md)
- [pg_ivm Comparison](research/PG_IVM_COMPARISON.md)
- [Custom SQL Syntax](research/CUSTOM_SQL_SYNTAX.md)
- [Triggers vs Replication](research/TRIGGERS_VS_REPLICATION.md)
- [Prior Art](research/PRIOR_ART.md)
- [Multi-DB Refresh Broker](research/multi_db_refresh_broker.md)
