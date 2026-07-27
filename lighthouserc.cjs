// DX-503: Lighthouse CI budgets for the landing page and one docs page.
//
// Thresholds are set from a real measured baseline, not chosen aspirationally
// — median-of-3 scores against the built `website/dist` on 2026-07-27:
//   /                          perf .97-.99  a11y 1.00  best-practices .96  seo 1.00
//   /00-executive/mission/     perf .99-1.0  a11y 1.00  best-practices 1.00 seo 1.00
// Each assertion below sits comfortably under its measured floor so normal
// run-to-run variance doesn't flake the gate, while still catching a real
// regression (e.g. an unbudgeted script, a dropped landmark, a broken meta
// tag) rather than being purely decorative.
module.exports = {
  ci: {
    collect: {
      staticDistDir: './website/dist',
      url: ['/index.html', '/00-executive/mission/index.html'],
      numberOfRuns: 3,
      chromeFlags: '--no-sandbox --headless=new',
    },
    assert: {
      assertions: {
        'categories:performance': ['error', { minScore: 0.85 }],
        'categories:accessibility': ['error', { minScore: 0.95 }],
        'categories:best-practices': ['error', { minScore: 0.9 }],
        'categories:seo': ['error', { minScore: 0.95 }],
      },
    },
    upload: {
      target: 'filesystem',
      outputDir: './lhci-reports',
    },
  },
};
