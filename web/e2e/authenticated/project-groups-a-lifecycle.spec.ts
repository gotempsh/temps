// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

/**
 * Live-instance lifecycle for project groups, driven through the console:
 * create (with validation + no stale dialog state), add, move, remove,
 * rename, delete. Creates (and removes) its own two services.
 */

const SHOTS = '/tmp/e2e-a-shots'
const SVC_WEB = 'e2e-a-svc-web'
const SVC_API = 'e2e-a-svc-api'
const G1 = 'e2e-a Shop'
const G2 = 'e2e-a Backoffice'
const G1_RENAMED = 'e2e-a Shop Renamed'

interface GroupJson {
  id: number
  name: string
  slug: string
  projects: { id: number; slug: string }[]
}

async function groups(page: Page): Promise<GroupJson[]> {
  const res = await page.request.get('/api/project-groups?page_size=100')
  expect(res.ok()).toBeTruthy()
  return (await res.json()).groups as GroupJson[]
}

async function shot(page: Page, name: string) {
  await page.screenshot({ path: `${SHOTS}/${name}.png`, fullPage: true })
}

async function cleanup(page: Page) {
  for (const g of await groups(page)) {
    if ([G1, G2, G1_RENAMED].includes(g.name)) {
      await page.request.delete(`/api/project-groups/${g.id}`)
    }
  }
}


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

test.describe.configure({ mode: 'serial' })

