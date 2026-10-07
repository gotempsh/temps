// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

/**
 * Route-mocked: the on-demand TLS settings page, the Certificates empty state
 * that links to it, and the Workflows empty state must stay usable from phone
 * to desktop without horizontal scrolling.
 */

const settings = {
  external_url: 'http://localhost:3000',
  letsencrypt: { email: null, environment: 'production' },
  on_demand_tls: {
    enabled: true,
    zone: null,
    max_concurrent: 3,
    hourly_cap: 10,
    deployment_url_mode: 'http',
  },
}

async function mockApi(page: Page, onDemandEnabled: boolean) {
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    let json: unknown = []
    if (path === '/api/user/me')
      json = {
        id: 42,
        name: 'Test Operator',
        username: 'operator',
        email: 'operator@example.com',
        avatar_url: '',
        mfa_enabled: false,
        role: 'admin',
      }
    else if (path === '/api/settings')
      json = {
        ...settings,
        on_demand_tls: { ...settings.on_demand_tls, enabled: onDemandEnabled },
      }
    else if (path === '/api/domains/on-demand-certs')
      json = { certs: [], total: 0, page: 1, page_size: 20 }
    else if (path === '/api/projects')
      json = { projects: [], total: 0, page: 1, per_page: 50 }
    await route.fulfill({ json })
  })
}

async function expectNoHorizontalScroll(page: Page, width: number) {
  const scrollWidth = await page.evaluate(
    () => document.documentElement.scrollWidth
  )
  expect(scrollWidth).toBeLessThanOrEqual(width)
}

for (const width of [360, 768, 1440]) {
  test(`on-demand TLS settings fit at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    await mockApi(page, true)

    await page.goto('/settings/on-demand-tls')
    await expect(
      page.getByRole('heading', { name: 'On-demand TLS', level: 1 })
    ).toBeVisible()
    await expect(page.getByText('Issuance will not start yet')).toBeVisible()
    await expect(page.getByLabel('Zone')).toBeVisible()
    await expect(
      page.getByRole('button', { name: 'Save Changes' })
    ).toBeVisible()
    await expectNoHorizontalScroll(page, width)
    expect(errors).toEqual([])
  })

  test(`certificates empty state links to the switch at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 })
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    await mockApi(page, false)

    await page.goto('/certificates')
    await expect(page.getByText('On-demand TLS is off')).toBeVisible()
    const action = page.getByRole('link', { name: 'Turn on on-demand TLS' })
    await expect(action).toBeVisible()
    await expectNoHorizontalScroll(page, width)
    await action.click()
    await expect(page).toHaveURL(/\/settings\/on-demand-tls$/)
    expect(errors).toEqual([])
  })

  test(`workflows empty state offers project creation at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 })
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    await mockApi(page, false)

    await page.goto('/ai-workflows')
    await expect(
      page.getByRole('heading', { name: 'Workflows', level: 1 })
    ).toBeVisible()
    await expect(
      page.getByRole('link', { name: 'Create project' })
    ).toBeVisible()
    await expectNoHorizontalScroll(page, width)
    expect(errors).toEqual([])
  })
}
