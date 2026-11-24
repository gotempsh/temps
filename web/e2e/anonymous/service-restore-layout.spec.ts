// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

test('the restore picker keeps snapshots identifiable and selectable without horizontal scrolling', async ({
  page,
}) => {
  const origin = 'service-' + 'a'.repeat(60)
  const created = '2026-01-02T03:04:05Z'
  const location =
    's3://backups-bucket/service-a/walg/basebackups_005/base_0001_backup_stop_sentinel.json'
  const responses: Record<string, unknown> = {
    '/api/user/me': { id: 1, name: 'Operator', role: 'admin' },
    '/api/external-services/7': {
      service: { id: 7, name: 'service-b', service_type: 'postgres' },
      sensitive_parameters: [],
    },
    '/api/external-services/7/restore-capabilities': {
      service_type: 'postgres',
      capabilities: {},
      restore_in_place: true,
    },
    '/api/external-services/7/restore-plan': {
      errors: [],
      warnings: [],
      steps: [],
      strategy: 'walg_restore',
      target_service: { container: 'service-b' },
      source_backup: { location },
    },
    '/api/backups/s3-sources': [
      {
        id: 1,
        name: 'backups-bucket',
        bucket_name: 'backups-bucket',
        is_default: true,
      },
    ],
    '/api/backups/s3-sources/1/backups': {
      scan_errors: [],
      last_updated: created,
      backups: [
        {
          id: 0,
          backup_id: '',
          state: 'completed',
          source: 's3_scan',
          engine: 'postgres',
          format: 'walg',
          origin_service_name: origin,
          size_bytes: 2048,
          created_at: created,
          location,
        },
      ],
    },
  }
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    if (
      url.pathname === '/api/backups/s3-sources/1/backups' &&
      url.searchParams.get('include_s3_scan') !== 'true'
    ) {
      return route.fulfill({
        json: { backups: [], last_updated: created, scan_errors: [] },
      })
    }
    return route.fulfill({ json: responses[url.pathname] ?? [] })
  })
  await page.setViewportSize({ width: 320, height: 900 })
  await page.goto('/storage/7/restore?source=1')
  await page
    .getByRole('checkbox', {
      name: 'Also search the S3 bucket for backups made by other servers',
    })
    .check()
  const picker = page.getByRole('radiogroup', { name: 'Backup to restore' })
  const radio = picker.getByRole('radio')
  const row = picker.getByRole('row').filter({ has: page.getByRole('radio') })
  await expect(radio).toBeVisible()

  async function expectLayout(columns: number) {
    await expect(picker.getByRole('columnheader')).toHaveCount(columns)
    await expect(
      row.getByText(origin, { exact: true }).filter({ visible: true })
    ).toBeVisible()
    for (const label of [
      'completed',
      'WAL-G',
      '2.0 KB',
      'From another server',
    ]) {
      await expect(
        row.getByText(label, { exact: true }).filter({ visible: true })
      ).toHaveCount(1)
    }
    await expect(
      row.locator(`time[datetime="${created}"]:visible`)
    ).toHaveCount(1)
    expect(
      await picker.evaluate((el) =>
        [el, ...el.querySelectorAll('div, table, td')].every(
          (node) =>
            node.clientWidth === 0 || node.scrollWidth <= node.clientWidth + 1
        )
      )
    ).toBe(true)
  }
  await expectLayout(3)
  expect(
    await picker.evaluate((el) => el.getBoundingClientRect().right)
  ).toBeLessThanOrEqual(320)
  await radio.click()
  await expect(radio).toBeChecked()

  await page.setViewportSize({ width: 1600, height: 900 })
  await expectLayout(7)
  await picker.evaluate((el) => {
    el.style.maxWidth = '640px'
  })
  await expectLayout(4)
  await picker.evaluate((el) => {
    el.style.maxWidth = '320px'
  })
  await expectLayout(3)
  await expect(radio).toBeChecked()
})