test('group lifecycle through the console', async ({ page }) => {
  test.setTimeout(240_000)
  await page.setViewportSize({ width: 1440, height: 1100 })
  const pageErrors: string[] = []
  page.on('pageerror', (e) => pageErrors.push(e.message))
  await cleanup(page)
  const created: CreatedService[] = []
  created.push(await createService(page, SVC_WEB))
  created.push(await createService(page, SVC_API))

  try {
    // ---- services rename notice -------------------------------------
    await page.goto('/projects')
    const notice = page.getByText('Your apps are now called Services.')
    await expect(notice).toBeVisible()
    await shot(page, '01-projects-notice')
    await page.getByRole('button', { name: 'Dismiss this notice' }).click()
    await expect(notice).toHaveCount(0)
    await page.reload()
    await expect(page.getByRole('heading', { name: 'Projects', level: 1 })).toBeVisible()
    await expect(notice).toHaveCount(0)

    // ---- create dialog: validation + no stale state --------------------
    await page.getByRole('button', { name: 'New project' }).first().click()
    const dialog = page.getByRole('dialog')
    await expect(dialog.getByRole('heading', { name: 'New project' })).toBeVisible()
    await dialog.getByRole('button', { name: 'Create project' }).click()
    await expect(dialog.getByText('Give the project a name.')).toBeVisible()
    await shot(page, '02-create-validation')
    // dirty the dialog, then close and reopen: must be empty again
    await dialog.getByLabel('Name').fill('stale name')
    await dialog.getByLabel(/Description/).fill('stale description')
    await dialog.getByRole('button', { name: 'Cancel' }).click()
    await expect(dialog).toHaveCount(0)
    await page.getByRole('button', { name: 'New project' }).first().click()
    await expect(dialog.getByLabel('Name')).toHaveValue('')
    await expect(dialog.getByLabel(/Description/)).toHaveValue('')
    await expect(dialog.getByText('Give the project a name.')).toHaveCount(0)
    await shot(page, '03-create-reopened-empty')

    // ---- create group 1 with svc-web --------------------------------
    await dialog.getByLabel('Name').fill(G1)
    await dialog.getByRole('button', { name: new RegExp(SVC_WEB) }).or(
      dialog.getByText(SVC_WEB, { exact: true })
    ).first().click()
    await expect(dialog.getByRole('button', { name: 'Create with 1 service' })).toBeVisible()
    await dialog.getByRole('button', { name: 'Create with 1 service' }).click()
    await expect(page.getByText(`Project “${G1}” created`)).toBeVisible()
    await page.waitForURL(/\/p\/e2e-a-shop/)
    await expect(page.getByRole('heading', { name: G1 }).first()).toBeVisible()
    await expect(page.getByText(SVC_WEB).first()).toBeVisible()
    await shot(page, '04-group1-created')
    const g1 = (await groups(page)).find((g) => g.name === G1)!
    expect(g1.projects.map((p) => p.slug)).toEqual([SVC_WEB])

    // ---- create empty group 2 ----------------------------------------
    await page.goto('/projects')
    await page.getByRole('button', { name: 'New project' }).first().click()
    await dialog.getByLabel('Name').fill(G2)
    await dialog.getByRole('button', { name: 'Create project' }).click()
    await expect(page.getByText(`Project “${G2}” created`)).toBeVisible()
    await page.waitForURL(/\/p\/e2e-a-backoffice/)
    await shot(page, '05-group2-created-empty')

    // ---- add existing service (svc-api) to group 1 -------------------
    await page.goto('/projects')
    await expect(page.getByText(G1).first()).toBeVisible()
    await page.getByRole('button', { name: `Actions for ${G1}` }).click()
    await page.getByRole('menuitem', { name: 'Add services' }).click()
    await expect(dialog.getByRole('heading', { name: `Add services to ${G1}` })).toBeVisible()
    await dialog.getByText(SVC_API, { exact: true }).first().click()
    await dialog.getByRole('button', { name: 'Add 1 service' }).click()
    await expect(page.getByText(`Added 1 service to ${G1}`)).toBeVisible()
    await expect(dialog).toHaveCount(0)
    // the card for G1 now lists both services
    const card1 = page.locator('div,article,section,li').filter({ hasText: G1 }).filter({ hasText: SVC_API }).filter({ hasText: SVC_WEB }).last()
    await expect(card1).toBeVisible()
    await shot(page, '06-added-api-to-group1')
    let gs = await groups(page)
    expect(gs.find((g) => g.name === G1)!.projects.map((p) => p.slug).sort()).toEqual([SVC_API, SVC_WEB])

    // ---- move svc-api to group 2 (picker says "selecting it moves it here")
    await page.getByRole('button', { name: `Actions for ${G2}` }).click()
    await page.getByRole('menuitem', { name: 'Add services' }).click()
    await expect(dialog.getByRole('heading', { name: `Add services to ${G2}` })).toBeVisible()
    await expect(dialog.getByText(`In ${G1}`).first()).toBeVisible()
    await shot(page, '07-move-dialog-hint')
    await dialog.getByText(SVC_API, { exact: true }).first().click()
    await dialog.getByRole('button', { name: 'Add 1 service' }).click()
    await expect(page.getByText(`Added 1 service to ${G2}`)).toBeVisible()
    gs = await groups(page)
    expect(gs.find((g) => g.name === G1)!.projects.map((p) => p.slug)).toEqual([SVC_WEB])
    expect(gs.find((g) => g.name === G2)!.projects.map((p) => p.slug)).toEqual([SVC_API])
    await page.goto('/projects')
    await expect(page.getByRole('button', { name: `Actions for ${G2}` })).toBeVisible()
    const card2 = page.locator('div,article,section,li').filter({ hasText: G2 }).filter({ hasText: SVC_API }).last()
    await expect(card2).toBeVisible()
    await shot(page, '08-after-move')

    // ---- remove svc-web from group 1 via settings --------------------
    await page.goto(`/p/${g1.slug}?tab=settings&section=services`)
    await expect(page.getByText(`Services (1)`)).toBeVisible()
    await page.getByRole('button', { name: 'Remove' }).click()
    await expect(page.getByRole('alertdialog')).toContainText(`Remove service ${SVC_WEB}?`)
    await shot(page, '09-remove-confirm')
    await page.getByRole('button', { name: 'Remove from project' }).click()
    await expect(page.getByText(`${SVC_WEB} is now a standalone service`)).toBeVisible()
    await expect(page.getByText('No services in this project.')).toBeVisible()
    await shot(page, '10-after-remove')
    gs = await groups(page)
    expect(gs.find((g) => g.name === G1)!.projects).toHaveLength(0)

    // ---- rename group 1 ------------------------------------------------
    await page.goto(`/p/${g1.slug}?tab=settings&section=general`)
    const nameInput = page.locator('#group-settings-name')
    await expect(nameInput).toHaveValue(G1)
    await expect(page.getByRole('button', { name: 'Save changes' })).toBeDisabled()
    await nameInput.fill('')
    await page.locator('#group-settings-description').click()
    await page.getByRole('button', { name: 'Save changes' }).click()
    await shot(page, '11-rename-empty-validation')
    await nameInput.fill(G1_RENAMED)
    await page.getByRole('button', { name: 'Save changes' }).click()
    await expect(page.getByText('Project updated')).toBeVisible()
    await shot(page, '12-renamed')
    gs = await groups(page)
    const renamed = gs.find((g) => g.id === g1.id)!
    expect(renamed.name).toBe(G1_RENAMED)
    // breadcrumb / header reflect the new name
    await expect(page.getByRole('navigation', { name: 'breadcrumb' })).toContainText(G1_RENAMED)

    // ---- delete group 1 ------------------------------------------------
    await page.goto(`/p/${renamed.slug}?tab=settings&section=general`)
    // "Delete project" is a collapsed section; open it, then use the button
    await page.getByText('Delete project', { exact: true }).first().click()
    await page.getByRole('button', { name: 'Delete project' }).click({ timeout: 10_000 })
    await expect(page.getByRole('alertdialog')).toContainText(`Delete project “${G1_RENAMED}”?`)
    await shot(page, '13-delete-confirm')
    await page.getByRole('alertdialog').getByRole('button', { name: 'Delete project' }).click()
    await expect(page.getByText(`Project “${G1_RENAMED}” deleted`)).toBeVisible()
    await page.waitForURL(/\/projects(?:[?#]|$)/)
    await shot(page, '14-after-delete')
    gs = await groups(page)
    expect(gs.find((g) => g.id === g1.id)).toBeUndefined()
    // svc-web still exists, standalone
    const projs = await (await page.request.get('/api/projects?per_page=100')).json()
    expect(projs.projects.some((p: { slug: string }) => p.slug === SVC_WEB)).toBeTruthy()
  } finally {
    await cleanup(page)
    for (const service of created) await deleteService(page, service)
    expect(pageErrors, 'uncaught page errors').toEqual([])
  }
})
