// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

test('observability starter submits its tagged image and exposes collapsed validation errors', async ({
  page,
}) => {
  const errors: string[] = []
  const submissions: Record<string, unknown>[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  await page.route(/\/api\/external-services(?:\?.*)?$/, (route) =>
    route.fulfill({
      json: [
        {
          id: 101,
          name: 'Template Postgres',
          service_type: 'postgres',
          status: 'running',
          created_at: '2026-09-01T00:00:00Z',
          updated_at: '2026-09-01T00:00:00Z',
        },
      ],
    })
  )
  await page.route(
    '**/api/external-services/101/preview-environment-names',
    (route) => route.fulfill({ json: ['DATABASE_URL'] })
  )
  await page.route('**/api/projects/from-template', (route) => {
    submissions.push(route.request().postDataJSON())
    return route.fulfill({
      status: 400,
      json: {
        title: 'Creation rejected',
        detail: 'Template creation test response',
        status: 400,
      },
    })
  })
  await page.goto(
    '/projects/new?source=templates&template=observability-starter'
  )
  const create = page.getByRole('button', {
    name: 'Create Project from Template',
    exact: true,
  })
  await expect(create).toBeEnabled()
  await expect(
    page.getByText('Template Postgres will be linked automatically.', {
      exact: true,
    })
  ).toBeVisible()
  await expect(
    page
      .getByRole('button', { name: /environment variables provided by Temps/ })
      .getByText(/^\d+ variables$/)
  ).toBeVisible()
  await create.click()
  await expect.poll(() => submissions.length).toBe(1)
  expect(submissions[0]).toMatchObject({
    template_slug: 'observability-starter',
    image: 'ghcr.io/gotempsh/observability-starter:latest',
    storage_service_ids: [101],
  })
  await expect(
    page.getByText('Failed to create project: Template creation test response')
  ).toBeVisible()

  const configure = page.getByRole('button', {
    name: /Deploys instantly from a prebuilt image/,
  })
  await configure.click()
  await page.getByLabel('Container image', { exact: true }).fill('')
  await configure.click()
  await expect(page.getByLabel('Container image', { exact: true })).toBeHidden()
  await create.click()
  await expect(
    page.getByLabel('Container image', { exact: true })
  ).toBeVisible()
  await expect(
    page.getByText('Image reference is required', { exact: true })
  ).toBeVisible()
  expect(submissions).toHaveLength(1)
  expect(errors).toEqual([])
})
