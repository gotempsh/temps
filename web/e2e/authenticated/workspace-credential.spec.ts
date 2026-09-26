// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect } from '@playwright/test'
import type { ProviderCatalogDto } from '../../src/api/client'

test('OpenCode imports unverified once and verifies a chosen model without re-entering credentials', async ({
  page,
}) => {
  const provider = {
    id: 'opencode',
    name: 'OpenCode',
    install_command: 'install',
    auth_command: 'login',
    auth_flavors: [
      {
        id: 'config_file',
        label: 'Auth file',
        description: 'Private auth file',
        format: 'config_file',
      },
    ],
    models: ['openai/test-model'],
    runtime_models: [],
    permission_modes: [],
    default_permission_mode_id: 'auto',
    credential_saved: false,
    credential_verification_status: 'not_saved',
    host_authenticated: true,
    model_source: 'bootstrap',
    supports_max_turns: false,
    workspace_ready: false,
    local_credential: {
      auth_type: 'config_file',
      source: 'host_auth_store',
      label: 'Host CLI',
    },
  }
  await page.route('**/api/settings/ai-providers', (route) =>
    route.fulfill({
      json: { default_provider: 'opencode', providers: [provider] },
    })
  )
  let imports = 0
  await page.route(
    '**/api/settings/ai-providers/opencode/credential/import-local*',
    (route) => {
      imports++
      return route.fulfill({
        json: {
          saved: true,
          auth_type: 'config_file',
          workspace_ready: false,
          credential_verification_status: 'unverified',
          provider: {
            ...provider,
            credential_saved: true,
            credential_verification_status: 'unverified',
          },
        },
      })
    }
  )
  let attempts = 0
  await page.route(
    '**/api/settings/ai-providers/opencode/credential/verify-saved',
    (route) => {
      expect(route.request().postDataJSON()).toEqual({
        verification_model: 'openai/test-model',
      })
      attempts++
      if (attempts === 1)
        return route.fulfill({
          status: 400,
          json: { detail: 'The provider rejected this model request.' },
        })
      return route.fulfill({
        json: {
          saved: true,
          credential_verification_status: 'verified',
          provider: {
            ...provider,
            credential_saved: true,
            credential_verification_status: 'verified',
            workspace_ready: true,
            default_model: 'openai/test-model',
          },
        },
      })
    }
  )
  await page.goto('/ai-first?setup=workspace&setupStep=1&setupHarness=opencode')
  await page
    .getByRole('button', { name: 'Use local login', exact: true })
    .click()
  await expect(
    page.getByRole('button', { name: 'Continue', exact: true })
  ).toBeDisabled()
  await expect(
    page.getByRole('status').filter({ hasText: 'not verified' })
  ).toBeVisible()
  await expect(
    page.getByText('Credential verified and saved.', { exact: true })
  ).toHaveCount(0)
  await page
    .getByLabel('Model to verify', { exact: true })
    .fill('openai/test-model')
  await page
    .getByRole('button', { name: 'Verify saved login', exact: true })
    .click()
  await expect(
    page.getByRole('alert').filter({ hasText: 'provider rejected' })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Continue', exact: true })
  ).toBeDisabled()
  await page
    .getByRole('button', { name: 'Verify saved login', exact: true })
    .click()
  await expect(
    page.getByRole('button', { name: 'Continue', exact: true })
  ).toBeEnabled()
  expect(imports).toBe(1)
  expect(attempts).toBe(2)
})

