-- RF2: Bulk DELETE from orders + lineitem.
-- Token replaced by harness: __RF_COUNT__
--
-- Deletes the __RF_COUNT__ oldest orders (lowest o_orderkey) and their lineitems.
-- Lineitem deleted first to maintain referential consistency.

DELETE FROM lineitem
WHERE l_orderkey IN (
    SELECT o_orderkey FROM orders ORDER BY o_orderkey LIMIT __RF_COUNT__
);

DELETE FROM orders
WHERE o_orderkey IN (
    SELECT o_orderkey FROM orders ORDER BY o_orderkey LIMIT __RF_COUNT__
);
