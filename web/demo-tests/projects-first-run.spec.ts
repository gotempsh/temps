// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { test, expect, type Page } from '@playwright/test'
async function mockApi(page: Page) {
  const writes: string[] = []
  await page.route('**/api/**', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname.replace('/api', '')
    if (request.method() !== 'GET') writes.push(path)
    const body =
      path === '/user/me'
        ? {
            id: 1,
            username: 'tester',
            name: 'Test user',
            role: 'admin',
            avatar_url: '',
            mfa_enabled: false,
          }
        : path === '/git-connections'
          ? { connections: [] }
          : path === '/projects'
            ? { projects: [], total: 0, page: 1, per_page: 9 }
            : path === '/platform/access-info'
              ? { access_mode: 'local' }
              : []
    await route.fulfill({ json: body })
  })
  return writes
}

test('first-run projects shows one starting point and reveals alternative paths on demand', async ({
  page,
}) => {
  const writes = await mockApi(page)
  await page.goto('/projects')
  const start = page.getByRole('region', { name: 'Get started with projects' })
  await expect(
    page.getByRole('heading', {
      name: 'Deploy your first application',
      level: 1,
    })
  ).toBeVisible()
  await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1)
  await expect(
    page.getByRole('link', { name: /^Create project/ })
  ).toHaveAttribute('href', '/projects/new')
  await expect(
    start.getByRole('link', { name: 'More ways to import a repository' })
  ).toHaveAttribute('href', '/projects/new')
  await expect(
    start.getByRole('link', { name: 'Import applications' })
  ).toHaveAttribute('href', '/projects/import-wizard')
  await expect(
    start.getByRole('link', { name: 'Try a demo app' })
  ).toHaveAttribute(
    'href',
    '/projects/new?source=templates&template=observability-starter'
  )
  await expect(page.getByLabel('Personal access token')).toHaveCount(0)
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await start.getByRole('button', { name: 'Use CLI' }).click()
  await expect(
    page.getByRole('dialog', { name: 'Deploy from your terminal' })
  ).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(start.getByRole('button', { name: 'Use CLI' })).toBeFocused()
  for (const name of [
    'Coolify',
    'Dokploy',
    'CapRover',
    'Kubernetes',
    'Docker',
  ]) {
    await expect(
      start.getByRole('link', { name: `Import from ${name}`, exact: true })
    ).toBeVisible()
  }
  await expect(
    start.getByRole('link', { name: 'Connect an AI agent' })
  ).toHaveAttribute('href', '/setup/ai')
  await expect(
    start.getByRole('button', { name: 'Choose file', exact: true })
  ).toBeVisible()
  expect(writes).toHaveLength(0)
  await start.getByRole('link', { name: 'Import applications' }).click()
  await expect(page).toHaveURL(/projects\/import-wizard/)
})

test('first-run layout fits mobile and desktop in both themes', async ({
  page,
}) => {
  await mockApi(page)
  for (const theme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme: theme })
    for (const width of [390, 1600]) {
      await page.setViewportSize({ width, height: 1000 })
      await page.goto('/projects')
      await expect(
        page.getByRole('heading', { name: 'Deploy your first application' })
      ).toBeVisible()
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth
        )
      ).toBe(true)
      await page.screenshot({
        path: `/tmp/temps-first-run-${theme}-${width}.png`,
      })
      await page.getByRole('button', { name: 'Use CLI' }).click()
      await expect(page.getByRole('dialog')).toBeVisible()
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth
        )
      ).toBe(true)
    }
  }
})

test('a project request failure offers retry instead of first-run onboarding', async ({
  page,
}) => {
  await mockApi(page)
  await page.route('**/api/projects?*', (route) =>
    route.fulfill({
      status: 503,
      json: { detail: 'Project service unavailable' },
    })
  )
  await page.goto('/projects')
  await expect(
    page.getByText('Projects could not be loaded', { exact: true })
  ).toBeVisible({ timeout: 20000 })
  await expect(
    page.getByRole('button', { name: 'Retry loading projects' })
  ).toBeVisible()
  await expect(
    page.getByRole('heading', { name: 'Deploy your first application' })
  ).toHaveCount(0)
})

