// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '../fixtures'

test.describe('user creation', () => {
  test('retries a failed team assignment without creating the account twice', async ({
    page,
  }) => {
    let createRequests = 0
    let teamRequests = 0
    const now = new Date().toISOString()

    await page.route(/\/api\/teams(?:\?.*)?$/, async (route) => {
      if (route.request().method() !== 'GET') return route.continue()
      await route.fulfill({
        json: {
          teams: [
            {
              id: 99,
              name: 'Platform',
              slug: 'platform',
              description: null,
              created_by: 1,
              created_at: now,
              updated_at: now,
            },
          ],
          page: 1,
          page_size: 100,
          total: 1,
        },
      })
    })
    await page.route('**/api/users', async (route) => {
      if (route.request().method() !== 'POST') return route.continue()
      createRequests += 1
      await route.fulfill({
        status: 201,
        json: {
          user: {
            id: 501,
            name: 'Alex Morgan',
            email: 'alex@example.com',
            image: '',
            mfa_enabled: false,
            email_verified: false,
            must_change_password: true,
            deleted_at: null,
            created_at: Date.now(),
            updated_at: Date.now(),
          },
          roles: [
            {
              id: 2,
              name: 'user',
              created_at: Date.now(),
              updated_at: Date.now(),
            },
          ],
        },
      })
    })
    await page.route('**/api/teams/99/members', async (route) => {
      teamRequests += 1
      if (teamRequests === 1) {
        await route.fulfill({
          status: 500,
          json: { detail: 'temporary failure' },
        })
        return
      }
      await route.fulfill({
        status: 201,
        json: {
          id: 1,
          team_id: 99,
          user_id: 501,
          role: 'viewer',
          added_by: 1,
          user_name: 'Alex Morgan',
          user_email: 'alex@example.com',
          created_at: now,
          updated_at: now,
        },
      })
    })

    await page.goto('/settings/users/new')
    await page.getByLabel('Name').fill('Alex Morgan')
    await page.getByLabel('Email').fill('alex@example.com')
    await page
      .getByRole('button', { name: 'Generate a secure temporary password' })
      .click()
    await page.getByLabel('Team', { exact: true }).click()
    await page.getByRole('option', { name: 'Platform' }).click()
    await page.getByRole('button', { name: 'Create user' }).click()

    await expect(
      page.getByText('Account created; team assignment pending')
    ).toBeVisible()
    expect(createRequests).toBe(1)
    expect(teamRequests).toBe(1)

    await page.getByRole('button', { name: 'Retry team assignment' }).click()
    await page.waitForURL(/\/settings\/users$/)
    expect(createRequests).toBe(1)
    expect(teamRequests).toBe(2)
  })
})
