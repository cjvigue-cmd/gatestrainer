import unittest
from solution import is_leap


class LeapYears(unittest.TestCase):
    def test_ordinary_years(self):
        self.assertTrue(is_leap(2024))
        self.assertFalse(is_leap(2023))

    def test_centuries_are_not_leap(self):
        # The reported bug: 1900 was called a leap year.
        self.assertFalse(is_leap(1900))
        self.assertFalse(is_leap(2100))

    def test_every_fourth_century_is(self):
        self.assertTrue(is_leap(2000))
        self.assertTrue(is_leap(1600))