test('initial bundle loading is styled and accessible', async ({ page }) => {
  await mockApi(page)
  let release!: () => void
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route('**/static/js/async/**', async (route) => {
    await gate
    await route.continue()
  })
  await page.goto('/projects', { waitUntil: 'domcontentloaded' })
  try {
    const loading = page.getByRole('status', { name: 'Loading application' })
    await expect(loading).toBeVisible()
    await expect(loading.getByText('Temps', { exact: true })).toBeVisible()
    expect(
      await loading.evaluate((element) => getComputedStyle(element).display)
    ).toBe('flex')
    await expect(page.getByText('Loading Temps…', { exact: true })).toHaveCount(
      0
    )
    await page.screenshot({ path: '/tmp/temps-app-loading.png' })
  } finally {
    release()
  }
  await expect(
    page.getByRole('heading', { name: 'Deploy your first application' })
  ).toBeVisible()
})

test('onboarding uses the real Drop files flow and preserves the selected file', async ({
  page,
}) => {
  const writes = await mockApi(page)
  await page.setViewportSize({ width: 1600, height: 1200 })
  let inspected = false
  await page.route('**/api/drop/inspect', async (route) => {
    inspected = true
    expect(route.request().headers()['content-type']).toContain(
      'multipart/form-data'
    )
    await route.fulfill({
      json: {
        suggestedName: 'hello-site',
        candidates: [
          {
            preset: 'static',
            label: 'Static site',
            directory: '.',
            isStatic: true,
            confidence: 'high',
            reason: 'HTML entry point found',
          },
        ],
      },
    })
  })
  await page.goto('/projects')
  await page.screenshot({
    path: '/tmp/temps-onboarding-real-drop-desktop.png',
    fullPage: true,
  })
  await page
    .getByLabel('Project archive or HTML', { exact: true })
    .setInputFiles({
      name: 'index.html',
      mimeType: 'text/html',
      buffer: Buffer.from('<h1>Hello</h1>'),
    })
  await expect(page).toHaveURL(/projects\/new\?source=drop/)
  await expect(
    page.getByRole('heading', { name: 'Configure drop', exact: true })
  ).toBeVisible()
  await expect(page.getByLabel('Project name', { exact: true })).toHaveValue(
    'hello-site'
  )
  expect(inspected).toBe(true)
  expect(writes).toHaveLength(0)
  await page.screenshot({
    path: '/tmp/temps-onboarding-real-drop-configure.png',
    fullPage: true,
  })
})

test('Drop files reports inspection failure and retains files for retry', async ({
  page,
}) => {
  const writes = await mockApi(page)
  await page.route('**/api/drop/inspect', async (route) => {
    await route.fulfill({
      status: 422,
      json: { detail: 'No supported preset found in this archive' },
    })
  })
  await page.goto('/projects')
  await page
    .getByLabel('Project archive or HTML', { exact: true })
    .setInputFiles({
      name: 'index.html',
      mimeType: 'text/html',
      buffer: Buffer.from('<h1>Hello</h1>'),
    })
  await expect(
    page.getByText('No supported preset found in this archive', { exact: true })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Retry preset detection', exact: true })
  ).toBeEnabled()
  expect(writes).toHaveLength(0)
})

test('stateless instances explain why local uploads are unavailable', async ({
  page,
}) => {
  await mockApi(page)
  await page.route('**/api/platform/features', (route) =>
    route.fulfill({ json: { stateless: true } })
  )
  await page.goto('/projects')
  await expect(
    page.getByRole('button', { name: 'Choose folder', exact: true })
  ).toBeDisabled()
  await expect(
    page.getByRole('link', { name: 'Deploy a prebuilt image', exact: true })
  ).toHaveAttribute('href', '/projects/new?source=manual')
})

