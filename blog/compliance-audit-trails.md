[← Back to Blog Index](README.md)

# Compliance and Audit Trails with Append-Only Stream Tables

## Building GDPR-compliant, tamper-evident audit logs as stream tables — including right-to-erasure reconciliation

---

Every regulated industry needs audit trails. Financial services, healthcare, government — they all require an immutable record of who did what, when, and why. The typical implementation is a `audit_log` table that receives INSERTs from application code or triggers. It grows forever, nobody queries it until an auditor shows up, and then someone discovers it's missing half the events because a developer forgot to add logging to a new endpoint.

pg_trickle offers a more robust approach. Instead of instrumenting every write path with explicit audit logging, you define audit views as stream tables that automatically derive the audit record from the operational data's change history. The change buffer tables that pg_trickle maintains for incremental view maintenance are themselves a complete, append-only record of every modification to the source tables. You're getting an audit trail as a side effect of the performance optimization.

---

## The Change Buffer as Audit Source

When you register a table as a source for a stream table, pg_trickle installs CDC triggers that capture every INSERT, UPDATE, and DELETE into a change buffer table. Each change record includes:

- The full before-image (for UPDATEs and DELETEs)
- The full after-image (for INSERTs and UPDATEs)
- A timestamp
- The transaction ID
- The operation type (I/U/D)

This is exactly the information an audit trail needs. The difference from traditional audit logging is that it's comprehensive by construction — if a table is a stream table source, every change is captured, regardless of which application code path triggered it. No missed events, no forgotten instrumentation.

---

## Deriving Audit Views

Instead of querying raw change buffers (which have an internal schema optimized for delta processing), you can define audit views as stream tables themselves:

```sql
-- Patient record modification audit trail (HIPAA requirement)
SELECT pgtrickle.create_stream_table(
    'patient_audit_trail',
    $$
    SELECT
        p.id AS patient_id,
        p.name AS patient_name,
        p.modified_by AS last_modified_by,
        p.modified_at AS last_modification_time,
        COUNT(*) AS total_modifications
    FROM patients p
    GROUP BY p.id, p.name, p.modified_by, p.modified_at
    $$
);
```

For a richer audit trail that tracks every change (not just the latest state), you can combine the operational table with a separate audit events table:

```sql
-- Application writes audit events alongside data changes
CREATE TABLE audit_events (
    id           bigserial PRIMARY KEY,
    table_name   text NOT NULL,
    record_id    bigint NOT NULL,
    action       text NOT NULL,  -- 'CREATE', 'UPDATE', 'DELETE'
    actor        text NOT NULL,
    changed_fields jsonb,
    old_values   jsonb,
    new_values   jsonb,
    reason       text,
    created_at   timestamptz DEFAULT now()
);

-- Stream table: compliance summary per record
SELECT pgtrickle.create_stream_table(
    'compliance_modification_summary',
    $$
    SELECT
        table_name,
        record_id,
        COUNT(*) AS change_count,
        COUNT(DISTINCT actor) AS distinct_actors,
        MIN(created_at) AS first_change,
        MAX(created_at) AS latest_change,
        COUNT(*) FILTER (WHERE action = 'DELETE') AS delete_count
    FROM audit_events
    GROUP BY table_name, record_id
    $$
);
```

This summary updates incrementally as audit events flow in. An auditor can instantly see which records have been modified most frequently, which records have been deleted, and which actors have been most active — without scanning the full audit log.

---

## GDPR Right-to-Erasure Reconciliation

The tension between "immutable audit trail" and "right to be forgotten" is one of the hardest problems in compliance engineering. GDPR Article 17 gives individuals the right to have their personal data deleted. But your audit log says you must never delete records. How do you reconcile?

The standard approach is pseudonymization: replace identifying information with opaque tokens, preserving the audit trail's structure (who did what to which record) while removing the ability to identify the individual.

With pg_trickle, you can maintain a "pseudonymized audit view" that automatically reflects erasure operations:

```sql
-- Erasure requests table
CREATE TABLE erasure_requests (
    id           serial PRIMARY KEY,
    subject_id   bigint NOT NULL,  -- the person requesting erasure
    requested_at timestamptz DEFAULT now(),
    completed_at timestamptz
);

-- Pseudonymized audit view: masks erased subjects
SELECT pgtrickle.create_stream_table(
    'pseudonymized_audit',
    $$
    SELECT
        a.id AS event_id,
        a.table_name,
        a.record_id,
        a.action,
        CASE
            WHEN er.id IS NOT NULL THEN 'REDACTED-' || md5(a.actor)
            ELSE a.actor
        END AS actor,
        a.created_at,
        CASE
            WHEN er.id IS NOT NULL THEN NULL
            ELSE a.changed_fields
        END AS changed_fields
    FROM audit_events a
    LEFT JOIN erasure_requests er
        ON er.subject_id = (a.new_values->>'subject_id')::bigint
        AND er.completed_at IS NOT NULL
    $$
);
```

