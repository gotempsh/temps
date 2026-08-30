// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

test.describe('AI-first conversation options', () => {
  test('opens the options menu and runs its conversation actions', async ({
    page,
    context,
  }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write'])
    await page.goto('/ai-first')

    const options = page.getByRole('button', { name: 'Conversation options' })
    await options.click()

    await expect(
      page.getByRole('menuitem', { name: 'Rename conversation' })
    ).toBeVisible()
    await expect(
      page.getByRole('menuitem', { name: 'Copy conversation link' })
    ).toBeVisible()
    await expect(
      page.getByRole('menuitem', { name: 'Reset prototype' })
    ).toBeVisible()

    await page.getByRole('menuitem', { name: 'Rename conversation' }).click()
    const name = page.getByLabel('Conversation name')
    await name.fill('Launch global commerce')
    await page.getByRole('button', { name: 'Save name' }).click()

    await expect(
      page.getByRole('heading', { name: 'Launch global commerce' })
    ).toBeVisible()
    await expect(page.getByRole('status')).toHaveText('Conversation renamed')

    await options.click()
    await page.getByRole('menuitem', { name: 'Copy conversation link' }).click()
    await expect(page.getByRole('status')).toHaveText(
      'Conversation link copied'
    )
    await expect
      .poll(() => page.evaluate(() => navigator.clipboard.readText()))
      .toContain('/ai-first')

    await options.click()
    await page.getByRole('menuitem', { name: 'Reset prototype' }).click()
    await expect(
      page.getByRole('heading', { name: 'Launch commerce suite' })
    ).toBeVisible()
    await expect(page.getByRole('status')).toHaveText('Prototype reset')
  })
})