async function liveGitFixtures(page: Page) {
  const requests: URL[] = []
  await page.route('**/api/presets', (route) =>
    route.fulfill({
      json: {
        presets: [
          { slug: 'nextjs', label: 'Next.js' },
          { slug: 'dockerfile', label: 'Dockerfile' },
        ],
        total: 2,
      },
    })
  )
  await page.route('**/api/git-providers', (route) =>
    route.fulfill({
      json: [
        { id: 1, provider_type: 'github', name: 'Host GitHub' },
        { id: 2, provider_type: 'gitlab', name: 'GitLab' },
      ],
    })
  )
  await page.route('**/api/git-connections*', (route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith('/repositories')) return route.fallback()
    return route.fulfill({
      json: {
        connections: [
          {
            id: 11,
            provider_id: 1,
            account_name: 'connected-owner',
            is_active: true,
            is_expired: false,
            has_authenticated_credentials: true,
            syncing: false,
          },
          {
            id: 12,
            provider_id: 1,
            account_name: 'second-owner',
            is_active: true,
            is_expired: false,
            has_authenticated_credentials: true,
            syncing: false,
          },
          {
            id: 21,
            provider_id: 2,
            account_name: 'gitlab-owner',
            is_active: true,
            is_expired: false,
            has_authenticated_credentials: true,
            syncing: false,
          },
        ],
        total_count: 3,
      },
    })
  })
  await page.route('**/api/git-connections/*/repositories?*', (route) => {
    const url = new URL(route.request().url())
    requests.push(url)
    const account = url.pathname.includes('/21/')
      ? 'gitlab-owner'
      : url.pathname.includes('/12/')
        ? 'second-owner'
        : 'connected-owner'
    const all = Array.from({ length: 7 }, (_, i) => ({
      id: 100 + i,
      full_name: `${account}/app-${i + 1}`,
      name: `app-${i + 1}`,
      owner: account,
      default_branch: 'main',
      private: i % 2 === 0,
      language: 'TypeScript',
      updated_at: new Date(Date.now() - i * 2 * 86400000).toISOString(),
      preset:
        i === 0
          ? null
          : [
              {
                preset: i === 6 ? 'nextjs' : 'dockerfile',
                presetLabel: i === 6 ? 'Next.js' : 'Dockerfile',
                path: './',
              },
            ],
    }))
    const matches = all.filter(
      (r) =>
        r.full_name.includes(url.searchParams.get('search') || '') &&
        (!url.searchParams.get('preset') ||
          (url.searchParams.get('preset') === '__undetected__'
            ? r.preset === null
            : r.preset?.some(
                (p) => p.preset === url.searchParams.get('preset')
              ))) &&
        (!url.searchParams.get('updated_after') ||
          r.updated_at >= url.searchParams.get('updated_after')!)
    )
    const pageNo = Number(url.searchParams.get('page') || 1)
    return route.fulfill({
      json: {
        repositories: matches.slice((pageNo - 1) * 5, pageNo * 5),
        total_count: matches.length,
      },
    })
  })
  return requests
}

test('connected repositories use server search, pagination, provider and account selection', async ({
  page,
}) => {
  const writes = await mockApi(page)
  const requests = await liveGitFixtures(page)
  await page.goto('/projects')
  await expect(
    page.getByRole('link', {
      name: 'Configure connected-owner/app-1',
      exact: true,
    })
  ).toHaveAttribute('href', '/projects/import/100')
  await expect(
    page.getByText('Sample repositories', { exact: false })
  ).toHaveCount(0)
  await page
    .getByRole('button', { name: 'Go to next page', exact: true })
    .click()
  await expect(
    page.getByRole('link', {
      name: 'Configure connected-owner/app-6',
      exact: true,
    })
  ).toBeVisible()
  await expect(page).toHaveURL(/repoPage=2/)
  await page.reload()
  await expect(
    page.getByRole('link', {
      name: 'Configure connected-owner/app-6',
      exact: true,
    })
  ).toBeVisible()
  await page.getByRole('textbox', { name: 'Search repositories' }).fill('app-7')
  await expect(
    page.getByRole('link', {
      name: 'Configure connected-owner/app-7',
      exact: true,
    })
  ).toBeVisible()
  await expect
    .poll(() =>
      requests.some(
        (url) =>
          url.searchParams.get('search') === 'app-7' &&
          url.searchParams.get('page') === '1'
      )
    )
    .toBe(true)
  await page.getByRole('tab', { name: 'GitLab', exact: true }).click()
  await expect(
    page.getByRole('textbox', { name: 'Search repositories' })
  ).toHaveValue('')
  await expect(
    page.getByRole('link', {
      name: 'Configure gitlab-owner/app-1',
      exact: true,
    })
  ).toBeVisible()
  await page.getByRole('tab', { name: 'GitHub', exact: true }).click()
  await page.getByRole('combobox', { name: 'Git account' }).click()
  await page.getByRole('option', { name: 'second-owner' }).click()
  await expect(
    page.getByRole('link', {
      name: 'Configure second-owner/app-1',
      exact: true,
    })
  ).toBeVisible()
  await expect(page).toHaveURL(/gitConnection=12/)
  expect(writes).toHaveLength(0)
  await page.setViewportSize({ width: 1600, height: 1100 })
  await page.screenshot({
    path: '/tmp/temps-onboarding-connected-repositories.png',
    fullPage: true,
  })
})

