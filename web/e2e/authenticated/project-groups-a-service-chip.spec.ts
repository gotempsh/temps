// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

/**
 * Live-instance check of the service page's "Add to project" entry point and
 * the "New project with <service>" pre-selection. Creates (and removes) its
 * own standalone service.
 */
const GROUP = 'e2e-a Chip'
const NEW_GROUP = 'e2e-a Chip New'

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

test('add a service to a project from its own page', async ({ page }) => {
  test.setTimeout(120_000)
  await page.setViewportSize({ width: 1440, height: 1100 })
  const list = async () =>
    (await (await page.request.get('/api/project-groups?page_size=100')).json())
      .groups as { id: number; name: string; projects: { slug: string }[] }[]
  const cleanup = async () => {
    for (const g of await list()) {
      if (g.name === GROUP || g.name === NEW_GROUP) {
        await page.request.delete(`/api/project-groups/${g.id}`)
      }
    }
  }
  await cleanup()
  const service = await createService(page, 'e2e-a-svc-chip')
  const created = await page.request.post('/api/project-groups', {
    data: { name: GROUP },
  })
  expect(created.status()).toBe(201)
  try {
    await page.goto('/projects/e2e-a-svc-chip')
    await page
      .getByRole('button', { name: 'Project actions' })
      .click({ timeout: 15_000 })
    await page.getByRole('menuitem', { name: 'Add to project…' }).click()
    await page
      .getByRole('button', { name: new RegExp(GROUP) })
      .click({ timeout: 10_000 })
    await expect(
      page.getByText(`e2e-a-svc-chip is now part of ${GROUP}`)
    ).toBeVisible()
    await expect(
      page.getByRole('link', { name: `Project ${GROUP}` })
    ).toBeVisible()
    await page.screenshot({
      path: '/tmp/e2e-a-shots/30-service-chip-added.png',
      fullPage: true,
    })
    const g = (await list()).find((x) => x.name === GROUP)!
    expect(g.projects.map((p) => p.slug)).toEqual(['e2e-a-svc-chip'])

    // remove via API, then use "New project with ..." from the chip
    const rm = await page.request.delete(
      `/api/project-groups/${g.id}/projects/${service.id}`
    )
    expect(rm.status()).toBe(204)
    await page.reload()
    await page
      .getByRole('button', { name: 'Project actions' })
      .click({ timeout: 15_000 })
    await page.getByRole('menuitem', { name: 'Add to project…' }).click()
    await page
      .getByRole('button', { name: /New project with e2e-a-svc-chip/ })
      .click()
    const dialog = page.getByRole('dialog')
    await expect(
      dialog.getByRole('heading', { name: 'New project' })
    ).toBeVisible()
    await expect(
      dialog.getByRole('button', { name: 'Create with 1 service' })
    ).toBeVisible()
    await page.screenshot({
      path: '/tmp/e2e-a-shots/31-new-project-with-service.png',
      fullPage: true,
    })
    await dialog.getByLabel('Name').fill(NEW_GROUP)
    await dialog.getByRole('button', { name: 'Create with 1 service' }).click()
    await expect(page.getByText(`Project “${NEW_GROUP}” created`)).toBeVisible()
    const g2 = (await list()).find((x) => x.name === NEW_GROUP)!
    expect(g2.projects.map((p) => p.slug)).toEqual(['e2e-a-svc-chip'])
  } finally {
    await cleanup()
    await deleteService(page, service)
  }
})
