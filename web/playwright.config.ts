import { defineConfig, devices } from '@playwright/test'

/**
 * Playwright drives the console as a user does, against a *real* running Temps
 * instance. It deliberately does NOT start a server itself.
 *
 * Everything we had before this was static: `tsc --noEmit` and `rsbuild build`
 * both pass happily on a console that renders a blank white page, and the only
 * runtime check in CI was `curl -w "%{http_code}" http://localhost:8081/`, which
 * returns 200 even when React has died on boot (the HTML shell is served fine;
 * the JS is what fails). That is how issue #504 -- react/react-dom on mismatched
 * versions, React error #527 -- reached a nightly release.
 *
 * So the server is external on purpose:
 *   - In CI, `e2e-tests.yml` has already built the WASM, the web UI and the
 *     release binary, provisioned TimescaleDB, run `temps setup` and started
 *     `temps serve`. Re-launching a second stack here would double a 20-minute
 *     job for no extra signal.
 *   - Locally, point E2E_BASE_URL at whatever slot you're running
 *     (see the start-temps skill -- slot N serves the console on 8081 + N*10).
 *
 * Note that the console under test is the bundle the Rust binary *embeds*
 * (crates/temps-cli/dist), not a dev-server build. That distinction matters:
 * #504 only manifested in the built bundle.
 */

const baseURL = process.env.E2E_BASE_URL ?? 'http://localhost:8081'

export default defineConfig({
  testDir: './e2e',
  // The project-creation spec clones a real repository server-side, which is
  // slower than a typical UI assertion but is the thing actually worth testing.
  timeout: 120_000,
  expect: { timeout: 15_000 },
  // A shared Temps instance is mutated by these specs (projects get created), so
  // running files in parallel against one backend invites cross-test flakiness.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI
    ? [['list'], ['html', { open: 'never', outputFolder: 'playwright-report' }]]
    : [['list']],
  use: {
    baseURL,
    // On a failed run these are the difference between "CI is red" and knowing
    // why within 30 seconds. Cheap because they are only kept on failure.
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
    // The e2e workflow serves the console over plain HTTP with a self-signed
    // localho.st certificate in play for deployed apps.
    ignoreHTTPSErrors: true,
  },
  // Specs are split by the session they need, because getting this wrong is
  // silent: an "unauthenticated" assertion that quietly inherits a logged-in
  // storage state still passes, while testing nothing.
  //   e2e/anonymous/    -- no session; boot, login, route guards
  //   e2e/authenticated -- reuses the session captured by the setup project
  projects: [
    // Logs in once and writes a storage state the authenticated project reuses,
    // rather than paying for a login round-trip in every spec.
    { name: 'setup', testMatch: /auth\.setup\.ts/ },
    {
      name: 'chromium-anonymous',
      testDir: './e2e/anonymous',
      use: { ...devices['Desktop Chrome'], storageState: { cookies: [], origins: [] } },
    },
    {
      name: 'chromium',
      testDir: './e2e/authenticated',
      use: { ...devices['Desktop Chrome'], storageState: 'e2e/.auth/user.json' },
      dependencies: ['setup'],
    },
  ],
})
