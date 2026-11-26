// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Page } from '@playwright/test'
import { expect, expectAppMounted, test } from '../fixtures'

/**
 * The top bar's "Ask" button opens the assistant (`/chat`) inside the console.
 * Whether it shows the chat or the setup is decided by the provider-status
 * capability, so
 * that is the only response these specs change: the real server answers it and
 * the gateway fields are overridden, keeping every other field the shape the
 * server actually sends.
 */
const SETUP_PATH = '/ai-gateway'

/**
 * The top bar's Ask button. The bar is a plain `header` inside `main`, which
 * has no banner role, so it is found as an element.
 */
const askButton = (page: Page) =>
  page.locator('header').getByRole('button', { name: 'Ask', exact: true })

const routeGatewayAvailability = async (
  page: Page,
  available: boolean,
  reason: string | null = null
) => {
  await page.route('**/ai/provider-status', async (route) => {
    const response = await route.fetch()
    const status = (await response.json()) as Record<string, unknown>
    await route.fulfill({
      response,
      json: {
        ...status,
        gateway_available: available,
        reason: available ? null : reason,
        // The path the server sends: provider settings, no vendor chosen.
        setup_path: available ? null : SETUP_PATH,
      },
    })
  })
}

const ONBOARDING_HEADING = 'Connect an AI provider'
const READY_HEADING = 'What should we get running?'

test.describe('AI entry and onboarding', () => {
  test('keeps AI discoverable and routes both setup choices', async ({
    page,
    consoleErrors,
  }) => {
    await routeGatewayAvailability(page, false, 'No AI provider is configured.')
    await page.goto('/projects')
    await expectAppMounted(page)

    const entry = askButton(page)
    await expect(entry).toBeVisible()
    await expect(entry).toHaveAttribute('aria-pressed', 'false')

    await entry.click()
    await expect(page).toHaveURL(/\/chat(?:[?#]|$)/)
    await expect(
      page.getByRole('heading', { name: ONBOARDING_HEADING })
    ).toBeVisible()
    await expect(page.getByText('No AI provider is configured.')).toBeVisible()
    await expect(page.getByLabel('OpenRouter API key')).toBeVisible()

    const settings = page.getByRole('link', {
      name: /Open AI provider settings/,
    })
    // Provider settings, with no vendor preselected.
    await expect(settings).toHaveAttribute('href', SETUP_PATH)
    await settings.click()
    await expect(page).toHaveURL(/\/ai-gateway$/)
    await expect(
      page.getByRole('heading', { name: 'AI Gateway', exact: true })
    ).toBeVisible()

    await page.goto('/chat')
    await page
      .getByRole('link', { name: /Use Temps Cloud managed AI instead/ })
      .click()
    await expect(page).toHaveURL(/\/settings\/cloud(?:[?#]|$)/)
    await expect(
      page.getByRole('heading', { name: 'Temps Cloud', exact: true })
    ).toBeVisible()

    expect(consoleErrors).toEqual([])
  })

  test('opens the chat when an AI provider is available', async ({
    page,
    consoleErrors,
  }) => {
    await routeGatewayAvailability(page, true)
    await page.goto('/projects')
    await expectAppMounted(page)

    await askButton(page).click()
    await expect(page).toHaveURL(/\/chat(?:[?#]|$)/)
    await expect(
      page.getByRole('heading', { name: READY_HEADING })
    ).toBeVisible()
    await expect(
      page.getByRole('heading', { name: ONBOARDING_HEADING })
    ).not.toBeVisible()
    expect(consoleErrors).toEqual([])
  })

  test('the assistant keeps the console around it, and Ask leads back', async ({
    page,
    consoleErrors,
  }) => {
    await routeGatewayAvailability(page, false, 'No AI provider is configured.')
    await page.goto('/chat')
    await expectAppMounted(page)
    await expect(
      page.getByRole('heading', { name: ONBOARDING_HEADING })
    ).toBeVisible()
    // The same sidebar as every other page, with Ask marked as the page shown.
    await expect(
      page.getByRole('link', { name: /^Ask/ }).first()
    ).toHaveAttribute('aria-current', 'page')

    const entry = askButton(page)
    await expect(entry).toHaveAttribute('aria-pressed', 'true')
    // A direct visit has nowhere to go back to, so it lands on a real page.
    await entry.click()
    await expect(page).toHaveURL(/\/projects(?:[?#]|$)/)
    await expect(entry).toHaveAttribute('aria-pressed', 'false')
    expect(consoleErrors).toEqual([])
  })
})
