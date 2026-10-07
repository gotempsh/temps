// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

const provider = {
  id: 7,
  name: 'Example GitHub App',
  provider_type: 'github',
  auth_method: 'github_app',
  base_url: 'https://github.com/apps/example-app',
  is_active: true,
  is_default: false,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

const installation = {
  id: 12,
  provider_id: provider.id,
  installation_id: '456',
  account_name: 'New example account',
  account_type: 'Organization',
  consecutive_health_failures: 0,
  has_authenticated_credentials: true,
  health_status: 'healthy',
  is_active: true,
  is_expired: false,
  synced_repository_count: 0,
  syncing: false,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

async function mockGitApi(page: Page, alreadyInstalled = false) {
  let webhookReceived = false
  let connectionRequests = 0
  const previous = {
    ...installation,
    id: 11,
    installation_id: '123',
    account_name: 'Existing example account',
  }
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname.replace(/^\/api/, '')
    let body: unknown = []
    const connections = [
      ...(alreadyInstalled ? [previous] : []),
      ...(webhookReceived ? [installation] : []),
    ]
    if (path === '/user/me') {
      body = {
        id: 42,
        name: 'Example owner',
        username: 'owner',
        email: 'owner@example.com',
        avatar_url: '',
        mfa_enabled: false,
        role: 'admin',
      }
    } else if (path === '/git-providers') {
      body = [provider]
    } else if (path === `/git-providers/${provider.id}`) {
      body = provider
    } else if (path === `/git-providers/${provider.id}/connections`) {
      connectionRequests += 1
      body = connections
    } else if (path === '/git-connections') {
      connectionRequests += 1
      body = {
        connections,
        page: 1,
        per_page: 100,
        total_count: connections.length,
      }
    } else if (path === '/projects') {
      body = { projects: [], total: 0, page: 1, per_page: 20 }
    } else if (path === '/domains') {
      body = { domains: [], total: 0, page: 1, page_size: 10 }
    } else if (path.endsWith('/repositories')) {
      body = { repositories: [], total_count: 0, page: 1, per_page: 5 }
    }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(body),
    })
  })
  // Keep the installation action local; GitHub's eventual webhook is simulated
  // by changing API responses after the console has begun waiting.
  await page.addInitScript(() => {
    window.open = () => null
  })
  return {
    deliverWebhook: () => {
      webhookReceived = true
    },
    requestCount: () => connectionRequests,
  }
}

for (const alreadyInstalled of [false, true]) {
  test(`provider detail discovers a delayed installation with ${alreadyInstalled ? 'an existing' : 'no'} account`, async ({
    page,
  }) => {
    const api = await mockGitApi(page, alreadyInstalled)
    await page.goto(
      `/git-providers/${provider.id}?installation_id=456&github_installation_processing=true`
    )
    await expect(
      page.getByRole('heading', { name: provider.name })
    ).toBeVisible()
    await expect.poll(api.requestCount).toBeGreaterThanOrEqual(2)
    await expect(
      page.getByText(installation.account_name, { exact: true })
    ).toHaveCount(0)
    const requestsBeforeWebhook = api.requestCount()
    api.deliverWebhook()
    await expect(
      page.getByText(installation.account_name, { exact: true })
    ).toBeVisible({ timeout: 3500 })
    expect(api.requestCount()).toBeGreaterThan(requestsBeforeWebhook)
  })
}

test('dashboard callback discovers an installation after an empty poll', async ({
  page,
}) => {
  const api = await mockGitApi(page)
  await page.goto('/dashboard')
  await expect(
    page.getByRole('region', { name: 'Deploy from a repository', exact: true })
  ).toBeVisible()
  await expect.poll(api.requestCount).toBeGreaterThanOrEqual(2)
  api.deliverWebhook()
  await expect(
    page.getByRole('combobox', { name: 'Git account' })
  ).toContainText(installation.account_name, { timeout: 3500 })
})

test('existing-app setup waits for a new connection and keeps polling beyond one minute', async ({
  page,
}) => {
  const api = await mockGitApi(page, true)
  await page.clock.install()
  await page.goto('/git-providers/add')
  await page.getByRole('button', { name: 'Select GitHub', exact: true }).click()
  await page.getByRole('button', { name: /Install Existing App/ }).click()
  await expect(
    page.getByText('Waiting for GitHub installation', { exact: true })
  ).toBeVisible()
  await expect(
    page.getByText('Provider Added Successfully!', { exact: true })
  ).toHaveCount(0)
  await page.clock.fastForward(65_000)
  await expect(
    page.getByText('Waiting for GitHub installation', { exact: true })
  ).toBeVisible()
  const requestsBeforeWebhook = api.requestCount()
  api.deliverWebhook()
  await page.clock.fastForward(2000)
  await expect(
    page.getByText('Provider Added Successfully!', { exact: true })
  ).toBeVisible()
  expect(api.requestCount()).toBeGreaterThan(requestsBeforeWebhook)
})

test('installing from the provider list returns to its live connections page', async ({
  page,
}) => {
  const api = await mockGitApi(page)
  await page.goto('/git-providers')
  await page
    .getByRole('button', { name: 'GitHub Install GitHub App', exact: true })
    .click()
  await expect(page).toHaveURL(`/git-providers/${provider.id}`)
  await expect.poll(api.requestCount).toBeGreaterThanOrEqual(2)
  api.deliverWebhook()
  await expect(
    page.getByText(installation.account_name, { exact: true })
  ).toBeVisible({ timeout: 3500 })
})
