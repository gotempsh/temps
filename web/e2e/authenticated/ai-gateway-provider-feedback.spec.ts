// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Page } from '@playwright/test'
import type { ProviderKeyResponse } from '../../src/api/client'
import { expect, test } from '../fixtures'

const displayName = 'Gateway fixture'
const apiKey = 'fixture-api-key'
const baseUrl = 'https://inference.example.com/v1'
const rejection =
  'API key verification failed. Check the key and custom base URL, then retry.'

async function openProviderDialog(page: Page) {
  await page.goto('/ai-gateway')
  await page
    .getByRole('row')
    .filter({ has: page.getByText('OpenAI', { exact: true }) })
    .getByRole('button', { name: 'Configure', exact: true })
    .click()
  const dialog = page.getByRole('dialog', { name: 'Configure OpenAI' })
  await dialog.getByLabel('Display Name', { exact: true }).fill(displayName)
  await dialog.getByLabel('API Key', { exact: true }).fill(apiKey)
  await dialog.getByLabel('Custom Base URL').fill(baseUrl)
  return dialog
}

test('a rejected gateway key keeps its inputs and can be corrected and retried', async ({
  page,
  consoleErrors,
}) => {
  let attempts = 0
  let keys: ProviderKeyResponse[] = []
  await page.route('**/api/ai/providers', async (route) => {
    if (route.request().method() === 'GET') {
      await route.fulfill({ json: keys })
      return
    }
    expect(route.request().method()).toBe('POST')
    attempts += 1
    if (attempts === 1) {
      await route.fulfill({
        status: 400,
        contentType: 'application/problem+json',
        json: { title: 'Validation error', detail: rejection },
      })
      return
    }
    expect(route.request().postDataJSON()).toEqual({
      provider: 'openai',
      display_name: displayName,
      api_key: 'corrected-fixture-key',
      base_url: baseUrl,
    })
    keys = [
      {
        id: 1,
        provider: 'openai',
        display_name: displayName,
        api_key_masked: '****-key',
        base_url: baseUrl,
        is_active: true,
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
      },
    ]
    await route.fulfill({ status: 201, json: keys[0] })
  })

  const dialog = await openProviderDialog(page)
  await dialog.getByRole('button', { name: 'Add key', exact: true }).click()
  await expect(dialog).toBeVisible()
  await expect(dialog.getByRole('alert')).toHaveText(rejection)
  await expect(dialog.getByLabel('Display Name', { exact: true })).toHaveValue(
    displayName
  )
  await expect(dialog.getByLabel('API Key', { exact: true })).toHaveValue(
    apiKey
  )
  await expect(dialog.getByLabel('Custom Base URL')).toHaveValue(baseUrl)
  await expect(
    page.getByText('Provider key added', { exact: true })
  ).toHaveCount(0)
  await expect(dialog.getByRole('button', { name: 'Add key' })).toBeEnabled()
  expect(keys).toEqual([])

  await dialog
    .getByLabel('API Key', { exact: true })
    .fill('corrected-fixture-key')
  await dialog.getByRole('button', { name: 'Add key', exact: true }).click()
  await expect(dialog).toBeHidden()
  await expect(
    page.getByText('Provider key added', { exact: true })
  ).toBeVisible()
  const providerRow = page
    .getByRole('row')
    .filter({ has: page.getByText('OpenAI', { exact: true }) })
  await expect(providerRow.getByText('Active', { exact: true })).toBeVisible()
  await providerRow.click()
  await expect(page.getByText(displayName, { exact: true })).toBeVisible()
  expect(attempts).toBe(2)
  expect(consoleErrors).toEqual([])
})

test('closing a failed gateway setup clears its error for the next attempt', async ({
  page,
  consoleErrors,
}) => {
  await page.route('**/api/ai/providers', (route) =>
    route.request().method() === 'GET'
      ? route.fulfill({ json: [] })
      : route.fulfill({
          status: 400,
          json: { title: 'Validation error', detail: rejection },
        })
  )
  const dialog = await openProviderDialog(page)
  await dialog.getByRole('button', { name: 'Add key', exact: true }).click()
  await expect(dialog.getByRole('alert')).toHaveText(rejection)
  await dialog.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(dialog).toBeHidden()
  await page
    .getByRole('row')
    .filter({ has: page.getByText('OpenAI', { exact: true }) })
    .getByRole('button', { name: 'Configure', exact: true })
    .click()
  await expect(dialog).toBeVisible()
  await expect(dialog.getByRole('alert')).toHaveCount(0)
  await expect(dialog.getByLabel('Display Name', { exact: true })).toHaveValue(
    'OpenAI'
  )
  await expect(dialog.getByLabel('API Key', { exact: true })).toHaveValue('')
  await expect(dialog.getByLabel('Custom Base URL')).toHaveValue('')
  expect(consoleErrors).toEqual([])
})

