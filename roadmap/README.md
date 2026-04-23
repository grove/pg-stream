# pg_trickle Roadmap — Feature Descriptions

> **Audience:** Product managers, stakeholders, and technically curious readers
> who want to understand what each release delivers and why it matters —
> without needing to read Rust code or SQL specifications.
>
> For the full technical roadmap with exit criteria and implementation phases,
> see [ROADMAP.md](../ROADMAP.md).

## Versions

| Version | Theme | Status | Scope |
|---------|-------|--------|-------|
| [v0.28.0](v0.28.0.md) | Reliable event messaging built into PostgreSQL | ✅ Released | Large |
| [v0.29.0](v0.29.0.md) | Off-the-shelf connector to Kafka, NATS, SQS, and more | Planned | Large |
| [v0.30.0](v0.30.0.md) | Quality gate before 1.0 — correctness, stability, and docs | Planned | Medium |
| [v0.31.0](v0.31.0.md) | Smarter scheduling and faster hot paths | Planned | Medium |
| [v0.32.0](v0.32.0.md) | Live push notifications and safe live schema changes | Planned | Medium |
| [v0.33.0](v0.33.0.md) | Time-travel queries and analytic storage | Planned | Medium |

## How these versions fit together

```
v0.28.0  ─── Reliable event messaging (outbox + inbox)
    │
v0.29.0  ─── Relay CLI connecting that messaging to Kafka, NATS, etc.
    │
v0.30.0  ─── Quality gate: correctness, stability, docs (required for 1.0)
    │
v0.31.0  ─── Scheduler intelligence and hot-path performance
    │
v0.32.0  ─── Live push notifications + zero-downtime schema changes
    │
v0.33.0  ─── Time-travel history + analytic columnar storage
    │
v1.0.0   ─── Stable release, PostgreSQL 19, package registries
```

v0.28.0 and v0.29.0 together deliver the event-driven integration story.
v0.30.0 is a mandatory correctness and polish gate before 1.0. v0.31.0
through v0.33.0 each add a distinct new capability — improved efficiency,
reactive UIs, and analytic workloads respectively — while the core IVM
engine underneath remains stable.

---

*For the full technical roadmap with exit criteria and implementation phases,
see [ROADMAP.md](../ROADMAP.md).*
