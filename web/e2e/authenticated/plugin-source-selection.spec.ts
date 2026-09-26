// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { test, expect } from '../fixtures'

// Real console/session with mocked plugin endpoints; no native plugin is installed.
test('changing a plugin ref clears the override and keeps its directory', async ({
  page,
  consoleErrors,
}) => {
  let ref = 'release/v1'
  let revision = 'a'.repeat(40)
  const updates: Array<{ ref_name?: string }> = []
  await page.route('**/api/x/plugins', (route) =>
    route.fulfill({
      json: [
        {
          name: 'demo',
          version: '1.0.0',
          display_name: 'Nested fixture',
          nav: [],
          requires_db: false,
          health_path: '/health',
        },
      ],
    })
  )
  await page.route('**/api/x/plugins/demo/status', (route) =>
    route.fulfill({
      json: {
        configured: true,
        setup_path: '/settings/plugins',
        source: {
          kind: 'github',
          repository_url: 'https://github.com/example/plugins',
          path: 'plugins/demo',
          ref_name: ref,
          commit: revision,
          version: '1.0.0',
          builder_image: 'fixture',
        },
      },
    })
  )
  await page.route('**/api/x/plugins/demo/update', (route) => {
    const body = route.request().postDataJSON() as { ref_name?: string }
    updates.push(body)
    ref = body.ref_name || ref
    revision = 'b'.repeat(40)
    return route.fulfill({
      json: {
        name: 'demo',
        version: '1.0.0',
        source_commit: revision,
        message: 'Fixture update complete',
        platform: 'darwin-arm64',
        sha256: 'a'.repeat(64),
      },
    })
  })
  await page.goto('/settings/plugins')
  await page.getByRole('tab', { name: /Running/ }).click()
  await expect(page.getByLabel('Update branch, tag, or commit')).toBeHidden()
  await page.getByRole('button', { name: 'Update', exact: true }).click()
  const field = page.getByLabel('Update branch, tag, or commit')
  await field.fill('release/v2')
  await page.getByRole('button', { name: 'Update from GitHub' }).click()
  await expect(field).toHaveValue('')
  await expect(field).toHaveAttribute('placeholder', 'Keep release/v2')
  await page.getByRole('button', { name: 'Update from GitHub' }).click()
  await expect.poll(() => updates).toEqual([{ ref_name: 'release/v2' }, {}])
  await expect(page.getByText('plugins/demo', { exact: true })).toBeVisible()
  await page.setViewportSize({ width: 390, height: 844 })
  await field.scrollIntoViewIfNeeded()
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth)
  ).toBeLessThanOrEqual(390)
  expect(consoleErrors).toEqual([])
})

test('plugin browsing separates management and opens a dedicated mobile installation page', async ({
  page,
}) => {
  await page.route('**/api/x/plugins', (route) => route.fulfill({ json: [] }))
  await page.route('**/api/x/plugins/catalog/repositories', (route) =>
    route.fulfill({
      json: {
        available: true,
        platform: 'darwin-arm64',
        plugins: [],
      },
    })
  )
  await page.goto('/settings/plugins')
  await expect(
    page.getByRole('tab', { name: 'Browse', exact: true })
  ).toHaveAttribute('aria-selected', 'true')
  await expect(
    page.getByRole('button', { name: 'Reload plugins', exact: true })
  ).toBeHidden()
  await page.getByRole('tab', { name: /Running/ }).click()
  await expect(
    page.getByRole('button', { name: 'Reload plugins', exact: true })
  ).toBeVisible()
  await page
    .getByRole('button', { name: 'Browse plugins', exact: true })
    .click()
  await page.setViewportSize({ width: 390, height: 844 })
  await page
    .getByRole('button', { name: 'Install from GitHub', exact: true })
    .click()
  await expect(page).toHaveURL(/settings\/plugins\/install$/)
  await page.reload()
  await expect(
    page.getByRole('heading', { name: 'Install from a custom repository' })
  ).toBeVisible()
  await expect(
    page.getByLabel('Plugin directory (optional)', { exact: true })
  ).toBeVisible()
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth)
  ).toBeLessThanOrEqual(390)
  await page.getByRole('link', { name: 'Back to plugins', exact: true }).click()
  await expect(
    page.getByRole('tab', { name: 'Browse', exact: true })
  ).toBeVisible()
})

test('catalog review survives direct navigation and blocks changed revisions', async ({
  page,
}) => {
  await page.route('**/api/x/plugins/catalog/repositories', (route) =>
    route.fulfill({
      json: {
        available: true,
        platform: 'darwin-arm64',
        plugins: [
          {
            name: 'review-demo',
            title: 'Review demo',
            summary: 'Check deployed routes',
            description: 'Find broken links.',
            author: 'Example team',
            category: 'SEO',
            repository: 'https://github.com/example/review-demo',
            latestVersion: '1.0.0',
            commit: 'a'.repeat(40),
            platforms: ['darwin-arm64'],
            screenshots: [],
            validation: { metadata: 'passed', build: 'passed' },
          },
        ],
      },
    })
  )
  await page.goto(
    '/settings/plugins/install?plugin=review-demo&commit=' + 'a'.repeat(40)
  )
  await expect(
    page.getByRole('region', { name: 'Source and compatibility' })
  ).toBeVisible()
  await page.reload()
  await expect(
    page.getByRole('region', { name: 'Source and compatibility' })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Build and install', exact: true })
  ).toBeVisible()
  await page.goto(
    '/settings/plugins/install?plugin=review-demo&commit=' + 'b'.repeat(40)
  )
  await expect(
    page.getByRole('heading', { name: 'The catalog revision has changed' })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Build and install', exact: true })
  ).toHaveCount(0)
})

