// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '../fixtures'

test('a delivery profile opens through a stable detail link', async ({
  page,
}) => {
  const now = new Date().toISOString()
  await page.route('**/api/delivery-profiles', async (route) => {
    await route.fulfill({
      json: [
        {
          id: 347,
          name: 'Cloudflare delivery',
          provider_kind: 'cloudflare',
          bunny_pull_zone_id: null,
          bunny_hostname: null,
          created_at: now,
          updated_at: now,
        },
      ],
    })
  })

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
