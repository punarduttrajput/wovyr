// WEB-305: copies the committed demo outputs from `../demo/out` into
// `public/demo/` so the landing page's Plate 05 can link a replay a visitor can
// actually watch. Before this the 90-second run existed only as files in the
// repo — the strongest single piece of evidence the project has, reachable only
// by cloning it.
//
// Copied at build time rather than committed a second time under `public/`,
// for the same reason `public/og.png` and `public/llms.txt` are generated:
// a duplicate in git is a copy that drifts. `public/demo/` is gitignored.
//
// `demo/out/player.html` is deliberately self-contained (fonts inlined as data
// URIs, the capture inlined as JSON, no network at all — see demo/README.md), so
// serving it as a static file from Astro's `public/` needs no build integration
// beyond this copy, and it keeps working behind the site's CSP.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SRC = path.join(ROOT, '..', 'demo', 'out');
const DEST = path.join(ROOT, 'public', 'demo');

// player.html is the deliverable and is committed, so a missing one means the
// checkout is broken rather than that someone simply hasn't run the demo —
// fail loudly. The cast and transcript are the machine-readable siblings; they
// are committed too, but the page does not hard-depend on them, so a missing
// one warns instead of breaking the build.
const REQUIRED = ['player.html'];
const OPTIONAL = ['demo.cast', 'transcript.json'];

fs.mkdirSync(DEST, { recursive: true });

for (const name of REQUIRED) {
  const from = path.join(SRC, name);
  if (!fs.existsSync(from)) {
    console.error(
      `sync-demo: missing ${path.relative(ROOT, from)}.\n` +
        `It is committed to the repository — regenerate it with ` +
        `\`node demo/killer-demo.mjs && node demo/build-player.mjs\` from the repo root.`,
    );
    process.exit(1);
  }
  fs.copyFileSync(from, path.join(DEST, name));
}

for (const name of OPTIONAL) {
  const from = path.join(SRC, name);
  if (!fs.existsSync(from)) {
    console.warn(`sync-demo: ${name} not found — skipping (the page degrades to the player only).`);
    continue;
  }
  fs.copyFileSync(from, path.join(DEST, name));
}

const copied = [...REQUIRED, ...OPTIONAL].filter((n) => fs.existsSync(path.join(DEST, n)));
console.log(`sync-demo: ${copied.length} file(s) → public/demo/ (${copied.join(', ')})`);
