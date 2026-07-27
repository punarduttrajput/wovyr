/**
 * A11Y-207: token-level WCAG contrast enforcement. `a11y.spec.ts` disables
 * axe's `color-contrast` rule because Karma renders detached fixtures that
 * can't be reliably measured for paint — but that left contrast checked "in
 * review" only, and review missed eight real failing pairs (A11Y-201's
 * --ink-3, A11Y-202's --accent-fg, A11Y-203's --ok/--warn/--crit/--accent-2).
 *
 * This test doesn't render a component or parse a CSS file by hand — it
 * reads the ACTUAL resolved custom-property values off `document
 * .documentElement` in both themes, the same tokens every real page in this
 * app resolves through `angular.json`'s global styles array (which loads
 * `packages/tokens/wovyr-tokens.css` before `styles.scss`). So this test
 * can never drift from what the app actually serves — there's no second
 * copy of the values to fall out of sync.
 *
 * Every pair below is a real rendered pairing in the app, not a hypothetical
 * combination — see the `label` on each for where it's used.
 */
describe('design tokens clear WCAG AA contrast (A11Y-207)', () => {
  function hexToRgb(hex: string): [number, number, number] {
    hex = hex.trim().replace('#', '');
    if (hex.length === 3) {
      hex = hex[0] + hex[0] + hex[1] + hex[1] + hex[2] + hex[2];
    }
    const n = parseInt(hex, 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  }

  function relativeLuminance([r, g, b]: [number, number, number]): number {
    const a = [r, g, b].map((v) => {
      const c = v / 255;
      return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
    });
    return 0.2126 * a[0] + 0.7152 * a[1] + 0.0722 * a[2];
  }

  function contrastRatio(fgHex: string, bgHex: string): number {
    const l1 = relativeLuminance(hexToRgb(fgHex));
    const l2 = relativeLuminance(hexToRgb(bgHex));
    const [hi, lo] = l1 > l2 ? [l1, l2] : [l2, l1];
    return (hi + 0.05) / (lo + 0.05);
  }

  function token(name: string): string {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  const AA_NORMAL = 4.5;

  /** Every pair is [label, foreground token, background token]. All are
   * normal-size text/UI, so the 4.5:1 threshold applies uniformly — none of
   * these render at WCAG's "large text" size (≥18.66px bold / 24px regular). */
  const PAIRS: Array<[string, string, string]> = [
    ['body text on canvas', '--ink', '--canvas'],
    ['secondary text (--ink-2) on surface', '--ink-2', '--surface'],
    ['tertiary text (--ink-3) on surface — A11Y-201', '--ink-3', '--surface'],
    ['tertiary text (--ink-3) on surface-2 — A11Y-201', '--ink-3', '--surface-2'],
    // DX-502's e2e suite caught this one AFTER the first --ink-3 fix shipped
    // (the .eyebrow breadcrumb renders directly on --canvas, not --surface/
    // --surface-2 — a background this list hadn't covered) — added here too
    // so the unit-level test alone would catch a future regression on it.
    ['tertiary text (--ink-3) on canvas — A11Y-201', '--ink-3', '--canvas'],
    ['filled-button foreground on accent (.btn.pri) — A11Y-202', '--accent-fg', '--accent'],
    ['ok status text on ok-bg (.pill.ok) — A11Y-203', '--ok', '--ok-bg'],
    ['warn status text on warn-bg (.pill.warn) — A11Y-203', '--warn', '--warn-bg'],
    ['crit status text on crit-bg (.pill.crit) — A11Y-203', '--crit', '--crit-bg'],
  ];

  for (const theme of ['light', 'dark'] as const) {
    describe(`${theme} theme`, () => {
      beforeEach(() => {
        document.documentElement.setAttribute('data-theme', theme);
      });
      afterEach(() => {
        document.documentElement.removeAttribute('data-theme');
      });

      for (const [label, fgVar, bgVar] of PAIRS) {
        it(`${label} clears ${AA_NORMAL}:1`, () => {
          const fg = token(fgVar);
          const bg = token(bgVar);
          expect(fg).withContext(`${fgVar} not resolved in ${theme} theme`).not.toBe('');
          expect(bg).withContext(`${bgVar} not resolved in ${theme} theme`).not.toBe('');
          const ratio = contrastRatio(fg, bg);
          expect(ratio)
            .withContext(`${label}: ${fgVar}(${fg}) on ${bgVar}(${bg}) = ${ratio.toFixed(2)}:1, needs ${AA_NORMAL}:1`)
            .toBeGreaterThanOrEqual(AA_NORMAL);
        });
      }
    });
  }
});
