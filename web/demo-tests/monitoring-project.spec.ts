// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { test, expect, type Page } from '@playwright/test'

async function setup(page: Page, failCreate = false) {
  const writes: { path: string; body: Record<string, unknown> }[] = []
  const reads: string[] = []
  let created = false
  let source = 'external'
  const project = () => ({
    id: 42,
    slug: 'external-app',
    name: 'External app',
    source_type: source,
    project_type: 'server',
    preset: null,
    main_branch: '',
    directory: '',
    environments: [],
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  })
  await page.route('**/api/**', async (route) => {
    const req = route.request()
    const path = new URL(req.url()).pathname.replace('/api', '')
    if (req.method() !== 'GET') {
      const body = req.postDataJSON()
      writes.push({ path, body })
      if (path === '/projects') {
        if (failCreate) {
          await route.fulfill({
            status: 403,
            json: { detail: 'You do not have permission to create projects.' },
          })
          return
        }
        created = true
        await route.fulfill({ json: project() })
        return
      }
      if (path === '/projects/42/source') {
        source = body.source_type
        await route.fulfill({ json: project() })
        return
      }
    }
    reads.push(path)
    let body: unknown = []
    if (path === '/user/me')
      body = {
        id: 1,
        username: 'tester',
        name: 'Test user',
        role: 'admin',
        mfa_enabled: false,
      }
    else if (path === '/projects')
      body = { projects: created ? [project()] : [], total: created ? 1 : 0 }
    else if (path.includes('/projects/') && path.includes('external-app'))
      body = project()
    else if (path === '/git-connections') body = { connections: [] }
    else if (path.includes('has-events')) body = { has_events: false }
    else if (path.includes('has-error')) body = { has_error_groups: false }
    else if (path.includes('has-traces')) body = { has_traces: false }
    else if (path === '/platform/access-info') body = { access_mode: 'local' }
    await route.fulfill({ json: body })
  })
  return { writes, reads }
}

test('monitor-only creation is discoverable, persists, and never requests deployment health', async ({
  page,
}) => {
  const { writes, reads } = await setup(page)
  await page.goto('/projects')
  await page
    .getByRole('link', { name: 'Monitor an existing application', exact: true })
    .click()
  await expect(
    page.getByRole('tab', { name: 'Monitor an existing application' })
  ).toHaveAttribute('data-state', 'active')
  await expect(
    page.getByRole('button', { name: 'Create monitoring project' })
  ).toBeDisabled()
  await page.getByLabel('Project name', { exact: true }).fill('External app')
  await page.getByRole('button', { name: 'Create monitoring project' }).click()
  await expect(page).toHaveURL(/external-app\/integrations/)
  await expect(
    page.getByRole('heading', { name: 'Connect your application' })
  ).toBeVisible()
  expect(writes).toEqual([
    {
      path: '/projects',
      body: { name: 'External app', source_type: 'external' },
    },
  ])
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  for (const [feature, skill] of [
    ['Analytics', 'add-react-analytics'],
    ['Error tracking', 'add-error-tracking'],
    ['OpenTelemetry', 'temps-best-practices'],
  ]) {
    await page
      .getByRole('button', { name: `Copy ${feature} setup prompt and skill` })
      .click()
    const copied = await page.evaluate(() => navigator.clipboard.readText())
    expect(copied).toContain(`--skill ${skill}`)
    expect(copied).toContain('"id":42')
    expect(copied).toContain('/projects/external-app/')
  }
  // Sidebar uses a semantic list rather than a menu role.
  const projectNav = page.getByLabel('Project navigation')
  await expect(
    projectNav.getByRole('link', { name: 'Deployments', exact: true })
  ).toHaveCount(0)
  await expect(
    projectNav.getByRole('link', { name: 'Integrations', exact: true })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Deploy', exact: true })
  ).toHaveCount(0)
  await page.reload()
  await expect(page.getByText('External', { exact: true })).toBeVisible()
  expect(
    reads.some((path) => /last-deployment|dashboard.*health/.test(path))
  ).toBe(false)
  await page
    .getByRole('link', { name: 'Set up Analytics', exact: true })
    .click()
  await expect(page).toHaveURL(/analytics\/setup/)
  await page.goto('/projects/external-app/hosting')
  await expect(
    page.getByRole('heading', { name: 'Run your app on Temps' })
  ).toBeVisible()
  await expect(
    page.getByRole('link', { name: 'Choose a repository', exact: true })
  ).toHaveAttribute('href', '/projects/external-app/connect-repository')
  await page.getByRole('button', { name: 'Continue with Docker' }).click()
  await expect(page).toHaveURL(/external-app\/project/)
  expect(writes[1]).toEqual({
    path: '/projects/42/source',
    body: { source_type: 'docker_image' },
  })
  expect(writes).toHaveLength(2)
})

test('creation errors preserve inputs and permit retry', async ({ page }) => {
  const { writes } = await setup(page, true)
  await page.goto('/projects/new?source=monitor')
  await page.getByLabel('Project name', { exact: true }).fill('External app')
  await page.getByRole('button', { name: 'Create monitoring project' }).click()
  await expect(page.getByRole('alert').first()).toBeVisible()
  await expect(page.getByLabel('Project name', { exact: true })).toHaveValue(
    'External app'
  )
  await expect(
    page.getByRole('button', { name: 'Create monitoring project' })
  ).toBeEnabled()
  expect(writes).toHaveLength(1)
})

test('monitoring creation fits mobile and keeps its URL through reload', async ({
  page,
}) => {
  await setup(page)
  await page.setViewportSize({ width: 390, height: 900 })
  await page.goto('/projects/new?source=monitor')
  await page.reload()
  await expect(
    page.getByRole('form', { name: 'Create monitoring project' })
  ).toBeVisible()
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth
    )
  ).toBe(true)
})

test('external project cards show visitors without deployment stats or nested links', async ({
  page,
}) => {
  await setup(page)
  await page.goto('/projects/new?source=monitor')
  await page.getByLabel('Project name', { exact: true }).fill('External app')
  await page.getByRole('button', { name: 'Create monitoring project' }).click()
  await expect(page).toHaveURL(/external-app\/integrations/)
  await page.goto('/projects')
  const card = page.getByRole('link').filter({ hasText: 'External' })
  await expect(card).toHaveCount(1)
  await expect(card).toContainText('0visitors')
  await expect(card).not.toContainText('API requests')
  await expect(card.locator('a')).toHaveCount(0)
  await expect(card).not.toContainText('Add hosting')
  await card.click()
  await expect(page).toHaveURL(/external-app\/project/)
})
