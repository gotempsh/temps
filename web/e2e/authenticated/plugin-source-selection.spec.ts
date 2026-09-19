// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { test, expect } from '../fixtures'

// Real console/session with mocked plugin endpoints; no native plugin is installed.
test('changing a plugin ref clears the override and keeps its directory', async ({
  page,
  consoleErrors,
}) => {
  let ref = 'release/v1'
  let revision = 'a'.repeat(40)
  const updates: Array<{ ref_name?: string }> = []
  await page.route('**/api/x/plugins', (route) =>
    route.fulfill({
      json: [
        {
          name: 'demo',
          version: '1.0.0',
          display_name: 'Nested fixture',
          nav: [],
          requires_db: false,
          health_path: '/health',
        },
      ],
    })
  )
  await page.route('**/api/x/plugins/demo/status', (route) =>
    route.fulfill({
      json: {
        configured: true,
        setup_path: '/settings/plugins',
        source: {
          kind: 'github',
          repository_url: 'https://github.com/example/plugins',
          path: 'plugins/demo',
          ref_name: ref,
          commit: revision,
          version: '1.0.0',
          builder_image: 'fixture',
        },
      },
    })
  )
  await page.route('**/api/x/plugins/demo/update', (route) => {
    const body = route.request().postDataJSON() as { ref_name?: string }
    updates.push(body)
    ref = body.ref_name || ref
    revision = 'b'.repeat(40)
    return route.fulfill({
      json: {
        name: 'demo',
        version: '1.0.0',
        source_commit: revision,
        message: 'Fixture update complete',
        platform: 'darwin-arm64',
        sha256: 'a'.repeat(64),
      },
    })
  })
  await page.goto('/settings/plugins')
  const field = page.getByLabel('Update branch, tag, or commit')
  await field.fill('release/v2')
  await page.getByRole('button', { name: 'Update from GitHub' }).click()
  await expect(field).toHaveValue('')
  await expect(field).toHaveAttribute('placeholder', 'Keep release/v2')
  await page.getByRole('button', { name: 'Update from GitHub' }).click()
  await expect.poll(() => updates).toEqual([{ ref_name: 'release/v2' }, {}])
  await expect(page.getByText('plugins/demo ·', { exact: false })).toBeVisible()
  await page.setViewportSize({ width: 390, height: 844 })
  await field.scrollIntoViewIfNeeded()
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth)
  ).toBeLessThanOrEqual(390)
  expect(consoleErrors).toEqual([])
})
