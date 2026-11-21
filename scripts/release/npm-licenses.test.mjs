import { test } from "node:test";
import assert from "node:assert/strict";
import { checkLicenses, flattenPnpm } from "./npm-licenses.mjs";

const e = (license) => ({ name: "x", version: "1", license });
const bad = (license) => checkLicenses([e(license)]).length;

test("returns only the violations", () => {
  assert.deepEqual(
    checkLicenses([{ name: "a", version: "1", license: "MIT" }, { name: "b", version: "1", license: "GPL-3.0" }]),
    [{ name: "b", license: "GPL-3.0" }],
  );
});
test("OR passes if any side passes", () => assert.equal(bad("(MIT OR Apache-2.0)"), 0));
test("OR with one allowed side passes", () => assert.equal(bad("(GPL-3.0 OR MIT)"), 0));
test("AND needs every side", () => {
  assert.equal(bad("MIT AND GPL-3.0"), 1);
  assert.equal(bad("MIT AND ISC"), 0);
});
test("mixed or nested expressions fail closed", () => {
  assert.equal(bad("(MIT OR ISC) AND GPL-3.0"), 1);
  assert.equal(bad("MIT AND (GPL-3.0 OR ISC)"), 1);
  assert.equal(bad("(MIT OR Apache-2.0)"), 0);
});
test("unknown or UNLICENSED fails", () => {
  assert.equal(bad("UNLICENSED"), 1);
  assert.equal(bad("Weird-1.0"), 1);
  assert.equal(bad(undefined), 1);
});
test("flattenPnpm turns the license-grouped shape into a flat list", () => {
  assert.deepEqual(flattenPnpm({ MIT: [{ name: "a", versions: ["1", "2"] }] }), [
    { name: "a", version: "1", license: "MIT" },
    { name: "a", version: "2", license: "MIT" },
  ]);
});
