// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { test, expect, type Page } from '@playwright/test'

const environments = [
  { id: 1, name: 'production', is_preview: false },
  { id: 2, name: 'preview', is_preview: true },
]
const variable = (
  id: number,
  key: string,
  ids: number[],
  preview = false,
  secret = false
) => ({
  id,
  key,
  environments: environments.filter((e) => ids.includes(e.id)),
  include_in_preview: preview,
  is_secret: secret,
  value: '********',
  updated_at: '2026-01-01T00:00:00Z',
})
const vars = [
  variable(1, 'PROD_ONLY', [1]),
  variable(2, 'SHARED_VALUE', [1, 2]),
  variable(3, 'PREVIEW_DEFAULT', [], true),
  variable(4, 'WRITE_ONLY', [1, 2], false, true),
]
const fullValue = 'example-value-'.repeat(30) + '\nsecond line preserved'
async function mockApi(page: Page) {
  const revealed: string[] = []
  const deleted: string[] = []
  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace('/api', '')
    if (route.request().method() === 'DELETE') {
      deleted.push(path)
      await route.fulfill({ status: 204 })
      return
    }
    if (path.endsWith('/value')) {
      revealed.push(path)
      await route.fulfill({ json: { value: fullValue } })
      return
    }
    if (path.endsWith('/last-deployment')) {
      await route.fulfill({ status: 404, json: { detail: 'No deployments' } })
      return
    }
    const json =
      path === '/user/me'
        ? {
            id: 1,
            username: 'tester',
            name: 'Test user',
            role: 'admin',
            mfa_enabled: false,
          }
        : path === '/projects/by-slug/example-app'
          ? {
              id: 1,
              slug: 'example-app',
              name: 'Example app',
              preset: 'nextjs',
              environments,
            }
          : path === '/projects/1/env-vars'
            ? vars
            : path === '/projects/1/environments'
              ? environments
              : path === '/projects/1/env-vars/resolved'
                ? [
                    {
                      key: 'DATABASE_URL',
                      value_preview: '********',
                      environments,
                      include_in_preview: false,
                      source: {
                        type: 'integration',
                        service: {
                          service_id: 7,
                          service_name: 'Example database',
                          service_type: 'postgres',
                          service_updated_at: '2026-01-01T00:00:00Z',
                        },
                      },
                    },
                  ]
                : path === '/projects'
                  ? { projects: [], total: 0 }
                  : path === '/git-connections'
                    ? { connections: [] }
                    : path === '/platform/access-info'
                      ? { access_mode: 'local' }
                      : []
    await route.fulfill({ json })
  })
  return { revealed, deleted }
}
const path = '/projects/example-app/environment-variables'
const row = (page: Page, key: string) =>
  page.getByRole('row').filter({
    has: page.getByRole('link', { name: `View ${key} details`, exact: true }),
  })

test('environment and name filters persist, include preview inheritance, and limit bulk selection', async ({
  page,
}) => {
  const { deleted } = await mockApi(page)
  await page.goto(path)
  await expect(row(page, 'PROD_ONLY')).toBeVisible()
  await expect(row(page, 'PREVIEW_DEFAULT')).toHaveCount(0)
  await page.getByRole('button', { name: 'Filters', exact: true }).click()
  await page.getByLabel('Environment', { exact: true }).click()
  await page.getByRole('option', { name: 'preview', exact: true }).click()
  await expect(row(page, 'PROD_ONLY')).toHaveCount(0)
  await expect(row(page, 'PREVIEW_DEFAULT')).toBeVisible()
  await expect(row(page, 'SHARED_VALUE')).toBeVisible()
  const search = page.getByRole('textbox', {
    name: 'Filter environment variables by name',
  })
  await search.fill('shared')
  await expect(row(page, 'PREVIEW_DEFAULT')).toHaveCount(0)
  await page.reload()
  await page.getByRole('button', { name: 'Filters', exact: true }).click()
  await expect(search).toHaveValue('shared')
  await expect(row(page, 'SHARED_VALUE')).toBeVisible()
  await expect(row(page, 'PROD_ONLY')).toHaveCount(0)
  await page
    .getByRole('checkbox', {
      name: 'Select all environment variables',
      exact: true,
    })
    .click()
  await page.getByRole('button', { name: 'Delete 1 selected' }).click()
  const dialog = page.getByRole('alertdialog')
  await expect(dialog).toContainText('SHARED_VALUE')
  await expect(dialog).not.toContainText('PROD_ONLY')
  await dialog.getByRole('button', { name: /delete/i }).click()
  expect(deleted).toEqual(['/projects/1/env-vars/2'])
  await search.fill('not_present')
  await expect(
    page.getByText('No variables match this environment and search.')
  ).toBeVisible()
  await page.getByRole('button', { name: 'Clear filters' }).click()
  await expect(row(page, 'PROD_ONLY')).toBeVisible()
})