test('repository failures offer retry and never display fake or stale repositories', async ({
  page,
}) => {
  await mockApi(page)
  await liveGitFixtures(page)
  let fail = true
  await page.route('**/api/git-connections/11/repositories?*', (route) =>
    fail
      ? route.fulfill({ status: 503, json: { detail: 'Unavailable' } })
      : route.fallback()
  )
  await page.goto('/projects')
  await expect(
    page.getByRole('button', { name: 'Retry repositories' })
  ).toBeVisible({ timeout: 20000 })
  await expect(
    page.getByRole('link', { name: /^Configure connected-owner/ })
  ).toHaveCount(0)
  fail = false
  await page.getByRole('button', { name: 'Retry repositories' }).click()
  await expect(
    page.getByRole('link', {
      name: 'Configure connected-owner/app-1',
      exact: true,
    })
  ).toBeVisible()
})

test('unconnected provider remains discoverable with setup action', async ({
  page,
}) => {
  await mockApi(page)
  await page.goto('/projects')
  await expect(
    page.getByRole('link', { name: 'Connect GitHub', exact: true })
  ).toHaveAttribute('href', '/git-providers/add')
  await page.getByRole('tab', { name: 'GitLab', exact: true }).click()
  await expect(
    page.getByRole('link', { name: 'Connect GitLab', exact: true })
  ).toHaveAttribute('href', '/git-providers/add')
  await expect(page.getByText('example-team', { exact: true })).toHaveCount(0)
})

test('saved presets and updated dates filter all pages and keep account settings on one row', async ({
  page,
}) => {
  await mockApi(page)
  const requests = await liveGitFixtures(page)
  await page.setViewportSize({ width: 1600, height: 1100 })
  await page.goto('/projects')
  const card = page.getByRole('region', { name: 'Deploy from a repository' })
  await expect(
    card.getByText('Preset not detected yet', { exact: true })
  ).toBeVisible()
  await expect(card.locator('time').first()).toHaveAttribute('title', /.+/)
  const account = await card
    .getByRole('combobox', { name: 'Git account' })
    .boundingBox()
  const settings = await card
    .getByRole('link', { name: 'Settings', exact: true })
    .boundingBox()
  expect(Math.abs(account!.y - settings!.y)).toBeLessThan(10)
  await card
    .getByRole('button', { name: 'Go to next page', exact: true })
    .click()
  await expect(page).toHaveURL(/repoPage=2/)
  await card.getByRole('button', { name: 'Filters', exact: true }).click()
  await card.getByRole('combobox', { name: 'Detected preset' }).click()
  await page.getByRole('option', { name: 'Next.js', exact: true }).click()
  await expect(
    card.getByRole('link', {
      name: 'Configure connected-owner/app-7',
      exact: true,
    })
  ).toBeVisible()
  await expect
    .poll(() =>
      requests.some(
        (url) =>
          url.searchParams.get('preset') === 'nextjs' &&
          url.searchParams.get('page') === '1'
      )
    )
    .toBe(true)
  await expect(card.getByText('Showing 1–1 of 1')).toBeVisible()
  await card.getByRole('combobox', { name: 'Last updated' }).click()
  await page.getByRole('option', { name: 'Last 7 days', exact: true }).click()
  await expect(
    card.getByText('No matching repositories.', { exact: true })
  ).toBeVisible()
  await expect
    .poll(() =>
      requests.some(
        (url) =>
          url.searchParams.get('preset') === 'nextjs' &&
          Boolean(url.searchParams.get('updated_after'))
      )
    )
    .toBe(true)
  await page.reload()
  await expect(
    card.getByRole('combobox', { name: 'Detected preset' })
  ).toContainText('Next.js')
  await expect(
    card.getByRole('combobox', { name: 'Last updated' })
  ).toContainText('Last 7 days')
  await card.getByRole('button', { name: 'Clear filters', exact: true }).click()
  await expect(
    card.getByRole('link', {
      name: 'Configure connected-owner/app-1',
      exact: true,
    })
  ).toBeVisible()
  await card.getByRole('button', { name: 'Filters', exact: true }).click()
  await page.screenshot({
    path: '/tmp/temps-repositories-presets-desktop.png',
    fullPage: true,
  })
  await page.setViewportSize({ width: 390, height: 1000 })
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth
    )
  ).toBe(true)
  await page.screenshot({
    path: '/tmp/temps-repositories-presets-mobile.png',
    fullPage: true,
  })
})

