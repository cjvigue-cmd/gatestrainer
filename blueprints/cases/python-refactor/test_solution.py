import unittest
from solution import shipping


class Shipping(unittest.TestCase):
    def test_bands(self):
        self.assertEqual(shipping(1, False), 5)
        self.assertEqual(shipping(3, False), 9)
        self.assertEqual(shipping(7, False), 16)

    def test_express_doubles(self):
        self.assertEqual(shipping(3, True), 18)

    def test_rejects_nonpositive(self):
        with self.assertRaises(ValueError):
            shipping(0, False)