for (const shortcut of ['Meta+Enter', 'Control+Enter']) {
  test(`${shortcut} creates an unnamed workspace once; plain Enter does not`, async ({
    page,
  }) => {
    await page.route('**/api/settings/ai-providers', (route) =>
      route.fulfill({
        json: {
          default_provider: 'claude_cli',
          providers: [
            {
              id: 'claude_cli',
              name: 'Claude Code',
              workspace_ready: true,
              credential_saved: true,
              auth_flavors: [],
              runtime_models: [],
              models: [],
              permission_modes: [{ id: 'auto', name: 'Auto' }],
              model_source: 'bootstrap',
            },
          ],
        },
      })
    )
    let requests = 0
    let submittedName: string | undefined
    let finish: () => void = () => {}
    const pending = new Promise<void>((resolve) => {
      finish = resolve
    })
    await page.route('**/api/ai/applications', async (route) => {
      if (route.request().method() !== 'POST') return route.continue()
      requests += 1
      submittedName = route.request().postDataJSON().name
      await pending
      await route.fulfill({
        status: 400,
        json: { detail: 'Controlled test failure; no workspace created.' },
      })
    })
    await page.goto(
      '/ai-first?setup=workspace&setupStep=4&setupHarness=claude_cli'
    )
    const prompt = page.getByRole('textbox', { name: 'Your first task' })
    await prompt.fill('')
    await prompt.press(shortcut)
    expect(requests).toBe(0)
    await prompt.fill('Build a test page')
    await prompt.press('Enter')
    expect(requests).toBe(0)
    await expect(prompt).toHaveValue('Build a test page\n')
    await prompt.press(shortcut)
    await expect.poll(() => requests).toBe(1)
    expect(submittedName).toBe('Untitled workspace')
    await expect(
      page.getByRole('button', { name: 'Send and create workspace' })
    ).toBeDisabled()
    await page.keyboard.press(shortcut)
    expect(requests).toBe(1)
    finish()
    await expect(
      page.getByRole('alert').filter({ hasText: 'Controlled test failure' })
    ).toBeVisible()
  })
}

