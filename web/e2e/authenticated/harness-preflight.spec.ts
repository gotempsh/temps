// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect } from '@playwright/test'

test('preflight is non-billable; smoke requires consent and reports failures compactly', async ({
  page,
}) => {
  const pageErrors: string[] = []
  page.on('pageerror', (error) => pageErrors.push(error.message))
  const provider = {
    id: 'codex_cli',
    name: 'Codex (OpenAI)',
    install_command: 'install',
    auth_command: 'login',
    auth_flavors: [
      {
        id: 'api_key',
        label: 'API key',
        description: 'API key',
        format: 'api_key',
      },
    ],
    models: ['test-model'],
    default_model: 'test-model',
    runtime_models: [],
    permission_modes: [],
    default_permission_mode_id: 'auto',
    credential_saved: true,
    credential_verification_status: 'verified',
    current_auth_type: 'api_key',
    host_authenticated: false,
    model_source: 'bootstrap',
    supports_max_turns: false,
    workspace_ready: true,
  }
  await page.route('**/api/settings/ai-providers', (route) =>
    route.fulfill({
      json: { default_provider: 'codex_cli', providers: [provider] },
    })
  )
  const report = {
    provider_id: 'codex_cli',
    mode: 'preflight',
    overall: 'failed',
    checked_at: '2026-09-15T10:00:00Z',
    diagnostic_id: 'browser-check-123',
    checks: [
      {
        id: 'runtime',
        label: 'Runtime connection',
        status: 'passed',
        detail: 'Connected',
        duration_ms: 150,
      },
      {
        id: 'image',
        label: 'Managed image',
        status: 'failed',
        detail: 'Image unavailable',
        action: 'Pull the required release image.',
        duration_ms: 2100,
      },
      {
        id: 'model',
        label: 'Model reply',
        status: 'not_tested',
        detail: 'Blocked by missing image.',
        duration_ms: 0,
      },
    ],
  }
  let preflights = 0
  let smokes = 0
  let finish: () => void = () => {}
  const pending = new Promise<void>((resolve) => {
    finish = resolve
  })
  await page.route(
    '**/api/settings/ai-providers/codex_cli/preflight',
    async (route) => {
      preflights++
      expect(route.request().postData()).toBeNull()
      await pending
      await route.fulfill({ json: report })
    }
  )
  await page.route('**/api/settings/ai-providers/codex_cli/smoke', (route) => {
    smokes++
    expect(route.request().postDataJSON()).toEqual({
      consent: true,
      model: 'test-model',
    })
    return route.fulfill({
      status: 503,
      json: {
        detail: 'Sandbox runtime is unavailable. No credential was changed.',
      },
    })
  })
  await page.goto('/agent-sandbox/providers/codex_cli')
  const panel = page.getByRole('region', { name: 'Harness diagnostics' })
  const smoke = panel.getByRole('button', { name: 'Run smoke test' })
  await expect(smoke).toBeDisabled()
  await panel.getByRole('button', { name: 'Check setup', exact: true }).click()
  await expect(panel.getByRole('status')).toContainText('Checking setup')
  await expect(
    panel.getByRole('button', { name: 'Check setup', exact: true })
  ).toBeDisabled()
  expect(smokes).toBe(0)
  finish()
  await expect(panel.getByRole('status')).toContainText('Setup check: Failed')
  await expect(panel.getByText('Not tested', { exact: true })).toBeVisible()
  await expect(
    panel.getByText('Pull the required release image.', { exact: true }).last()
  ).toBeVisible()
  await panel.getByRole('checkbox').check()
  await expect(smoke).toBeEnabled()
  await smoke.click()
  await expect(panel.getByRole('alert')).toContainText(
    'Sandbox runtime is unavailable'
  )
  expect(preflights).toBe(1)
  expect(smokes).toBe(1)
  await page.reload()
  await expect(smoke).toBeDisabled()
  await expect(
    panel.getByText('browser-check-123', { exact: false })
  ).toHaveCount(0)
  expect(pageErrors).toEqual([])
})
