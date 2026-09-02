// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Page } from '@playwright/test'
import { expect, expectAppMounted, test } from '../fixtures'

const routeProviders = async (page: Page, body: unknown[]) => {
  await page.route('**/ai/providers', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(body),
    })
  })
}

const routeCloudStatus = async (page: Page, linked: boolean) => {
  await page.route('**/cloud/capability', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        configured: true,
        reason: null,
        setup_path: '/settings/cloud',
      }),
    })
  })
  await page.route('**/cloud/status', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        status: linked ? 'linked' : 'not_configured',
        status_message: linked ? 'Connected' : 'Not linked',
        health: linked ? 'healthy' : 'not_linked',
        health_message: linked ? 'Signals reach Cloud' : 'Not linked',
        instance_id: linked ? '11111111-2222-3333-4444-555555555555' : null,
        account_email: linked ? 'operator@example.com' : null,
        spooled_spans: 0,
        backend_url: 'http://127.0.0.1:19202/',
        telemetry_enabled: false,
        backups_enabled: false,
        notifications_enabled: false,
      }),
    })
  })
}

const routeCloudAiCapability = async (
  page: Page,
  configured: boolean,
  reason: string | null = null
) => {
  await page.route('**/cloud/ai/capability', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        configured,
        reason,
        setup_path: '/settings/cloud',
        model: configured ? 'managed-model' : null,
      }),
    })
  })
}

test.describe('AI entry and Cloud onboarding', () => {
  test('keeps AI discoverable and routes both setup choices', async ({
    page,
    consoleErrors,
  }) => {
    await routeProviders(page, [])
    await routeCloudStatus(page, false)
    await routeCloudAiCapability(
      page,
      false,
      'Link this instance to use managed AI.'
    )
    await page.goto('/projects')
    await expectAppMounted(page)

    const entry = page.getByRole('button', {
      name: 'AI assistant',
      exact: true,
    })
    await expect(entry).toBeVisible()
    await expect(entry).toHaveAttribute('aria-expanded', 'false')

    await entry.click()
    await expect(entry).toHaveAttribute('aria-expanded', 'true')
    await expect(
      page.getByRole('heading', {
        name: 'Ask your stack. Keep the evidence attached.',
      })
    ).toBeVisible()
    await expect(page.getByText('Managed AI when configured')).toBeVisible()
    await expect(
      page.getByText(/does not enable AI usage or sell credits/)
    ).toBeVisible()
    await expect(page.getByText('Cited, read-only answers')).toBeVisible()

    await page.getByRole('link', { name: 'Review managed AI setup' }).click()
    await expect(page).toHaveURL(/\/settings\/cloud(?:[?#]|$)/)
    await expect(
      page.getByRole('heading', { name: 'Temps Cloud', exact: true })
    ).toBeVisible()
    await expect(entry).toHaveAttribute('aria-expanded', 'false')

    await page.goto('/projects')
    await entry.click()
    await page.getByRole('link', { name: 'Use my own AI provider' }).click()
    await expect(page).toHaveURL(/\/ai-gateway(?:[?#]|$)/)
    await expect(
      page.getByRole('heading', { name: 'AI Gateway', exact: true })
    ).toBeVisible()

    expect(consoleErrors).toEqual([])
  })

  test('opens the existing assistant when a local provider is configured', async ({
    page,
    consoleErrors,
  }) => {
    await routeProviders(page, [
      {
        api_key_masked: 'sk-…test',
        base_url: null,
        created_at: '2026-08-05T00:00:00Z',
        default_model: 'claude-sonnet-4-5',
        display_name: 'Anthropic',
        id: 1,
        is_active: true,
        provider: 'anthropic',
        updated_at: '2026-08-05T00:00:00Z',
      },
    ])
    await routeCloudStatus(page, false)
    await routeCloudAiCapability(page, false, 'Managed AI is not enabled.')
    await page.goto('/projects')
    await expectAppMounted(page)

    const entry = page.getByRole('button', {
      name: 'AI assistant',
      exact: true,
    })
    await entry.click()

    await expect(
      page.getByRole('heading', { name: 'AI assistant', exact: true })
    ).toBeVisible()
    await expect(
      page.getByRole('heading', {
        name: 'Ask your stack. Keep the evidence attached.',
      })
    ).not.toBeVisible()
    expect(consoleErrors).toEqual([])
  })

  test('opens the existing assistant when managed AI is available', async ({
    page,
    consoleErrors,
  }) => {
    await routeProviders(page, [])
    await routeCloudStatus(page, true)
    await routeCloudAiCapability(page, true)
    await page.goto('/projects')
    await expectAppMounted(page)

    await page
      .getByRole('button', { name: 'AI assistant', exact: true })
      .click()
    await expect(
      page.getByRole('heading', { name: 'AI assistant', exact: true })
    ).toBeVisible()
    await expect(
      page.getByRole('heading', {
        name: 'Ask your stack. Keep the evidence attached.',
      })
    ).not.toBeVisible()
    expect(consoleErrors).toEqual([])
  })

  test('does not treat a linked but unentitled instance as managed AI', async ({
    page,
    consoleErrors,
  }) => {
    await routeProviders(page, [])
    await routeCloudStatus(page, true)
    await routeCloudAiCapability(
      page,
      false,
      'Your Cloud plan does not include managed AI.'
    )
    await page.goto('/projects')
    await expectAppMounted(page)

    await page
      .getByRole('button', { name: 'AI assistant', exact: true })
      .click()
    await expect(
      page.getByText('Your Cloud plan does not include managed AI.')
    ).toBeVisible()
    await expect(
      page.getByRole('heading', { name: 'AI assistant', exact: true })
    ).not.toBeVisible()
    expect(consoleErrors).toEqual([])
  })

  test('renders an actionable error when AI capability checks fail', async ({
    page,
    consoleErrors,
  }) => {
    await routeProviders(page, [])
    await routeCloudStatus(page, true)
    let available = false
    await page.route('**/cloud/ai/capability', async (route) => {
      await route.fulfill({
        status: available ? 200 : 503,
        contentType: 'application/json',
        body: JSON.stringify(
          available
            ? {
                configured: false,
                reason: 'Managed AI is not enabled.',
                setup_path: '/settings/cloud',
                model: null,
              }
            : {
                title: 'Managed AI unavailable',
                status: 503,
                detail: 'The managed capability check timed out.',
              }
        ),
      })
    })
    await page.goto('/projects')
    await expectAppMounted(page)

    await page
      .getByRole('button', { name: 'AI assistant', exact: true })
      .click()
    await expect(page.getByText('AI availability is unknown')).toBeVisible()
    await expect(
      page.getByText('The managed capability check timed out.')
    ).toBeVisible()

    available = true
    await page.getByRole('button', { name: 'Check again' }).click()
    await expect(page.getByText('Managed AI is not enabled.')).toBeVisible()
    expect(consoleErrors).toEqual([])
  })
})
