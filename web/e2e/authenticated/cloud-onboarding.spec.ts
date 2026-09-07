// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Page } from '@playwright/test'
import { expect, expectAppMounted, test } from '../fixtures'

const cloudStatus = (linked: boolean) => ({
  account_email: linked ? 'owner@example.com' : null,
  backend_url: 'http://localhost:19200',
  health: linked ? 'healthy' : 'disconnected',
  health_message: linked ? 'Signals are reaching Temps Cloud' : 'Not linked',
  instance_id: linked ? 'instance-e2e-1234' : null,
  spooled_spans: 0,
  status: linked ? 'linked' : 'disconnected',
  status_message: linked
    ? 'This instance is reporting to Temps Cloud'
    : 'Connect this instance to begin reporting',
  telemetry_enabled: false,
  backups_enabled: false,
  notifications_enabled: false,
})

const routeCloudLifecycle = async (page: Page) => {
  let linked = false
  const enrollmentCodes: string[] = []
  let featureState = {
    telemetry_enabled: false,
    backups_enabled: false,
    notifications_enabled: false,
  }
  const featureUpdates: (typeof featureState)[] = []

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
      body: JSON.stringify({ ...cloudStatus(linked), ...featureState }),
    })
  })
  await page.route('**/cloud/enroll', async (route) => {
    enrollmentCodes.push(route.request().postDataJSON().enrollment_code)
    linked = true
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ...cloudStatus(true), ...featureState }),
    })
  })
  await page.route('**/cloud/features', async (route) => {
    featureState = route.request().postDataJSON()
    featureUpdates.push({ ...featureState })
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ...cloudStatus(true), ...featureState }),
    })
  })
  await page.route('**/cloud', async (route) => {
    if (route.request().method() !== 'DELETE') {
      await route.fallback()
      return
    }
    linked = false
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(cloudStatus(false)),
    })
  })

  return { enrollmentCodes, featureUpdates }
}

