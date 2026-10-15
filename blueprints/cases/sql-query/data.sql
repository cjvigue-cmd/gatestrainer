CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), total REAL NOT NULL, status TEXT NOT NULL);
INSERT INTO customers VALUES (1, 'Ana'), (2, 'Bo'), (3, 'Cy'), (4, 'Di');
INSERT INTO orders VALUES
  (1, 1, 20.0, 'paid'), (2, 1, 15.5, 'paid'), (3, 1, 99.0, 'refunded'),
  (4, 2, 40.0, 'paid'),
  (5, 3, 5.0, 'paid'), (6, 3, 7.25, 'paid'), (7, 3, 1.0, 'paid'),
  (8, 4, 50.0, 'refunded'), (9, 4, 60.0, 'refunded');