for (const appOnly of [true, false]) {
  test(`GitHub App accounts share the GitHub tab (${appOnly ? 'App only' : 'App and PAT'})`, async ({
    page,
  }) => {
    await mockApi(page)
    await liveGitFixtures(page)
    await page.route('**/api/git-providers', (route) =>
      route.fulfill({
        json: [
          { id: 1, provider_type: 'github_app', name: 'Workspace App' },
          { id: 3, provider_type: 'github', name: 'Personal token' },
        ],
      })
    )
    await page.route('**/api/git-connections?*', (route) =>
      route.fulfill({
        json: {
          connections: [
            {
              id: 11,
              provider_id: 1,
              account_name: 'app-owner',
              is_active: true,
              is_expired: false,
              has_authenticated_credentials: true,
              syncing: false,
            },
            ...(!appOnly
              ? [
                  {
                    id: 12,
                    provider_id: 3,
                    account_name: 'token-owner',
                    is_active: true,
                    is_expired: false,
                    has_authenticated_credentials: true,
                    syncing: false,
                  },
                ]
              : []),
          ],
          total_count: appOnly ? 1 : 2,
        },
      })
    )
    await page.goto('/projects?gitProvider=github_app')
    const card = page.getByRole('region', { name: 'Deploy from a repository' })
    await expect(
      card.getByRole('tab', { name: 'GitHub', exact: true })
    ).toHaveAttribute('aria-selected', 'true')
    await expect(
      card.getByRole('link', { name: 'Configure connected-owner/app-1' })
    ).toBeVisible()
    await card.getByRole('tab', { name: 'GitHub', exact: true }).click()
    await expect(
      card.getByRole('combobox', { name: 'Git account' })
    ).toContainText('app-owner')
    await expect(
      card.getByRole('link', { name: 'Connect GitHub', exact: true })
    ).toHaveCount(0)
    if (!appOnly) {
      await card.getByRole('combobox', { name: 'Git account' }).click()
      await page
        .getByRole('option', { name: 'token-owner', exact: true })
        .click()
      await expect(
        card.getByRole('link', { name: 'Configure second-owner/app-1' })
      ).toBeVisible()
      await card.getByRole('combobox', { name: 'Git account' }).click()
      await page.getByRole('option', { name: 'app-owner', exact: true }).click()
      await expect(
        card.getByRole('link', { name: 'Configure connected-owner/app-1' })
      ).toBeVisible()
    }
  })
}

test('repository pagination has one mobile row and clamps a saved page after results shrink', async ({
  page,
}) => {
  await mockApi(page)
  await liveGitFixtures(page)
  await page.setViewportSize({ width: 390, height: 1000 })
  await page.goto('/projects?repoPage=99')
  const pagination = page.getByRole('navigation', {
    name: 'Repository pagination',
  })
  await expect(page).toHaveURL(/repoPage=2/)
  await expect(
    pagination.getByLabel('Page 2 of 2', { exact: true })
  ).toBeVisible()
  const previous = pagination.getByRole('button', {
    name: 'Previous page',
    exact: true,
  })
  const next = pagination.getByRole('button', {
    name: 'Next page',
    exact: true,
  })
  await expect(previous).toBeEnabled()
  await expect(next).toBeDisabled()
  const previousBounds = await previous.boundingBox()
  const nextBounds = await next.boundingBox()
  expect(Math.abs(previousBounds!.y - nextBounds!.y)).toBeLessThan(2)
  await previous.click()
  await expect(page).toHaveURL(/repoPage=1/)
  await expect(
    pagination.getByLabel('Page 1 of 2', { exact: true })
  ).toBeVisible()
  await expect(next).toBeEnabled()
  await expect(pagination.getByLabel('Page number')).toBeHidden()
})