test('a gateway server failure without problem details gives a retry message', async ({
  page,
  consoleErrors,
}) => {
  await page.route('**/api/ai/providers', (route) =>
    route.request().method() === 'GET'
      ? route.fulfill({ json: [] })
      : route.fulfill({ status: 500, body: '' })
  )
  const dialog = await openProviderDialog(page)
  await dialog.getByRole('button', { name: 'Add key', exact: true }).click()
  await expect(dialog.getByRole('alert')).toHaveText(
    'Could not add the provider key. Please try again.'
  )
  await expect(dialog.getByLabel('API Key', { exact: true })).toHaveValue(
    apiKey
  )
  await expect(dialog.getByRole('button', { name: 'Add key' })).toBeEnabled()
  await expect(
    page.getByText('Provider key added', { exact: true })
  ).toHaveCount(0)
  expect(consoleErrors).toEqual([])
})

for (const outcome of ['saved', 'rejected'] as const) {
  test(`a pending gateway save cannot be dismissed before it is ${outcome}`, async ({
    page,
    consoleErrors,
  }) => {
    let finishSave!: () => void
    const pendingResponse = new Promise<void>((resolve) => {
      finishSave = resolve
    })
    let attempts = 0
    let keys: ProviderKeyResponse[] = []
    await page.route('**/api/ai/providers', async (route) => {
      if (route.request().method() === 'GET') {
        await route.fulfill({ json: keys })
        return
      }
      expect(route.request().method()).toBe('POST')
      attempts += 1
      await pendingResponse
      if (outcome === 'rejected') {
        await route.fulfill({
          status: 400,
          json: { title: 'Validation error', detail: rejection },
        })
        return
      }
      keys = [
        {
          id: 1,
          provider: 'openai',
          display_name: displayName,
          api_key_masked: '****-key',
          base_url: baseUrl,
          is_active: true,
          created_at: '2026-01-01T00:00:00Z',
          updated_at: '2026-01-01T00:00:00Z',
        },
      ]
      await route.fulfill({ status: 201, json: keys[0] })
    })

    const dialog = await openProviderDialog(page)
    const saveResponse = page.waitForResponse(
      (response) =>
        response.url().endsWith('/api/ai/providers') &&
        response.request().method() === 'POST'
    )
    await dialog.getByRole('button', { name: 'Add key', exact: true }).click()
    try {
      await expect.poll(() => attempts).toBe(1)
      const pendingButton = dialog.getByRole('button', {
        name: 'Verifying & saving…',
        exact: true,
      })
      await expect(pendingButton).toBeDisabled()
      await dialog.getByRole('button', { name: 'Close', exact: true }).click()
      await expect(dialog).toBeVisible({ timeout: 2_000 })
      await page.keyboard.press('Escape')
      await expect(dialog).toBeVisible()
      await page.mouse.click(5, 5)
      await expect(dialog).toBeVisible()
      await expect(pendingButton).toBeDisabled()
      await expect(
        dialog.getByLabel('Display Name', { exact: true })
      ).toHaveValue(displayName)
      await expect(dialog.getByLabel('API Key', { exact: true })).toHaveValue(
        apiKey
      )
      await expect(dialog.getByLabel('Custom Base URL')).toHaveValue(baseUrl)
      await expect(
        dialog.getByLabel('Display Name', { exact: true })
      ).toBeDisabled()
      await expect(dialog.getByLabel('API Key', { exact: true })).toBeDisabled()
      await expect(dialog.getByLabel('Custom Base URL')).toBeDisabled()

      finishSave()
      expect((await saveResponse).status()).toBe(
        outcome === 'saved' ? 201 : 400
      )
      if (outcome === 'saved') {
        await expect(dialog).toBeHidden()
        await expect(
          page.getByText('Provider key added', { exact: true })
        ).toBeVisible()
      } else {
        await expect(dialog.getByRole('alert')).toHaveText(rejection)
        await dialog.getByRole('button', { name: 'Close', exact: true }).click()
        await expect(dialog).toBeHidden()
      }
      await page
        .getByRole('row')
        .filter({ has: page.getByText('OpenAI', { exact: true }) })
        .getByRole('button', {
          name: outcome === 'saved' ? 'Add key' : 'Configure',
          exact: true,
        })
        .click()
      await expect(dialog).toBeVisible()
      await expect(dialog.getByRole('alert')).toHaveCount(0)
      await expect(dialog.getByLabel('API Key', { exact: true })).toHaveValue(
        ''
      )
      await dialog
        .getByLabel('Display Name', { exact: true })
        .fill('Next setup')
      await expect(
        dialog.getByLabel('Display Name', { exact: true })
      ).toHaveValue('Next setup')
      await expect(
        dialog.getByRole('button', { name: 'Add key' })
      ).toBeEnabled()
      expect(attempts).toBe(1)
      expect(consoleErrors).toEqual([])
    } finally {
      finishSave()
      await saveResponse
    }
  })
}
