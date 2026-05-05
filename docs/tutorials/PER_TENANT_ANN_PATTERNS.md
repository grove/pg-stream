# Per-Tenant ANN Indexing Patterns

> VA-3 (v0.48.0) — Production patterns for multi-tenant RAG using RLS-scoped
> embedding corpora.

## Security Model

**Trust boundaries** for multi-tenant ANN stream tables:

1. **Row-level security enforces tenant isolation** — every SELECT against the
   stream table must pass through RLS policies.  pg_trickle respects PostgreSQL
   RLS; the stream table itself is a regular table.
2. **The refresh runs as a superuser background worker** — the background
   worker bypasses RLS when writing to the stream table.  This is intentional
   and correct: the worker writes denormalised data from authorised source
   tables.  Tenant isolation is enforced on reads, not writes.
3. **Never grant direct INSERT/UPDATE to application users** — only the
   pg_trickle background worker should write to stream tables.
4. **Audit the defining query** — if the defining query joins tenant data
   across boundaries (e.g. `FROM all_tenants_table`), the stream table will
   contain cross-tenant data.  Verify RLS on source tables applies during the
   defining query execution if you rely on source-level isolation.

---

## Pattern 1: Tenant Column + RLS Policy

```sql
-- Source table with tenant isolation
CREATE TABLE tenant_embeddings (
    id          BIGSERIAL PRIMARY KEY,
    tenant_id   UUID NOT NULL,
    content     TEXT NOT NULL,
    embedding   vector(1536) NOT NULL
);

-- Stream table (inherits tenant_id column)
SELECT pgtrickle.create_stream_table(
    'tenant_corpus',
    $$
        SELECT id, tenant_id, content, embedding
        FROM tenant_embeddings
    $$,
    '1m',
    'DIFFERENTIAL'
);

-- Enable RLS
ALTER TABLE tenant_corpus ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenant_corpus FORCE ROW LEVEL SECURITY;

-- Policy: each user sees only their tenant's rows
CREATE POLICY tenant_isolation ON tenant_corpus
    USING (tenant_id = current_setting('app.tenant_id', true)::uuid);

-- ANN index (spans all tenants — filtered at query time by RLS)
CREATE INDEX ON tenant_corpus USING hnsw(embedding vector_cosine_ops);
```

**Query pattern:**

```sql
-- Set tenant context before querying
SET app.tenant_id = 'your-tenant-uuid';

-- This query automatically applies the RLS policy
SELECT id, content, embedding <=> '[...]'::vector AS distance
FROM tenant_corpus
ORDER BY embedding <=> '[...]'::vector
LIMIT 10;
```

---

## Pattern 2: Partitioned by Tenant (High-Volume)

For tenants with millions of embeddings each, partition by `tenant_id` and
create per-partition indexes:

```sql
SELECT pgtrickle.create_stream_table(
    'tenant_corpus_partitioned',
    $$
        SELECT id, tenant_id, content, embedding
        FROM tenant_embeddings
    $$,
    '1m',
    'DIFFERENTIAL',
    partition_key => 'HASH:tenant_id:16'
);

-- Create HNSW index on each partition
DO $$
DECLARE
    i INT;
BEGIN
    FOR i IN 0..15 LOOP
        EXECUTE format(
            'CREATE INDEX ON tenant_corpus_partitioned_p%s USING hnsw(embedding vector_cosine_ops)',
            i
        );
    END LOOP;
END $$;
```

---

## Pattern 3: Separate Stream Tables per Tier

For SLA isolation between tenant tiers:

```sql
-- Premium tenants (frequent refresh, larger index)
SELECT pgtrickle.create_stream_table(
    'premium_corpus',
    'SELECT id, tenant_id, content, embedding FROM tenant_embeddings WHERE tier = ''premium''',
    '10s',
    'DIFFERENTIAL'
);

-- Standard tenants (less frequent)
SELECT pgtrickle.create_stream_table(
    'standard_corpus',
    'SELECT id, tenant_id, content, embedding FROM tenant_embeddings WHERE tier = ''standard''',
    '5m',
    'DIFFERENTIAL'
);
```

---

## Security Checklist

- [ ] RLS enabled (`ALTER TABLE ... ENABLE ROW LEVEL SECURITY`)
- [ ] RLS forced (`ALTER TABLE ... FORCE ROW LEVEL SECURITY`) to prevent
      superuser bypass during app queries
- [ ] Defining query audited: no unintentional cross-tenant joins
- [ ] Source tables have RLS if they contain cross-tenant data
- [ ] Application uses parameterised `SET app.tenant_id = $1` not string
      interpolation
- [ ] Stream table not directly writable by application users

---

## Monitoring

```sql
-- Per-tenant embedding lag (requires tenant_id in defining query)
SELECT
    tenant_id,
    COUNT(*) AS embedding_count,
    MAX(updated_at) AS newest_embedding
FROM tenant_corpus
GROUP BY tenant_id;

-- Overall vector stream table health
SELECT * FROM pgtrickle.vector_status();
```
