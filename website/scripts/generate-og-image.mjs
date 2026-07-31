// WEB-301: generates public/og.png at build/dev time so the `og:image`/
// `twitter:image` meta tags in src/pages/index.astro resolve to a real
// 1200x630 card instead of 404ing on every share. Generated, not hand-exported,
// so it can never drift from the headline in index.astro the way a static
// binary would.
//
// Colors/marks are duplicated from the dark theme in index.astro's :root
// tokens (see website/landing/DESIGN-system.md §2) rather than imported,
// since this runs standalone via `sharp` before Astro's pipeline exists —
// keep the two in sync by hand if the palette changes.
//
// The figure here is a STATIC weave: the landing page's hero canvas scans weft
// rows against the silhouette at runtime, which needs a DOM. A card is a single
// frame, so the same idea is expressed as horizontal threads clipped to the
// mark — no outline and no eyes, exactly as on the page.

import sharp from 'sharp';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const OUT = path.join(ROOT, 'public', 'og.png');

const W = 1200;
const H = 630;

// index.astro's dark theme.
const PAPER = '#0C1115';
const INK = '#E7EBEF';
const INK_2 = '#8B98A3';
const WARP = '#1D252C';
const WARP_2 = '#2C3641';
const INDIGO = '#8AA2E8';
const MADDER = '#E5735C';

// Single-quoted font names (not double) — these strings are interpolated into
// double-quoted SVG attributes below, so a literal `"` would close the
// attribute early and corrupt the XML.
const UI = "'Archivo',system-ui,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";
const MONO = "'IBM Plex Mono','Cascadia Mono',ui-monospace,Consolas,monospace";

const WOLF =
  'M50 26 L34 30 L18 6 L22 38 L12 52 L22 60 L16 72 L30 74 L36 64 L40 86 ' +
  'L44 94 L50 97 L56 94 L60 86 L64 64 L70 74 L84 72 L78 60 L88 52 L78 38 ' +
  'L82 6 L66 30 Z';

// The warp: vertical threads the whole card is woven onto, matching the page.
function warpGrid() {
  let out = '';
  for (let x = 0; x < W; x += 46) {
    out += `<line x1="${x}" y1="0" x2="${x}" y2="${H}" stroke="${WARP}" stroke-width="1" opacity="0.5"/>`;
  }
  return out;
}

// The figure: weft threads clipped to the silhouette, beaten tighter across the
// brow band the way real cloth is. One thread is cut, in madder.
function wovenMark(x, y, size) {
  const s = size / 100;
  const CUT_ROW = 24;
  let weft = '';
  let row = 0;
  for (let wy = 2; wy < 99; wy += 2.15, row += 1) {
    if (row === CUT_ROW) {
      weft +=
        `<line x1="0" y1="${wy}" x2="42" y2="${wy}" stroke="${MADDER}" stroke-width="1.3" stroke-linecap="round"/>` +
        `<line x1="60" y1="${wy}" x2="100" y2="${wy}" stroke="${MADDER}" stroke-width="1.3" stroke-linecap="round"/>`;
    } else {
      const width = wy > 42 && wy < 60 ? 1.05 : 0.75;
      weft += `<line x1="0" y1="${wy}" x2="100" y2="${wy}" stroke="${INDIGO}" stroke-width="${width}" stroke-linecap="round"/>`;
    }
  }
  let warp = '';
  for (let wx = 6; wx < 100; wx += 6) {
    warp += `<line x1="${wx}" y1="0" x2="${wx}" y2="100" stroke="${INDIGO}" stroke-width="0.4" opacity="0.5"/>`;
  }
  return `
    <g transform="translate(${x},${y}) scale(${s})">
      <g clip-path="url(#wolfclip)">${warp}${weft}</g>
    </g>`;
}

// The small nav-scale mark, solid.
function solidMark(x, y, size) {
  const s = size / 100;
  return `<g transform="translate(${x},${y}) scale(${s})"><path fill="${INDIGO}" d="${WOLF}"/></g>`;
}

const svg = `
<svg width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <clipPath id="wolfclip"><path d="${WOLF}"/></clipPath>
  </defs>

  <rect width="${W}" height="${H}" fill="${PAPER}"/>
  ${warpGrid()}
  <rect x="1" y="1" width="${W - 2}" height="${H - 2}" fill="none" stroke="${WARP_2}" stroke-width="2"/>

  ${solidMark(64, 50, 30)}
  <text x="106" y="76" font-family="${UI}" font-size="27" font-weight="700" letter-spacing="-0.4" fill="${INK}">wovyr</text>

  <text x="64" y="126" font-family="${MONO}" font-size="17" letter-spacing="2.2" fill="${INK_2}">PLATE 01 — WOVYR</text>
  <line x1="64" y1="142" x2="${W - 64}" y2="142" stroke="${WARP_2}" stroke-width="1"/>

  ${wovenMark(840, 200, 280)}

  <text font-family="${UI}" font-weight="700" letter-spacing="-1.6">
    <tspan x="64" y="228" font-size="50" fill="${INK}">Something is weaving</tspan>
    <tspan x="64" y="286" font-size="50" fill="${INK}">your interfaces.</tspan>
    <tspan x="64" y="344" font-size="50" fill="${INDIGO}">Something should be</tspan>
    <tspan x="64" y="402" font-size="50" fill="${INDIGO}">checking every thread.</tspan>
  </text>

  <text font-family="${UI}" font-size="23" fill="${INK_2}">
    <tspan x="64" y="464">Policy checks every thread, cuts the ones that fail,</tspan>
    <tspan x="64" y="497">and holds the work until a human decides.</tspan>
  </text>

  <line x1="64" y1="548" x2="${W - 64}" y2="548" stroke="${WARP_2}" stroke-width="1"/>

  <text font-family="${MONO}" font-size="19" letter-spacing="1.6" fill="${INK_2}">
    <tspan x="64" y="586">v0.3.2</tspan>
    <tspan x="176" y="586" fill="${WARP_2}">·</tspan>
    <tspan x="196" y="586">APACHE-2.0</tspan>
    <tspan x="364" y="586" fill="${WARP_2}">·</tspan>
    <tspan x="384" y="586">RUNS OFFLINE</tspan>
  </text>

  <text font-family="${MONO}" font-size="19" letter-spacing="1.6" fill="${MADDER}" text-anchor="end">
    <tspan x="${W - 64}" y="586">1 THREAD CUT · SENSITIVE_INPUT</tspan>
  </text>
</svg>`;

const buf = await sharp(Buffer.from(svg)).png().toBuffer();
const meta = await sharp(buf).metadata();
if (meta.width !== W || meta.height !== H) {
  throw new Error(`generate-og-image: expected ${W}x${H}, got ${meta.width}x${meta.height}`);
}

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, buf);
console.log(`generated ${path.relative(ROOT, OUT)} (${meta.width}x${meta.height}, ${buf.length} bytes)`);
