import { test, expect } from '@playwright/test';

/**
 * DX-502/DSY-101: cross-surface brand-token agreement — "a test asserts
 * cross-surface agreement, so the next divergence fails CI."
 *
 * **Scope changed by the 2026-07-31 landing redesign.** DSY-101 was written when
 * all three surfaces consumed `packages/tokens/wovyr-tokens.css`, so it compared
 * the landing page against the dashboard. The landing page has since migrated to
 * the woven/indigo system and now scopes its palette locally *on purpose* —
 * `website/landing/DESIGN-system.md` §8 records the decision explicitly:
 * migrating the canonical token file "would restyle the docs and the dashboard in
 * one commit, which is a separate, larger piece of work with its own review."
 *
 * So the landing-vs-dashboard comparison was asserting a property the project has
 * deliberately decided not to hold yet, and it failed for that reason rather than
 * catching a regression. Rather than delete the gate (drift then hides) or migrate
 * the dashboard here (a design decision this suite has no business making), it is
 * retargeted at the boundary that *is* still meant to hold, plus a guard on the
 * exception itself:
 *
 *   1. **Docs ↔ dashboard must agree.** These two genuinely still share the
 *      canonical token file, so any divergence between them is a real bug — the
 *      original DSY-101 failure mode (a build-config mistake reintroducing drift
 *      even with the canonical file unchanged) is unchanged for this pair.
 *   2. **The landing exception must stay exactly as documented.** The landing must
 *      define the woven palette and must *not* resolve the canonical tokens. This
 *      fails if the landing silently starts importing the canonical file, or if
 *      someone half-migrates it — either of which means §8's boundary moved and
 *      this gate needs to be widened back to all three surfaces.
 */

/** Tokens defined by `packages/tokens/wovyr-tokens.css` (the cobalt system). */
const CANONICAL = ['--accent', '--canvas', '--surface', '--ink', '--line', '--r'];

/** Tokens defined by the landing page's own woven/indigo system. */
const WOVEN = ['--indigo', '--paper', '--warp'];

async function readTokens(
  page: import('@playwright/test').Page,
  theme: 'light' | 'dark',
  names: readonly string[],
) {
  await page.evaluate((t) => document.documentElement.setAttribute('data-theme', t), theme);
  return page.evaluate((ns) => {
    const cs = getComputedStyle(document.documentElement);
    const out: Record<string, string> = {};
    for (const n of ns) out[n] = cs.getPropertyValue(n).trim();
    return out;
  }, names as string[]);
}

test.describe('cross-surface token agreement (DSY-101)', () => {
  for (const theme of ['light', 'dark'] as const) {
    test(`docs and dashboard agree on canonical tokens in ${theme} theme`, async ({ browser }) => {
      // Same Astro build as the landing page, different route — the docs site is
      // one of the two surfaces still on the canonical token file.
      const docsPage = await (await browser.newContext()).newPage();
      await docsPage.goto('http://127.0.0.1:4321/00-executive/vision/');
      const docsTokens = await readTokens(docsPage, theme, CANONICAL);

      const dashboardPage = await (await browser.newContext()).newPage();
      await dashboardPage.goto('http://127.0.0.1:4300/');
      const dashboardTokens = await readTokens(dashboardPage, theme, CANONICAL);

      for (const name of CANONICAL) {
        // Guard against a vacuous pass: an empty string on both sides would
        // otherwise compare equal and assert nothing at all.
        expect(dashboardTokens[name], `${name} should resolve on the dashboard`).not.toBe('');
        expect(docsTokens[name], `${name} in ${theme} theme`).toBe(dashboardTokens[name]);
      }
    });

    test(`the landing page stays on its own woven palette in ${theme} theme (DESIGN-system.md §8)`, async ({
      browser,
    }) => {
      const landingPage = await (await browser.newContext()).newPage();
      await landingPage.goto('http://127.0.0.1:4321/');

      const woven = await readTokens(landingPage, theme, WOVEN);
      for (const name of WOVEN) {
        expect(woven[name], `${name} should resolve on the landing page`).not.toBe('');
      }

      // The documented exception: the landing does not import the canonical file.
      // If this starts resolving, the §8 migration has begun and the docs↔dashboard
      // test above should be widened to include the landing again.
      const canonical = await readTokens(landingPage, theme, ['--accent', '--canvas']);
      expect(
        canonical['--accent'],
        'the landing resolves --accent, so it now consumes the canonical token file — ' +
          'DESIGN-system.md §8 migration has started; widen this gate to all three surfaces',
      ).toBe('');
    });
  }
});
