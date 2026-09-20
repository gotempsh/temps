// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

test('Compose policy exceptions require acknowledgment and persist independently', async ({
  page,
}, testInfo) => {
  const created = await page.request.post('/api/projects', {
    data: {
      name: `compose-security-${testInfo.workerIndex}-${Date.now()}`,
      directory: '.',
      main_branch: 'main',
      preset: 'docker-compose',
      storage_service_ids: [],
      automatic_deploy: false,
      source_type: 'git',
      repo_owner: 'gotempsh',
      repo_name: 'temps',
      is_public_repo: true,
      git_url: 'https://github.com/gotempsh/temps',
    },
  })
  expect(created.ok()).toBe(true)
  const project = await created.json()
  const endpoint = `/api/projects/${project.id}/compose-security`
  try {
    const catalog = await (await page.request.get(endpoint)).json()
    expect(catalog.policy.disabled_checks).toEqual([])
    const denied = await page.request.put(endpoint, {
      data: {
        policy: { disabled_checks: ['extends'] },
        expected_policy: catalog.policy,
        acknowledge_risks: false,
      },
    })
    expect(denied.status()).toBe(400)
    await page.goto(`/projects/${project.slug}/settings/build?tab=build`)
    const trigger = page.getByRole('button', {
      name: 'Advanced security settings',
    })
    await expect(trigger).toHaveAttribute('aria-expanded', 'false')
    await trigger.click()
    const section = page.locator('#compose-security')
    await expect(section.getByRole('switch')).toHaveCount(catalog.checks.length)
    const search = page.getByRole('textbox', {
      name: 'Search Compose security checks',
    })
    await search.fill('extends')
    const check = page.locator('#compose-check-extends')
    await expect(check).toBeChecked()
    await check.click()
    const dialog = page.getByRole('alertdialog')
    const confirm = dialog.getByRole('button', { name: 'Disable check' })
    await expect(confirm).toBeDisabled()
    await dialog.getByRole('checkbox').check()
    await confirm.click()
    await expect(dialog).not.toBeVisible()
    await expect(check).not.toBeChecked()
    await page.reload()
    await expect(trigger).toContainText('1 disabled')
    await expect(trigger).toHaveAttribute('aria-expanded', 'false')
    await trigger.click()
    await search.fill('extends')
    await expect(check).not.toBeChecked()
    await check.click()
    await expect(check).toBeChecked()
    expect(
      (await (await page.request.get(endpoint)).json()).policy.disabled_checks
    ).toEqual([])

    // Another administrator changes the server policy while this editor remains stale.
    const currentPolicy = (await (await page.request.get(endpoint)).json())
      .policy
    const otherAdmin = await page.request.put(endpoint, {
      data: {
        expected_policy: currentPolicy,
        policy: { disabled_checks: ['external_volumes'] },
        acknowledge_risks: true,
      },
    })
    expect(otherAdmin.ok()).toBe(true)
    await check.click()
    await dialog.getByRole('checkbox').check()
    await confirm.click()
    await expect(
      page.getByText(
        'Another administrator changed these settings. Review the refreshed policy and try again.'
      )
    ).toBeVisible()
    await expect(dialog).not.toBeVisible()
    await expect(check).toBeChecked()
    expect(
      (await (await page.request.get(endpoint)).json()).policy.disabled_checks
    ).toEqual(['external_volumes'])

    await search.fill('external volumes')
    await expect(
      page.locator('#compose-check-external_volumes')
    ).not.toBeChecked()
    await search.fill('no-matching-policy')
    await expect(
      page.getByText('No security checks match your search.')
    ).toBeVisible()
    await search.fill('volumes')
    await page.setViewportSize({ width: 390, height: 844 })
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth
      )
    ).toBe(true)

    let migrationPending = true
    await page.route(`**${endpoint}`, async (route) => {
      if (route.request().method() === 'PUT') {
        expect(
          route.request().postDataJSON().acknowledge_legacy_migration
        ).toBe(true)
        migrationPending = false
      }
      await route.fulfill({
        json: { ...catalog, legacy_migration_pending: migrationPending },
      })
    })
    await page.reload()
    await expect(page.getByRole('note')).toBeVisible()
    await trigger.click()
    const completeMigration = page.getByRole('button', {
      name: 'Complete legacy migration',
    })
    await expect(completeMigration).toBeDisabled()
    await page
      .getByRole('checkbox', {
        name: 'I have reviewed the exceptions this project needs for its next deployment.',
      })
      .check()
    await completeMigration.click()
    await expect(page.getByRole('note')).not.toBeVisible()
    expect(migrationPending).toBe(false)
    await page.unroute(`**${endpoint}`)

    await page.route(`**${endpoint}`, (route) =>
      route.fulfill({
        json: { ...catalog, can_edit: false, legacy_migration_pending: true },
      })
    )
    await page.reload()
    await expect(page.getByRole('note')).toContainText(
      'legacy “Disable sandbox” settings'
    )
    await expect(trigger).toHaveAttribute('aria-expanded', 'false')
    await trigger.click()
    await expect(
      page.getByText('Only instance administrators can change these settings.')
    ).toBeVisible()
    await expect(check).toBeDisabled()
  } finally {
    // This project never deploys; remove its settings and audit history after the test.
    const removed = await page.request.delete(`/api/projects/${project.id}`)
    expect(removed.ok()).toBe(true)
  }
})
