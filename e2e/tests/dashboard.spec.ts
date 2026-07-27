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

  test('DSY-106: the rail brand mark is the unified window/scanline mark', async ({ page }) => {
    await page.goto('/');
    const mark = page.locator('.brand .mark svg');
    await expect(mark.locator('.mk-win')).toHaveCount(1);
    await expect(mark.locator('.mk-scan')).toHaveCount(1);
    const html = await page.content();
    expect(html).not.toContain('M12 3L21 19H3L12 3Z');
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
