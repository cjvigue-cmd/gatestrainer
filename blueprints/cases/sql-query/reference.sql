SELECT c.name, SUM(o.total) AS spent
FROM customers c JOIN orders o ON o.customer_id = c.id
WHERE o.status = 'paid'
GROUP BY c.id
HAVING COUNT(*) >= 2
ORDER BY spent DESC;
