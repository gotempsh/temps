// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Route } from '@playwright/test'
import { expect, test } from '../fixtures'

/** `GET /delivery-profiles` with or without paging parameters. */
const PROFILE_LIST = /\/api\/delivery-profiles(?:\?.*)?$/

/** Answer a profile list request with the requested page of `profiles`. */
function fulfillProfilePage(route: Route, profiles: unknown[]) {
  const query = new URL(route.request().url()).searchParams
  const page = Number(query.get('page') ?? 1)
  const pageSize = Number(query.get('page_size') ?? 20)
  return route.fulfill({
    json: {
      items: profiles.slice((page - 1) * pageSize, page * pageSize),
      total: profiles.length,
      page,
      page_size: pageSize,
    },
  })
}

test('a delivery profile opens through a stable detail link', async ({
  page,
}) => {
  const now = new Date().toISOString()
  const profile = {
    id: 347,
    name: 'Cloudflare delivery',
    provider_kind: 'cloudflare',
    bunny_pull_zone_id: null,
    bunny_hostname: null,
    created_at: now,
    updated_at: now,
  }
  await page.route(PROFILE_LIST, (route) =>
    fulfillProfilePage(route, [profile])
  )
  await page.route('**/api/delivery-profiles/347', (route) =>
    route.fulfill({ json: profile })
  )

  await page.goto('/delivery-profiles')
  const link = page.getByRole('link', {
    name: 'View Cloudflare delivery details',
  })
  await expect(link).toHaveAttribute('href', '/delivery-profiles/347')
  await link.click()
  await expect(page).toHaveURL(/\/delivery-profiles\/347$/)
  await expect(
    page.getByRole('heading', { name: 'Cloudflare delivery' })
  ).toBeVisible()
  await expect(page.getByText('DNS connection', { exact: true })).toBeVisible()

  await page.reload()
  await expect(
    page.getByRole('heading', { name: 'Cloudflare delivery' })
  ).toBeVisible()
})

test('a missing delivery profile says so instead of failing', async ({
  page,
  consoleErrors,
  httpFailures,
}) => {
  await page.route('**/api/delivery-profiles/999', (route) =>
    route.fulfill({
      status: 404,
      json: {
        title: 'Delivery Profile Not Found',
        detail: 'Delivery profile 999 not found',
      },
    })
  )

  await page.goto('/delivery-profiles/999')
  await expect(
    page.getByRole('heading', { name: 'Delivery profile not found' })
  ).toBeVisible()
  await expect(
    page.getByText('No delivery profile has ID 999. It may have been deleted.')
  ).toBeVisible()
  await expect(
    page.getByRole('link', { name: 'Back to profiles' })
  ).toHaveAttribute('href', '/delivery-profiles')
  expect(httpFailures).toEqual([
    expect.stringMatching(/404 .*\/api\/delivery-profiles\/999$/),
  ])
  expect(consoleErrors).toEqual([])
})

test('the profile list pages through every profile', async ({ page }) => {
  const now = new Date().toISOString()
  const profiles = Array.from({ length: 25 }, (_, index) => ({
    id: index + 1,
    name: `Delivery profile ${String(index + 1).padStart(2, '0')}`,
    provider_kind: 'direct',
    bunny_pull_zone_id: null,
    bunny_hostname: null,
    created_at: now,
    updated_at: now,
  }))
  await page.route(PROFILE_LIST, (route) => fulfillProfilePage(route, profiles))

  await page.goto('/delivery-profiles')
  await expect(
    page.getByRole('link', { name: 'View Delivery profile 20 details' })
  ).toBeVisible()
  await expect(
    page.getByRole('link', { name: 'View Delivery profile 21 details' })
  ).toHaveCount(0)

  const pagination = page.getByRole('navigation', {
    name: 'Delivery profile pagination',
  })
  await pagination
    .getByRole('button', { name: /^(Go to next page|Next page)$/ })
    .filter({ visible: true })
    .click()
  await expect(
    page.getByRole('link', { name: 'View Delivery profile 25 details' })
  ).toBeVisible()
  await expect(
    page.getByRole('link', { name: 'View Delivery profile 01 details' })
  ).toHaveCount(0)
})
