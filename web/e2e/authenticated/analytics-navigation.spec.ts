// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

async function firstProject(page: Page) {
  const response = await page.request.get('/api/projects')
  const { projects } = await response.json()
  const project = projects[0]
  test.skip(!project, 'Requires a local test project')
  return project
}

test('analytics overview restores the default range when back navigation removes its filter', async ({
  page,
}) => {
  const project = await firstProject(page)
  await page.route(/\/has-events(?:\?|$)/, (route) =>
    route.fulfill({ json: { has_events: true } })
  )

  await page.goto(`/projects/${project.slug}/analytics`)
  const range = page.getByRole('group', {
    name: 'Date and time range',
    exact: true,
  })
  await expect(
    range.getByRole('button', { name: '24h', exact: true })
  ).toHaveAttribute('aria-pressed', 'true')

  await range.getByRole('button', { name: '6h', exact: true }).click()
  await expect(page).toHaveURL(/\?filter=6hours$/)
  await expect(
    range.getByRole('button', { name: '6h', exact: true })
  ).toHaveAttribute('aria-pressed', 'true')

  await page.goBack()
  await expect(page).toHaveURL(`/projects/${project.slug}/analytics`)
  await expect(
    range.getByRole('button', { name: '24h', exact: true })
  ).toHaveAttribute('aria-pressed', 'true')
})

test('analytics setup redirects after the first event is detected', async ({
  page,
}) => {
  const project = await firstProject(page)
  await page.route(/\/has-events(?:\?|$)/, (route) =>
    route.fulfill({ json: { has_events: true } })
  )

  await page.goto(`/projects/${project.slug}/analytics/setup`)
  await page.getByRole('button', { name: 'Continue' }).click()
  await page
    .getByRole('button', { name: "I've installed it — start listening" })
    .click()

  await expect(
    page.getByRole('heading', { name: 'First event received' })
  ).toBeVisible()
  await expect(page).toHaveURL(`/projects/${project.slug}/analytics`, {
    timeout: 5_000,
  })
})
