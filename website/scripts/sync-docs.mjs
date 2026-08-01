// Syncs ../docs (the canonical, spec-driven source of truth) into
// src/content/docs for the Starlight build. The copies are generated —
// never edit them; edit ../docs and re-run `npm run sync`.
//
// Per page it:
//  - adds the frontmatter Starlight requires (title, explicit slug)
//  - derives a unique `description` from the page's own first prose paragraph
//    (SEO-101: without this every page inherited the one site-wide fallback,
//    so all 203 shipped byte-identical `<meta name="description">` — a pure
//    duplicate-content signal, and nothing an answer engine could extract)
//  - demotes every ATX heading one level (SEO-102: the canonical docs use
//    `# N. Section` for sections, so a page rendered ~17 `<h1>`s under
//    Starlight's own title H1 — broken hierarchy on 200 pages, and an empty
//    on-page table of contents, which keys off h2/h3)
//  - carries the doc's own `**Last Updated:**` into `lastUpdated`, which is
//    what gives the sitemap a real `<lastmod>` (SEO-103)
//  - injects a "Status: Planned" caution on docs whose own `**Status:**`
//    header marks them planned / exploratory / aspirational / target-state
//  - rewrites relative .md cross-links to site routes, and links that leave
//    docs/ (crate paths, openapi.yaml, …) to GitHub blob URLs
//
// It then emits two generated GEO artifacts into public/ (GEO-101):
// `llms.txt` (the llmstxt.org index: one titled, described link per page) and
// `llms-full.txt` (the whole corpus in one file). Both are derived from this
// same pass, so neither can drift from what the site actually publishes.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
// Shared with src/components/Head.astro's BreadcrumbList, so /llms.txt's
// grouping and the docs' structured data can never disagree.
import { sectionTitle } from '../src/lib/doc-sections.mjs';

// fileURLToPath (not `new URL(...).pathname`) so this resolves correctly on
// Windows too — .pathname yields a URL-encoded, leading-slash path like
// `/D:/New%20folder/...` that breaks on drive letters and spaces.
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DOCS = path.resolve(ROOT, '../docs');
const OUT = path.join(ROOT, 'src/content/docs');
const PUBLIC = path.join(ROOT, 'public');
const SITE = 'https://wovyr.com';
const GITHUB_BLOB = 'https://github.com/punarduttrajput/wovyr/blob/main/';

// The one site-wide description, kept identical to astro.config.mjs's Starlight
// `description`. Used only as the last-resort per-page fallback (a page with no
// prose at all) and as the summary line in the generated llms*.txt.
const SITE_DESCRIPTION =
  'Wovyr — Generative UI Trust Runtime, built on an enterprise AI Agent Operating System written in Rust.';

// Manual overrides for pages the Status heuristic misclassifies.
// Keys are docs-relative paths, e.g. '12-deployment/docker-compose.md'.
const FORCE_PLANNED = new Set([]);
const FORCE_SHIPPED = new Set([]);

/** All markdown files, as posix paths relative to DOCS. */
function collectFiles(dir = DOCS) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const abs = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...collectFiles(abs));
    else if (entry.name.endsWith('.md')) {
      out.push(path.relative(DOCS, abs).split(path.sep).join('/'));
    }
  }
  return out;
}

const files = collectFiles().filter((f) => f !== 'SUMMARY.md');
const fileSet = new Set(files);

function slugifySegment(seg) {
  return seg
    .toLowerCase()
    .replace(/\s+/g, '-')
    .replace(/[^a-z0-9._-]/g, '');
}

/** Site route (no leading/trailing slash) for a docs-relative .md path. */
function routeFor(rel) {
  let p = rel.replace(/\.md$/, '');
  if (p.endsWith('/index')) {
    const dir = p.slice(0, -'/index'.length);
    // `v1.0/index.md` next to `v1.0.md`: the bare file keeps the natural
    // route, the index moves aside so the two never collide. (A slug ending
    // in `/index` is not usable for that — Astro collapses it back onto the
    // directory route.)
    p = fileSet.has(dir + '.md')
      ? dir + (fileSet.has(dir + '/overview.md') ? '/doc-index' : '/overview')
      : dir;
  }
  return p.split('/').map(slugifySegment).join('/');
}