test('rejects invalid credentials and enables Continue after verification without reloading', async ({
  page,
}) => {
  const provider: ProviderCatalogDto = {
    id: 'claude_cli',
    name: 'Claude Code',
    install_command: 'install',
    auth_command: 'login',
    auth_flavors: [
      {
        id: 'subscription',
        label: 'Subscription (OAuth)',
        description: 'Use your token.',
        format: 'oauth_token',
        env_var: null,
      },
    ],
    models: [],
    runtime_models: [],
    permission_modes: [
      { id: 'default', name: 'Ask each time' },
      { id: 'auto', name: 'Auto' },
    ],
    default_permission_mode_id: 'default',
    credential_saved: false,
    credential_verification_status: 'not_saved',
    host_authenticated: false,
    model_source: 'bootstrap',
    supports_max_turns: true,
    workspace_ready: false,
  }
  // Keep GET deliberately stale: the successful save response must update the
  // mounted wizard, not rely on a reload or another catalog request.
  await page.route('**/api/settings/ai-providers?*', (route) =>
    route.fulfill({
      json: { default_provider: provider.id, providers: [provider] },
    })
  )
  await page.route('**/api/settings/ai-providers', (route) =>
    route.fulfill({
      json: { default_provider: provider.id, providers: [provider] },
    })
  )
  let attempts = 0
  let modelAttempts = 0
  await page.route(
    '**/api/settings/ai-providers/claude_cli/models/refresh',
    async (route) => {
      modelAttempts += 1
      await route.fulfill(
        modelAttempts === 1
          ? {
              status: 503,
              json: { detail: 'Model discovery temporarily unavailable.' },
            }
          : {
              json: {
                provider_id: provider.id,
                model_source: 'live',
                models: ['test-model'],
                runtime_models: [
                  { id: 'test-model', name: 'Test model', thinking_modes: [] },
                ],
                default_runtime_model_id: 'test-model',
              },
            }
      )
    }
  )
  let completeVerification: () => void = () => {}
  const verificationGate = new Promise<void>((resolve) => {
    completeVerification = resolve
  })
  await page.route(
    '**/api/settings/ai-providers/claude_cli/credential',
    async (route) => {
      attempts += 1
      if (attempts === 2) await verificationGate
      await route.fulfill(
        attempts === 1
          ? {
              status: 400,
              contentType: 'application/problem+json',
              body: JSON.stringify({
                title: 'Credential rejected',
                detail:
                  'Claude Code rejected this credential. Check the token and try again.',
              }),
            }
          : {
              json: {
                saved: true,
                provider_id: provider.id,
                auth_type: 'subscription',
                provider: {
                  ...provider,
                  credential_saved: true,
                  workspace_ready: true,
                },
              },
            }
      )
    }
  )
  await page.goto('/ai-first')
  await page.getByRole('button', { name: 'New workspace', exact: true }).click()
  await page.getByRole('button', { name: 'Continue', exact: true }).click()
  await page
    .getByRole('button', { name: /Subscription.*Paste a token/ })
    .click()
  await page
    .getByLabel('Subscription (OAuth) credential', { exact: true })
    .fill('invalid-test-token')
  await page.getByRole('button', { name: 'Verify & save', exact: true }).click()
  await expect(
    page.getByRole('alert').filter({ hasText: 'Claude Code rejected' })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Continue', exact: true })
  ).toBeDisabled()
  await page
    .getByLabel('Subscription (OAuth) credential', { exact: true })
    .fill('valid-mocked-token')
  await page.getByRole('button', { name: 'Verify & save', exact: true }).click()
  await expect(
    page.getByRole('button', { name: 'Verifying…', exact: true })
  ).toBeDisabled()
  await expect(
    page.getByRole('button', { name: 'Continue', exact: true })
  ).toBeDisabled()
  completeVerification()
  await expect(
    page.getByRole('button', { name: 'Continue', exact: true })
  ).toBeEnabled()
  await page.getByRole('button', { name: 'Continue', exact: true }).click()
  await expect(
    page.getByRole('textbox', { name: 'Your first task', exact: true })
  ).toBeVisible()
  await expect(page).toHaveURL(/setupStep=4/)
  await page.getByRole('button', { name: 'Model and tool settings' }).click()
  expect(attempts).toBe(2)
  await expect(
    page
      .getByRole('alert')
      .filter({ hasText: 'Model discovery temporarily unavailable' })
  ).toBeVisible()
  await page.getByRole('button', { name: 'Retry loading models' }).click()
  await expect(page.locator('#workspace-permissions')).toHaveText('Auto')
  await page.getByRole('combobox').first().click()
  await expect(page.getByRole('option', { name: 'Test model' })).toBeVisible()
  await expect(
    page
      .getByRole('option', { name: 'Test model' })
      .locator('[data-harness="claude_cli"]')
  ).toBeVisible()
  expect(modelAttempts).toBe(2)
  await page.getByRole('option', { name: 'Test model' }).click()
  await page.keyboard.press('Escape')
  await page
    .getByRole('textbox', { name: 'Your first task' })
    .fill('Build a landing page')
  await expect(
    page.getByRole('button', { name: 'Send and create workspace' })
  ).toBeEnabled()
  await expect(
    page.getByRole('button', { name: 'Workspace and source' })
  ).toContainText('Untitled workspace')
  await page.getByRole('button', { name: 'Workspace and source' }).click()
  await page
    .getByLabel('Workspace name (optional)', { exact: true })
    .fill('Composer test')
  await expect(page).toHaveURL(/setupName=Composer/)
  await page.keyboard.press('Escape')
  await page
    .getByRole('textbox', { name: 'Your first task' })
    .fill('Build a landing page')
  await expect(
    page.getByRole('button', { name: 'Send and create workspace' })
  ).toBeEnabled()
  await expect(
    page.getByRole('button', { name: 'Continue', exact: true })
  ).toHaveCount(0)
  await page.screenshot({ path: '/tmp/temps-workspace-composer-desktop.png' })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(
    page.getByRole('button', { name: 'Send and create workspace' })
  ).toBeVisible()
  await page.screenshot({ path: '/tmp/temps-workspace-composer-mobile.png' })
  await page.reload()
  await expect(
    page.getByRole('textbox', { name: 'Your first task' })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Workspace and source' })
  ).toContainText('Composer test')
  await expect(
    page.getByRole('textbox', { name: 'Your first task' })
  ).toHaveValue('Build a landing page')
  expect(page.url()).not.toContain('Build')
  expect(page.url()).not.toContain('token')
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  await expect(page).not.toHaveURL(/setup=/)
  await page.goBack()
  await expect(
    page.getByRole('textbox', { name: 'Your first task' })
  ).toBeVisible()
})
