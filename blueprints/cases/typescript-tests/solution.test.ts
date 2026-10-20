import { test } from "node:test";
import assert from "node:assert/strict";
import { parseDuration } from "./solution.ts";

test("plain units", () => {
  assert.equal(parseDuration("90s"), 90);
  assert.equal(parseDuration("15m"), 900);
  assert.equal(parseDuration("2h"), 7200);
});

test("units combine, largest first", () => {
  assert.equal(parseDuration("1h30m"), 5400);
  assert.equal(parseDuration("1h0m5s"), 3605);
});

test("zero is a duration", () => {
  assert.equal(parseDuration("0s"), 0);
});

test("malformed input throws", () => {
  for (const bad of ["", "10", "5x", "m5", "1m1h", "1h1h", "-5s", "1.5h", " 5s"]) {
    assert.throws(() => parseDuration(bad), RangeError, JSON.stringify(bad));
  }
});
