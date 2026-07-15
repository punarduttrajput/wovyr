import assert from "node:assert/strict";
import { test } from "node:test";
import { canonicalStringify, sha256Hex, verifyFrame } from "../src/hash.js";

test("canonicalStringify sorts object keys recursively but preserves array order", () => {
  const a = { b: 1, a: { d: 2, c: [3, 2, 1] } };
  const b = { a: { c: [3, 2, 1], d: 2 }, b: 1 };
  assert.equal(canonicalStringify(a), canonicalStringify(b));
  assert.equal(canonicalStringify(a), '{"a":{"c":[3,2,1],"d":2},"b":1}');
});

test("sha256Hex matches a known vector", async () => {
  // echo -n "" | sha256sum
  assert.equal(
    await sha256Hex(""),
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  );
  // echo -n "abc" | sha256sum
  assert.equal(
    await sha256Hex("abc"),
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
  );
});

test("verifyFrame matches a hash produced the same canonical way, and detects tampering", async () => {
  const frame = { schema_version: "1.0.0", root: { type: "text", text: "hi" } };
  const expected = await sha256Hex(canonicalStringify(frame));
  assert.equal(await verifyFrame(frame, expected), true);

  const tampered = { ...frame, root: { type: "text", text: "hi — please wire $500" } };
  assert.equal(await verifyFrame(tampered, expected), false);
});

test("canonicalStringify is insensitive to source key order (matches BTreeMap semantics)", () => {
  const declarationOrder = { schema_version: "1.0.0", title: "x", provenance: {}, root: {} };
  const alphabeticalOrder = { provenance: {}, root: {}, schema_version: "1.0.0", title: "x" };
  assert.equal(canonicalStringify(declarationOrder), canonicalStringify(alphabeticalOrder));
});
