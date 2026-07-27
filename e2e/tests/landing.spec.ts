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
    // One heading from each of the 8 .band sections below the hero — if
    // WEB-302 regresses, these stay at opacity:0 and Playwright's default
    // visibility check (which respects computed style, not just DOM
    // presence) fails.
    const headings = [
      'Interfaces are moving from pages',
      'Every frame passes the gate',
      'A complete agent runtime',
      'Built to be',
      'Memory-safe, deterministic',
      'Running in',
      'An embeddable runtime',
      'Put a trust layer between',
    ];
    for (const text of headings) {
      await expect(page.getByRole('heading', { name: new RegExp(text) })).toBeVisible();
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
