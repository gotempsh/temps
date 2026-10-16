// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import type {
  OperationEntry,
  OperationsListResponse,
} from '../../src/api/client/types.gen'

function autofix(status: 'running' | 'waiting'): OperationEntry {
  return {
    id: 'autofix:8',
    kind: 'autofix',
    status,
    title: 'Autofix run #8',
    project_id: 3,
    project_slug: 'review-app',
    environment_id: null,
    environment_name: null,
    deployment_id: null,
    service_id: null,
    service_name: null,
    backup_id: null,
    restore_run_id: null,
    agent_run_id: 8,
    phase: status === 'waiting' ? 'analyzed' : 'analyzing',
    failure_reason: null,
    created_at: '2026-07-01T10:00:00Z',
    started_at: '2026-07-01T10:00:05Z',
    finished_at: null,
    triggered_by_user_id: null,
    link: '/projects/review-app/agents/8',
  }
}

for (const initialStatus of ['waiting', 'running'] as const) {
  test(`autofix ${initialStatus} is grouped correctly and review stops closed polling`, async ({
    page,
  }) => {
    let status: 'running' | 'waiting' = initialStatus
    const requests: string[] = []
    await page.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname
      if (path !== '/api/operations') {
        const json =
          path === '/api/user/me'
            ? {
                id: 42,
                name: 'Test Operator',
                username: 'operator',
                email: 'operator@example.test',
                avatar_url: '',
                mfa_enabled: false,
                role: 'admin',
              }
            : path === '/api/projects'
              ? { projects: [], total: 0 }
              : []
        await route.fulfill({ json })
        return
      }
      const url = new URL(route.request().url())
      const filter = url.searchParams.get('status') ?? 'all'
      requests.push(filter)
      const operations =
        filter === status || filter === 'all' ? [autofix(status)] : []
      const response: OperationsListResponse = {
        operations,
        total: operations.length,
        page: 1,
        page_size: Number(url.searchParams.get('page_size') ?? 20),
        running_count: status === 'running' ? 1 : 0,
      }
      await route.fulfill({ json: response })
    })
    await page.goto('/settings')
    const trigger = page.getByRole('button', { name: /^Operations(?: \(|$)/ })
    await expect(trigger).toBeVisible({ timeout: 45_000 })
    await page.clock.install()
    await trigger.click()
    if (initialStatus === 'running') {
      await expect(trigger).toHaveAccessibleName('Operations (1 running)')
      await expect(
        page.getByRole('region', { name: 'Running', exact: true })
      ).toContainText('Autofix run #8')
      await page.keyboard.press('Escape')
      await expect(
        page.getByRole('region', { name: 'Running', exact: true })
      ).toBeHidden()
      status = 'waiting'
      // The final closed-tray poll must refresh reviews even as the
      // running count reaches zero and its polling interval turns off.
      await page.clock.runFor(10_100)
      await expect(trigger).toHaveAccessibleName('Operations')
      await trigger.click()
    }
    const reviews = page.getByRole('region', {
      name: 'Needs review',
      exact: true,
    })
    await expect(reviews).toContainText('Autofix run #8')
    await expect(reviews.getByRole('link')).toHaveAttribute(
      'href',
      '/projects/review-app/agents/8'
    )
    await expect(trigger).toHaveAccessibleName('Operations')
    await expect(
      page.getByRole('region', { name: 'Running', exact: true })
    ).toHaveCount(0)
    await page.keyboard.press('Escape')
    await expect(reviews).toBeHidden()
    const settledRequests = requests.length
    await page.clock.runFor(30_000)
    expect(requests).toHaveLength(settledRequests)
  })
}
