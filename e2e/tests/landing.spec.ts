import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

/**
 * DX-502: the landing page (`website`, real built + previewed static output,
 * not the dev server). Each assertion here fails against the pre-fix code —
 * that's the actual point of this harness, not an incidental property; see
 * the v1.5 roadmap doc's Phase 1/2 "Done" notes for what each ticket fixed
 * and how it was previously verified only by hand.
 */

test.describe('landing page (website)', () => {
  test('WEB-301: og:image resolves and is the right size', async ({ page, request }) => {
    await page.goto('/');
    const ogImage = await page.locator('meta[property="og:image"]').getAttribute('content');
    expect(ogImage).toBe('/og.png');
    const res = await request.get(ogImage!);
    expect(res.status()).toBe(200);
    expect(res.headers()['content-type']).toContain('image/png');
    // 1200x630 PNG: width/height are big-endian uint32 at bytes 16/20 (IHDR).
    const buf = await res.body();
    const width = buf.readUInt32BE(16);
    const height = buf.readUInt32BE(20);
    expect(width).toBe(1200);
    expect(height).toBe(630);
  });

  test('WEB-302: full content is visible with JavaScript disabled', async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false });
    const page = await context.newPage();
    await page.goto('/');

    // One heading from each of the 7 `.plate` sections below the hero. These are
    // the post-2026-07-31 (woven/indigo) headings; the previous list still named
    // the retired cobalt-era sections, so 7 of its 8 entries no longer existed in
    // the DOM at all and this test failed on a stale fixture rather than on a
    // real regression.
    const headings = [
      'An interface used to be something a person',
      'One pass of the shuttle',
      'Fail-closed means the thread',
      'Eight engines',
      'Running in five minutes',
      'Every engine on this page is',
      'Put a loom between your agents',
    ];
    for (const text of headings) {
      await expect(page.getByRole('heading', { name: new RegExp(text) })).toBeVisible();
    }

    // `toBeVisible()` above is necessary but NOT sufficient for WEB-302, and the
    // previous version of this test wrongly relied on it alone: Playwright treats
    // an element as visible when it has a non-empty bounding box and is not
    // `visibility:hidden`/`display:none` — **`opacity:0` still counts as
    // visible**. The reveal animation parks every `.rv` section at `opacity:0`
    // until an IntersectionObserver adds `.in`, so with JS disabled the real
    // failure mode is fully-laid-out but completely transparent content, which
    // the check above cannot see. Assert the computed opacity directly.
    const opacities = await page
      .locator('.plate .rv')
      .evaluateAll((els) => els.map((el) => getComputedStyle(el).opacity));
    expect(opacities.length, 'expected the .rv reveal wrappers to be present').toBeGreaterThan(0);
    for (const o of opacities) {
      expect(
        Number(o),
        'a .rv section is transparent with JS disabled — the <noscript> WEB-302 ' +
          'fallback in index.astro is missing or was overridden',
      ).toBeGreaterThan(0);
    }

    await context.close();
  });

  test('A11Y-204: a skip link is the first focusable element and reaches <main>', async ({ page }) => {
    await page.goto('/');
    await page.keyboard.press('Tab');
    const focused = page.locator(':focus');
    await expect(focused).toHaveAttribute('href', '#main');
    await expect(focused).toBeVisible();
    await page.keyboard.press('Enter');
    // jumping via the skip link should move focus/scroll into <main>
    const main = page.locator('main#main');
    await expect(main).toBeAttached();
  });

  test('A11Y-204/205/207: axe reports zero violations, including color-contrast', async ({ page }) => {
    await page.goto('/');
    const results = await new AxeBuilder({ page }).analyze();
    const summary = results.violations
      .map((v) => `${v.id} (${v.impact}): ${v.help} — ${v.nodes.length} node(s)`)
      .join('\n');
    expect(results.violations, summary).toEqual([]);
  });

  test('DSY-104: an explicit dark preference set here carries to a docs page', async ({ page, baseURL }) => {
    await page.goto('/');
    await page.evaluate(() => localStorage.setItem('starlight-theme', 'dark'));
    await page.reload();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');

    await page.goto('/00-executive/vision/');
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
    const stored = await page.evaluate(() => localStorage.getItem('starlight-theme'));
    expect(stored).toBe('dark');
  });
});
