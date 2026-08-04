import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

/**
 * DX-502/DASH-401: the dashboard shell, run against `ng serve` with no live
 * `wovyr-server` backend. Scoped deliberately to what's true without a
 * backend: the rail's identity block, tenant link, and auth-mode pill all
 * come from `Session`'s client-side defaults (no API call needed) — see
 * `dashboard/src/app/core/session.ts`. Fails against the pre-DASH-401/402/
 * 403 code, where the rail showed a hardcoded "Punar R." / "org.admin" /
 * "Acme · production" with no relation to Session, and the tenant control
 * was a dead `<button>`.
 *
 * NOT covered here (would need a live `wovyr-server` in CI): the Surfaces
 * panel's real present→render→decide flow (DSY-105's live-toggle theme
 * forwarding into `<wovyr-ui-frame>` — already verified manually, see the
 * roadmap doc's DSY-105 "Done" note), and any route that actually renders
 * API-fetched data. A documented follow-on, not silently skipped.
 */

test.describe('dashboard shell (no backend)', () => {
  test('DASH-401: identity block reflects Session defaults, not hardcoded strings', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByText('admin@wovyr.local')).toBeVisible();
    // "acme" legitimately appears twice — the tenant link and the identity
    // block's own tenant line — both are real, both should exist.
    await expect(page.getByText('acme', { exact: true })).toHaveCount(2);
    await expect(page.getByText('Punar R.')).toHaveCount(0);
    await expect(page.getByText('org.admin')).toHaveCount(0);
    await expect(page.getByText('Acme · production')).toHaveCount(0);
  });

  test('DASH-402: an auth-mode pill states no credential is set (dev mode)', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByText(/no credential/i)).toBeVisible();
  });

  test('DASH-403: the tenant control is a real link to Sign in, not a dead button', async ({ page }) => {
    await page.goto('/');
    const scope = page.locator('a.scope');
    await expect(scope).toHaveAttribute('href', '/login');
  });

  // Updated 2026-08-04: this asserted the window/scanline mark DSY-106 unified on,
  // which the 2026-07-31 identity redesign superseded with the wolf head — the rail
  // had simply never been updated to match. Pins the path geometry rather than only
  // the class, since a `.mk-wolf` element drawing the wrong `d` would still pass a
  // class-presence check while showing a different mark, and guards against
  // regression to *both* retired marks now, not just the pre-DSY-106 triangle.
  const WOLF_PATH =
    'M50 26 L34 30 L18 6 L22 38 L12 52 L22 60 L16 72 L30 74 L36 64 L40 86 L44 94 ' +
    'L50 97 L56 94 L60 86 L64 64 L70 74 L84 72 L78 60 L88 52 L78 38 L82 6 L66 30 Z';

  test('the rail brand mark is the wolf head, not either retired mark', async ({ page }) => {
    await page.goto('/');
    const mark = page.locator('.brand .mark svg');
    await expect(mark.locator('.mk-wolf')).toHaveCount(1);
    // Identical geometry to website/public/favicon.svg and the Starlight
    // logo-{light,dark}.svg assets — the mark is only *the* Wovyr mark if it
    // draws the same path those do.
    await expect(mark.locator('.mk-wolf')).toHaveAttribute('d', WOLF_PATH);
    // The pre-DSY-106 plain triangle.
    const html = await page.content();
    expect(html).not.toContain('M12 3L21 19H3L12 3Z');
    // The window/scanline mark that replaced it and was in turn superseded.
    await expect(mark.locator('.mk-win')).toHaveCount(0);
    await expect(mark.locator('.mk-scan')).toHaveCount(0);
  });

  test('app shell has no detectable axe violations', async ({ page }) => {
    await page.goto('/');
    const results = await new AxeBuilder({ page }).analyze();
    const summary = results.violations
      .map((v) => `${v.id} (${v.impact}): ${v.help} — ${v.nodes.length} node(s)`)
      .join('\n');
    expect(results.violations, summary).toEqual([]);
  });
});
