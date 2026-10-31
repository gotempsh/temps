// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, uniqueSlug } from '../fixtures'

interface SavedRule {
  id: number
  name: string
  mode: 'enforce' | 'log_only'
  enabled: boolean
}

/**
 * The edge rules page against a real backend: every step in the console is
 * checked against what the API then holds. Live enforcement through the proxy
 * is covered by `apps/temps-e2e`'s `edge-rules-scenario`.
 */
test('edge rules: add an example, test it, enforce, rename, disable and delete', async ({
  page,
  consoleErrors,
}, testInfo) => {
  const created = await page.request.post('/api/projects', {
    data: {
      name: uniqueSlug('e2e-edge-rules', testInfo),
      directory: '.',
      main_branch: 'main',
      preset: 'dockerfile',
      storage_service_ids: [],
      source_type: 'docker_image',
      exposed_port: 80,
      is_web_app: true,
    },
  })
  expect(created.ok()).toBe(true)
  const project = await created.json()
  const rules = async (): Promise<SavedRule[]> => {
    const res = await page.request.get(`/api/projects/${project.id}/edge-rules`)
    expect(res.ok()).toBe(true)
    return (await res.json()).rules
  }

  try {
    await page.goto(`/projects/${project.slug}/settings/edge-rules`)
    await expect(page.getByText('No edge rules yet')).toBeVisible()

    // Examples are added log-only, so nothing is blocked until enforced.
    await page
      .getByRole('button', {
        name: 'Add example: Block CMS and secret-file probes',
      })
      .click()
    const firstRule = page.getByRole('row', {
      name: /Block CMS and secret-file probes/,
    })
    await expect(firstRule.getByText('Log only')).toBeVisible()
    const [example] = await rules()
    expect(example).toMatchObject({ mode: 'log_only', enabled: true })

    // The tester runs the saved rules without sending anything.
    const tester = page.getByRole('dialog', { name: 'Test a request' })
    const testPath = async (url: string) => {
      await page.getByRole('button', { name: 'Test a request' }).click()
      await tester.getByLabel('URL').fill(url)
    }
    await testPath('https://app.example.com/WP-login.php')
    await expect(tester.getByText('Matched, logged')).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(tester).not.toBeVisible()

    await page
      .getByRole('button', { name: `More actions for ${example.name}` })
      .click()
    await page.getByRole('menuitem', { name: 'Enforce' }).click()
    await expect(firstRule.getByText('Enforced')).toBeVisible()
    await expect.poll(async () => (await rules())[0].mode).toBe('enforce')
    await testPath('https://app.example.com/WP-login.php')
    await expect(tester.getByText('Blocked (404)')).toBeVisible()
    await page.keyboard.press('Escape')

    // The editor saves against the revision it loaded.
    await page.getByRole('link', { name: `Edit ${example.name}` }).click()
    await page.getByLabel(/^Name/).fill('E2E probes')
    await page.getByRole('button', { name: 'Save rule' }).click()
    await expect(page).toHaveURL(/\/settings\/edge-rules$/)
    await expect.poll(async () => (await rules())[0].name).toBe('E2E probes')

    await page.getByRole('switch', { name: 'Disable E2E probes' }).click()
    await expect.poll(async () => (await rules())[0].enabled).toBe(false)

    await page
      .getByRole('button', { name: 'More actions for E2E probes' })
      .click()
    await page.getByRole('menuitem', { name: 'Delete' }).click()
    const confirm = page.getByRole('alertdialog')
    await expect(confirm).toContainText('E2E probes')
    await confirm.getByRole('button', { name: 'Delete', exact: true }).click()
    await expect(confirm).not.toBeVisible()
    await expect(page.getByText('No edge rules yet')).toBeVisible()
    expect(await rules()).toEqual([])
    expect(consoleErrors).toEqual([])
  } finally {
    // Deleting the project removes its rules with it.
    const removed = await page.request.delete(`/api/projects/${project.id}`)
    expect(removed.ok()).toBe(true)
  }
})
