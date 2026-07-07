import { defineConfig, devices } from '@playwright/test';

/**
 * Playwright E2E configuration for Opex UI.
 */
export default defineConfig({
  testDir: './src/__e2e__',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: 'html',
  use: {
    baseURL: process.env.PLAYWRIGHT_BASE_URL || 'http://localhost:18789',
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  // Static server for the self-contained overflow guard. `serve` (no -s) serves
  // the Next static export as-is so /__overflow_check resolves to its export
  // html. Existing live-backend e2e use absolute :18789 URLs and ignore this.
  webServer: {
    command: 'npx --yes serve out -l 4321 -n',
    url: 'http://localhost:4321/overflow-check',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
