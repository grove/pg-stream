-- examples/non-differential.sql
--
-- Stream table examples for queries that cannot be maintained differentially.
--
-- DIFFERENTIAL and IMMEDIATE modes rely on CDC-based incremental delta
-- computation.  The features shown here make that computation impossible or
-- incorrect, so they require:
--
--   refresh_mode => 'FULL'     -- explicit full recomputation every cycle
--   refresh_mode => 'AUTO'     -- automatic; pgtrickle detects the limitation
--                              -- and silently downgrades to FULL
--
-- Each section describes one feature, why it forces FULL, and one runnable
-- example including the source schema.
--
-- Features covered in this file:
--
--   1.  random()           — volatile stochastic sampling
--   2.  clock_timestamp()  — real-time wall-clock timestamp
--   3.  gen_random_uuid()  — volatile UUID generation
--   4.  nextval()          — sequence consumption
--   5.  txid_current()     — current transaction identifier
--   6.  Materialized view  — CDC cannot track REFRESH MATERIALIZED VIEW
--   7.  Foreign table      — row-level triggers cannot be placed on FDW tables
--
-- After each working example there is a note on TABLESAMPLE and FOR UPDATE,
-- which are completely blocked (rejected even with refresh_mode => 'FULL').
-- ──────────────────────────────────────────────────────────────────────────


-- ══════════════════════════════════════════════════════════════════════════
-- 1.  random() — volatile stochastic / probability sampling
-- ══════════════════════════════════════════════════════════════════════════
--
-- WHY FULL:  random() is VOLATILE — it returns a different value on every
-- call, even within the same query.  The delta engine needs to re-evaluate
-- each expression on exactly the same rows it saw before; a volatile
-- function can flip a row in or out of the result between the pre-change
-- snapshot and the post-change evaluation, producing phantom inserts and
-- phantom deletes that have nothing to do with actual source changes.
--
-- FULL refresh re-evaluates the entire query from scratch so random() is
-- called once per row per refresh cycle — correct and consistent.
--
-- USE CASE:  1-in-10 random sample of a large events table, refreshed every
-- minute for an approximate real-time dashboard at reduced data volume.

