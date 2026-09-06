// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineConfig, devices } from '@playwright/test'

/**
 * `DS_PORT` overrides the port so a second checkout can run the suite against
 * its own dev server without colliding with the one already on 5183.
 */
const PORT = Number(process.env.DS_PORT ?? 5183)
const BASE_URL = `http://localhost:${PORT}`

/**
 * The design-system sandbox's end-to-end suite. Chromium only: this is a
 * reference implementation, not a product, and the rules under test (keyboard
 * focus, overflow, contrast, snapshots) are engine-independent.
 *
 * `webServer` reuses whatever is already serving `PORT` — the sandbox is
 * usually open in a tab while it is being worked on, and starting a second
 * server on a strict port would just fail.
 */
export default defineConfig({
  testDir: './e2e',
  // Baselines live with the suite, in the repo, not in a scratch directory.
  snapshotPathTemplate: '{testDir}/__screenshots__/{testFileName}/{arg}-{projectName}{ext}',
  fullyParallel: true,
  retries: 0,
  reporter: [['list'], ['html', { open: 'never' }]],
  expect: {
    timeout: 5_000,
    toHaveScreenshot: { maxDiffPixelRatio: 0.01, animations: 'disabled', caret: 'hide' },
  },
  use: {
    baseURL: BASE_URL,
    trace: 'retain-on-failure',
    // The sandbox has no motion by design; disabling it in the browser keeps
    // Radix's mount transitions from racing an assertion.
    launchOptions: { args: ['--force-prefers-reduced-motion'] },
  },
  projects: [
    { name: 'desktop', use: { ...devices['Desktop Chrome'], viewport: { width: 1440, height: 900 } } },
    {
      // The phone runs the two suites that are about layout. Keyboard, focus
      // return and axe are the same DOM at either width; running them twice
      // would only double the time.
      name: 'phone',
      testMatch: /(overflow|visual)\.spec\.ts$/,
      use: { ...devices['Desktop Chrome'], viewport: { width: 390, height: 900 }, hasTouch: true },
    },
  ],
  webServer: {
    command: `bun run dev -- --port ${PORT} --strictPort`,
    url: BASE_URL,
    reuseExistingServer: true,
    timeout: 120_000,
  },
})
