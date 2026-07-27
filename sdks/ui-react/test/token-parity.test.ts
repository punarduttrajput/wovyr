import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

// DSY-102: this package can't literally `@import` the canonical brand-token
// file (packages/tokens/wovyr-tokens.css) — it's a published, dependency-free
// SDK that must render correctly standalone with zero host tokens present, so
// its own `--wovyr-ui-*` values in src/styles.css are a hand-kept-in-sync copy,
// not a build-time reference. This test is what makes "kept in sync" a real
// guarantee rather than a comment: it parses both stylesheets and fails the
// instant a mapped pair diverges, so a future edit to one file without the
// other breaks `npm test` here, not just at the next design audit.

const HERE = path.dirname(fileURLToPath(import.meta.url));
// compiled to dist/test/token-parity.test.js — dist/test -> dist -> ui-react -> sdks -> repo root
const REPO_ROOT = path.resolve(HERE, "..", "..", "..", "..");
const canonicalCss = readFileSync(
  path.join(REPO_ROOT, "packages", "tokens", "wovyr-tokens.css"),
  "utf8",
);
const uiReactCss = readFileSync(path.resolve(HERE, "..", "..", "src", "styles.css"), "utf8");

/** Extracts `--name: value;` pairs from the FIRST block whose selector
 * matches `selectorRe`. Good enough for these two hand-written, flat
 * (non-nested) stylesheets — not a general CSS parser. */
function parseBlock(css: string, selectorRe: RegExp): Record<string, string> {
  const m = selectorRe.exec(css);
  if (!m) throw new Error(`no block matched ${selectorRe} in stylesheet`);
  const braceStart = css.indexOf("{", m.index);
  const braceEnd = css.indexOf("}", braceStart);
  const body = css.slice(braceStart + 1, braceEnd);
  const out: Record<string, string> = {};
  for (const decl of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    out[decl[1]] = decl[2].trim();
  }
  return out;
}

/** Normalizes a colour/value for comparison — lowercases hex, collapses
 * whitespace, so `#FFF` vs `#fff` or `rgba(45, 84, 232, .1)` vs
 * `rgba(45,84,232,.1)` don't register as false drift. */
function norm(v: string): string {
  return v.toLowerCase().replace(/\s+/g, "");
}

const canonicalLight = parseBlock(canonicalCss, /:root\[data-theme="light"\]/);
const canonicalDark = parseBlock(canonicalCss, /:root\[data-theme="dark"\]/);
const uiReactLight = parseBlock(uiReactCss, /\.wovyr-ui\[data-theme="light"\]/);
const uiReactDark = parseBlock(uiReactCss, /\.wovyr-ui\[data-theme="dark"\]/);

// ui-react name -> canonical name, for every value that's meant to track the
// brand palette. `--wovyr-ui-danger-fg` (dark) is deliberately NOT mapped to
// `--accent-fg` or any canonical token — it diverges from a simple "same
// value as accent-fg" assumption for its own measured reason (see
// styles.css's header comment: white-on-danger is 3.14:1 there, near-black
// is 6.04:1), so it's asserted directly below instead of via this table.
const MAPPING: Record<string, string> = {
  "--wovyr-ui-fg": "--ink",
  "--wovyr-ui-fg-muted": "--ink-2",
  "--wovyr-ui-bg": "--surface",
  "--wovyr-ui-bg-subtle": "--surface-2",
  "--wovyr-ui-border": "--line",
  "--wovyr-ui-accent": "--accent",
  "--wovyr-ui-accent-fg": "--accent-fg",
  "--wovyr-ui-danger": "--crit",
  "--wovyr-ui-success": "--ok",
  "--wovyr-ui-warning": "--warn",
  "--wovyr-ui-focus-ring": "--accent",
  "--wovyr-ui-unknown-bg": "--warn-bg",
  "--wovyr-ui-unknown-border": "--warn",
};

for (const [themeName, uiReact, canonical] of [
  ["light", uiReactLight, canonicalLight],
  ["dark", uiReactDark, canonicalDark],
] as const) {
  test(`@wovyr/ui-react's ${themeName}-theme tokens match the canonical brand source`, () => {
    for (const [uiReactName, canonicalName] of Object.entries(MAPPING)) {
      const uiReactValue = uiReact[uiReactName];
      const canonicalValue = canonical[canonicalName];
      assert.ok(uiReactValue, `${uiReactName} missing from sdks/ui-react/src/styles.css (${themeName})`);
      assert.ok(
        canonicalValue,
        `${canonicalName} missing from packages/tokens/wovyr-tokens.css (${themeName})`,
      );
      assert.equal(
        norm(uiReactValue),
        norm(canonicalValue),
        `${uiReactName} (${uiReactValue}) has drifted from its canonical source ` +
          `${canonicalName} (${canonicalValue}) in ${themeName} theme — update ` +
          `sdks/ui-react/src/styles.css to match (see DSY-102).`,
      );
    }
  });
}

test("--wovyr-ui-radius matches the canonical --r (not --r-sm)", () => {
  // --wovyr-ui-radius doesn't vary by theme, so it lives only in the bare
  // `.wovyr-ui { ... }` block, not the [data-theme] blocks parsed above.
  const canonicalR = parseBlock(canonicalCss, /:root(?!\[)/)["--r"];
  const uiReactRadius = parseBlock(uiReactCss, /\.wovyr-ui\s*\{/)["--wovyr-ui-radius"];
  assert.ok(canonicalR, "--r missing from the canonical bare :root block");
  assert.ok(uiReactRadius, "--wovyr-ui-radius missing from the bare .wovyr-ui block");
  assert.equal(norm(uiReactRadius), norm(canonicalR));
});

test("--wovyr-ui-danger-fg is white in light mode, near-black in dark mode (measured, not assumed)", () => {
  assert.equal(norm(uiReactLight["--wovyr-ui-danger-fg"]), norm("#ffffff"));
  assert.equal(norm(uiReactDark["--wovyr-ui-danger-fg"]), norm("#101014"));
});
