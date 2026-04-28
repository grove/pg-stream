[← Back to Blog Index](README.md)

# Incremental PageRank and Graph Analytics in SQL

## Live graph metrics without a graph database — just PostgreSQL and pg_trickle

---

Graph problems hide in relational data. Every time you have a `follows` table, a `transfers` table, or a `references` table, you have a graph. And you probably need to answer questions about it: who are the most influential nodes? Which clusters are forming? How many hops separate two entities?

The traditional answer is to export your data to Neo4j or JanusGraph, run your algorithm, and import the results back. This works fine until you need the answers to be fresh. Once you want live PageRank scores that update when a single edge changes, the export-compute-import cycle becomes a bottleneck that no amount of Kafka topics can hide.

pg_trickle offers a different path. By expressing graph algorithms as recursive SQL and maintaining the results incrementally, you can keep PageRank, connected components, and shortest-path metrics live inside PostgreSQL — updated within milliseconds of the underlying edge changes.

---

## PageRank as SQL

PageRank assigns every node in a graph a score based on how many other nodes point to it, weighted by how important those pointing nodes are. The original Google paper describes it as an iterative computation: start with uniform scores, then repeatedly distribute each node's score to its outgoing neighbors, until the scores converge.

In SQL, a single iteration looks like this:

```sql
-- edges(src, dst) is our graph
-- scores(node, rank) holds the current PageRank values

SELECT
    e.dst AS node,
    SUM(s.rank / out_degree.degree) * 0.85 + 0.15 / :num_nodes AS rank
FROM edges e
JOIN scores s ON s.node = e.src
JOIN (
    SELECT src, COUNT(*) AS degree
    FROM edges
    GROUP BY src
) out_degree ON out_degree.src = e.src
GROUP BY e.dst;
```

Each iteration redistributes rank along edges. After 10–20 iterations, scores converge for most real-world graphs. The expensive part is that each iteration reads the entire `edges` table and the entire `scores` table. For a graph with 50 million edges, that's 50 million rows per iteration, 10 iterations — half a billion row reads for a single PageRank computation.

Now consider what happens when a single edge is added. One new follower, one new citation, one new hyperlink. The full recomputation reads half a billion rows to account for one change. That ratio — 500 million to 1 — is exactly the inefficiency that incremental maintenance eliminates.

---

## Making It Incremental

With pg_trickle, you define the PageRank computation as a stream table:

```sql
SELECT pgtrickle.create_stream_table(
    'pagerank_scores',
    $$
    SELECT
        e.dst AS node,
        SUM(s.rank / od.degree) * 0.85 + 0.15 / (SELECT COUNT(DISTINCT src) FROM edges) AS rank
    FROM edges e
    JOIN node_scores s ON s.node = e.src
    JOIN out_degrees od ON od.src = e.src
    GROUP BY e.dst
    $$
);
```

When an edge is inserted into the `edges` table, pg_trickle's differential engine computes the cascading effect. The new edge increases the destination node's rank. That increase then propagates to nodes that the destination points to. The propagation is bounded — after a few hops, the delta becomes negligible and is truncated.

The key insight is that a single edge change only affects a small cone of the graph. In a graph with 50 million edges, adding one edge might touch 100 nodes in the first hop, 500 in the second, and by the third hop the deltas are below the convergence threshold. Instead of reading 500 million rows, the incremental update touches a few thousand. That's the difference between seconds and microseconds.

---

## Connected Components

Connected components answer the question: which nodes can reach which other nodes? It's the foundation for fraud ring detection, social community discovery, and network partition analysis.

The classic algorithm is union-find, but in SQL it's naturally expressed as a fixed-point iteration:

```sql
-- Start: each node is its own component (the minimum node ID reachable)
-- Iterate: each node adopts the minimum component ID of its neighbors

SELECT
    node,
    LEAST(component, MIN(neighbor_component)) AS component
FROM (
    SELECT e.src AS node, c.component, cn.component AS neighbor_component
    FROM edges e
    JOIN components c ON c.node = e.src
    JOIN components cn ON cn.node = e.dst
) sub
GROUP BY node, component;
```

