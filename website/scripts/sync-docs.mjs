// Syncs ../docs (the canonical, spec-driven source of truth) into
// src/content/docs for the Starlight build. The copies are generated —
// never edit them; edit ../docs and re-run `npm run sync`.
//
// Per page it:
//  - adds the frontmatter Starlight requires (title, explicit slug)
//  - injects a "Status: Planned" caution on docs whose own `**Status:**`
//    header marks them planned / exploratory / aspirational / target-state
//  - rewrites relative .md cross-links to site routes, and links that leave
//    docs/ (crate paths, openapi.yaml, …) to GitHub blob URLs

import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const DOCS = path.resolve(ROOT, '../docs');
const OUT = path.join(ROOT, 'src/content/docs');
const GITHUB_BLOB = 'https://github.com/punarduttrajput/Wovyr/blob/main/';

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
for (const rel of files) {
  const raw = fs.readFileSync(path.join(DOCS, rel), 'utf8');
  const lines = raw.split('\n');
  const { title, stripLine } = extractTitle(rel, lines);
  const planned = isPlanned(rel, statusBlock(lines));
  if (planned) plannedCount++;

  const body = lines.filter((_, i) => i !== stripLine).join('\n');
  const out = [
    '---',
    `title: ${JSON.stringify(title)}`,
    `slug: ${JSON.stringify(routeFor(rel))}`,
    '---',
    '',
    ...(planned ? [PLANNED_NOTICE, ''] : []),
    rewriteLinks(rel, body),
  ].join('\n');

  const dest = path.join(OUT, rel);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, out);
}

// Landing page (not sourced from ../docs).
fs.writeFileSync(
  path.join(OUT, 'index.mdx'),
  `---
title: Wovyr
description: Generative UI Trust Runtime — built on an enterprise AI Agent Operating System written in Rust.
template: splash
hero:
  tagline: The infrastructure that lets AI agents render rich, interactive interfaces to humans — safely, auditable, and with durable human-in-the-loop decisions.
  actions:
    - text: Read the vision
      link: /00-executive/vision/
      icon: right-arrow
    - text: Hello, agent
      link: /16-examples/hello-agent/
      variant: minimal
    - text: GitHub
      link: https://github.com/punarduttrajput/Wovyr
      icon: external
      variant: minimal
---

import { Card, CardGrid } from '@astrojs/starlight/components';

<CardGrid stagger>
  <Card title="Trust & policy" icon="approve-check">
    Every agent-generated frame is validated against declarative, fail-closed
    policy and recorded in a tamper-evident audit chain before a human sees it.
  </Card>
  <Card title="Durable interaction" icon="setting">
    Agent shows an interface → human decides → agent continues, on an
    event-sourced workflow engine that survives crashes, restarts, and time.
  </Card>
  <Card title="Embeddable runtime" icon="puzzle">
    A frame protocol, React renderer SDK, and MCP surface any agent stack can
    adopt as middleware.
  </Card>
  <Card title="Enterprise engine" icon="rocket">
    Multi-LLM gateway, tool sandboxing, memory engine, plugin marketplace,
    multi-tenancy — one Rust binary.
  </Card>
</CardGrid>
`
);

console.log(
  `synced ${files.length} pages (${plannedCount} marked "Status: Planned") → ${path.relative(ROOT, OUT)}`
);
