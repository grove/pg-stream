-- Single-row DELETE for T5 (single-row mutations test).
-- Removes the order and lineitems inserted by single_row_insert.sql.
-- Lineitems deleted first to maintain referential consistency.
-- Safe to run even if the rows do not exist (DELETE 0 rows is a no-op).

DELETE FROM lineitem WHERE l_orderkey = 9999991;

DELETE FROM orders WHERE o_orderkey = 9999991
