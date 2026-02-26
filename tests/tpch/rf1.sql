-- RF1: Bulk INSERT into orders + lineitem.
-- Tokens replaced by harness: __RF_COUNT__, __SF_CUSTOMERS__, __SF_PARTS__, __SF_SUPPLIERS__, __NEXT_ORDERKEY__
--
-- Inserts __RF_COUNT__ new orders and 1–7 lineitems per order.
-- Must run in a single transaction (tests multi-table CDC atomicity).

INSERT INTO orders (o_orderkey, o_custkey, o_orderstatus, o_totalprice, o_orderdate,
                    o_orderpriority, o_clerk, o_shippriority, o_comment)
SELECT
    __NEXT_ORDERKEY__ + i AS o_orderkey,
    1 + ((__NEXT_ORDERKEY__ + i) * 1013 % __SF_CUSTOMERS__) AS o_custkey,
    (ARRAY['O','F','P'])[1 + i % 3] AS o_orderstatus,
    round((10000 + (i * 7919) % 500000)::numeric / 100, 2) AS o_totalprice,
    DATE '1995-01-01' + (i % 1000)::int AS o_orderdate,
    (ARRAY['1-URGENT','2-HIGH','3-MEDIUM','4-NOT SPECIFIED','5-LOW'])[1 + i % 5] AS o_orderpriority,
    'Clerk#' || lpad((1 + i % 1000)::text, 9, '0') AS o_clerk,
    0 AS o_shippriority,
    'rf1 order ' || i AS o_comment
FROM generate_series(1, __RF_COUNT__) AS s(i);

INSERT INTO lineitem (l_orderkey, l_linenumber, l_partkey, l_suppkey,
                      l_quantity, l_extendedprice, l_discount, l_tax,
                      l_returnflag, l_linestatus, l_shipdate, l_commitdate,
                      l_receiptdate, l_shipinstruct, l_shipmode, l_comment)
SELECT
    __NEXT_ORDERKEY__ + o AS l_orderkey,
    ln AS l_linenumber,
    1 + ((o * 7 + ln * 1013) % __SF_PARTS__) AS l_partkey,
    1 + ((o * 7 + ln * 1013 + ln * 251) % __SF_SUPPLIERS__) AS l_suppkey,
    1 + ((o * 3 + ln * 17) % 50) AS l_quantity,
    round((100 + ((o * 31 + ln * 97) % 100000))::numeric / 100 *
          (1 + ((o * 3 + ln * 17) % 50)), 2) AS l_extendedprice,
    round(((o * 13 + ln * 7) % 11)::numeric / 100, 2) AS l_discount,
    round(((o * 17 + ln * 3) % 9)::numeric / 100, 2) AS l_tax,
    (ARRAY['A','N','R'])[1 + (o + ln) % 3] AS l_returnflag,
    (ARRAY['O','F'])[1 + (o + ln * 3) % 2] AS l_linestatus,
    DATE '1995-01-01' + (o % 1000)::int + 1 + ((o * 3 + ln * 11) % 121) AS l_shipdate,
    DATE '1995-01-01' + (o % 1000)::int + 30 + ((o * 7 + ln * 5) % 61) AS l_commitdate,
    DATE '1995-01-01' + (o % 1000)::int + 1 + ((o * 3 + ln * 11) % 121) + 1 + ((o + ln * 13) % 30) AS l_receiptdate,
    (ARRAY['DELIVER IN PERSON','COLLECT COD','NONE','TAKE BACK RETURN'])[1 + (o + ln) % 4] AS l_shipinstruct,
    (ARRAY['REG AIR','AIR','RAIL','SHIP','TRUCK','MAIL','FOB'])[1 + (o * 3 + ln) % 7] AS l_shipmode,
    'rf1 lineitem ' || o || '-' || ln AS l_comment
FROM generate_series(1, __RF_COUNT__) AS orders(o),
     generate_series(1, 1 + (o % 7)) AS lines(ln);
