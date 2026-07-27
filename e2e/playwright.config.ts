import { defineConfig, devices } from '@playwright/test';

/**
 * DX-502: the first browser e2e harness in this workspace. Runs against a
 * real built website (Astro `build` + `preview`, so it's the actual static
 * output a deploy would serve — WEB-301/302's claims are about the built
 * artifact, not just the dev server) and a real dashboard dev server (no
 * live `wovyr-server` — every assertion in this suite is scoped to what's
 * verifiable from the dashboard's default, no-backend state; see each
 * spec's own header comment for exactly what that does and doesn't cover).
 *
 * Two `projects`, not one — the website and the dashboard are two different
 * origins/ports, so each gets its own `baseURL` and its own subset of spec
 * files via `testMatch`, rather than one global baseURL that would only be
 * correct for one of them.
 */
export default defineConfig({
  testDir: './tests',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    trace: 'retain-on-failure',
  },
  projects: [
    {
      name: 'website',
      testMatch: /landing\.spec\.ts|docs-brand\.spec\.ts/,
      use: { ...devices['Desktop Chrome'], baseURL: 'http://127.0.0.1:4321' },
    },
    {
      name: 'dashboard',
      testMatch: /dashboard\.spec\.ts/,
      use: { ...devices['Desktop Chrome'], baseURL: 'http://127.0.0.1:4300' },
    },
    {
      name: 'cross-surface',
      testMatch: /token-agreement\.spec\.ts/,
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: [
    {
      command: 'npm run build && npm run preview -- --port 4321 --host 127.0.0.1',
      cwd: '../website',
      url: 'http://127.0.0.1:4321/',
      timeout: 180_000,
      reuseExistingServer: !process.env.CI,
    },
    {
      // Angular's dev server binds `localhost` (IPv6-only [::1] on this
      // machine) unless --host is explicit — Playwright's health-check hits
      // 127.0.0.1 specifically, so without this flag the two never meet and
      // the whole webServer array times out waiting.
      command: 'npm start -- --port 4300 --host 127.0.0.1',
      cwd: '../dashboard',
      url: 'http://127.0.0.1:4300/',
      timeout: 180_000,
      reuseExistingServer: !process.env.CI,
    },
  ],
});
