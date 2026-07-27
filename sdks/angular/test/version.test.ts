import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { SDK_VERSION, versionSkew } from "../src/version.js";

test("SDK_VERSION stays in lockstep with package.json", async () => {
  const pkg = JSON.parse(
    await readFile(new URL("../../package.json", import.meta.url), "utf8"),
  ) as { version: string };
  assert.equal(SDK_VERSION, pkg.version);
});

test("versionSkew is silent on matching major.minor", () => {
  assert.equal(versionSkew("0.3.0"), null);
  assert.equal(versionSkew("0.3.9"), null);
});

test("versionSkew warns on a major.minor mismatch", () => {
  const w = versionSkew("0.4.0");
  assert.match(w ?? "", /SDK|server/);
});

test("versionSkew is silent for unparseable versions", () => {
  assert.equal(versionSkew("dry-run"), null);
});
