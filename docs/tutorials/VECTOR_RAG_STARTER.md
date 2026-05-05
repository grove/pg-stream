# pg_trickle Starter: Vector RAG Corpus

> VA-5 (v0.48.0) — Quick-start for building a production RAG corpus with
> pg_trickle and pgvector.

## Prerequisites

- PostgreSQL 18+ with pg_trickle installed
- pgvector extension

## 5-Minute Quick Start

```sql
-- 1. Enable required extensions
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trickle;

-- 2. Create source table
CREATE TABLE documents (
    id         BIGSERIAL PRIMARY KEY,
    content    TEXT NOT NULL,
    metadata   JSONB,
    embedding  vector(1536),
    updated_at TIMESTAMPTZ DEFAULT now()
);

-- 3. Create embedding stream table (one call does everything)
SELECT pgtrickle.embedding_stream_table(
    'doc_corpus',           -- stream table name
    'documents',            -- source table
    'embedding',            -- vector column
    refresh_interval => '1m'
);

-- 4. Insert documents (populate embeddings from your model)
INSERT INTO documents (content, embedding) VALUES
    ('Hello world', '[0.1,0.2,...]'),
    ('Another doc', '[0.3,0.4,...]');

-- 5. Query with vector similarity
SELECT id, content, embedding <=> '[0.1,0.2,...]'::vector AS distance
FROM doc_corpus
ORDER BY embedding <=> '[0.1,0.2,...]'::vector
LIMIT 5;
```

## Next Steps

- **Hybrid search**: Add a `tsvector` column — see [HYBRID_SEARCH_PATTERNS.md](../tutorials/HYBRID_SEARCH_PATTERNS.md)
- **Multi-tenant isolation**: See [PER_TENANT_ANN_PATTERNS.md](../tutorials/PER_TENANT_ANN_PATTERNS.md)
- **Distance alerts**: Use `subscribe_distance()` for real-time anomaly detection
- **Embedding outbox**: Use `attach_embedding_outbox()` to publish embedding changes downstream

## Architecture Diagram

```
┌─────────────┐   INSERT/UPDATE   ┌──────────────────┐
│  documents  │ ──────────────── ▶│  doc_corpus (ST) │
│  (source)   │                   │  HNSW index      │
└─────────────┘                   └────────┬─────────┘
                                            │
                                  ┌─────────▼──────────┐
                                  │  Your application  │
                                  │  (vector search)   │
                                  └────────────────────┘
```

## Ecosystem

pg_trickle works alongside:

| Tool | Role |
|------|------|
| pgvector | Vector storage and ANN search |
| pg_tide | Transactional outbox for embedding events |
| LangChain / LlamaIndex | RAG framework integration |
| pgai | Automated embedding generation from model APIs |

See the [pg_trickle blog](https://pg-trickle.dev/blog) for in-depth integration guides.