test('full values are available only after reveal and on demand, including integration values', async ({
  page,
}) => {
  const { revealed } = await mockApi(page)
  await page.goto(path)
  await expect(row(page, 'SHARED_VALUE')).toBeVisible()
  expect(revealed).toHaveLength(0)
  await expect(
    page.getByRole('button', { name: 'View full SHARED_VALUE value' })
  ).toHaveCount(0)
  await expect(
    row(page, 'WRITE_ONLY').getByRole('button', { name: /reveal/i })
  ).toHaveCount(0)
  await page
    .getByRole('button', { name: 'Reveal SHARED_VALUE', exact: true })
    .click()
  await page
    .getByRole('button', { name: 'View full SHARED_VALUE value', exact: true })
    .click()
  await expect(page.getByRole('dialog').locator('pre')).toHaveText(fullValue)
  await page.keyboard.press('Escape')
  await page
    .getByRole('button', { name: 'Hide SHARED_VALUE', exact: true })
    .click()
  await expect(
    page.getByRole('button', { name: 'View full SHARED_VALUE value' })
  ).toHaveCount(0)
  await page
    .getByRole('button', { name: 'Reveal DATABASE_URL', exact: true })
    .click()
  await page
    .getByRole('button', { name: 'View full DATABASE_URL value', exact: true })
    .click()
  await expect(page.getByRole('dialog').locator('pre')).toHaveText(fullValue)
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: 'Filters', exact: true }).click()
  await page.getByLabel('Environment', { exact: true }).click()
  await page.getByRole('option', { name: 'preview', exact: true }).click()
  await expect(
    page.getByRole('button', { name: 'View full DATABASE_URL value' })
  ).toHaveCount(0)
  expect(revealed).toHaveLength(2)
})

test('failed reveal never exposes a full-value action; failed list offers retry', async ({
  page,
}) => {
  await mockApi(page)
  await page.route('**/api/projects/1/env-vars/SHARED_VALUE/value*', (route) =>
    route.fulfill({ status: 403, json: { detail: 'Reveal denied' } })
  )
  await page.goto(path)
  await page
    .getByRole('button', { name: 'Reveal SHARED_VALUE', exact: true })
    .click()
  await expect(page.getByText('Failed to reveal SHARED_VALUE')).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'View full SHARED_VALUE value' })
  ).toHaveCount(0)
  await page.route('**/api/projects/1/env-vars', (route) =>
    route.fulfill({ status: 503, json: { detail: 'Variables unavailable' } })
  )
  await page.reload()
  await expect(
    page.getByRole('button', { name: 'Retry variables' })
  ).toBeVisible()
  await expect(
    page.getByText('No environment variables', { exact: true })
  ).toHaveCount(0)
})

