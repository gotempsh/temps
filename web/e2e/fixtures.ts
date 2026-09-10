// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  test as base,
  expect,
  type Page,
  type TestInfo,
} from '@playwright/test'

/**
 * Shared fixtures. The important one is `consoleErrors`: every test collects
 * browser-side errors and asserts the list is empty at the end.
 *
 * That assertion is the whole point of this suite. A console that boots to a
 * blank page still serves HTTP 200 and still type-checks -- the only place the
 * failure is visible is the browser console, which nothing in CI was reading.
 */

/**
 * Messages that are noise rather than defects. Keep this list SHORT and
 * specific; every entry is a hole in the net.
 *
 * Nothing React-related may ever be added here -- see `isAlwaysFatal`.
 */
const IGNORED_CONSOLE_PATTERNS: RegExp[] = [
  // Benign browser-internal warning, fires on any resizable layout. Not a bug:
  // https://developer.mozilla.org/en-US/docs/Web/API/ResizeObserver
  /ResizeObserver loop (limit exceeded|completed with undelivered notifications)/,
  // Chrome logs every non-2xx subresource as
  //   "Failed to load resource: the server responded with a status of 404 (Not Found)"
  // with NO URL in the message text, so these entries cannot be triaged or
  // allowlisted individually -- you cannot tell a broken endpoint from an
  // optional avatar. They are covered properly by the `httpFailures` fixture
  // below, which sees the request URL. Filtering them here is what keeps this
  // list from degrading into "ignore all 4xx".
  /^Failed to load resource:/,
]

/**
 * HTTP responses that are expected and carry no signal.
 *
 * Unlike the console patterns above these match on the request URL, so they can
 * be precise about which endpoint is allowed to fail and why.
 */
const EXPECTED_HTTP_FAILURES: { pattern: RegExp; why: string }[] = [
  {
    // The console optimistically asks for a per-project favicon; projects
    // without one legitimately 404. Not user-visible.
    pattern: /\/api\/projects\/\d+\/favicon/,
    why: 'per-project favicon is optional',
  },
  {
    // A signed-out visitor's first paint probes the session endpoints before it
    // knows there is no session. That is the auth layer working.
    pattern: /\/api\/(user\/me|presets|projects)\b/,
    why: 'unauthenticated session probe on first paint',
  },
]

export function isExpectedHttpFailure(status: number, url: string): boolean {
  if (status === 401 || status === 403) {
    return EXPECTED_HTTP_FAILURES.some((entry) => entry.pattern.test(url))
  }
  if (status === 404) {
    return EXPECTED_HTTP_FAILURES.some((entry) => entry.pattern.test(url))
  }
  return false
}

/**
 * Failures that are never acceptable, regardless of the ignore list above.
 * `Minified React error #527` (the react/react-dom mismatch from #504) lands
 * here, as does any hook-order or hydration violation.
 */
function isAlwaysFatal(message: string): boolean {
  return /minified react error|invalid hook call|rendered more hooks|hydrat|is not a function|undefined is not an object/i.test(
    message
  )
}

export function isRealConsoleError(message: string): boolean {
  if (isAlwaysFatal(message)) return true
  return !IGNORED_CONSOLE_PATTERNS.some((pattern) => pattern.test(message))
}

export const test = base.extend<{
  consoleErrors: string[]
  httpFailures: string[]
}>({
  consoleErrors: async ({ page }, use) => {
    const errors: string[] = []

    // Uncaught exceptions -- this is where React error #527 surfaces.
    page.on('pageerror', (error) => errors.push(`pageerror: ${error.message}`))
    page.on('console', (message) => {
      if (message.type() !== 'error') return
      const text = message.text()
      if (isRealConsoleError(text)) errors.push(`console.error: ${text}`)
    })

    await use(errors)

    // Asserting here rather than in each spec means a test cannot silently
    // forget to check. If a spec needs to tolerate an error it must splice it
    // out of the array explicitly, which is visible in review.
    expect(errors, 'browser reported errors during this test').toEqual([])
  },

  /**
   * Unexpected HTTP failures, with URLs -- opt-in, and NOT auto-asserted.
   *
   * Whether a given 4xx is a bug depends entirely on what the spec was doing,
   * so the assertion belongs in the spec rather than here.
   */
  httpFailures: async ({ page }, use) => {
    const failures: string[] = []
    page.on('response', (response) => {
      const status = response.status()
      if (status < 400) return
      const url = response.url()
      if (isExpectedHttpFailure(status, url)) return
      failures.push(`${status} ${url}`)
    })
    await use(failures)
  },
})

export { expect }

/**
 * Asserts the SPA actually mounted, as opposed to merely serving its shell.
 *
 * `curl` cannot tell these apart: index.html is byte-identical whether React
 * boots or dies. `#root` having children is the cheapest honest signal.
 */
export async function expectAppMounted(page: Page): Promise<void> {
  const root = page.locator('#root')
  await expect(root, 'the SPA root should exist').toHaveCount(1)
  await expect(
    root.locator(':scope > *'),
    'the SPA root should have mounted children -- an empty #root is the blank-page failure from issue #504'
  ).not.toHaveCount(0)
}

/** Credentials come from the CI job env; the defaults match `e2e-tests.yml`. */
export const ADMIN_EMAIL =
  process.env.E2E_EMAIL ?? process.env.ADMIN_EMAIL ?? 'admin@localho.st'
export const ADMIN_PASSWORD =
  process.env.E2E_PASSWORD ?? process.env.ADMIN_PASSWORD ?? 'E2eTestPass123!'

/** Performs a UI login. Used by the setup project and by the auth spec. */
export async function login(
  page: Page,
  email = ADMIN_EMAIL,
  password = ADMIN_PASSWORD
) {
  await page.goto('/login')
  await page.getByLabel('Email').fill(email)
  await page.getByLabel('Password').fill(password)
  await page.getByRole('button', { name: 'Sign in' }).click()
}

/**
 * URL matchers.
 *
 * These are RegExps rather than Playwright's glob strings on purpose: when
 * `baseURL` is configured, a glob is resolved *relative to it*, so the natural
 * -looking `waitForURL('** /projects')` never matches `http://host/projects`
 * and fails with an unhelpful 30s timeout. Regexes match the full URL directly.
 */
export const URL_PROJECTS = /\/projects(?:[?#]|$)/
export const URL_LOGIN = /\/login(?:[?#]|$)/
export const urlForProject = (name: string) =>
  new RegExp(`/projects/${name}(?:[/?#]|$)`)

/**
 * Generates a resource name that is unique across parallel workers, retries,
 * and CI shards -- not just across runs.
 *
 * The CI run id alone (the pattern this replaces) is identical for every
 * worker in one job, so two workers creating similarly-prefixed resources in
 * the same run collide the moment `fullyParallel` is on. Mixing in
 * `parallelIndex` (unique per concurrently-running worker) and a short random
 * suffix (unique per call, so retries of the same test in the same worker
 * still get a fresh name) closes both gaps.
 */
export function uniqueSlug(prefix: string, testInfo: TestInfo): string {
  const worker = testInfo.parallelIndex
  const retry = testInfo.retry
  const rand = Math.random().toString(36).slice(2, 8)
  return `${prefix}-${worker}-${retry}-${rand}`.toLowerCase().slice(0, 32)
}
