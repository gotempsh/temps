// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect } from '../fixtures'
import type { PluginManifest } from '../../src/types/plugins'

const plugin: PluginManifest = {
  name: 'deployment-pulse',
  version: '0.1.1',
  display_name: 'Deployment Pulse',
  nav: [],
  requires_db: false,
  health_path: '/health',
}

// Only plugin APIs are mocked; the real console and authenticated session render
// the controls. These tests never install or stop binaries on a shared instance.
test.beforeEach(async ({ page }) => {
  await page.route('**/api/x/plugins/catalog', (route) =>
    route.fulfill({
      json: {
        available: true,
        source: 'https://registry.temps.sh',
        plugins: [],
      },
    })
  )
})

test('uninstall requires confirmation and automatically refreshes Running', async ({
  page,
  consoleErrors,
}) => {
  let installed = true
  let uninstallRequests = 0
  await page.route('**/api/x/plugins', (route) =>
    route.fulfill({ json: installed ? [plugin] : [] })
  )
  await page.route(
    '**/api/x/plugins/deployment-pulse/uninstall',
    async (route) => {
      uninstallRequests++
      installed = false
      await route.fulfill({
        json: {
          name: plugin.name,
          message: 'Plugin uninstalled; data preserved.',
        },
      })
    }
  )
  await page.goto('/settings/plugins')
  await page.getByRole('button', { name: 'Uninstall', exact: true }).click()
  const dialog = page.getByRole('alertdialog')
  await expect(dialog).toContainText('data is preserved')
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
  await expect(dialog).toBeHidden()
  expect(uninstallRequests).toBe(0)
  await page.getByRole('button', { name: 'Uninstall', exact: true }).click()
  await dialog.getByRole('button', { name: 'Uninstall', exact: true }).click()
  await expect(dialog).toBeHidden()
  await expect(
    page.getByText('No verified plugins are running.', { exact: true })
  ).toBeVisible()
  expect(uninstallRequests).toBe(1)
  expect(consoleErrors).toEqual([])
})

for (const status of [207, 502]) {
  test(`reload ${status} shows plugin and registry failure reasons, refreshes Running, and clears on recovery`, async ({
    page,
    consoleErrors,
  }, testInfo) => {
    let installed = true
    let reloads = 0
    await page.route('**/api/x/plugins', (route) =>
      route.fulfill({
        json: installed
          ? [plugin]
          : status === 207 && reloads === 1
            ? [
                {
                  ...plugin,
                  name: 'healthy-plugin',
                  display_name: 'Healthy Plugin',
                },
              ]
            : [],
      })
    )
    await page.route('**/api/x/plugins/reload', async (route) => {
      reloads++
      installed = false
      await route.fulfill({
        status: reloads === 1 ? status : 200,
        json: {
          loaded: status === 207 && reloads === 1 ? 1 : 0,
          plugins: status === 207 && reloads === 1 ? ['healthy-plugin'] : [],
          message:
            reloads === 1 ? 'Reload failed for 2 plugins.' : 'Reload complete.',
          failures:
            reloads === 1
              ? [
                  {
                    plugin: plugin.name,
                    reason: 'Signing key has been revoked.',
                  },
                  { plugin: null, reason: 'Unable to read plugin directory.' },
                ]
              : [],
        },
      })
    })
    await page.goto('/settings/plugins')
    await expect(
      page.getByRole('button', { name: 'Uninstall', exact: true })
    ).toBeVisible()
    await page
      .getByRole('button', { name: 'Reload Plugins', exact: true })
      .click()
    const alert = page
      .getByRole('alert')
      .filter({ hasText: 'Some plugins could not be reloaded' })
    await expect(alert).toContainText(
      'deployment-pulse: Signing key has been revoked.'
    )
    await expect(alert).toContainText(
      'Plugin registry: Unable to read plugin directory.'
    )
    if (status === 502) {
      await expect(
        page.getByText('No verified plugins are running.', { exact: true })
      ).toBeVisible()
    } else {
      await expect(
        page.getByText('Healthy Plugin', { exact: true })
      ).toBeVisible()
      await expect(
        page.getByText('Deployment Pulse', { exact: true })
      ).toBeHidden()
    }
    await testInfo.attach(`reload-${status}`, {
      body: await page.screenshot(),
      contentType: 'image/png',
    })
    await page
      .getByRole('button', { name: 'Reload Plugins', exact: true })
      .click()
    await expect(alert).toBeHidden()
    expect(consoleErrors).toEqual([])
  })
}
