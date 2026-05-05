# Hybrid Search Patterns with pg_trickle

> VH-3 (v0.48.0) — Cookbook for BM25 + vector + metadata retrieval on
> incrementally maintained stream tables.

## Overview

pg_trickle makes it easy to maintain a hybrid-search corpus — combining
full-text (BM25) search, vector similarity, and structured metadata filters —
using a single stream table that stays fresh automatically.

---

## Pattern 1: Flat Denormalised Corpus

The simplest pattern: one stream table holds everything needed for a hybrid
search query.

```sql
-- Source tables
CREATE TABLE documents (
    id          BIGSERIAL PRIMARY KEY,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL,
    category    TEXT,
    created_at  TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE document_embeddings (
    doc_id      BIGINT PRIMARY KEY REFERENCES documents(id),
    embedding   vector(1536) NOT NULL,
    updated_at  TIMESTAMPTZ DEFAULT now()
);

-- Hybrid search corpus stream table
SELECT pgtrickle.create_stream_table(
    'hybrid_corpus',
    $$
        SELECT
            d.id,
            d.title,
            d.body,
            d.category,
            d.created_at,
            e.embedding,
            to_tsvector('english', d.title || ' ' || d.body) AS fts_vector
        FROM documents d
        JOIN document_embeddings e ON e.doc_id = d.id
    $$,
    '30s',
    'DIFFERENTIAL'
);

-- Full-text index for BM25
CREATE INDEX ON hybrid_corpus USING gin(fts_vector);

-- Vector index for ANN search
CREATE INDEX ON hybrid_corpus USING hnsw(embedding vector_cosine_ops);
```

Alternatively, use the one-call API:

```sql
SELECT pgtrickle.embedding_stream_table(
    'hybrid_corpus_v2',
    'document_embeddings',
    'embedding',
    extra_columns => 'doc_id, updated_at'
);
```

### Hybrid query

```sql
-- Combine BM25 rank and cosine similarity
SELECT
    id,
    title,
    ts_rank(fts_vector, query) AS bm25_score,
    1 - (embedding <=> '[...]'::vector) AS cosine_score,
    ts_rank(fts_vector, query) * 0.4 + (1 - (embedding <=> '[...]'::vector)) * 0.6 AS hybrid_score
FROM
    hybrid_corpus,
    plainto_tsquery('english', 'your search terms') AS query
WHERE
    fts_vector @@ query
    OR embedding <=> '[...]'::vector < 0.3
ORDER BY hybrid_score DESC
LIMIT 20;
```

---

## Pattern 2: RLS-Scoped Corpus (Multi-Tenant)

See [PER_TENANT_ANN_PATTERNS.md](PER_TENANT_ANN_PATTERNS.md) for detailed
multi-tenant patterns. For single-tenant, enable RLS on the stream table:

```sql
ALTER TABLE hybrid_corpus ENABLE ROW LEVEL SECURITY;

CREATE POLICY corpus_access ON hybrid_corpus
    USING (category = current_setting('app.user_category', true));
```

---

## Pattern 3: Tiered Storage (halfvec + sparsevec)

Use pg_trickle's `halfvec_avg` / `sparsevec_avg` support (VH-1) to maintain
storage-efficient tiers:

```sql
-- Full-precision embeddings
SELECT pgtrickle.create_stream_table(
    'embeddings_full',
    'SELECT id, embedding FROM raw_embeddings',
    '1m', 'DIFFERENTIAL'
);

-- Half-precision for HNSW index efficiency  
SELECT pgtrickle.create_stream_table(
    'embeddings_half',
    'SELECT id, embedding::halfvec(1536) AS embedding FROM raw_embeddings',
    '1m', 'DIFFERENTIAL'
);
CREATE INDEX ON embeddings_half USING hnsw(embedding halfvec_cosine_ops);

-- Per-category centroid for sparse representations
SELECT pgtrickle.create_stream_table(
    'category_centroids',
    $$
        SELECT category, avg(embedding) AS centroid
        FROM raw_embeddings
        GROUP BY category
    $$,
    '5m', 'DIFFERENTIAL'
);
SET pg_trickle.enable_vector_agg = on;
```

---

## Performance Tuning Notes

| Tip | Recommendation |
|-----|---------------|
| HNSW `m` parameter | Default 16; increase to 32–64 for high-recall |
| HNSW `ef_construction` | Default 64; increase for better recall at index build cost |
| Index maintenance | Use `post_refresh_action = 'reindex_if_drift'` for automatic drift-based REINDEX |
| halfvec storage | ~50% storage savings vs `vector`; use for index columns when precision allows |
| Refresh interval | Match to your ingestion rate; 30s–5m is typical for RAG |

---

## Latency Assertions

Measure your actual p99 latencies before adding hard latency gates —
pg_trickle publishes measured baselines via `pgtrickle.vector_status()`,
not aspirational numbers.

```sql
-- Check embedding lag and drift
SELECT name, embedding_lag, drift_pct, last_reindex_at
FROM pgtrickle.vector_status();
```
