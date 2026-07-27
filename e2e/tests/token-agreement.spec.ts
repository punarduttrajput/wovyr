import { test, expect } from '@playwright/test';

/**
 * DX-502/DSY-101: the actual regression gate DSY-101's own acceptance
 * criteria calls for — "a test asserts cross-surface agreement, so the
 * next divergence fails CI." Navigates to both real running surfaces and
 * asserts their resolved brand tokens are identical, rather than trusting
 * that both happen to `@import`/reference the same canonical file (a
 * config mistake in either build could silently reintroduce drift even
 * with the canonical file unchanged).
 */

const TOKENS = ['--accent', '--canvas', '--surface', '--ink', '--line', '--r'];

async function readTokens(page: import('@playwright/test').Page, theme: 'light' | 'dark') {
  await page.evaluate((t) => document.documentElement.setAttribute('data-theme', t), theme);
  return page.evaluate((names) => {
    const cs = getComputedStyle(document.documentElement);
    const out: Record<string, string> = {};
    for (const n of names) out[n] = cs.getPropertyValue(n).trim();
    return out;
  }, TOKENS);
}

test.describe('cross-surface token agreement (DSY-101)', () => {
  for (const theme of ['light', 'dark'] as const) {
    test(`landing and dashboard agree on brand tokens in ${theme} theme`, async ({ browser }) => {
      const landingPage = await (await browser.newContext()).newPage();
      await landingPage.goto('http://127.0.0.1:4321/');
      const landingTokens = await readTokens(landingPage, theme);

      const dashboardPage = await (await browser.newContext()).newPage();
      await dashboardPage.goto('http://127.0.0.1:4300/');
      const dashboardTokens = await readTokens(dashboardPage, theme);

      for (const name of TOKENS) {
        expect(dashboardTokens[name], `${name} in ${theme} theme`).toBe(landingTokens[name]);
      }
    });
  }
});
