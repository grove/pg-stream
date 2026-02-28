-- TPC-H Schema for pg_trickle correctness testing.
-- Standard 8-table schema with primary keys (required for CDC triggers).
-- No foreign keys — they are not required and slow down RF1/RF2.

CREATE TABLE region (
    r_regionkey  INT PRIMARY KEY,
    r_name       TEXT NOT NULL,
    r_comment    TEXT
);

CREATE TABLE nation (
    n_nationkey  INT PRIMARY KEY,
    n_name       TEXT NOT NULL,
    n_regionkey  INT NOT NULL,
    n_comment    TEXT
);

CREATE TABLE supplier (
    s_suppkey    INT PRIMARY KEY,
    s_name       TEXT NOT NULL,
    s_address    TEXT NOT NULL,
    s_nationkey  INT NOT NULL,
    s_phone      TEXT NOT NULL,
    s_acctbal    NUMERIC NOT NULL,
    s_comment    TEXT
);

CREATE TABLE part (
    p_partkey       INT PRIMARY KEY,
    p_name          TEXT NOT NULL,
    p_mfgr          TEXT NOT NULL,
    p_brand         TEXT NOT NULL,
    p_type          TEXT NOT NULL,
    p_size          INT NOT NULL,
    p_container     TEXT NOT NULL,
    p_retailprice   NUMERIC NOT NULL,
    p_comment       TEXT
);

CREATE TABLE partsupp (
    ps_partkey     INT NOT NULL,
    ps_suppkey     INT NOT NULL,
    ps_availqty    INT NOT NULL,
    ps_supplycost  NUMERIC NOT NULL,
    ps_comment     TEXT,
    PRIMARY KEY (ps_partkey, ps_suppkey)
);

CREATE TABLE customer (
    c_custkey     INT PRIMARY KEY,
    c_name        TEXT NOT NULL,
    c_address     TEXT NOT NULL,
    c_nationkey   INT NOT NULL,
    c_phone       TEXT NOT NULL,
    c_acctbal     NUMERIC NOT NULL,
    c_mktsegment  TEXT NOT NULL,
    c_comment     TEXT
);

CREATE TABLE orders (
    o_orderkey      INT PRIMARY KEY,
    o_custkey       INT NOT NULL,
    o_orderstatus   TEXT NOT NULL,
    o_totalprice    NUMERIC NOT NULL,
    o_orderdate     DATE NOT NULL,
    o_orderpriority TEXT NOT NULL,
    o_clerk         TEXT NOT NULL,
    o_shippriority  INT NOT NULL,
    o_comment       TEXT
);

CREATE TABLE lineitem (
    l_orderkey      INT NOT NULL,
    l_linenumber    INT NOT NULL,
    l_partkey       INT NOT NULL,
    l_suppkey       INT NOT NULL,
    l_quantity      NUMERIC NOT NULL,
    l_extendedprice NUMERIC NOT NULL,
    l_discount      NUMERIC NOT NULL,
    l_tax           NUMERIC NOT NULL,
    l_returnflag    TEXT NOT NULL,
    l_linestatus    TEXT NOT NULL,
    l_shipdate      DATE NOT NULL,
    l_commitdate    DATE NOT NULL,
    l_receiptdate   DATE NOT NULL,
    l_shipinstruct  TEXT NOT NULL,
    l_shipmode      TEXT NOT NULL,
    l_comment       TEXT,
    PRIMARY KEY (l_orderkey, l_linenumber)
);
