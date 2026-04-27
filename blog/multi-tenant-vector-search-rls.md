# Multi-Tenant Vector Search with Row-Level Security and pg_trickle

## Zero cross-tenant data leakage without separate tables or databases

---

Multi-tenant SaaS and vector search are a challenging combination.

The standard options are: one embedding table per tenant (operational nightmare at scale), one shared table filtered by a `tenant_id` column (correctness depends entirely on never forgetting the WHERE clause), or a dedicated vector database instance per tenant (very expensive, very complex).

PostgreSQL's Row-Level Security (RLS) offers a fourth option: a shared table where the database *enforces* per-tenant isolation, independent of application code. Combine this with pg_trickle stream tables and you get per-tenant search corpora that are maintained incrementally, correctly isolated, and queryable efficiently.

This post is about how to build that.

---

## The Problem With Shared Tables

The naive approach to multi-tenant vector search:

```sql
CREATE TABLE document_embeddings (
  id          bigserial PRIMARY KEY,
  tenant_id   bigint NOT NULL,
  content     text,
  embedding   vector(1536),
  created_at  timestamptz DEFAULT NOW()
);

CREATE INDEX ON document_embeddings USING hnsw (embedding vector_cosine_ops);
```

Application queries then filter by tenant:

```sql
SELECT id, content, embedding <=> $query AS distance
FROM document_embeddings
WHERE tenant_id = $current_tenant_id
ORDER BY embedding <=> $query
LIMIT 10;
```

This works until it doesn't. The failure modes:

**Application bugs:** A query that forgets the `WHERE tenant_id = ?` clause leaks data across tenants. This is an application-level constraint enforced only by developer discipline.

**Poor ANN performance with filtering:** HNSW searches find the approximate nearest neighbors across the *entire* index, then filters to the tenant. If a tenant has 1% of the total rows, you might retrieve 100 candidates from the full index before finding 10 that pass the tenant filter. You're doing ~10× wasted work.

**Uneven distributions:** A large tenant dominates the ANN index. Their document embeddings cluster tightly. Smaller tenants have their embeddings scattered across the index graph among the dominant tenant's nodes. Query latency and recall vary wildly across tenants.

---

## Row-Level Security: Database-Enforced Isolation

RLS moves the isolation guarantee from application code to the database engine.

```sql
-- Enable RLS on the table
ALTER TABLE document_embeddings ENABLE ROW LEVEL SECURITY;
ALTER TABLE document_embeddings FORCE ROW LEVEL SECURITY;

-- Create a policy that restricts reads to the current tenant
CREATE POLICY tenant_isolation ON document_embeddings
  AS RESTRICTIVE
  FOR ALL
  USING (tenant_id = current_setting('app.current_tenant_id')::bigint);
```

Now every query against `document_embeddings` automatically scopes to the current tenant, set via `SET LOCAL app.current_tenant_id = ?` at the start of each request.

```sql
-- Application code sets the tenant context
SET LOCAL app.current_tenant_id = 42;

-- This query now implicitly filters to tenant 42 — the RLS policy applies
SELECT id, content, embedding <=> $query AS distance
FROM document_embeddings
ORDER BY distance
LIMIT 10;
```

The `WHERE tenant_id = ?` clause is gone from the query. The database enforces it. Forgetting to filter is now impossible — the policy applies to *all* queries, including ORMs, raw SQL, and debugging queries run manually.

The `FORCE ROW LEVEL SECURITY` clause makes the policy apply to table owners too, preventing superuser-adjacent roles from bypassing it.

---

## The ANN Performance Problem Remains

RLS fixes the correctness problem but not the ANN performance problem.

HNSW builds a graph over the entire table. When you query with an RLS policy filtering to one tenant, the database:
1. Starts an ANN search from the query vector's neighborhood
2. Traverses the HNSW graph
3. Applies the RLS filter to each candidate
4. Collects enough passing candidates to return `LIMIT k` results

For small tenants, this is expensive. The graph traversal visits many nodes from other tenants before finding enough that pass the filter.

The pgvector `iterative_scan` feature helps — it expands the search until enough candidates pass filtering:

```sql
SET hnsw.iterative_scan = strict_order;

SELECT id, content, embedding <=> $query AS distance
FROM document_embeddings
ORDER BY distance
LIMIT 10;
```

But iterative scan has a cost ceiling (`hnsw.max_scan_tuples`), and for very small tenants in a large shared table, even iterative scan may exhaust the ceiling before finding 10 results.

The right solution is **per-tenant partial indexes**:

```sql
-- One index per tenant, covering only their rows
CREATE INDEX CONCURRENTLY doc_emb_tenant_42_idx
ON document_embeddings USING hnsw (embedding vector_cosine_ops)
WHERE tenant_id = 42;
```

With a partial index, the ANN search only navigates the subgraph for tenant 42. It's as if they have their own dedicated index. Recall is high, latency is consistent, and there's no wasted work scanning other tenants' data.

The problem with per-tenant partial indexes is operationally managing them: creating them for new tenants, dropping them for churned tenants, rebuilding them as distributions drift, and monitoring their health.

---

## pg_trickle Stream Tables for Per-Tenant Corpora

pg_trickle stream tables change the operational model. Instead of maintaining indexes on the shared raw table, you maintain per-tenant (or group-of-tenant) stream tables that contain only that tenant's data:

