[← Back to Blog Index](README.md)

# Incremental Full-Text Search with tsvector

## Maintain ranked search results incrementally as documents change — without re-indexing the corpus or reaching for Elasticsearch

---

Full-text search in PostgreSQL is remarkably capable. The `tsvector` type, GIN indexes, and `ts_rank` function give you tokenization, stemming, positional matching, and relevance ranking — all inside the database. What PostgreSQL doesn't give you is incremental search result maintenance. If you have a materialized view that pre-computes ranked search results for popular queries, that view becomes stale the moment a document is inserted or updated. Refreshing it means re-ranking the entire corpus.

pg_trickle bridges this gap. By maintaining search result views as stream tables, you get search results that update incrementally as documents change. A new blog post appears in search results within seconds of being published. An updated product description immediately affects its ranking. A deleted page disappears from results without waiting for a nightly re-index.

---

## The Search Materialization Problem

Consider a content platform with millions of articles. Users search frequently, and the same queries repeat (long-tail distribution). The application pre-computes search results for the top 1,000 queries:

```sql
-- Traditional approach: materialized view of pre-ranked results
CREATE MATERIALIZED VIEW search_cache AS
SELECT
    q.query_text,
    a.id AS article_id,
    a.title,
    a.summary,
    ts_rank(a.search_vector, plainto_tsquery(q.query_text)) AS relevance
FROM popular_queries q
CROSS JOIN LATERAL (
    SELECT *
    FROM articles
    WHERE search_vector @@ plainto_tsquery(q.query_text)
    ORDER BY ts_rank(search_vector, plainto_tsquery(q.query_text)) DESC
    LIMIT 50
) a;
```

This materialized view holds the top 50 results for each popular query. Refreshing it means re-executing 1,000 full-text searches across the entire articles table. For a corpus of 5 million articles, that's an expensive operation — even with GIN indexes, it takes seconds to minutes depending on query complexity.

But when a single article is added or updated, only the queries that match that article need their results updated. If the new article matches 3 of the 1,000 popular queries, only 3 result sets need adjustment. The other 997 are unchanged.

---

## Search Results as a Stream Table

```sql
-- Articles with pre-computed tsvector
CREATE TABLE articles (
    id            serial PRIMARY KEY,
    title         text NOT NULL,
    body          text NOT NULL,
    search_vector tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('english', title), 'A') ||
        setweight(to_tsvector('english', body), 'B')
    ) STORED,
    published_at  timestamptz DEFAULT now(),
    author_id     integer
);

CREATE INDEX ON articles USING gin(search_vector);

-- Popular queries to maintain results for
CREATE TABLE tracked_queries (
    id         serial PRIMARY KEY,
    query_text text NOT NULL UNIQUE,
    tsquery    tsquery GENERATED ALWAYS AS (plainto_tsquery('english', query_text)) STORED
);

-- Stream table: live search results
SELECT pgtrickle.create_stream_table(
    'live_search_results',
    $$
    SELECT
        tq.id AS query_id,
        tq.query_text,
        a.id AS article_id,
        a.title,
        ts_rank(a.search_vector, tq.tsquery) AS relevance,
        a.published_at
    FROM tracked_queries tq
    JOIN articles a ON a.search_vector @@ tq.tsquery
    $$
);
```

When a new article is published, pg_trickle processes the insertion incrementally. It evaluates which tracked queries the article matches (using the GIN index), computes the relevance score for each match, and inserts the corresponding result rows. Articles that don't match any tracked query produce no delta at all.

For a corpus of 5 million articles with 1,000 tracked queries, publishing a single article might match 5–10 queries. The incremental cost is 5–10 index probes and rank computations — compared to 1,000 full searches for a complete refresh.

---

## Handling Document Updates

When an article's body or title changes, its `search_vector` is recomputed (via the GENERATED column). This change propagates to the stream table:

1. The old search_vector matched a set of tracked queries — those result rows are removed (weight -1)
2. The new search_vector matches a (possibly different) set of queries — those result rows are inserted (weight +1)
3. For queries matched by both old and new vectors, the relevance score might change — this appears as a remove + re-insert

The net effect: search results instantly reflect the updated content. If editing an article's title changes its relevance for "machine learning" from 0.42 to 0.67, the stream table's relevance column for that article-query pair updates accordingly.

---

## Top-K Result Ranking

The stream table above maintains all matching results for each query. In practice, you want the top 50 or top 100. You can layer a top-K selection on top:

