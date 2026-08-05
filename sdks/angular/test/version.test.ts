import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { SDK_VERSION, TARGET_SERVER_VERSION, versionSkew } from "../src/version.js";

/** The generation this SDK targets, derived rather than written down. Both
 * skew tests below used to hardcode `0.3.x` as "matching" and `0.4.0` as
 * "mismatching"; when TARGET_SERVER_VERSION moved to 0.4.x they inverted and
 * started failing — unnoticed, because this package has no CI job. Deriving
 * the fixtures from the target keeps them correct across every future bump. */
const [major, minor] = TARGET_SERVER_VERSION.split(".").map(Number);

test("SDK_VERSION stays in lockstep with package.json", async () => {
  const pkg = JSON.parse(
    await readFile(new URL("../../package.json", import.meta.url), "utf8"),
  ) as { version: string };
  assert.equal(SDK_VERSION, pkg.version);
});

test("versionSkew is silent on matching major.minor", () => {
  assert.equal(versionSkew(`${major}.${minor}.0`), null);
  assert.equal(versionSkew(`${major}.${minor}.9`), null);
});

test("versionSkew warns on a major.minor mismatch", () => {
  const w = versionSkew(`${major}.${minor + 1}.0`);
  assert.match(w ?? "", /SDK|server/);
});

test("versionSkew is silent for unparseable versions", () => {
  assert.equal(versionSkew("dry-run"), null);
});
