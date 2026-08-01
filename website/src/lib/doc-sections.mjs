// Human-readable names for the numbered top-level docs directories.
//
// Shared deliberately: scripts/sync-docs.mjs groups /llms.txt by these, and
// src/components/Head.astro builds each page's BreadcrumbList JSON-LD from
// them. Two copies would drift, and a breadcrumb that disagrees with the
// sitemap/llms.txt grouping is worse than no breadcrumb — search engines treat
// a mismatch against the visible page as a structured-data quality problem.
//
// The docs sidebar in astro.config.mjs uses the same names with their numeric
// prefixes ("05 · LLM Gateway"); the prefix is dropped here because a
// breadcrumb and an LLM-facing index both want the plain name.
export const SECTION_TITLES = {
  '00-executive': 'Executive',
  '01-product': 'Product',
  '02-architecture': 'Architecture',
  '03-workflow-engine': 'Workflow engine',
  '04-agent-framework': 'Agent framework',
  '05-llm-gateway': 'LLM gateway',
  '06-memory-engine': 'Memory engine',
  '07-tool-runtime': 'Tool runtime',
  '08-plugin-sdk': 'Plugin SDK',
  '09-api': 'API',
  '10-dashboard': 'Dashboard',
  '11-cli': 'CLI',
  '12-deployment': 'Deployment',
  '13-security': 'Security',
  '14-observability': 'Observability',
  '15-testing': 'Testing',
  '16-examples': 'Examples',
  '17-adr': 'Architecture decision records',
  '18-roadmap': 'Roadmap',
  '19-implementation-guide': 'Implementation guide',
};

/** Section name for a docs route/path segment, falling back to the raw segment. */
export function sectionTitle(segment) {
  return SECTION_TITLES[segment] ?? segment;
}