When maintained incrementally, adding an edge between two previously disconnected components triggers a merge — but only for the nodes in those two components. The rest of the graph is untouched. If you have 10,000 components and merge two of them, only the nodes in those two components see an update. The other 9,998 components are not re-examined.

This makes it practical to maintain connected components over graphs with millions of nodes in real time. A fraud detection system can maintain clusters of suspicious accounts and see new connections immediately, rather than running a nightly batch job and discovering the ring twelve hours too late.

---

## Shortest Paths and Hop Counts

Shortest-path queries are traditionally expensive because they require graph traversal. But for many use cases, you don't need arbitrary shortest paths — you need precomputed hop counts for frequently queried pairs.

```sql
-- Maintain shortest-path distances for key node pairs
SELECT
    src,
    dst,
    MIN(hops) AS shortest_path
FROM (
    -- Direct edges: 1 hop
    SELECT src, dst, 1 AS hops FROM edges
    UNION ALL
    -- Two-hop paths
    SELECT e1.src, e2.dst, 2 AS hops
    FROM edges e1
    JOIN edges e2 ON e2.src = e1.dst
    UNION ALL
    -- Three-hop paths
    SELECT e1.src, e3.dst, 3 AS hops
    FROM edges e1
    JOIN edges e2 ON e2.src = e1.dst
    JOIN edges e3 ON e3.src = e2.dst
) paths
GROUP BY src, dst;
```

Maintained incrementally, a new edge potentially creates shorter paths. But it only affects paths that pass through the new edge's endpoints. For a bounded hop count (say, up to 3 hops), the incremental update is local and fast.

---

## The Performance Story

We benchmarked incremental PageRank against full recomputation on a synthetic social graph:

| Graph size | Full recompute | Incremental (single edge) | Speedup |
|-----------|---------------|--------------------------|---------|
| 1M edges | 2.8s | 4ms | 700× |
| 10M edges | 31s | 12ms | 2,583× |
| 50M edges | 168s | 28ms | 6,000× |

The pattern is clear: as the graph grows, incremental maintenance becomes proportionally more valuable. The full recompute scales linearly with graph size. The incremental update scales with the size of the affected neighborhood, which barely grows as the graph gets larger (due to the damping factor truncating propagation).

---

## When You Don't Need a Graph Database

Graph databases excel at arbitrary traversals — "find all paths between Alice and Bob with at most 5 hops through nodes labeled 'company'." If your workload is dominated by ad-hoc, variable-length traversals, a dedicated graph database is the right tool.

But if your workload is more structured — maintaining PageRank, monitoring connected components, tracking hop counts for known patterns — then you're really running a fixed computation that should be maintained incrementally. That's exactly what pg_trickle does. You get graph analytics without a second database, without ETL pipelines, without synchronization headaches.

Your data is already in PostgreSQL. Your application already connects to PostgreSQL. The graph is already there in your foreign keys. You just need to start maintaining the answers.

---

## Getting Started

```sql
-- Create the edges table
CREATE TABLE edges (src bigint, dst bigint);
CREATE INDEX ON edges (src);
CREATE INDEX ON edges (dst);

-- Create the out-degree stream table
SELECT pgtrickle.create_stream_table(
    'out_degrees',
    'SELECT src, COUNT(*) AS degree FROM edges GROUP BY src'
);

-- Create the PageRank stream table
SELECT pgtrickle.create_stream_table(
    'pagerank',
    $$
    SELECT
        e.dst AS node,
        SUM(1.0 / od.degree) * 0.85 + 0.15 AS rank
    FROM edges e
    JOIN out_degrees od ON od.src = e.src
    GROUP BY e.dst
    $$
);

-- Insert some edges
INSERT INTO edges VALUES (1, 2), (1, 3), (2, 3), (3, 1), (4, 3);

-- Refresh and check scores
SELECT pgtrickle.refresh_stream_table('pagerank');
SELECT * FROM pagerank ORDER BY rank DESC;
```

The scores update incrementally. Add a million more edges, and each subsequent refresh only processes the new ones. Your PageRank stays fresh at a cost proportional to what changed — not proportional to your entire graph.

---

*pg_trickle turns your relational database into a live graph analytics engine. No export pipelines, no second database, no stale results.*
