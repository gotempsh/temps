// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

test('backup forms use the full dashboard content width', async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1080 })
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    let json: unknown = []

    if (path === '/api/user/me') {
      json = {
        id: 42,
        name: 'Test Operator',
        username: 'operator',
        email: 'operator@example.com',
        avatar_url: '',
        mfa_enabled: false,
        role: 'admin',
      }
    } else if (path === '/api/backups/s3-sources/2') {
      json = { id: 2, name: 'Test storage' }
    } else if (path === '/api/backups/schedules/3') {
      json = {
        id: 3,
        s3_source_id: 2,
        name: 'Daily backup',
        description: '',
        backup_type: 'scheduled',
        schedule_expression: '0 0 * * *',
        retention_period: 7,
        enabled: true,
        target_all_services: true,
        include_control_plane: true,
      }
    }

    await route.fulfill({ contentType: 'application/json', json })
  })

  for (const [path, panelTitle, panelAncestor] of [
    [
      '/backups/s3-sources/2/schedules/new',
      'Schedule details',
      'xpath=ancestor::section[1]',
    ],
    [
      '/backups/s3-sources/2/schedules/3/edit',
      'Edit backup schedule',
      'xpath=ancestor::div[contains(@class, "rounded-lg") and contains(@class, "border")][1]',
    ],
    [
      '/backups/s3-sources/new',
      'S3 Configuration',
      'xpath=ancestor::div[contains(@class, "rounded-lg") and contains(@class, "border")][1]',
    ],
  ] as const) {
    await page.goto(path)
    const panel = page
      .getByText(panelTitle, { exact: true })
      .first()
      .locator(panelAncestor)
    await expect(panel).toBeVisible()

    const width = await panel.evaluate((element) => ({
      content: element.getBoundingClientRect().width,
      available: element.parentElement!.getBoundingClientRect().width,
    }))

    console.log(
      `${path}: ${Math.round(width.content)}px / ${Math.round(width.available)}px`
    )
    expect(width.content / width.available).toBeGreaterThan(0.95)
  }
})
