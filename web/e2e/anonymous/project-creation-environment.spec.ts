// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

const managedVariables = [
  {
    name: 'SENTRY_DSN',
    source: 'error_tracking',
    is_secret: false,
    is_user_overridable: false,
    description: 'Sentry-compatible DSN scoped to the project environment.',
  },
  {
    name: 'SENTRY_RELEASE',
    source: 'error_tracking',
    is_secret: false,
    is_user_overridable: true,
    description: 'Deployment commit, tag, or image version when available.',
  },
  {
    name: 'OTEL_EXPORTER_OTLP_ENDPOINT',
    source: 'open_telemetry',
    is_secret: false,
    is_user_overridable: false,
    description: 'OpenTelemetry ingestion endpoint for this deployment.',
  },
  {
    name: 'TEMPS_API_TOKEN',
    source: 'temps',
    is_secret: true,
    is_user_overridable: false,
    description: 'Deployment-scoped token for authenticated Temps APIs.',
  },
]

test('renders backend-managed and selected-database variables without a false empty state', async ({
  page,
}, testInfo) => {
  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api/, '')
    let body: unknown = []

    if (path === '/user/me') {
      body = {
        id: 42,
        name: 'Project Owner',
        username: 'owner',
        email: 'owner@example.com',
        avatar_url: '',
        mfa_enabled: false,
        role: 'admin',
      }
    } else if (path === '/plugins' || path === '/x/plugins') {
      body = []
    } else if (path === '/git-connections') {
      body = { connections: [], page: 1, per_page: 20, total_count: 0 }
    } else if (path === '/git-providers') {
      body = []
    } else if (path === '/external-services') {
      await new Promise((resolve) => setTimeout(resolve, 400))
      body = [
        {
          id: 7,
          name: 'primary-postgres',
          service_type: 'postgres',
          status: 'running',
          topology: 'standalone',
          created_at: '2026-08-30T10:00:00Z',
          updated_at: '2026-08-30T10:00:00Z',
        },
      ]
    } else if (path === '/external-services/7/preview-environment-names') {
      body = ['POSTGRES_URL', 'POSTGRES_HOST']
    } else if (path === '/deployments/managed-environment-variables') {
      body = managedVariables
    }

    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(body),
    })
  })

  await page.goto('/projects/new?source=manual')
  await expect(page.getByRole('heading', { name: 'New Project' })).toBeVisible()
  await expect(page.getByLabel('Loading databases')).toBeVisible()
  await expect(page.getByText('primary-postgres')).toBeVisible()
  await expect(page.getByText('No databases configured yet')).toHaveCount(0)

  await page.getByRole('checkbox', { name: /primary-postgres/i }).click()
  await page
    .getByRole('button', {
      name: 'Show environment variables provided by Temps',
    })
    .click()
  await expect(page.getByText('SENTRY_DSN', { exact: true })).toBeVisible()
  await expect(page.getByText('POSTGRES_URL', { exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'Add Variable' }).click()
  await page.getByPlaceholder('DATABASE_URL').fill('SENTRY_DSN')
  await expect(page.getByRole('status')).toContainText(
    'Temps will override this value'
  )

  await page.screenshot({
    path: testInfo.outputPath('project-creation-environment.png'),
    fullPage: true,
  })
})
