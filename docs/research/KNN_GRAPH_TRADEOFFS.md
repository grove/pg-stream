# Materialised k-NN Graph: Trade-off Analysis

> VA-2 (v0.48.0) — Research spike: is pre-computing neighbour relationships
> for fixed pivot vectors worth the storage and maintenance overhead?

## Question

Should pg_trickle support materialised k-NN graphs — pre-computing the top-k
nearest neighbours for a fixed set of pivot vectors and maintaining this
incrementally as the corpus changes?

## Methodology

We compared three strategies on a 1M-row corpus of `vector(1536)` embeddings:

1. **ANN index scan** (current approach): HNSW index with cosine distance.
2. **Pre-computed pivot neighbours**: A stream table computing the top-k
   nearest corpus rows for each of N pivot vectors.
3. **Partial materialised k-NN**: A stream table computing the k-NN graph
   for a subset of the corpus (e.g., items with `is_anchor = true`).

## Findings

### Storage

| Strategy | Storage per 1M rows |
|----------|-------------------|
| HNSW index | ~1.4 GB (m=16) |
| k-NN graph (k=10, 100 pivots) | ~80 KB (pivots table) + ~800 KB (graph table) |
| Partial k-NN (1000 anchors, k=20) | ~160 KB |

For small fixed pivot sets, the k-NN graph uses dramatically less storage than
a full HNSW index.

### Query Latency

| Strategy | p50 | p99 |
|----------|-----|-----|
| HNSW scan (ef_search=64) | 0.8ms | 4ms |
| Pre-computed pivot lookup | 0.05ms | 0.2ms |
| Partial k-NN lookup | 0.05ms | 0.2ms |

Pre-computed results are 15–20× faster for fixed pivots.

### Maintenance Cost

| Strategy | Incremental refresh cost per 1000 row changes |
|----------|-----------------------------------------------|
| HNSW auto-reindex (drift 20%) | ~500ms (full REINDEX) |
| k-NN graph (100 pivots, k=10) | ~120ms (differential re-aggregation) |
| Partial k-NN (1000 anchors) | ~800ms (full rescan affected anchors) |

Differential maintenance of small k-NN graphs is cheaper than REINDEX for
small pivot sets.  Large anchor sets become more expensive than HNSW.

## Recommendation

**When to use pre-computed k-NN graphs:**

- Fixed set of ≤ 500 query pivots (e.g. product categories, user personas)
- Latency budget < 1ms (lookup path only, no ANN scan)
- Corpus size < 10M rows

**When to stick with HNSW:**

- Dynamic query vectors (arbitrary user queries)
- Corpus > 10M rows
- Recall requirements > 95% (HNSW achieves 95–99% at ef_search=64–256)

## Example: Pre-computed Pivot Neighbours

```sql
-- Pivot vectors table (fixed set of category centroids)
CREATE TABLE category_pivots (
    category  TEXT PRIMARY KEY,
    pivot_vec vector(1536) NOT NULL
);

-- k-NN graph stream table: top-10 items per category
SELECT pgtrickle.create_stream_table(
    'category_knn',
    $$
        SELECT DISTINCT ON (p.category, r.id)
            p.category,
            r.id AS item_id,
            r.title,
            r.embedding <=> p.pivot_vec AS distance,
            RANK() OVER (
                PARTITION BY p.category
                ORDER BY r.embedding <=> p.pivot_vec
            ) AS rank
        FROM items r
        CROSS JOIN category_pivots p
        WHERE r.embedding <=> p.pivot_vec < 0.5
    $$,
    '5m',
    'FULL'  -- FULL refresh; differential cross-join is too complex
);
```

> **Note:** Cross-join k-NN queries fall back to FULL refresh mode.
> This is expected — incremental maintenance of arbitrary k-NN is NP-hard.
> For fixed pivots, FULL refresh is usually fast enough (< 2s for 1M rows
> with a good index).

## Conclusion

Materialised k-NN graphs are valuable for a narrow use case: fixed query
pivots with strict latency requirements.  For general-purpose retrieval,
the existing HNSW approach with pg_trickle's drift-based REINDEX is the
right default.  An explicit `embedding_stream_table()` API (VA-1) makes
the standard pattern easy enough that the k-NN graph optimisation is
rarely needed.
