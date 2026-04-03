-- Q3: Local item suggestion — join + filter
-- Tests: INNER JOIN, multi-column filter
SELECT
    p.name,
    p.city,
    p.state,
    a.id AS auction_id
FROM auction a
JOIN person p ON a.seller = p.id
WHERE a.category = 10
  AND (p.state = 'OR' OR p.state = 'CA' OR p.state = 'WA')
