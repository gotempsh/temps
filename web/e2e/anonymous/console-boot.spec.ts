// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, expectAppMounted, test } from '../fixtures'

/**
 * The regression guard for issue #504.
 *
 * On the broken nightly (v0.1.0-nightly.20260801) this page threw
 *   Uncaught Error: Minified React error #527  (react 19.2.7 vs react-dom 19.2.8)
 * and left `#root` completely empty -- while still returning HTTP 200, still
 * type-checking, and still building cleanly.
 *
 * These tests run unauthenticated (see the `chromium-anonymous` project) so they
 * exercise the very first paint, which is exactly where a boot failure lands.
 * They are also the broadest net in the suite: any top-level crash -- a throwing
 * effect, a broken lazy chunk, a bad `source.define` -- fails here too.
 */
test.describe('console boot', () => {
  test('renders the login screen instead of a blank page', async ({
    page,
    consoleErrors,
  }) => {
    const response = await page.goto('/')

    // Worth asserting explicitly: a 200 here is what the old curl check relied
    // on, and it is true in BOTH the working and the blank-page case. The
    // assertions after it are the ones that carry real signal.
    expect(response?.status(), 'the console shell should be served').toBe(200)

    await expectAppMounted(page)

    // A specific piece of app-rendered content, not just "something rendered".
    await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible()
    await expect(page.getByLabel('Email')).toBeVisible()
    await expect(page.getByLabel('Password')).toBeVisible()

    expect(consoleErrors).toEqual([])
  })

  test('loads the React vendor chunk without a version mismatch', async ({
    page,
  }) => {
    // Belt and braces alongside web/scripts/check-react-versions.mjs: that
    // script guards the lockfile, this guards what actually reached the bundle
    // the binary serves. React only exposes error #527 at runtime.
    const failures: string[] = []
    page.on('pageerror', (error) => {
      if (/react error #(418|423|425|527)/i.test(error.message))
        failures.push(error.message)
    })

    await page.goto('/')
    await expectAppMounted(page)

    expect(
      failures,
      'React threw a version-mismatch/hydration error -- check that react and react-dom resolve to the same version in web/bun.lock'
    ).toEqual([])
  })

  test('gates a protected route behind the login form', async ({
    page,
    consoleErrors,
  }) => {
    await page.goto('/projects')

    // Note: the app does NOT redirect. `/projects` still falls through to
    // ProtectedLayout, which renders the login form *in place* and leaves the
    // URL untouched. Asserting on a redirect here would be asserting on
    // behaviour the console does not have -- what matters is that the guarded
    // content is withheld and the user is given a way in.
    await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible()
    await expect(
      page.getByRole('heading', { name: 'Projects', exact: true }),
      'protected content must not render for a signed-out visitor'
    ).toHaveCount(0)

    expect(consoleErrors).toEqual([])
  })
})
