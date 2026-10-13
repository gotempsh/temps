// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

/**
 * Live-instance check of hosting: 'external' versus the monitor-only
 * source_type: 'external' projects. Creates (and removes) `e2e-a-ext`
 * (hosting external) and `e2e-a-mon` (source_type external, created like
 * MonitorProjectForm).
 */
const SHOTS = '/tmp/e2e-a-shots'

interface CreatedService {
  id: number
  slug: string
}

/** Creates a service that needs no Docker or Git; returns it with its id. */
async function createService(
  page: Page,
  name: string,
  extra: Record<string, unknown> = {}
): Promise<CreatedService> {
  const res = await page.request.post('/api/projects', {
    data: {
      name,
      hosting: 'hosted',
      preset: 'dockerfile',
      directory: './',
      main_branch: 'main',
      source_type: 'manual',
      automatic_deploy: false,
      storage_service_ids: [],
      ...extra,
    },
  })
  expect(res.ok(), `creating ${name}: ${res.status()}`).toBeTruthy()
  return (await res.json()) as CreatedService
}

async function deleteService(page: Page, service: CreatedService | undefined) {
  if (service) await page.request.delete(`/api/projects/${service.id}`)
}

async function shot(page: Page, name: string) {
  await page.screenshot({ path: `${SHOTS}/${name}.png`, fullPage: true })
}

async function setHosting(page: Page, id: number, hosting: 'hosted' | 'external') {
  const res = await page.request.patch(`/api/projects/${id}/hosting`, {
    data: { hosting },
  })
  expect(res.ok()).toBeTruthy()
}

test('external hosting surfaces in the console', async ({ page }) => {
  test.setTimeout(180_000)
  await page.setViewportSize({ width: 1440, height: 1100 })
  const ext = await createService(page, 'e2e-a-ext')
  // Same payload MonitorProjectForm sends: monitoring only, no hosting fields.
  const monRes = await page.request.post('/api/projects', {
    data: { name: 'e2e-a-mon', source_type: 'external' },
  })
  expect(monRes.ok()).toBeTruthy()
  const mon = (await monRes.json()) as CreatedService
  try {
    await setHosting(page, ext.id, 'external')

    // project list: both projects
    await page.goto('/projects')
    await page.getByPlaceholder('Filter projects and services…').fill('e2e-a-ext')
    await expect(page.getByText('Runs outside Temps').or(page.getByText('Not deployed')).first()).toBeVisible({ timeout: 30_000 })
    await shot(page, '20-ext-list')
    console.log('LIST ext has "Runs outside Temps":', await page.getByText('Runs outside Temps').count(), ' "Not deployed":', await page.getByText('Not deployed').count(), ' External badge:', await page.getByText('External', { exact: true }).count())
    await page.getByPlaceholder('Filter projects and services…').fill('e2e-a-mon')
    await expect(page.getByText('1 matching service outside them')).toBeVisible({ timeout: 30_000 })
    await shot(page, '20b-mon-list')
    console.log('LIST mon has "Runs outside Temps":', await page.getByText('Runs outside Temps').count(), ' "Not deployed":', await page.getByText('Not deployed').count(), ' External badge:', await page.getByText('External', { exact: true }).count())

    // detail header: external hosting
    await page.goto('/projects/e2e-a-ext')
    await expect(page.getByRole('heading', { name: 'e2e-a-ext' })).toBeVisible()
    await expect(page.getByText('External', { exact: true }).first()).toBeVisible()
    await expect(page.getByRole('button', { name: 'Deploy', exact: true })).toHaveCount(0)
    await shot(page, '21-ext-detail')
    const deployish = await page.getByRole('button', { name: /deploy/i }).allInnerTexts()
    console.log('EXT detail deploy-ish buttons:', JSON.stringify(deployish))
    // A stale failure banner with Redeploy is offered although the project is external
    const redeploy = page.getByRole('button', { name: 'Redeploy' }).first()
    if (await redeploy.count()) {
      await redeploy.click({ timeout: 10_000 })
      await page.waitForTimeout(1000)
      await shot(page, '21b-ext-redeploy-clicked')
      await page.getByRole('alertdialog').getByRole('button', { name: 'Redeploy' }).click({ timeout: 10_000 })
      await page.waitForTimeout(2500)
      await shot(page, '21c-ext-redeploy-confirmed')
      const toasts = await page.locator('[data-sonner-toast]').allInnerTexts()
      console.log('EXT redeploy toasts:', JSON.stringify(toasts))
    }

    // monitor-only project: header
    await page.goto('/projects/e2e-a-mon')
    await expect(page.getByText('e2e-a-mon', { exact: true }).first()).toBeVisible()
    await page.waitForTimeout(1500)
    await shot(page, '22-mon-detail')
    console.log('MON deploy button count:', await page.getByRole('button', { name: 'Deploy', exact: true }).count())
    console.log('MON External badge count:', await page.getByText('External', { exact: true }).count())

    // settings: hosting card
    await page.goto('/projects/e2e-a-ext/settings/delivery')
    await page.getByText('Source', { exact: true }).first().click()
    await expect(page.getByText('Where it runs')).toBeVisible()
    await expect(page.getByText('This app runs outside Temps.')).toBeVisible()
    await shot(page, '23-ext-hosting-card')
    // switch back to hosted via the UI
    await page.getByRole('button', { name: /Temps runs it/ }).click({ timeout: 10_000 })
    await page.getByRole('button', { name: 'Let Temps host it' }).click({ timeout: 10_000 })
    await expect(page.getByText('Temps can deploy e2e-a-ext again')).toBeVisible()
    await shot(page, '24-ext-back-to-hosted')
    await page.goto('/projects/e2e-a-ext')
    await expect(page.getByRole('button', { name: 'Deploy', exact: true })).toBeVisible()
    await shot(page, '25-hosted-deploy-button')
    const after = await (await page.request.get(`/api/projects/${ext.id}`)).json()
    expect(after.hosting).toBe('hosted')
  } finally {
    await deleteService(page, ext)
    await deleteService(page, mon)
  }
})
