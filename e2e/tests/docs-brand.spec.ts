import { test, expect } from '@playwright/test';

/**
 * DX-502/DSY-103: the docs site (same Astro build as the landing page,
 * different route). Fails against the pre-DSY-103 code, where Starlight ran
 * with zero customCss/logo — accent resolved to Starlight's default
 * `hsl(234,90%,60%)`, not the brand's `#5B7BFF`.
 */

test.describe('docs site brand (DSY-103)', () => {
  test('brand tokens resolve on a docs page, both themes', async ({ page }) => {
    await page.goto('/00-executive/vision/');

    await page.evaluate(() => document.documentElement.setAttribute('data-theme', 'dark'));
    const dark = await page.evaluate(() => {
      const cs = getComputedStyle(document.documentElement);
      return {
        accent: cs.getPropertyValue('--sl-color-accent').trim(),
        bg: cs.getPropertyValue('--sl-color-bg').trim(),
      };
    });
    expect(dark.accent.toLowerCase()).toBe('#5b7bff');
    expect(dark.bg.toLowerCase()).toBe('#0a0e18');

    await page.evaluate(() => document.documentElement.setAttribute('data-theme', 'light'));
    const light = await page.evaluate(() => {
      const cs = getComputedStyle(document.documentElement);
      return {
        accent: cs.getPropertyValue('--sl-color-accent').trim(),
        bg: cs.getPropertyValue('--sl-color-bg').trim(),
      };
    });
    expect(light.accent.toLowerCase()).toBe('#2d54e8');
    expect(light.bg.toLowerCase()).toBe('#f6f7f9');
  });

  test('the brand mark (not stock Starlight, no mark) appears in the docs header', async ({ page }) => {
    await page.goto('/00-executive/vision/');
    // Starlight renders BOTH light/dark <img> variants and toggles which is
    // visible via CSS (`light:sl-hidden`/`dark:sl-hidden`), not one-or-the-
    // other in the DOM — so both must exist and exactly one is visible.
    const logos = page.locator('.site-title img');
    await expect(logos).toHaveCount(2);
    await expect(logos.nth(0)).toHaveAttribute('src', /logo-(dark|light)/);
    await expect(logos.nth(1)).toHaveAttribute('src', /logo-(dark|light)/);
    const visibleCount = await logos.evaluateAll((imgs) =>
      imgs.filter((img) => (img as HTMLElement).offsetParent !== null).length,
    );
    expect(visibleCount).toBe(1);
  });

  test('headings render in the brand mono face', async ({ page }) => {
    await page.goto('/00-executive/vision/');
    const h1 = page.locator('h1').first();
    const fontFamily = await h1.evaluate((el) => getComputedStyle(el).fontFamily);
    expect(fontFamily).toMatch(/JetBrains Mono/);
  });
});
