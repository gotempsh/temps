// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '../fixtures'

for (const scenario of ['legacy', 'unavailable', 'stateless'] as const) {
  test(`source upload controls honor ${scenario} capability discovery`, async ({
    page,
  }) => {
    await page.route('**/api/platform/features', async (route) => {
      if (scenario === 'unavailable') {
        await route.fulfill({
          status: 503,
          json: { detail: 'Temporarily unavailable' },
        })
      } else {
        await route.fulfill({
          json:
            scenario === 'stateless'
              ? {
                  profile: 'control-plane',
                  stateless: true,
                  persistent_workspaces: false,
                }
              : { profile: 'full' },
        })
      }
    })
    const capabilities = page.waitForResponse('**/api/platform/features')
    await page.goto('/drop')
    await capabilities
    const chooseFile = page.getByRole('button', {
      name: 'Choose file',
      exact: true,
    })
    const chooseFolder = page.getByRole('button', {
      name: 'Choose folder',
      exact: true,
    })
    const restriction = page.getByText('File uploads need persistent storage', {
      exact: true,
    })
    if (scenario === 'stateless') {
      await expect(chooseFile).toBeDisabled()
      await expect(chooseFolder).toBeDisabled()
      await expect(restriction).toBeVisible()
    } else {
      await expect(chooseFile).toBeEnabled()
      await expect(chooseFolder).toBeEnabled()
      await expect(restriction).toHaveCount(0)
    }
  })
}
