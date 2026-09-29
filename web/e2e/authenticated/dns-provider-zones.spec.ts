// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '../fixtures'

test('Cloudflare zones are selected from the provider and a failed verification never reports success', async ({
  page,
}) => {
  const now = new Date().toISOString()
  await page.route('**/api/dns-providers/42', async (route) => {
    await route.fulfill({
      json: {
        id: 42,
        name: 'Cloudflare test',
        provider_type: 'cloudflare',
        description: null,
        credentials: { api_token: '********' },
        flat_hostnames_supported: true,
        is_active: true,
        last_error: null,
        last_used_at: null,
        created_at: now,
        updated_at: now,
      },
    })
  })
  await page.route('**/api/dns-providers/42/domains', async (route) => {
    await route.fulfill({
      json: [
        {
          id: 1,
          provider_id: 42,
          domain: 'existing.dev',
          zone_id: 'zone-existing',
          auto_manage: true,
          proxied_by_default: false,
          verified: false,
          generated_hostname_mode: 'standard',
          sync_generated_records: false,
          zone_access_ok: false,
          zone_access_error: null,
          verification_error: null,
          created_at: now,
          updated_at: now,
        },
      ],
    })
  })
  await page.route('**/api/dns-providers/42/zones', async (route) => {
    await route.fulfill({
      json: {
        zones: [
          {
            id: 'zone-existing',
            name: 'existing.dev',
            status: 'active',
            nameservers: [],
          },
          {
            id: 'zone-new',
            name: 'new.dev',
            status: 'active',
            nameservers: [],
          },
        ],
      },
    })
  })
  await page.route(
    '**/api/dns-providers/42/domains/existing.dev/verify',
    async (route) => {
      await route.fulfill({
        status: 500,
        json: { detail: 'Cloudflare zone access failed' },
      })
    }
  )

  await page.goto('/dns-providers/42')
  await expect(
    page.getByText('Managed zones', { exact: true }).first()
  ).toBeVisible()
  await page.getByRole('button', { name: 'Verify' }).click()
  await expect(page.getByText('Failed to verify zone access')).toBeVisible()
  await expect(page.getByText('Cloudflare zone access failed')).toBeVisible()
  await expect(page.getByText('Zone access verified')).toHaveCount(0)

  await page.getByRole('button', { name: 'Add zone' }).click()
  await page.getByRole('combobox').click()
  await expect(page.getByRole('option', { name: 'new.dev' })).toBeVisible()
  await expect(page.getByRole('option', { name: 'existing.dev' })).toHaveCount(
    0
  )
  await page.getByRole('option', { name: 'new.dev' }).click()
  await expect(page.getByRole('combobox')).toContainText('new.dev')
})