test('permission checklist requires explicit core approval and leaves optional access denied', async ({
  page,
}) => {
  const requests: unknown[] = []
  await page.route('**/api/x/plugins', (route) => route.fulfill({ json: [] }))
  await page.route('**/api/x/plugins/catalog/repositories', (route) =>
    route.fulfill({
      json: {
        available: true,
        platform: 'darwin-arm64',
        plugins: [
          {
            name: 'permission-demo',
            title: 'Permission demo',
            summary: 'Inspect project routes',
            description: 'Inspect project routes',
            author: 'Example team',
            category: 'SEO',
            repository: 'https://github.com/example/permission-demo',
            latestVersion: '1.0.0',
            commit: 'a'.repeat(40),
            platforms: ['darwin-arm64'],
            screenshots: [],
            validation: { metadata: 'passed', build: 'passed' },
            permissions: [
              {
                permission: 'projects_read',
                required: true,
                reason: 'Lists projects for route inspection.',
              },
              {
                permission: 'events_read',
                required: false,
                reason: 'Automatic scans; manual scans work without it.',
              },
            ],
          },
        ],
      },
    })
  )
  await page.route('**/api/x/plugins/install/repository', (route) => {
    requests.push(route.request().postDataJSON())
    return route.fulfill({
      json: {
        name: 'permission-demo',
        version: '1.0.0',
        source_commit: 'a'.repeat(40),
        message: 'Fixture installed',
      },
    })
  })
  await page.goto(
    '/settings/plugins/install?plugin=permission-demo&commit=' + 'a'.repeat(40)
  )
  await expect(page.getByText('Required', { exact: true })).toBeVisible()
  await expect(page.getByText('Optional', { exact: true })).toBeVisible()
  await expect(
    page.getByRole('checkbox', { name: 'Use AI', exact: true })
  ).toHaveCount(0)
  await expect(
    page.getByRole('checkbox', { name: 'Read projects', exact: true })
  ).not.toBeChecked()
  await expect(
    page.getByRole('checkbox', { name: 'Receive platform events', exact: true })
  ).not.toBeChecked()
  await page.getByRole('checkbox', { name: /I trust this repository/ }).check()
  await page
    .getByRole('button', { name: 'Build and install', exact: true })
    .click()
  await expect(page.getByRole('alert')).toContainText(
    'Approve the required permissions'
  )
  expect(requests).toEqual([])
  await page
    .getByRole('checkbox', { name: 'Read projects', exact: true })
    .check()
  await page
    .getByRole('button', { name: 'Build and install', exact: true })
    .click()
  await expect.poll(() => requests.length).toBe(1)
  expect(requests[0]).toMatchObject({
    ref_name: 'a'.repeat(40),
    grants: { permissions: ['projects_read'] },
  })
})

for (const outcome of ['completed', 'failed'] as const) {
  test(`installation shows host stages and retains ${outcome} result`, async ({
    page,
  }) => {
    let stage = 'fetching_source'
    let finish: (() => void) | undefined
    let requestId = ''
    const released = new Promise<void>((resolve) => {
      finish = resolve
    })
    await page.route('**/api/x/plugins/install/repository', async (route) => {
      requestId = route.request().postDataJSON().progressId
      await released
      await route.fulfill(
        outcome === 'completed'
          ? {
              json: {
                name: 'demo',
                version: '1.0.0',
                message: 'Plugin installed',
              },
            }
          : {
              status: 400,
              json: {
                title: 'Build failed',
                detail: 'Dependency download timed out.',
              },
            }
      )
    })
    await page.route('**/api/x/plugins/install/progress/*', (route) => {
      const id = new URL(route.request().url()).pathname.split('/').pop()
      return route.fulfill({
        json: {
          id,
          status: stage === 'finished' ? outcome : 'running',
          elapsed_ms: 12000,
          stages: [
            {
              stage: 'fetching_source',
              message: 'Fetching repository source',
              status: stage === 'fetching_source' ? 'running' : 'completed',
              elapsed_ms: 2000,
            },
            ...(stage === 'fetching_source'
              ? []
              : [
                  {
                    stage: 'installing_dependencies',
                    message: 'Installing dependencies',
                    status: stage === 'finished' ? outcome : 'running',
                    elapsed_ms: 10000,
                  },
                ]),
          ],
        },
      })
    })
    await page.goto('/settings/plugins/install')
    await page
      .getByLabel('GitHub repository', { exact: true })
      .fill('https://github.com/example/demo')
    await page
      .getByRole('checkbox', { name: /I trust this repository/ })
      .check()
    await page
      .getByRole('button', { name: 'Build and install', exact: true })
      .click()
    const progress = page.getByRole('region', { name: 'Installation progress' })
    await expect(progress).toContainText('Fetching repository source')
    await expect(progress).not.toContainText('Installing dependencies')
    expect(requestId).toMatch(/^[0-9a-f-]{36}$/)
    stage = 'installing_dependencies'
    await expect(progress).toContainText('Installing dependencies')
    await expect(progress).not.toContainText('Plugin installed')
    stage = 'finished'
    finish?.()
    await expect(progress.getByRole('heading')).toHaveText(
      outcome === 'completed' ? 'Plugin installed' : 'Installation failed'
    )
    await expect(progress).toContainText('Fetching repository source')
    await expect(page).toHaveURL(/settings\/plugins\/install$/)
    if (outcome === 'completed') {
      await expect(
        page.getByRole('button', { name: 'View plugins', exact: true })
      ).toBeVisible()
    } else {
      await expect(progress.getByRole('alert')).toBeVisible()
      await expect(
        page.getByRole('button', { name: 'Build and install', exact: true })
      ).toBeEnabled()
    }
    await page.setViewportSize({ width: 390, height: 844 })
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth)
    ).toBeLessThanOrEqual(390)
  })
}