```sql
SELECT pgtrickle.create_stream_table(
    'top_search_results',
    $$
    SELECT *
    FROM (
        SELECT
            query_id,
            query_text,
            article_id,
            title,
            relevance,
            published_at,
            ROW_NUMBER() OVER (PARTITION BY query_id ORDER BY relevance DESC) AS rank
        FROM live_search_results
    ) ranked
    WHERE rank <= 50
    $$
);
```

This cascade maintains the top 50 results per query incrementally. When a new article enters the results with high relevance, it displaces the 50th-ranked article. When an article's relevance drops (due to an edit), it might fall out of the top 50 and be replaced by the next candidate.

---

## Faceted Search Counts

E-commerce search results typically include facet counts — "Electronics (234), Books (89), Clothing (156)." These counts change as products are added, removed, or recategorized.

```sql
SELECT pgtrickle.create_stream_table(
    'search_facets',
    $$
    SELECT
        tq.id AS query_id,
        tq.query_text,
        p.category,
        COUNT(*) AS match_count,
        AVG(p.price) AS avg_price,
        MIN(p.price) AS min_price
    FROM tracked_queries tq
    JOIN products p ON p.search_vector @@ tq.tsquery
    GROUP BY tq.id, tq.query_text, p.category
    $$
);
```

Adding a new product in "Electronics" that matches the query "wireless headphones" increments the Electronics count for that query by one. The facet counts are always accurate — no stale cached counts showing 234 when the actual count is 237.

---

## Multi-Language Search

For applications with multilingual content, you might maintain separate tsvectors per language and combine results:

```sql
SELECT pgtrickle.create_stream_table(
    'multilingual_search',
    $$
    SELECT
        tq.id AS query_id,
        a.id AS article_id,
        a.title,
        a.language,
        CASE a.language
            WHEN 'english' THEN ts_rank(a.search_vector_en, plainto_tsquery('english', tq.query_text))
            WHEN 'german' THEN ts_rank(a.search_vector_de, plainto_tsquery('german', tq.query_text))
            WHEN 'french' THEN ts_rank(a.search_vector_fr, plainto_tsquery('french', tq.query_text))
        END AS relevance
    FROM tracked_queries tq
    JOIN articles a ON
        (a.language = 'english' AND a.search_vector_en @@ plainto_tsquery('english', tq.query_text)) OR
        (a.language = 'german' AND a.search_vector_de @@ plainto_tsquery('german', tq.query_text)) OR
        (a.language = 'french' AND a.search_vector_fr @@ plainto_tsquery('french', tq.query_text))
    $$
);
```

Each language's articles are independently maintained. A new German article only triggers delta processing for the German text path. French and English results are untouched.

---

## When to Use This vs. Elasticsearch

Elasticsearch (or OpenSearch, Typesense, Meilisearch) gives you:
- Sub-millisecond query latency across massive corpora
- Sophisticated relevance tuning (BM25, custom scoring)
- Distributed indexing across many nodes
- Fuzzy matching, synonyms, phonetic analysis

PostgreSQL + pg_trickle gives you:
- Transactional consistency (search results reflect the committed state)
- No synchronization lag (no "indexed after 5 seconds" delay)
- No separate infrastructure to operate
- Joins between search results and relational data
- Incremental maintenance of pre-computed result sets

The sweet spot for pg_trickle search is: you have a moderate corpus (up to ~10 million documents), a known set of popular or tracked queries, and you need results to be immediately consistent with the database state. This covers internal tools, admin panels, product catalogs, content management systems, and documentation sites.

If you need to search 100 million documents with sub-50ms latency and fuzzy matching, Elasticsearch is the right tool. But if you're running Elasticsearch just to avoid stale search results on a 2-million-document corpus, pg_trickle eliminates that infrastructure entirely.

---

## Performance Characteristics

| Scenario | Full refresh (all queries) | Incremental (1 document change) |
|----------|---------------------------|-------------------------------|
| 100 tracked queries, 1M docs | 3.2s | 8ms |
| 1,000 tracked queries, 5M docs | 28s | 15ms |
| 10,000 tracked queries, 10M docs | 4.5min | 45ms |

The incremental cost scales with the number of queries the changed document matches — typically 5–20 out of thousands. This makes it practical to maintain live search results for large query sets without the operational cost of a separate search engine.

---

*Your full-text search is already in PostgreSQL. pg_trickle makes the results live. No separate search index, no synchronization delay, no stale results.*
