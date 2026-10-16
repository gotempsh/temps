// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import type {
  AgentRunResponse,
  OperationEntry,
  OperationsListResponse,
} from '../../src/api/client/types.gen'

function autofix(status: 'running' | 'waiting' | 'succeeded'): OperationEntry {
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
    phase:
      status === 'waiting'
        ? 'analyzed'
        : status === 'succeeded'
          ? 'completed'
          : 'analyzing',
    failure_reason: null,
    created_at: '2026-07-01T10:00:00Z',
    started_at: '2026-07-01T10:00:05Z',
    finished_at: null,
    triggered_by_user_id: null,
    link: '/projects/review-app/agents/8',
  }
}

test('creating an Autofix PR appears as running before the push request returns', async ({
  page,
}) => {
  let status = 'fix_ready'
  let runRequests = 0
  let pushReturned = false
  let releasePush!: () => void
  const push = new Promise<void>((resolve) => {
    releasePush = resolve
  })
  const project = {
    id: 3,
    slug: 'review-app',
    name: 'Review app',
    source_type: 'git',
    directory: '.',
    main_branch: 'main',
    repo_owner: 'example',
    repo_name: 'app',
    environments: [],
    created_at: 1,
    updated_at: 1,
  }
  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api/, '')
    let json: unknown = []
    if (path.endsWith('/create-pr')) {
      status = 'pushing'
      await push
      status = 'completed'
      pushReturned = true
      await route.fulfill({ json: { url: 'https://example.test/pulls/1' } })
      return
    }
    if (path.endsWith('/stream')) {
      await route.fulfill({ contentType: 'text/event-stream', body: '' })
      return
    }
    if (path === '/user/me') {
      json = {
        id: 42,
        name: 'Test Operator',
        username: 'operator',
        email: 'operator@example.test',
        role: 'admin',
        mfa_enabled: false,
      }
    } else if (path === '/projects') {
      json = { projects: [project], total: 1 }
    } else if (
      path === '/projects/by-slug/review-app' ||
      path === '/projects/3'
    ) {
      json = project
    } else if (path === '/projects/3/agents/runs/8') {
      runRequests += 1
      const run: AgentRunResponse = {
        id: 8,
        project_id: 3,
        status,
        phase: status === 'completed' ? 'completed' : 'fix_ready',
        trigger_type: 'autofixer',
        trigger_source_type: 'error_group',
        source: 'committed',
        sandbox_enabled: true,
        created_at: '2026-07-01T10:00:00Z',
        estimated_cost_cents: 0,
        files_changed: 1,
        tokens_input: 0,
        tokens_output: 0,
      }
      json = { run, logs: [] }
    } else if (path === '/operations') {
      const op = autofix(
        status === 'pushing'
          ? 'running'
          : status === 'completed'
            ? 'succeeded'
            : 'waiting'
      )
      const filter = url.searchParams.get('status')
      const operations =
        filter === op.status ||
        (filter === 'finished' && op.status === 'succeeded')
          ? [op]
          : []
      json = {
        operations,
        total: operations.length,
        running_count: status === 'pushing' ? 1 : 0,
        page: 1,
        page_size: Number(url.searchParams.get('page_size') ?? 20),
      } satisfies OperationsListResponse
    } else if (path.endsWith('/last-deployment')) {
      await route.fulfill({ status: 404, json: { detail: 'No deployments' } })
      return
    } else if (path.includes('active-visitors')) {
      json = { count: 0 }
    }
    await route.fulfill({ json })
  })
  await page.goto('/projects/review-app/agents/8')
  const createPr = page.getByRole('button', { name: 'Create PR', exact: true })
  await expect(createPr).toBeVisible({ timeout: 45_000 })
  const trigger = page.getByRole('button', { name: /^Operations(?: \(|$)/ })
  await expect(trigger).toHaveAccessibleName('Operations')
  await page.clock.install()
  await createPr.click()
  await expect.poll(() => status).toBe('pushing')
  await expect(createPr).toBeDisabled()
  // The tray is closed and its previous running count was zero. The detail
  // query must discover pushing while the create-PR response stays pending.
  await page.clock.runFor(5_100)
  await expect(trigger).toHaveAccessibleName('Operations (1 running)')
  expect(pushReturned).toBe(false)
  await trigger.click()
  await expect(
    page.getByRole('region', { name: 'Running', exact: true })
  ).toContainText('Autofix run #8')
  await expect(
    page.getByRole('region', { name: 'Needs review', exact: true })
  ).toHaveCount(0)
  await page.keyboard.press('Escape')
  releasePush()
  await expect(trigger).toHaveAccessibleName('Operations')
  await expect(createPr).toHaveCount(0)
  const settledRequests = runRequests
  await page.clock.runFor(30_000)
  expect(runRequests).toBe(settledRequests)
})

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
