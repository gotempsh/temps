// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import type { DeploymentResponse } from '../../src/api/client'

const now = Date.now()
const deployments: DeploymentResponse[] = Array.from(
  { length: 10 },
  (_, index) => ({
    id: 3733 - index,
    project_id: 2,
    environment_id: 2,
    environment: {
      id: 2,
      name:
        index === 8
          ? 'preview-' + 'very-long-environment-name-'.repeat(6)
          : index < 6
            ? 'production'
            : 'preview',
      slug: 'production',
      domains: [],
    },
    status: [
      'completed',
      'stopped',
      'failed',
      'running',
      'pending',
      'cancelled',
    ][index % 6],
    is_current: index === 0 || index === 6,
    created_at: now - (index + 1) * 3600000,
    started_at: now - 120000,
    finished_at: index === 3 || index === 4 ? null : now - 103000,
    url: 'https://example.test',
    ...(index < 6
      ? {
          metadata: {
            deploymentSourceType: 'docker_image',
            externalImageRef:
              'temps.internal/project-2/environment-2/upload-' +
              '991c2e92dcff41e3b5a5645528184788'.repeat(4) +
              ':immutable',
          },
        }
      : index < 9
        ? {
            branch: 'fix/vps-billing-calendar-month-alignment',
            commit_hash: '123456789abcdef',
            commit_message:
              'feat(billing): align existing subscriptions to calendar-month billing; '.repeat(
                10
              ),
          }
        : {
            metadata: {
              deploymentSourceType: 'static_files',
              staticBundleContentType: 'application/zip',
            },
          }),
  })
)

for (const width of [1920, 1024, 768, 640, 390, 320]) {
  test(`deployment list keeps content separate at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 1000 })
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    await page.route('**/api/**', async (route) => {
      const url = new URL(route.request().url())
      const path = url.pathname
      let json: unknown = []
      if (path === '/api/user/me')
        json = {
          id: 42,
          name: 'Test Operator',
          username: 'operator',
          email: 'operator@example.com',
          avatar_url: '',
          mfa_enabled: false,
          role: 'admin',
        }
      else if (path === '/api/projects/by-slug/layout-test')
        json = {
          id: 2,
          slug: 'layout-test',
          name: 'Layout test',
          source_type: 'docker_image',
          project_type: 'application',
          main_branch: 'main',
          directory: '.',
          created_at: now,
          updated_at: now,
          deployment_config: {},
          preset: 'dockerfile',
        }
      else if (path.endsWith('/last-deployment')) json = deployments[0]
      else if (path === '/api/projects/2/deployments')
        json = {
          deployments:
            url.searchParams.get('page') === '2'
              ? [{ ...deployments[1], id: 3700 }]
              : deployments,
          total: 11,
        }
      else if (/\/api\/projects\/2\/deployments\/\d+$/.test(path))
        json = deployments.find((d) => d.id === Number(path.split('/').pop()))
      else if (path.endsWith('/environments'))
        json = [
          {
            id: 2,
            name: 'production',
            slug: 'production',
            project_id: 2,
            main_url: 'https://example.test',
            created_at: now,
            updated_at: now,
          },
        ]
      else if (path.endsWith('/active-visitors')) json = { count: 0 }
      await route.fulfill({ json })
    })
    await page.goto('/projects/layout-test/deployments')
    const first = page.locator('li').filter({ hasText: '#3733' })
    await expect(first).toBeVisible()
    // Badge contents must fit their region: catches the original fixed-column overlap.
    const geometry = await first.evaluate((li) => {
      const row = li.querySelector('a > div')!
      const [identity, source] = Array.from(row.children)
      const primary = identity.getBoundingClientRect()
      const sourceBox = source.getBoundingClientRect()
      const current = Array.from(identity.querySelectorAll('div'))
        .find((el) => el.textContent?.trim() === 'Current')!
        .getBoundingClientRect()
      return {
        primaryRight: primary.right,
        badgeRight: current.right,
        primaryBottom: primary.bottom,
        sourceTop: sourceBox.top,
      }
    })
    expect(geometry.badgeRight).toBeLessThanOrEqual(geometry.primaryRight + 1)
    expect(geometry.sourceTop).toBeGreaterThanOrEqual(geometry.primaryBottom)
    const list = first.locator('..')
    await expect(list.locator(':scope > li')).toHaveCount(10)
    for (const item of await list.locator(':scope > li').all()) {
      const overflow = await item.evaluate((el) => ({
        scroll: el.scrollWidth,
        client: el.clientWidth,
      }))
      expect(overflow.scroll).toBeLessThanOrEqual(overflow.client + 1)
    }
    await page.screenshot({
      path: testInfo.outputPath(`deployments-${width}.png`),
      fullPage: true,
    })
    await first.getByRole('button', { name: 'Open menu' }).click()
    await expect(
      page.getByRole('menuitem', { name: 'Redeploy', exact: true })
    ).toBeVisible()
    await expect(
      page.getByRole('menuitem', { name: 'Rollback to this' })
    ).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(page).toHaveURL(/\/deployments$/)
    await page.getByRole('button', { name: 'Next' }).click()
    await expect(page.getByText('#3700', { exact: true })).toBeVisible()
    await expect(page.getByText('#3733', { exact: true })).toHaveCount(0)
    await page.getByRole('button', { name: 'Previous' }).click()
    await expect(first).toBeVisible()
    await expect(first.getByRole('link')).toHaveAttribute(
      'href',
      '/projects/layout-test/deployments/3733'
    )
    expect(errors).toEqual([])
  })
}