When an erasure request is completed (the `completed_at` field is set), the stream table automatically redacts all audit events associated with that subject. The audit trail still shows that actions occurred (preserving regulatory requirements for financial record-keeping), but personal identifiers are replaced with irreversible hashes.

The incremental nature means that processing an erasure request doesn't require scanning the entire audit log. pg_trickle identifies the audit events affected by the new erasure record (via the join) and updates only those rows in the pseudonymized view.

---

## Tamper Evidence

An audit trail is worthless if it can be silently modified. Deleted audit records, altered timestamps, and changed actor fields undermine the entire system. Traditional approaches to tamper evidence include:

- Hash chains (each record includes a hash of the previous record)
- Write-once storage (append-only file systems)
- External witnesses (send a hash to a timestamping service)

pg_trickle's change buffers provide a degree of tamper evidence by design. Because the CDC triggers capture every modification to source tables, any attempt to alter the audit events table itself would be captured as a change event. You'd need to disable the trigger, modify the data, and re-enable the trigger — which requires superuser access and leaves gaps in the sequence numbering.

For stronger tamper evidence, combine stream tables with a hash chain:

```sql
-- Maintain a running hash chain over audit events
SELECT pgtrickle.create_stream_table(
    'audit_chain',
    $$
    SELECT
        a.id AS event_id,
        a.action,
        a.actor,
        a.created_at,
        md5(
            a.id::text ||
            a.action ||
            a.actor ||
            a.created_at::text ||
            COALESCE(
                (SELECT md5_hash FROM audit_chain_prev WHERE event_id = a.id - 1),
                'GENESIS'
            )
        ) AS chain_hash
    FROM audit_events a
    $$
);
```

Any modification to a historical audit event would break the hash chain from that point forward — a tampering attempt becomes immediately detectable by verifying the chain.

---

## Access Pattern Monitoring

Beyond recording data changes, compliance often requires monitoring who accesses sensitive data and how often. Stream tables can maintain access pattern summaries:

```sql
-- Track data access patterns for compliance reporting
CREATE TABLE data_access_log (
    id          bigserial PRIMARY KEY,
    accessor    text NOT NULL,
    resource    text NOT NULL,
    access_type text NOT NULL,  -- 'READ', 'EXPORT', 'SHARE'
    accessed_at timestamptz DEFAULT now()
);

SELECT pgtrickle.create_stream_table(
    'access_pattern_summary',
    $$
    SELECT
        accessor,
        resource,
        access_type,
        date_trunc('day', accessed_at) AS day,
        COUNT(*) AS access_count
    FROM data_access_log
    GROUP BY accessor, resource, access_type, date_trunc('day', accessed_at)
    $$
);

-- Anomaly detection: who accessed more than usual?
SELECT pgtrickle.create_stream_table(
    'access_anomalies',
    $$
    SELECT
        accessor,
        resource,
        day,
        access_count,
        AVG(access_count) OVER (
            PARTITION BY accessor, resource
            ORDER BY day
            ROWS BETWEEN 30 PRECEDING AND 1 PRECEDING
        ) AS rolling_avg_30d
    FROM access_pattern_summary
    $$
);
```

The anomaly detection stream table compares today's access count against the 30-day rolling average. Spikes — an employee suddenly downloading 100x their normal volume of patient records — are immediately visible without running expensive ad-hoc queries.

---

## Retention Policies

Compliance regulations specify how long audit data must be retained. SOX requires 7 years for financial records. HIPAA requires 6 years. After the retention period, data should be purged — both for storage efficiency and to limit the blast radius of a breach.

With stream tables, you can implement retention-aware views:

```sql
SELECT pgtrickle.create_stream_table(
    'active_audit_records',
    $$
    SELECT *
    FROM audit_events
    WHERE created_at > now() - interval '7 years'
    $$
);
```

Records that age past the retention window automatically disappear from the stream table. The source audit events table can be partitioned by time, with old partitions archived to cold storage or dropped after the retention period.

The stream table's incremental maintenance handles the window expiry naturally: as records age out, they're subtracted from any aggregates that reference them. Your compliance dashboard always shows the correct counts within the retention window, without manual recalculation.

---

## Putting It Together

A complete compliance architecture with pg_trickle:

1. **Source tables** with CDC triggers (automatic with stream tables) — captures every data modification
2. **Audit events table** — explicit audit log for application-level actions (login, export, share)
3. **Pseudonymized audit view** (stream table) — GDPR-safe view with automatic redaction on erasure
4. **Access pattern summary** (stream table) — incremental aggregation of who accessed what
5. **Compliance dashboard** (stream table) — high-level metrics (total changes, distinct actors, anomalies)

Each layer is incrementally maintained. The cost of compliance monitoring scales with the rate of changes, not the volume of historical data. An auditor querying the compliance dashboard reads from pre-computed stream tables — no full scans, no waiting, no stale reports.

---

*Compliance doesn't have to be expensive. When your audit trail maintains itself incrementally, you get tamper evidence, GDPR compatibility, and instant compliance reports — all as side effects of how pg_trickle already works.*