test.describe('Temps Cloud activation onboarding', () => {
  test('connects and disconnects an instance from the two-step setup', async ({
    page,
    consoleErrors,
  }) => {
    const cloud = await routeCloudLifecycle(page)
    await page.goto('/settings/cloud')
    await expectAppMounted(page)

    await expect(
      page.getByRole('heading', { name: 'Connect this instance' })
    ).toBeVisible()
    await expect(
      page.getByRole('link', { name: 'Get a code' })
    ).toHaveAttribute('href', 'http://localhost:19200')
    await page.getByLabel('1. Paste enrollment code').fill('ABCD-EFGH')
    await page.getByRole('button', { name: '2. Connect' }).click()

    await expect(page.getByRole('heading', { name: 'Connected' })).toBeVisible()
    await expect(
      page.getByText('Cloud account: owner@example.com')
    ).toBeVisible()
    await expect(page.getByText('instance-e2')).toBeVisible()
    expect(cloud.enrollmentCodes).toEqual(['ABCD-EFGH'])

    const telemetry = page.getByRole('switch', {
      name: 'Export telemetry to Cloud',
    })
    const backups = page.getByRole('switch', {
      name: 'Export backups to Cloud',
    })
    const notifications = page.getByRole('switch', {
      name: 'Send notifications through Cloud',
    })
    await expect(telemetry).not.toBeChecked()
    await expect(backups).not.toBeChecked()
    await expect(notifications).not.toBeChecked()

    await telemetry.click()
    await expect(telemetry).toBeChecked()
    await backups.click()
    await expect(backups).toBeChecked()
    await notifications.click()
    await expect(notifications).toBeChecked()
    expect(cloud.featureUpdates).toEqual([
      {
        telemetry_enabled: true,
        backups_enabled: false,
        notifications_enabled: false,
      },
      {
        telemetry_enabled: true,
        backups_enabled: true,
        notifications_enabled: false,
      },
      {
        telemetry_enabled: true,
        backups_enabled: true,
        notifications_enabled: true,
      },
    ])

    await page.getByRole('button', { name: 'Disconnect' }).click()
    await expect(
      page.getByRole('heading', { name: 'Connect this instance' })
    ).toBeVisible()

    await page.getByLabel('1. Paste enrollment code').fill('WXYZ-IJKL')
    await page.getByRole('button', { name: '2. Connect' }).click()
    await expect(page.getByRole('heading', { name: 'Connected' })).toBeVisible()
    expect(cloud.enrollmentCodes).toEqual(['ABCD-EFGH', 'WXYZ-IJKL'])
    expect(consoleErrors).toEqual([])
  })

  test('shows an actionable capability error and recovers on retry', async ({
    page,
    consoleErrors,
  }) => {
    let capabilityAvailable = false
    await page.route('**/cloud/status', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(cloudStatus(false)),
      })
    })
    await page.route('**/cloud/capability', async (route) => {
      await route.fulfill({
        status: capabilityAvailable ? 200 : 503,
        contentType: 'application/json',
        body: JSON.stringify(
          capabilityAvailable
            ? { configured: true, reason: null, setup_path: '/settings/cloud' }
            : {
                type: 'https://temps.sh/probs/cloud-link',
                title: 'Managed control plane unavailable',
                status: 503,
                detail:
                  'Cloud capability checks timed out. Check connectivity and retry.',
              }
        ),
      })
    })

    await page.goto('/settings/cloud')
    await expectAppMounted(page)
    await expect(
      page.getByText('Temps Cloud capability unavailable')
    ).toBeVisible()
    await expect(
      page.getByText(
        'Cloud capability checks timed out. Check connectivity and retry.'
      )
    ).toBeVisible()

    capabilityAvailable = true
    await page.getByRole('button', { name: 'Try again' }).click()
    await expect(
      page.getByRole('heading', { name: 'Connect this instance' })
    ).toBeVisible()
    expect(consoleErrors).toEqual([])
  })

  test('explains when Cloud is unavailable and disables enrollment', async ({
    page,
    consoleErrors,
  }) => {
    await page.route('**/cloud/status', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(cloudStatus(false)),
      })
    })
    await page.route('**/cloud/capability', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          configured: false,
          reason: 'Set a valid managed backend URL before connecting.',
          setup_path: '/settings/cloud',
        }),
      })
    })

    await page.goto('/settings/cloud')
    await expectAppMounted(page)
    await expect(
      page.getByText('Cloud connection needs configuration')
    ).toBeVisible()
    await expect(
      page.getByText('Set a valid managed backend URL before connecting.')
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: '2. Connect' })
    ).toBeDisabled()
    expect(consoleErrors).toEqual([])
  })

  test('surfaces a linked but degraded connection', async ({
    page,
    consoleErrors,
  }) => {
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
          ...cloudStatus(true),
          health: 'buffering',
          health_message:
            'Cloud is unreachable; 12 spans are buffered locally.',
          spooled_spans: 12,
        }),
      })
    })

    await page.goto('/settings/cloud')
    await expectAppMounted(page)
    await expect(page.getByText('Cloud connection is degraded')).toBeVisible()
    await expect(page.getByRole('alert')).toContainText(
      'Cloud is unreachable; 12 spans are buffered locally.'
    )
    await expect(
      page.getByRole('button', { name: 'Check again' })
    ).toBeVisible()
    expect(consoleErrors).toEqual([])
  })

  test('blocks enrollment when the saved Cloud credential cannot be read', async ({
    page,
    consoleErrors,
  }) => {
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
          ...cloudStatus(false),
          status: 'state_unreadable',
          status_message:
            'Cloud link state at /data/cloud-link/state.json is unreadable.',
        }),
      })
    })

    await page.goto('/settings/cloud')
    await expectAppMounted(page)
    await expect(
      page.getByText('Cloud credentials need recovery')
    ).toBeVisible()
    await expect(
      page.getByText(/Temps will not overwrite the existing credential file/)
    ).toBeVisible()
    await expect(
      page.getByRole('heading', { name: 'Connect this instance' })
    ).toHaveCount(0)
    expect(consoleErrors).toEqual([])
  })
})
