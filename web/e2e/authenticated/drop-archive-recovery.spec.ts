// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import { strToU8, zipSync } from 'fflate'

const validArchive = {
  name: 'project.zip',
  mimeType: 'application/zip',
  buffer: Buffer.from(zipSync({ 'index.html': strToU8('<h1>Project</h1>') })),
}
const inspection = {
  suggestedName: 'project',
  candidates: [
    {
      directory: '.',
      preset: 'static',
      label: 'Static Site',
      confidence: 'high',
      reason: 'Root index.html',
      isStatic: true,
    },
  ],
}

test('invalid ZIP feedback replaces the archive instead of reinspecting its bytes', async ({
  page,
}) => {
  let inspections = 0
  await page.route('**/api/drop/inspect', async (route) => {
    inspections += 1
    await route.fulfill(
      inspections === 1
        ? {
            status: 400,
            json: {
              title: 'Invalid ZIP Archive',
              detail: 'ZIP end-of-central-directory record not found',
            },
          }
        : { json: inspection }
    )
  })
  await page.goto('/drop')
  await page.getByLabel('Project archive or HTML').setInputFiles({
    name: 'broken.zip',
    mimeType: 'application/zip',
    buffer: Buffer.from('This is plain text, not a ZIP archive'),
  })
  const feedback = page.getByRole('alert')
  await expect(feedback).toContainText('Invalid ZIP archive')
  await expect(feedback).toContainText(
    'Create a new ZIP from your project folder'
  )
  await expect(
    page.getByRole('button', { name: 'Retry preset detection' })
  ).toHaveCount(0)
  await feedback.getByText('Technical details', { exact: true }).click()
  await expect(feedback).toContainText(
    'ZIP end-of-central-directory record not found'
  )

  await page.getByRole('button', { name: 'Choose another archive' }).click()
  await expect(
    page.getByRole('button', { name: 'Choose file', exact: true })
  ).toBeVisible()
  expect(inspections).toBe(1)
  await expect(
    page.getByText('Invalid ZIP archive', { exact: true })
  ).toHaveCount(0)
  await page.getByLabel('Project archive or HTML').setInputFiles(validArchive)
  await expect(
    page.getByRole('button', { name: 'Deploy Static Site', exact: true })
  ).toBeEnabled()
  await expect(
    page.getByText('ZIP end-of-central-directory record not found')
  ).toHaveCount(0)
  expect(inspections).toBe(2)
})

test('temporary inspection failures retain a working retry on the same archive', async ({
  page,
}) => {
  let inspections = 0
  await page.route('**/api/drop/inspect', async (route) => {
    inspections += 1
    await route.fulfill(
      inspections === 1
        ? {
            status: 503,
            json: { title: 'Service Unavailable', detail: 'Try again shortly' },
          }
        : { json: inspection }
    )
  })
  await page.goto('/drop')
  await page.getByLabel('Project archive or HTML').setInputFiles(validArchive)
  await expect(page.getByRole('alert')).toContainText('Try again shortly')
  await page
    .getByRole('button', { name: 'Retry preset detection', exact: true })
    .click()
  await expect(
    page.getByRole('button', { name: 'Deploy Static Site', exact: true })
  ).toBeEnabled()
  expect(inspections).toBe(2)
})
