// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  ADMIN_EMAIL,
  ADMIN_PASSWORD,
  expect,
  expectAppMounted,
  login,
  test,
  URL_LOGIN,
  URL_PROJECTS,
} from '../fixtures'

/**
 * Authentication is the narrowest useful definition of "the console works":
 * it exercises the boot path, the form layer, the API client, cookie handling
 * and the post-login redirect in one flow.
 */
test.describe('authentication', () => {
  // These run against a clean slate rather than the shared logged-in state.
  test.use({ storageState: { cookies: [], origins: [] } })

  test('signs in with valid credentials and lands on projects', async ({
    page,
    consoleErrors,
  }) => {
    await login(page, ADMIN_EMAIL, ADMIN_PASSWORD)

    await page.waitForURL(URL_PROJECTS, { timeout: 30_000 })
    await expectAppMounted(page)
    await expect(
      page.getByRole('heading', { name: 'Projects', exact: true })
    ).toBeVisible()

    expect(consoleErrors).toEqual([])
  })

  test('rejects a wrong password without navigating away', async ({ page }) => {
    await login(page, ADMIN_EMAIL, 'definitely-not-the-password')

    // The failure must be visible to the user. Silently staying on the form
    // with no message is its own bug -- self-hosted users have nobody to ask
    // why nothing happened.
    await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible()
    await expect(page).toHaveURL(URL_LOGIN)

    const feedback = page
      .getByText(/invalid|incorrect|failed|unauthor/i)
      .first()
    await expect(
      feedback,
      'a failed login should surface a visible reason'
    ).toBeVisible({
      timeout: 20_000,
    })

    // Deliberately not asserting an empty console here: a rejected login
    // legitimately produces a 401, which the browser logs as a console error.
  })

  test('keeps a protected route private while signed out', async ({
    page,
    consoleErrors,
  }) => {
    await page.goto('/storage')

    // The login form is rendered in place rather than redirected to; see the
    // matching note in anonymous/console-boot.spec.ts.
    await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible()
    await expect(
      page.getByRole('heading', { name: 'Databases', exact: true }),
      'protected content must not render for a signed-out visitor'
    ).toHaveCount(0)

    expect(consoleErrors).toEqual([])
  })

  test('lands on the app, not a 404, when signing in from /login directly', async ({
    page,
  }) => {
    // Regression test for a bug this suite found: `/login` is not a registered
    // route -- it falls through to ProtectedLayout, which renders the login
    // form in place and calls captureReturnTo(), storing "/login". On success
    // the app navigated *back* to /login, now authenticated, where
    // AuthenticatedRoutes has no such route -- so a correct login ended on
    // "404 - Page Not Found". Fixed by adding /login to AUTH_PATHS in
    // src/lib/return-to.ts.
    await login(page)

    await page.waitForURL(URL_PROJECTS, { timeout: 30_000 })
    await expect(
      page.getByRole('heading', { name: 'Projects', exact: true })
    ).toBeVisible()
    await expect(page.getByText(/404\s*-\s*Page Not Found/i)).toHaveCount(0)
  })
})