test('long-value dialog fits mobile and desktop in both themes', async ({
  page,
}) => {
  await mockApi(page)
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [390, 1600]) {
      await page.setViewportSize({ width, height: 900 })
      await page.goto(path)
      await page
        .getByRole('button', { name: 'Reveal SHARED_VALUE', exact: true })
        .click()
      await page
        .getByRole('button', {
          name: 'View full SHARED_VALUE value',
          exact: true,
        })
        .click()
      const dialog = page.getByRole('dialog')
      await expect(dialog.locator('pre')).toHaveText(fullValue)
      expect(
        await dialog.evaluate((el) => {
          const bounds = el.getBoundingClientRect()
          return bounds.left >= 0 && bounds.right <= innerWidth
        })
      ).toBe(true)
      expect(
        await dialog
          .locator('pre')
          .evaluate((el) => el.scrollWidth <= el.clientWidth)
      ).toBe(true)
      await page.screenshot({
        path: `/tmp/temps-env-value-${colorScheme}-${width}.png`,
      })
      await page.keyboard.press('Escape')
      await expect(dialog).toHaveCount(0)
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth
        )
      ).toBe(true)
    }
  }
})

test('filters and full-value dialog stay closed until requested', async ({
  page,
}) => {
  await mockApi(page)
  await page.setViewportSize({ width: 1600, height: 1000 })
  await page.goto(path + '?environment=2&q=shared')
  const filters = page.getByRole('button', { name: 'Filters', exact: true })
  await expect(row(page, 'SHARED_VALUE')).toBeVisible()
  await expect(filters).toHaveAttribute('aria-expanded', 'false')
  await expect(filters).toContainText('preview')
  await expect(
    page.getByRole('textbox', { name: 'Filter environment variables by name' })
  ).toBeHidden()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page
    .getByRole('button', { name: 'Reveal SHARED_VALUE', exact: true })
    .click()
  await expect(
    page.getByRole('button', { name: 'View full SHARED_VALUE value' })
  ).toBeVisible()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.screenshot({
    path: '/tmp/temps-variables-compact.png',
    fullPage: true,
  })
  await filters.click()
  await expect(
    page.getByRole('textbox', { name: 'Filter environment variables by name' })
  ).toHaveValue('shared')
  await page.screenshot({
    path: '/tmp/temps-variables-filters-open.png',
    fullPage: true,
  })
  await filters.click()
  await expect(row(page, 'SHARED_VALUE')).toBeVisible()
  await expect(row(page, 'PROD_ONLY')).toHaveCount(0)
  await expect(
    page.getByRole('textbox', { name: 'Filter environment variables by name' })
  ).toBeHidden()
})

test('short and empty values stay inline; clipped and multiline values offer expansion', async ({
  page,
}) => {
  await mockApi(page)
  let value = 'short-value'
  await page.route('**/api/projects/1/env-vars/SHARED_VALUE/value*', (route) =>
    route.fulfill({ json: { value } })
  )
  await page.goto(path)
  for (const sample of [
    'short-value',
    '',
    'x'.repeat(200),
    'line one\nline two',
  ]) {
    value = sample
    await page
      .getByRole('button', { name: 'Reveal SHARED_VALUE', exact: true })
      .click()
    const currentRow = row(page, 'SHARED_VALUE')
    await expect(
      currentRow.getByText(sample || '(empty value)', { exact: true })
    ).toBeVisible()
    const expand = page.getByRole('button', {
      name: 'View full SHARED_VALUE value',
      exact: true,
    })
    if (sample.length > 100 || sample.includes('\n')) {
      await expect(expand).toBeVisible()
      await expect(page.getByRole('dialog')).toHaveCount(0)
      await expand.click()
      await expect(page.getByRole('dialog').locator('pre')).toHaveText(sample)
      await page.keyboard.press('Escape')
    } else {
      await expect(expand).toHaveCount(0)
      await expect(page.getByRole('dialog')).toHaveCount(0)
    }
    await page
      .getByRole('button', { name: 'Hide SHARED_VALUE', exact: true })
      .click()
  }
})
