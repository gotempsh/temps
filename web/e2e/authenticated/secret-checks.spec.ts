// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Page } from '@playwright/test'
import { expect, test, uniqueSlug } from '../fixtures'

// Detection and the first run of an automatic check happen in the server's
// background loop (one tick every 5 seconds), so the first result can take a
// few ticks to appear.
const BACKGROUND_LOOP_TIMEOUT = 90_000

/** An unsigned JWT; local expiry checks read only `exp`, `nbf` and `iat`. */
function jwtExpiringIn(days: number): string {
  const encode = (value: object) =>
    Buffer.from(JSON.stringify(value)).toString('base64url')
  const now = Math.floor(Date.now() / 1000)
  return [
    encode({ alg: 'HS256', typ: 'JWT' }),
    encode({ sub: 'e2e-service', iat: now, exp: now + days * 86_400 }),
    'c2lnbmF0dXJl',
  ].join('.')
}

async function createProject(page: Page, name: string) {
  const created = await page.request.post('/api/projects', {
    data: {
      name,
      directory: '/',
      main_branch: 'main',
      preset: 'dockerfile',
      source_type: 'docker_image',
      docker_image: 'nginx:alpine',
      automatic_deploy: false,
    },
  })
  expect(created.ok(), await created.text()).toBe(true)
  return (await created.json()) as { id: number; slug: string }
}

async function createSecret(
  page: Page,
  projectId: number,
  key: string,
  value: string
) {
  const created = await page.request.post(
    `/api/projects/${projectId}/secrets`,
    {
      data: { key, value, environment_ids: [], include_in_preview: false },
    }
  )
  expect(created.ok(), await created.text()).toBe(true)
  return (await created.json()) as { id: number }
}

async function deleteProject(page: Page, projectId: number) {
  // Secrets, checks and their history are removed with the project.
  const removed = await page.request.delete(`/api/projects/${projectId}`)
  expect(removed.ok()).toBe(true)
}

test.describe('secret credential checks', () => {
  test.describe.configure({ timeout: 180_000 })

  test('a JWT saved as a secret gets an automatic expiry check that warns before it expires', async ({
    page,
    consoleErrors,
    httpFailures,
  }, testInfo) => {
    const project = await createProject(
      page,
      uniqueSlug('secret-checks', testInfo)
    )
    try {
      await page.goto(`/projects/${project.slug}/settings/secrets`)
      await page
        .getByRole('button', { name: /New secret/ })
        .first()
        .click()
      const dialog = page.getByRole('dialog')
      await dialog.getByRole('textbox', { name: 'Key' }).fill('SERVICE_JWT')
      await dialog
        .getByRole('textbox', { name: 'Value' })
        .fill(jwtExpiringIn(3))
      await dialog.getByRole('button', { name: 'Create secret' }).click()
      await expect(dialog).toBeHidden()

      const indicator = page.getByRole('button', {
        name: 'Checks: Credential expiry: warning',
      })
      await expect(async () => {
        await page.reload()
        await expect(indicator).toBeVisible({ timeout: 2_000 })
      }).toPass({ timeout: BACKGROUND_LOOP_TIMEOUT })

      await page.getByRole('link', { name: 'SERVICE_JWT' }).click()
      const checks = page.getByRole('tabpanel', { name: /Checks/ })
      await expect(
        checks.getByRole('img', { name: 'Expiry check on this server' })
      ).toBeVisible()
      await expect(checks.getByText('Automatic expiry check')).toBeVisible()
      await checks.getByText(/View findings/).click()
      await expect(checks.getByText('Found 1 JWT.')).toBeVisible()
      await expect(
        checks.getByText(/^JWT expires within 7 days \(/)
      ).toBeVisible()
      expect(httpFailures).toEqual([])
      expect(consoleErrors).toEqual([])
    } finally {
      await deleteProject(page, project.id)
    }
  })

  test('moving a check to another secret shows in the history of both', async ({
    page,
    consoleErrors,
    httpFailures,
  }, testInfo) => {
    const project = await createProject(
      page,
      uniqueSlug('secret-history', testInfo)
    )
    try {
      const left = await createSecret(
        page,
        project.id,
        'OLD_TOKEN',
        jwtExpiringIn(3)
      )
      const target = await createSecret(
        page,
        project.id,
        'NEW_TOKEN',
        jwtExpiringIn(20)
      )

      // A manual expiry check, created in the console.
      await page.goto(
        `/projects/${project.slug}/settings/secrets/${left.id}/checks`
      )
      await page.getByRole('button', { name: /^Credential expiry/ }).click()
      await page.getByRole('textbox', { name: 'Name' }).fill('Manual expiry')
      await page.getByRole('button', { name: 'Save check' }).click()
      await expect(
        page.getByRole('button', { name: /^Checks: Manual expiry/ })
      ).toBeVisible()

      // The console binds a check to the credential it is configured on;
      // moving it to another credential is an API update.
      const listed = await page.request.get(
        `/api/projects/${project.id}/http-checks?page_size=100`
      )
      const { items } = (await listed.json()) as {
        items: { id: number; name: string }[]
      }
      const check = items.find((item) => item.name === 'Manual expiry')
      expect(check, 'the manual check is listed').toBeDefined()
      const moved = await page.request.put(
        `/api/projects/${project.id}/http-checks/${check!.id}`,
        {
          data: {
            name: 'Manual expiry',
            secret_id: target.id,
            kind: 'local',
            local: { warning_days: [30, 7, 1] },
            interval_seconds: 86_400,
            enabled: true,
          },
        }
      )
      expect(moved.ok(), await moved.text()).toBe(true)

      const historyRow = (event: string) =>
        page
          .getByRole('table', { name: 'Secret activity' })
          .getByRole('row')
          .filter({ hasText: event })
          .filter({ hasText: 'Manual expiry' })

      await page.goto(
        `/projects/${project.slug}/settings/secrets/${left.id}?tab=history`
      )
      await expect(historyRow('Check removed')).toBeVisible()

      await page.goto(
        `/projects/${project.slug}/settings/secrets/${target.id}?tab=history`
      )
      await expect(historyRow('Check added')).toBeVisible()
      expect(httpFailures).toEqual([])
      expect(consoleErrors).toEqual([])
    } finally {
      await deleteProject(page, project.id)
    }
  })
})
