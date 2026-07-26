// WEB-301: generates public/og.png at build/dev time so the `og:image`/
// `twitter:image` meta tags in src/pages/index.astro (currently pointing at a
// file that has never existed) resolve to a real 1200x630 card instead of
// 404ing on every share. Generated, not hand-exported, so it can never drift
// from the headline/description in index.astro the way a static binary would.
//
// Colors/marks are duplicated from the dark theme in index.astro's :root
// tokens (see website/landing/DESIGN-system.md §2) rather than imported,
// since this runs standalone via `sharp` before Astro's pipeline exists —
// keep the two in sync by hand if the palette changes.

import sharp from 'sharp';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const OUT = path.join(ROOT, 'public', 'og.png');

const W = 1200;
const H = 630;

const CANVAS = '#0A0E18';
const INK = '#EAEEF6';
const INK_DIM = '#9AA6BF';
const ACCENT = '#5B7BFF';
const LINE = '#1E2740';
const OK = '#3FBF86';

// Single-quoted font names (not double) — these strings are interpolated
// into double-quoted SVG attributes below, so a literal `"` would close the
// attribute early and corrupt the XML.
const MONO = "'Cascadia Code','SF Mono','JetBrains Mono',ui-monospace,Menlo,Consolas,monospace";
const SANS = "ui-sans-serif,system-ui,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";

// The window/scanline brand mark (website/public/favicon.svg), scaled up.
function brandMark(x, y, size) {
  const s = size / 24;
  return `
    <g transform="translate(${x},${y}) scale(${s})">
      <rect x="3.5" y="4.5" width="17" height="15" rx="2.6" fill="none" stroke="${INK}" stroke-width="1.6" stroke-linejoin="round"/>
      <circle cx="6.6" cy="8" r="0.9" fill="${INK_DIM}"/>
      <circle cx="9.2" cy="8" r="0.9" fill="${INK_DIM}"/>
      <path d="M6.4 14H18" stroke="${ACCENT}" stroke-width="1.7" stroke-linecap="round"/>
      <circle cx="14.6" cy="14" r="1.7" fill="${ACCENT}"/>
    </g>`;
}

const svg = `
<svg width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <radialGradient id="halo" cx="15%" cy="30%" r="75%">
      <stop offset="0" stop-color="${ACCENT}" stop-opacity="0.16"/>
      <stop offset="0.5" stop-color="${ACCENT}" stop-opacity="0.04"/>
      <stop offset="1" stop-color="${CANVAS}" stop-opacity="0"/>
    </radialGradient>
  </defs>

  <rect width="${W}" height="${H}" fill="${CANVAS}"/>
  <rect width="${W}" height="${H}" fill="url(#halo)"/>
  <rect x="1" y="1" width="${W - 2}" height="${H - 2}" fill="none" stroke="${LINE}" stroke-width="2"/>

  ${brandMark(64, 56, 34)}
  <text x="112" y="83" font-family="${MONO}" font-size="26" font-weight="650" letter-spacing="0.5" fill="${INK}">wovyr</text>

  <text font-family="${MONO}" font-weight="650" letter-spacing="-1">
    <tspan x="64" y="230" font-size="58" fill="${INK}">The trust layer for</tspan>
    <tspan x="64" y="298" font-size="58" fill="${ACCENT}">AI-generated interfaces.</tspan>
  </text>

  <text font-family="${SANS}" font-size="27" fill="${INK_DIM}">
    <tspan x="64" y="368">Validates, records, and durably runs every interface</tspan>
    <tspan x="64" y="406">an AI agent shows a human — on a Rust runtime that</tspan>
    <tspan x="64" y="444">survives crashes, restarts, and time.</tspan>
  </text>

  <line x1="64" y1="500" x2="${W - 64}" y2="500" stroke="${LINE}" stroke-width="1"/>

  <text font-family="${MONO}" font-size="20" fill="${INK_DIM}">
    <tspan x="64" y="548">v0.3.0</tspan>
    <tspan x="176" y="548" fill="${LINE}">·</tspan>
    <tspan x="196" y="548">Apache-2.0</tspan>
    <tspan x="360" y="548" fill="${LINE}">·</tspan>
    <tspan x="380" y="548">runs offline</tspan>
  </text>

  <text font-family="${MONO}" font-size="20" fill="${OK}" text-anchor="end">
    <tspan x="${W - 64}" y="548">✓ verified · sha256:1a2f9c…</tspan>
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
