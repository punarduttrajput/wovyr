// SEO-103: stamp a real `<lastmod>` onto each docs URL in the generated sitemap.
//
// Starlight always registers its own `@astrojs/sitemap` instance
// (integrations/sitemap.ts) and exposes no `serialize` hook through its config,
// so adding a second `sitemap()` to `integrations` would mean two integrations
// writing the same sitemap-*.xml files. This post-processes Starlight's output
// in `astro:build:done` instead — no conflict, and it runs on the real emitted
// file rather than a parallel guess at what it contains.
//
// Dates come from each page's own `lastUpdated` frontmatter, which
// scripts/sync-docs.mjs derives from the canonical doc's `**Last Updated:**`
// header. Pages with no such header (and the marketing landing page) get NO
// lastmod at all rather than a build timestamp: `lastmod` is optional per URL,
// and stamping build time on every URL every deploy is precisely the pattern
// that makes crawlers stop trusting the field.

import fs from 'node:fs';
import path from 'node:path';
// fileURLToPath, not `new URL(dir).pathname` — the latter yields a URL-encoded
// `/D:/New%20folder/...` that fails on drive letters and spaces. Same trap
// scripts/sync-docs.mjs documents at the top; hit for real here first.
import { fileURLToPath } from 'node:url';

/** route (no leading/trailing slash) → `YYYY-MM-DD`, from synced frontmatter. */
function collectDates(contentDir) {
  const dates = new Map();
  const walk = (dir) => {
    if (!fs.existsSync(dir)) return;
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const abs = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(abs);
      else if (entry.name.endsWith('.md')) {
        const src = fs.readFileSync(abs, 'utf8');
        const slug = src.match(/^slug: "(.*)"$/m);
        const last = src.match(/^lastUpdated: (\d{4}-\d{2}-\d{2})$/m);
        if (slug && last) dates.set(slug[1], last[1]);
      }
    }
  };
  walk(contentDir);
  return dates;
}

export function sitemapLastmod({ contentDir = 'src/content/docs' } = {}) {
  return {
    name: 'wovyr:sitemap-lastmod',
    hooks: {
      'astro:build:done': ({ dir, logger }) => {
        const dates = collectDates(contentDir);
        const outDir = fileURLToPath(dir);

        let stamped = 0;
        for (const file of fs.readdirSync(outDir)) {
          if (!/^sitemap-\d+\.xml$/.test(file)) continue;
          const target = path.join(outDir, file);
          const xml = fs.readFileSync(target, 'utf8');

          const next = xml.replace(/<loc>([^<]+)<\/loc>/g, (match, loc) => {
            // `https://wovyr.com/05-llm-gateway/overview/` → `05-llm-gateway/overview`
            const route = new URL(loc).pathname.replace(/^\/|\/$/g, '');
            const date = dates.get(route);
            if (!date) return match;
            stamped++;
            return `${match}<lastmod>${date}</lastmod>`;
          });

          if (next !== xml) fs.writeFileSync(target, next);
        }

        logger.info(
          `stamped <lastmod> on ${stamped} of ${dates.size} dated docs URLs ` +
            `(undated pages deliberately left without one)`
        );
      },
    },
  };
}