/** The `**Status:** ...` block: its line plus continuation lines up to a blank. */
function statusBlock(lines) {
  const i = lines.findIndex((l) => /^\*\*Status:\*\*/i.test(l));
  if (i === -1) return null;
  const block = [lines[i].replace(/^\*\*Status:\*\*\s*/i, '')];
  for (let j = i + 1; j < lines.length && lines[j].trim() !== ''; j++) {
    block.push(lines[j]);
  }
  return block.join(' ');
}

const SHIPPED_PREFIX =
  /^[*_\s]*(done|shipped|in progress|in delivery|active|implemented|ready|tagged|substantially|core implemented|exit criteria|track a|all of|phases|ga-hardening engineering scope complete)/i;
const PLANNED_KEYWORDS =
  /planned|exploratory|aspirational|not (yet )?implemented|not a commitment|spec-only|target-state|day-1|zero artifacts/i;

function isPlanned(rel, status) {
  if (FORCE_PLANNED.has(rel)) return true;
  if (FORCE_SHIPPED.has(rel)) return false;
  if (!status) return false;
  if (SHIPPED_PREFIX.test(status)) return false;
  return PLANNED_KEYWORDS.test(status);
}

/** First H1 counts as the title only when nothing but comments/blanks precede it. */
function extractTitle(rel, lines) {
  let inComment = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const t = line.trim();
    if (inComment) {
      if (t.includes('-->')) inComment = false;
      continue;
    }
    if (t === '') continue;
    if (t.startsWith('<!--')) {
      if (!t.includes('-->')) inComment = true;
      continue;
    }
    if (t.startsWith('# ')) {
      const title = t
        .slice(2)
        .replace(/[*_`]/g, '')
        .trim();
      return { title, stripLine: i };
    }
    break; // real content before any H1 → not a title heading
  }
  const base = path.basename(rel, '.md');
  const name = base === 'index' ? path.basename(path.dirname(rel)) : base;
  const ACRONYMS = new Set([
    'api', 'cli', 'adr', 'llm', 'ui', 'sdk', 'kms', 'mcp', 'rbac', 'abac',
    'prd', 'ga', 'dr', 'ha', 'sbom', 'ci', 'cd', 'wasi', 'yaml', 'sse',
  ]);
  const title = name
    .split(/[-_]/)
    .filter(Boolean)
    .map((w) =>
      ACRONYMS.has(w.toLowerCase())
        ? w.toUpperCase()
        : w.charAt(0).toUpperCase() + w.slice(1)
    )
    .join(' ');
  return { title, stripLine: -1 };
}

/**
 * SEO-102: shift every ATX heading down one level in the generated copy.
 *
 * The canonical docs number their top-level sections `# 1. Purpose`, so
 * Starlight — which renders the frontmatter `title` as the page's `<h1>` —
 * ended up with ~17 `<h1>`s per page. Two consequences, both real: the
 * document outline says every section is the page's subject, and Starlight's
 * on-page ToC (h2/h3 by default) had nothing to collect, so 200 pages of
 * long-form spec shipped with no in-page navigation at all.
 *
 * Deliberately a transform on the *generated* copy, not an edit to ../docs:
 * the canonical files are read on GitHub too, where a leading `#` per section
 * is correct because there is no injected title above them.
 *
 * Fenced code blocks are skipped so a shell comment (`# install the CLI`) is
 * never mistaken for a heading. Corpus-wide the deepest real heading is h3,
 * so nothing collides with the h6 floor; the clamp is kept anyway so a future
 * h6 degrades to staying h6 rather than emitting an invalid `#######`.
 */
function demoteHeadings(content) {
  let fence = null; // the exact opening fence marker, or null when outside one
  return content
    .split('\n')
    .map((line) => {
      const fenceMatch = line.match(/^\s*(`{3,}|~{3,})/);
      if (fenceMatch) {
        const marker = fenceMatch[1][0].repeat(3);
        if (fence === null) fence = marker;
        else if (fence === marker) fence = null;
        return line;
      }
      if (fence !== null) return line;
      const heading = line.match(/^(#{1,6})(\s)/);
      if (!heading) return line;
      const level = Math.min(heading[1].length + 1, 6);
      return '#'.repeat(level) + line.slice(heading[1].length);
    })
    .join('\n');
}

/** True for lines that are structural/metadata rather than prose. */
function isNonProse(line) {
  const t = line.trim();
  return (
    t === '' ||
    t.startsWith('#') || // heading
    t.startsWith('---') || // thematic break / frontmatter fence
    t.startsWith('|') || // table row
    t.startsWith(':::') || // admonition
    t.startsWith('>') || // blockquote
    t.startsWith('<') || // raw HTML
    t.startsWith('!') || // image
    /^\*\*[A-Za-z][\w /-]*:\*\*/.test(t) || // **Document ID:** … metadata
    /^[-*+]\s/.test(t) || // list item
    /^\d+\.\s/.test(t) // ordered list item
  );
}

/** Markdown → plain sentence text, for a meta description. */
function stripMarkdown(text) {
  return text
    .replace(/!\[[^\]]*\]\([^)]*\)/g, '') // images
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1') // links → their text
    .replace(/<[^>]+>/g, '') // inline HTML
    .replace(/`([^`]+)`/g, '$1') // inline code
    .replace(/\*\*([^*]+)\*\*/g, '$1') // bold
    .replace(/(^|\W)_([^_]+)_(?=\W|$)/g, '$1$2') // underscore italics
    .replace(/\*([^*]+)\*/g, '$1') // star italics
    .replace(/\s+/g, ' ')
    .trim();
}

/**
 * SEO-101: a unique `<meta name="description">` per page, taken from the
 * page's own first substantive prose paragraph.
 *
 * These docs are consistently shaped — an HTML comment, the title H1, a
 * `**Document ID:** …` metadata block, a rule, then `# 1. Purpose` and a real
 * paragraph ("This document specifies the LLM Gateway, …"). That paragraph is
 * exactly the extractable summary both a SERP snippet and an answer engine
 * want, so it is preferred over anything synthesised.
 *
 * Falls back to the site-wide description only when a page genuinely has no
 * prose (a pure table or link index) — better one honest fallback on a handful
 * of pages than a fabricated sentence on any.
 */
function deriveDescription(lines, title, fallback) {
  // Enough prose to be a useful snippet. Below this, the first paragraph is a
  // one-line restatement ("This document defines the business goals.") and the
  // paragraph after it carries the substance, so keep gathering.
  const ENOUGH = 110;

  let fence = null;
  let inComment = false;
  // A `**Status:** …` value often wraps over several lines. Those continuation
  // lines match no non-prose pattern of their own, so without tracking the
  // block they read as the page's first paragraph — which is how four pages
  // ended up described mid-sentence ("Interface → Infrastructure) genuinely
  // describes how the real crates are organized."). Same shape as statusBlock().
  let inMetadata = false;
  const paragraphs = [];
  let current = [];

  const closeParagraph = () => {
    if (current.length) paragraphs.push(current.join(' '));
    current = [];
  };

  for (const line of lines) {
    // HTML comment blocks. These docs open with a `<!-- File: … -->` provenance
    // block whose inner lines look like ordinary prose — without this the
    // description became "File: docs/09-api/tools.md Document ID: API-006 -->".
    if (inComment) {
      if (line.includes('-->')) inComment = false;
      continue;
    }
    if (line.trim().startsWith('<!--')) {
      if (!line.includes('-->')) inComment = true;
      continue;
    }

    const fenceMatch = line.match(/^\s*(`{3,}|~{3,})/);
    if (fenceMatch) {
      const marker = fenceMatch[1][0].repeat(3);
      if (fence === null) fence = marker;
      else if (fence === marker) fence = null;
      closeParagraph();
      continue;
    }
    if (fence !== null) continue;

    const t = line.trim();
    if (inMetadata) {
      if (t === '') inMetadata = false;
      continue;
    }
    if (/^\*\*[A-Za-z][\w /-]*:\*\*/.test(t)) {
      inMetadata = true;
      continue;
    }

    if (t === '') {
      closeParagraph();
    } else if (isNonProse(line)) {
      // A heading or rule ends the summary — but only once we have something.
      // Before that we are still walking the title/metadata preamble.
      closeParagraph();
      if (paragraphs.join(' ').length >= ENOUGH) break;
    } else {
      current.push(t);
    }
    if (paragraphs.join(' ').length >= ENOUGH) break;
  }
  closeParagraph();

  // Join only as many paragraphs as it takes to clear ENOUGH, so a page with a
  // strong single paragraph is not padded with an unrelated second one.
  let joined = '';
  for (const p of paragraphs) {
    joined = joined ? `${joined} ${p}` : p;
    if (stripMarkdown(joined).length >= ENOUGH) break;
  }

  let text = stripMarkdown(joined);
  // A paragraph that only restates the title carries no extra signal.
  if (!text || text.toLowerCase() === title.toLowerCase()) return fallback;

  const MAX = 158;
  if (text.length <= MAX) return text;

  // Prefer ending on a sentence boundary if one lands in the usable range,
  // else cut at the last whole word and mark the truncation.
  const window = text.slice(0, MAX + 1);
  const sentenceEnd = Math.max(
    window.lastIndexOf('. '),
    window.lastIndexOf('? '),
    window.lastIndexOf('! ')
  );
  if (sentenceEnd >= 80) return text.slice(0, sentenceEnd + 1);
  const cut = window.lastIndexOf(' ');
  return text.slice(0, cut > 0 ? cut : MAX).replace(/[,;:—-]$/, '') + '…';
}

/** SEO-103: the doc's own `**Last Updated:** YYYY-MM-DD`, for sitemap lastmod. */
function extractLastUpdated(raw) {
  const m = raw.match(/^\*\*Last Updated:\*\*\s*(\d{4}-\d{2}-\d{2})/im);
  return m ? m[1] : null;
}

function rewriteLinks(rel, content) {
  const dir = path.posix.dirname(rel);
  return content.replace(/\]\(([^)\s]+)\)/g, (match, url) => {
    if (/^(https?:|mailto:|#|\/)/i.test(url)) return match;
    const [target, anchor = ''] = url.split(/(#.*)/s, 2);
    if (!target) return match;
    const resolved = path.posix.normalize(path.posix.join(dir, target));
    if (resolved.endsWith('.md') && fileSet.has(resolved)) {
      return `](/${routeFor(resolved)}/${anchor})`;
    }
    // Leaves docs/ (crates/, sdks/, …) or a non-markdown file → GitHub.
    const repoRel = path.posix.normalize(path.posix.join('docs', dir, target));
    if (!repoRel.startsWith('..')) return `](${GITHUB_BLOB}${repoRel}${anchor})`;
    return match;
  });
}

const PLANNED_NOTICE = `:::caution[Status: Planned]
This document's own status header marks it as **planned or aspirational** — what it describes may not be implemented yet. See the [roadmap](/18-roadmap/) for what has actually shipped.
:::`;

fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });

let plannedCount = 0;
let describedCount = 0;
/** Per-page facts the llms.txt / llms-full.txt writers below reuse. */
const pages = [];

for (const rel of files) {
  const raw = fs.readFileSync(path.join(DOCS, rel), 'utf8');
  const lines = raw.split('\n');
  const { title, stripLine } = extractTitle(rel, lines);
  const planned = isPlanned(rel, statusBlock(lines));
  if (planned) plannedCount++;

  const route = routeFor(rel);
  const bodyLines = lines.filter((_, i) => i !== stripLine);
  const description = deriveDescription(bodyLines, title, SITE_DESCRIPTION);
  if (description !== SITE_DESCRIPTION) describedCount++;
  const lastUpdated = extractLastUpdated(raw);

  const body = rewriteLinks(rel, demoteHeadings(bodyLines.join('\n')));
  const out = [
    '---',
    `title: ${JSON.stringify(title)}`,
    `description: ${JSON.stringify(description)}`,
    `slug: ${JSON.stringify(route)}`,
    ...(lastUpdated ? [`lastUpdated: ${lastUpdated}`] : []),
    '---',
    '',
    ...(planned ? [PLANNED_NOTICE, ''] : []),
    body,
  ].join('\n');

  const dest = path.join(OUT, rel);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, out);

  pages.push({ rel, route, title, description, planned, body });
}

// ─────────────────────────────────────────────────────── GEO-101: llms.txt
//
// Two generated files, both written into public/ so Astro copies them to the
// site root verbatim:
//
//   /llms.txt       the llmstxt.org index — H1, a blockquote summary, then one
//                   `- [Title](url): description` line per page, grouped by
//                   the same numbered sections the docs sidebar uses.
//   /llms-full.txt  the whole corpus inline, so a model that can take it needs
//                   one fetch instead of 200.
//
// Generated from the same pass that writes the pages, so they cannot drift
// from what the site publishes. Planned/aspirational pages are marked inline —
// an answer engine repeating this corpus should not present a roadmap item as
// a shipped feature.

const grouped = new Map();
for (const p of pages) {
  const key = p.rel.split('/')[0];
  if (!grouped.has(key)) grouped.set(key, []);
  grouped.get(key).push(p);
}

const llmsIndex = [
  '# Wovyr',
  '',
  `> ${SITE_DESCRIPTION}`,
  '',
  'Wovyr is an Apache-2.0, self-hosted trust layer for AI-generated user',
  'interfaces, running on a complete AI agent operating system written in Rust',
  'and deployed as a single binary. An agent composes an interface from a',
  'constrained component vocabulary; Wovyr evaluates every frame against',
  'policy before it renders, fails closed on anything the policy does not',
  'recognise, records the frame and the verdict in a keyed tamper-evident hash',
  'chain, and durably suspends the run until a human decides.',
  '',
  '- Source: https://github.com/punarduttrajput/wovyr (Apache-2.0)',
  '- Install: `cargo install wovyr-cli`',
  '- Full documentation corpus in one file: /llms-full.txt',
  '',
  'Documentation status is marked per page. Pages whose own status header says',
  'planned or aspirational are labelled "(planned)" below — those describe',
  'intended behaviour that is not implemented yet.',
  '',
];

for (const [key, group] of grouped) {
  llmsIndex.push(`## ${sectionTitle(key)}`, '');
  for (const p of group) {
    const flag = p.planned ? ' (planned)' : '';
    llmsIndex.push(`- [${p.title}](${SITE}/${p.route}/)${flag}: ${p.description}`);
  }
  llmsIndex.push('');
}

fs.mkdirSync(PUBLIC, { recursive: true });
fs.writeFileSync(path.join(PUBLIC, 'llms.txt'), llmsIndex.join('\n'));

const llmsFull = [
  '# Wovyr — full documentation',
  '',
  `> ${SITE_DESCRIPTION}`,
  '',
  'This file is the complete Wovyr documentation corpus, generated from the',
  'canonical specs in the repository. Source: https://github.com/punarduttrajput/wovyr',
  'Licence: Apache-2.0. Index with per-page links: /llms.txt',
  '',
  'Each document below is preceded by its canonical URL and, where its own',
  'status header marks it planned or aspirational, an explicit PLANNED note.',
  '',
];

for (const p of pages) {
  llmsFull.push(
    '---',
    '',
    `# ${p.title}`,
    '',
    `URL: ${SITE}/${p.route}/`,
    p.planned
      ? 'STATUS: PLANNED — this document describes intended behaviour that may not be implemented.'
      : '',
    '',
    // rewriteLinks has already turned cross-doc links into site-root paths
    // (`](/13-security/audit/)`); absolutise them so the corpus resolves on its
    // own, without the reader having to know which origin it came from.
    p.body.trim().replace(/\]\(\/(?!\/)/g, `](${SITE}/`),
    ''
  );
}

fs.writeFileSync(path.join(PUBLIC, 'llms-full.txt'), llmsFull.join('\n'));

// The site root (/) is a custom marketing landing page at
// src/pages/index.astro — NOT a Starlight splash. We deliberately do NOT
// generate src/content/docs/index.mdx here: that would claim `/` and collide
// with the Astro page. Docs keep their own section routes (/00-executive/…).

console.log(
  `synced ${files.length} pages (${plannedCount} marked "Status: Planned", ` +
    `${describedCount} with a derived description) → ${path.relative(ROOT, OUT)}`
);
console.log(
  `wrote public/llms.txt (${pages.length} entries) and public/llms-full.txt ` +
    `(${(fs.statSync(path.join(PUBLIC, 'llms-full.txt')).size / 1048576).toFixed(2)} MiB)`
);