CREATE TABLE events (
    id          BIGINT PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
    user_id     BIGINT NOT NULL,
    event_type  TEXT   NOT NULL,
    payload     JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

SELECT pgtrickle.create_stream_table(
    'public.events_sample',
    $$
        SELECT id, user_id, event_type, payload, created_at
        FROM   events
        WHERE  random() < 0.10          -- ~10 % stochastic sample
    $$,
    schedule      => '1m',
    refresh_mode  => 'FULL'
);


-- ══════════════════════════════════════════════════════════════════════════
-- 2.  clock_timestamp() — real-time wall-clock timestamp
-- ══════════════════════════════════════════════════════════════════════════
--
-- WHY FULL:  clock_timestamp() is VOLATILE — it advances with real time
-- even within a single transaction, so two evaluations of the same row
-- produce different values.  now() and current_timestamp are STABLE
-- (fixed for the whole transaction) and ARE allowed in DIFFERENTIAL mode;
-- only the fine-grained wall-clock variants are rejected.
--
-- FULL refresh stamps each row with the actual wall-clock time at the
-- moment the refresh runs, which is the correct semantics here.
--
-- USE CASE:  Materialized snapshot of pending orders with their age
-- computed against the real current time (not transaction start time).

CREATE TABLE orders (
    id          BIGINT PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
    customer_id BIGINT      NOT NULL,
    amount      NUMERIC     NOT NULL,
    status      TEXT        NOT NULL DEFAULT 'pending',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

SELECT pgtrickle.create_stream_table(
    'public.pending_orders_age',
    $$
        SELECT
            id,
            customer_id,
            amount,
            created_at,
            -- clock_timestamp() gives the true wall-clock age, not the
            -- age relative to transaction start (which now() would give).
            EXTRACT(EPOCH FROM (clock_timestamp() - created_at))::INT
                AS age_seconds
        FROM   orders
        WHERE  status = 'pending'
    $$,
    schedule      => '30s',
    refresh_mode  => 'FULL'
);


-- ══════════════════════════════════════════════════════════════════════════
-- 3.  gen_random_uuid() — volatile UUID generation
-- ══════════════════════════════════════════════════════════════════════════
--
-- WHY FULL:  gen_random_uuid() is VOLATILE.  On a DIFFERENTIAL refresh,
-- the delta engine re-evaluates changed rows to compute the new result;
-- re-evaluating gen_random_uuid() for the same input row produces a
-- different UUID each time, so the row would be flagged as changed on
-- every refresh even when nothing in the source actually changed.
--
-- FULL refresh generates UUIDs once per row per refresh cycle; as long as
-- the downstream consumer treats the UUID as a refresh-scoped key (not a
-- persistent row identifier) the result is correct.
--
-- USE CASE:  Export feed that must include a unique nonce per-row in each
-- snapshot for idempotent downstream deduplication.

CREATE TABLE products (
    id          INT  PRIMARY KEY,
    name        TEXT NOT NULL,
    unit_price  NUMERIC NOT NULL,
    active      BOOLEAN NOT NULL DEFAULT TRUE
);

SELECT pgtrickle.create_stream_table(
    'public.product_export',
    $$
        SELECT
            gen_random_uuid() AS export_nonce,  -- fresh nonce each refresh
            id,
            name,
            unit_price
        FROM   products
        WHERE  active
    $$,
    schedule      => '5m',
    refresh_mode  => 'FULL'
);


-- ══════════════════════════════════════════════════════════════════════════
-- 4.  nextval() — sequence consumption
-- ══════════════════════════════════════════════════════════════════════════
--
-- WHY FULL:  nextval() is VOLATILE and has the extra problem of side
-- effects: each call permanently advances the sequence.  Incremental delta
-- computation would call nextval() again for every changed row to compute
-- the "new" value, consuming sequence slots and producing a different
-- number than was originally assigned.  FULL refresh consumes exactly one
-- sequence value per row per refresh cycle.
--
-- USE CASE:  Refresh-scoped line-item numbering for an invoice export view.
-- The sequence is reset between refreshes to produce stable output numbers.

CREATE SEQUENCE invoice_line_seq START 1;

CREATE TABLE invoice_lines (
    id          BIGINT PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
    invoice_id  BIGINT  NOT NULL,
    product_id  INT     NOT NULL,
    quantity    INT     NOT NULL,
    unit_price  NUMERIC NOT NULL
);

SELECT pgtrickle.create_stream_table(
    'public.invoice_export',
    $$
        SELECT
            nextval('invoice_line_seq') AS line_no,
            invoice_id,
            product_id,
            quantity,
            unit_price,
            quantity * unit_price AS line_total
        FROM   invoice_lines
        ORDER  BY invoice_id, id
    $$,
    schedule      => '10m',
    refresh_mode  => 'FULL'
);


-- ══════════════════════════════════════════════════════════════════════════
-- 5.  txid_current() — current transaction identifier
-- ══════════════════════════════════════════════════════════════════════════
--
-- WHY FULL:  txid_current() is VOLATILE — it returns the XID of the
-- current transaction, which changes on every evaluation.  Adding any use
-- of it to a query makes incremental maintenance impossible for the same
-- reason as random(): each re-evaluation of the same source row produces
-- a different XID and the delta engine cannot distinguish "row changed" from
-- "volatile function returned different value".
--
-- USE CASE:  Audit snapshot that tags each row with the XID at which the
-- last full refresh ran — useful for external replication monitoring.

CREATE TABLE audit_log (
    id          BIGINT PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
    table_name  TEXT        NOT NULL,
    operation   TEXT        NOT NULL,
    changed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    changed_by  TEXT        NOT NULL DEFAULT current_user
);

SELECT pgtrickle.create_stream_table(
    'public.audit_snapshot',
    $$
        SELECT
            id,
            table_name,
            operation,
            changed_at,
            changed_by,
            txid_current() AS snapshot_xid   -- XID at refresh time
        FROM   audit_log
        WHERE  changed_at >= now() - INTERVAL '7 days'
    $$,
    schedule      => '1h',
    refresh_mode  => 'FULL'
);


-- ══════════════════════════════════════════════════════════════════════════
-- 6.  Materialized view as source
-- ══════════════════════════════════════════════════════════════════════════
--
-- WHY FULL:  CDC row-level triggers track INSERT, UPDATE, DELETE on plain
-- heap tables.  A materialized view is refreshed via
--   REFRESH MATERIALIZED VIEW ...
-- which internally truncates and re-inserts the data without firing
-- row-level triggers on the view itself.  pgtrickle therefore cannot see
-- what changed — the change buffer stays empty and the stream table would
-- never update.
--
-- With FULL refresh, pgtrickle simply re-queries the materialized view on
-- each cycle; staleness relative to the base tables is limited by how often
-- the materialized view is refreshed externally.
--
-- ALTERNATIVE:  Use the underlying query directly (view inlining) so that
-- CDC triggers land on the real base tables.  Or enable polling-based CDC:
--   SET pg_trickle.matview_polling = on;
--
-- USE CASE:  Stream table consuming a third-party materialized view that is
-- not owned by the current application schema.

CREATE MATERIALIZED VIEW monthly_revenue AS
    SELECT
        date_trunc('month', created_at) AS month,
        SUM(amount)                     AS revenue
    FROM   orders
    GROUP  BY 1;

SELECT pgtrickle.create_stream_table(
    'public.revenue_top3',
    $$
        SELECT month, revenue
        FROM   monthly_revenue
        ORDER  BY revenue DESC
        LIMIT  3
    $$,
    schedule      => '1h',
    refresh_mode  => 'AUTO'
);


-- ══════════════════════════════════════════════════════════════════════════
-- 7.  Foreign table (postgres_fdw / file_fdw / …) as source
-- ══════════════════════════════════════════════════════════════════════════
--
-- WHY FULL:  Row-level AFTER triggers cannot be created on foreign tables
-- (neither postgres_fdw nor most other FDWs support them), and foreign
-- tables do not generate local WAL entries.  pgtrickle therefore has no way
-- to observe which rows changed in the remote data source.
--
-- With FULL refresh, pgtrickle re-queries the foreign table on each cycle.
-- Latency is bounded by the refresh schedule and the FDW round-trip time.
--
-- ALTERNATIVE:  Create a local copy of the remote data with
--   pg_trickle.foreign_table_polling = on
-- which polls the full table periodically and derives a diff. For large
-- remote tables, FULL mode with an appropriate schedule is simpler.
--
-- USE CASE:  Aggregating rows from a remote ERP database via postgres_fdw.

-- Prerequisites (adjust connection details for your environment):
--   CREATE EXTENSION IF NOT EXISTS postgres_fdw;
--   CREATE SERVER erp_server
--       FOREIGN DATA WRAPPER postgres_fdw
--       OPTIONS (host 'erp.internal', port '5432', dbname 'erp');
--   CREATE USER MAPPING FOR CURRENT_USER
--       SERVER erp_server OPTIONS (user 'readonly', password 'secret');

CREATE FOREIGN TABLE erp_invoices (
    id          BIGINT,
    customer_id BIGINT,
    total       NUMERIC,
    issued_at   DATE
)
SERVER erp_server
OPTIONS (schema_name 'public', table_name 'invoices');

SELECT pgtrickle.create_stream_table(
    'public.erp_invoice_summary',
    $$
        SELECT
            customer_id,
            COUNT(*)        AS invoice_count,
            SUM(total)      AS total_billed,
            MAX(issued_at)  AS last_invoice_date
        FROM   erp_invoices
        GROUP  BY customer_id
    $$,
    schedule      => '15m',
    refresh_mode  => 'FULL'
);


-- ══════════════════════════════════════════════════════════════════════════
-- Completely blocked features (rejected in every refresh mode)
-- ══════════════════════════════════════════════════════════════════════════
--
-- The following SQL constructs are invalid in a stream table defining query
-- regardless of the chosen refresh mode.  pgtrickle raises an error at
-- CREATE time rather than silently discarding them.
--
-- TABLESAMPLE
-- ───────────
-- Stream tables materialize the *complete* result set.  Applying a sampling
-- method (BERNOULLI, SYSTEM) produces a non-deterministic fraction of rows
-- that changes on every scan, which has no coherent meaning for a
-- continuously maintained table.
--
-- ❌ SELECT id, val FROM events TABLESAMPLE BERNOULLI(10)
-- ✅ Use WHERE random() < 0.10 with refresh_mode => 'FULL' instead (see §1).
--
-- FOR UPDATE / FOR SHARE
-- ──────────────────────
-- Row-level lock clauses are meaningless outside an interactive transaction
-- context.  A stream table defining query is executed by the background
-- worker at refresh time, not inside an application transaction, so locking
-- has no effect and is rejected.
--
-- ❌ SELECT id, amount FROM orders WHERE status = 'pending' FOR UPDATE
-- ✅ Remove the FOR UPDATE / FOR SHARE clause entirely.
--
-- LIMIT without ORDER BY
-- ──────────────────────
-- LIMIT without ORDER BY produces a non-deterministic subset.  The TopK
-- operator (ORDER BY … LIMIT N) is supported and maintains exactly the
-- top-N rows, but a bare LIMIT with no ordering is not meaningful.
--
-- ❌ SELECT id, amount FROM orders LIMIT 100
-- ✅ SELECT id, amount FROM orders ORDER BY created_at DESC LIMIT 100
--
-- OFFSET without ORDER BY + LIMIT
-- ────────────────────────────────
-- OFFSET alone is similarly non-deterministic.  Use it only together with
-- ORDER BY + LIMIT to define a paginated TopK window.
--
-- ❌ SELECT id, amount FROM orders OFFSET 50
-- ✅ SELECT id, amount FROM orders ORDER BY id LIMIT 50 OFFSET 50