```sql
-- Per-tenant search corpus
SELECT pgtrickle.create_stream_table(
  name         => 'tenant_42_corpus',
  query        => $$
    SELECT
      d.id,
      d.title,
      d.content,
      d.embedding,
      u.display_name AS author,
      array_agg(t.name ORDER BY t.name) AS tags,
      d.created_at
    FROM documents d
    JOIN users u ON u.id = d.author_id
    LEFT JOIN document_tags dt ON dt.document_id = d.id
    LEFT JOIN tags t ON t.id = dt.tag_id
    WHERE d.tenant_id = 42
    AND d.published = true
    GROUP BY d.id, d.title, d.content, d.embedding,
             u.display_name, d.created_at
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

CREATE INDEX ON tenant_42_corpus USING hnsw (embedding vector_cosine_ops);
```

Now `tenant_42_corpus` is a real table containing only tenant 42's published documents, denormalized with author names and tags, with its own HNSW index. Queries against this table are fast because the index covers exactly the right data.

The DVM engine maintains the corpus: new documents appear within 5 seconds, tag changes propagate, unpublished documents are removed.

---

## The Operational Challenge: Many Tenants

For 10 tenants, creating individual stream tables is fine. For 1,000 tenants, it's not.

The practical solution at scale is tiered tenancy:

**Large tenants** (top 5–10% by document count): individual stream tables with dedicated HNSW indexes.

**Medium tenants**: grouped stream tables covering 10–50 tenants per group, with partial indexes per tenant within the group.

**Small tenants** (long tail, few documents each): shared table with RLS and partial indexes, or (for very small tenants) exact search is fast enough and an ANN index isn't worth the overhead.

```sql
-- Tier 1: Large tenant, individual corpus
SELECT pgtrickle.create_stream_table(
  name    => 'corpus_tenant_42',
  query   => $$ ... WHERE tenant_id = 42 ... $$,
  ...
);

-- Tier 2: Medium tenants grouped by a hash
SELECT pgtrickle.create_stream_table(
  name    => 'corpus_group_07',
  query   => $$ ... WHERE tenant_id % 20 = 7 ... $$,
  ...
);
-- Plus per-tenant partial HNSW within the group

-- Tier 3: Long tail, shared table with RLS (no stream table)
-- Exact search for tiny tenants is fast enough
```

The tier assignment can change as tenants grow. When a medium tenant exceeds a threshold, create an individual stream table and remove them from the group.

---

## Combining RLS With Stream Tables

Stream tables are regular PostgreSQL tables. You can apply RLS policies to them:

```sql
-- The shared "all tenants" stream table approach with RLS
SELECT pgtrickle.create_stream_table(
  name  => 'document_corpus',
  query => $$
    SELECT
      d.id, d.tenant_id, d.title, d.content, d.embedding,
      u.display_name AS author
    FROM documents d
    JOIN users u ON u.id = d.author_id
    WHERE d.published = true
  $$,
  ...
);

ALTER TABLE document_corpus ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON document_corpus
  AS RESTRICTIVE FOR ALL
  USING (tenant_id = current_setting('app.current_tenant_id')::bigint);

-- Per-tenant partial HNSW indexes on the stream table
CREATE INDEX corpus_tenant_42_idx ON document_corpus
USING hnsw (embedding vector_cosine_ops) WHERE tenant_id = 42;
```

This combines incremental maintenance (the stream table stays fresh) with database-enforced isolation (RLS) and efficient search (partial indexes per tenant).

The stream table contains all tenants' data — maintained by a single refresh cycle — but each query is automatically scoped to the current tenant by RLS, and executes using that tenant's partial index.

---

## The USING INDEX HINT

For the query planner to use the correct partial index, the query needs to make the tenant constraint visible to the planner.

With RLS, the policy is applied after the planner generates the query plan. The planner doesn't see the `tenant_id = ?` constraint at plan time, so it may choose the full-table HNSW index over the partial index.

The workaround is to set the tenant context *before* planning, using `SET LOCAL` in the same transaction, and to include the tenant condition explicitly in the query:

```sql
BEGIN;
SET LOCAL app.current_tenant_id = 42;

-- Include tenant_id explicitly so the planner can choose the partial index
SELECT id, content, embedding <=> $query AS distance
FROM document_corpus
WHERE tenant_id = 42  -- explicit, even though RLS would filter anyway
ORDER BY distance
LIMIT 10;

COMMIT;
```

The explicit `WHERE tenant_id = 42` lets the planner see that the partial index `corpus_tenant_42_idx` (which covers `WHERE tenant_id = 42`) is applicable, and choose it over the full-table index. The RLS policy remains as a safety net.

---

## Drift-Aware Reindexing Per Tenant

With per-tenant partial indexes, drift affects each index independently. A tenant that adds 30% new documents needs their partial index rebuilt. A tenant with stable content doesn't.

```sql
-- Set drift policy on the corpus stream table
SELECT pgtrickle.alter_stream_table(
  'document_corpus',
  post_refresh_action     => 'reindex_if_drift',
  reindex_drift_threshold => 0.20,
  reindex_scope           => 'per_partition'  -- v0.38+
);
```

The `per_partition` scope applies the drift threshold per-tenant (or per-partition for partitioned tables), so only the tenants with high churn get their indexes rebuilt.

---

## What Isolation Level You Actually Get

With this setup:
- **Data isolation**: RLS enforces it. Even if application code is buggy, cross-tenant reads are impossible.
- **Index isolation**: Partial indexes ensure ANN search stays within tenant boundaries.
- **Freshness isolation**: Stream table refresh applies to all tenants uniformly. A tenant with high write volume doesn't delay freshness for others.
- **Schema isolation**: None. All tenants share the same schema. For full schema isolation (different columns per tenant), you'd need separate tables or a tenant-specific metadata system.

The tradeoff: shared infrastructure is efficient and manageable. True schema-level isolation requires separate tables or separate database instances, which is operationally much heavier.

For most multi-tenant SaaS use cases — same schema, different data — the RLS + stream table + partial index approach provides correct isolation at operational sanity.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
