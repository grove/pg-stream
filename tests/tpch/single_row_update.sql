-- Single-row UPDATE for T5 (single-row mutations test).
-- Updates l_discount and l_extendedprice on both lineitems inserted by
-- single_row_insert.sql.  Uses a 1-row OLD TABLE / NEW TABLE transition
-- per statement, targeting the IVM UPDATE delta path.

UPDATE lineitem
SET l_discount = 0.07,
    l_extendedprice = l_extendedprice * 1.05
WHERE l_orderkey = 9999991
