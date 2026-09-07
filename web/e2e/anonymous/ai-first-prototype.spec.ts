// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

test.describe('AI-first workspace access', () => {
  test('keeps the application workspace behind authentication', async ({
    page,
  }) => {
    await page.goto('/ai-first')

    await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible()
    await expect(
      page.getByRole('button', { name: 'Create application' })
    ).toHaveCount(0)
  })
})
