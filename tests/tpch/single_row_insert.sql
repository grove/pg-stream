-- Single-row INSERT for T5 (single-row mutations test).
-- Uses fixed o_orderkey = 9999991, well above the SF=0.01 generated range
-- (~1,500 orders) to avoid primary-key collisions with generated data.
--
-- Inserts 1 order and 2 lineitems with known, deterministic values so
-- that the IVM trigger delta path (1-row NEW TABLE) is exercised.

INSERT INTO orders (o_orderkey, o_custkey, o_orderstatus, o_totalprice, o_orderdate,
                    o_orderpriority, o_clerk, o_shippriority, o_comment)
VALUES (9999991, 1, 'O', 12345.00, DATE '1995-06-15',
        '1-URGENT', 'Clerk#000000001', 0, 'single-row test order');

INSERT INTO lineitem (l_orderkey, l_linenumber, l_partkey, l_suppkey,
                      l_quantity, l_extendedprice, l_discount, l_tax,
                      l_returnflag, l_linestatus, l_shipdate, l_commitdate,
                      l_receiptdate, l_shipinstruct, l_shipmode, l_comment)
VALUES
    (9999991, 1, 1, 1, 10, 1000.00, 0.05, 0.08,
     'N', 'O', DATE '1995-07-01', DATE '1995-07-15', DATE '1995-07-02',
     'DELIVER IN PERSON', 'TRUCK', 'single-row test lineitem 1'),
    (9999991, 2, 2, 1, 5, 500.00, 0.03, 0.08,
     'R', 'F', DATE '1994-06-01', DATE '1994-06-15', DATE '1994-06-02',
     'COLLECT COD', 'AIR', 'single-row test lineitem 2')
