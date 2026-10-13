// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  ContainerInfoResponse,
  DeploymentResponse,
  EnvironmentResponse,
} from '@/api/client'
import {
  containerHealthIssues,
  findLastSuccessfulDeployment,
  healthEnvironment,
  lastSuccessfulDeployment,
  RECENT_RESTART_WINDOW_MS,
} from './project-failure-state'

const NOW = Date.parse('2026-10-06T12:00:00Z')

function container(
  overrides: Partial<ContainerInfoResponse>
): ContainerInfoResponse {
  return {
    container_id: 'abc123',
    container_name: 'web-app-1',
    created_at: '2026-10-06T10:00:00Z',
    image_name: 'web:1',
    status: 'running',
    ...overrides,
  }
}

function environment(
  overrides: Partial<EnvironmentResponse>
): EnvironmentResponse {
  return {
    created_at: 0,
    id: 1,
    is_preview: false,
    main_url: '',
    name: 'production',
    project_id: 1,
    protected: false,
    sleeping: false,
    slug: 'production',
    subdomain: 'app-production',
    updated_at: 0,
    current_deployment_id: 10,
    ...overrides,
  }
}

function deployment(
  id: number,
  status: string,
  overrides: Partial<DeploymentResponse> = {}
): DeploymentResponse {
  return {
    id,
    status,
    environment_id: 1,
    created_at: id * 1000,
    is_current: false,
    project_id: 1,
    url: '',
    environment: { id: 1, name: 'production', domains: [] },
    ...overrides,
  } as DeploymentResponse
}

describe('healthEnvironment', () => {
  test('prefers a live non-preview environment', () => {
    const picked = healthEnvironment([
      environment({ id: 3, is_preview: true }),
      environment({ id: 4, current_deployment_id: null }),
      environment({ id: 5 }),
    ])
    expect(picked?.id).toBe(5)
  })

  test('falls back to a live preview when nothing else is live', () => {
    const picked = healthEnvironment([
      environment({ id: 3, is_preview: true }),
      environment({ id: 4, current_deployment_id: null }),
    ])
    expect(picked?.id).toBe(3)
  })

  test('returns nothing when no environment is live', () => {
    expect(
      healthEnvironment([environment({ current_deployment_id: null })])
    ).toBe(undefined)
    expect(healthEnvironment(undefined)).toBe(undefined)
  })
})

describe('containerHealthIssues', () => {
  test('reports an exited container with its exit reason', () => {
    const issues = containerHealthIssues(
      [container({ status: 'exited', exit_reason: 'Exit code 1' })],
      { now: NOW }
    )
    expect(issues).toEqual([
      {
        containerId: 'abc123',
        containerName: 'web-app-1',
        kind: 'down',
        detail: 'Exited: Exit code 1',
      },
    ])
  })

  test('names an OOM kill explicitly', () => {
    const [issue] = containerHealthIssues(
      [container({ status: 'dead', oom_killed: true, exit_reason: 'x' })],
      { now: NOW }
    )
    expect(issue.detail).toBe('Dead: OOMKilled')
  })

  test('uses the compose service name when there is one', () => {
    const [issue] = containerHealthIssues(
      [container({ status: 'stopped', service_name: 'worker' })],
      { now: NOW }
    )
    expect(issue.containerName).toBe('worker')
    expect(issue.detail).toBe('Stopped')
  })

  test('flags a container that restarted recently as restarting', () => {
    const [issue] = containerHealthIssues(
      [
        container({
          restart_count: 3,
          started_at: new Date(NOW - 60_000).toISOString(),
        }),
      ],
      { now: NOW }
    )
    expect(issue.kind).toBe('restarting')
    expect(issue.detail).toBe('Restarted 3 times')
  })

  test('ignores an old restart followed by long uptime', () => {
    expect(
      containerHealthIssues(
        [
          container({
            restart_count: 1,
            started_at: new Date(
              NOW - RECENT_RESTART_WINDOW_MS - 1
            ).toISOString(),
          }),
        ],
        { now: NOW }
      )
    ).toEqual([])
  })

  test('ignores healthy, never-restarted and paused containers', () => {
    expect(
      containerHealthIssues(
        [
          container({
            restart_count: 0,
            started_at: new Date(NOW).toISOString(),
          }),
          container({ status: 'paused' }),
        ],
        { now: NOW }
      )
    ).toEqual([])
  })

  test('reports nothing for a sleeping on-demand environment', () => {
    expect(
      containerHealthIssues([container({ status: 'exited' })], {
        sleeping: true,
        now: NOW,
      })
    ).toEqual([])
  })
})

describe('lastSuccessfulDeployment', () => {
  const failed = deployment(20, 'failed')

  test('picks the newest earlier rollback-able deployment in the environment', () => {
    const target = lastSuccessfulDeployment(
      [
        deployment(12, 'stopped'),
        deployment(15, 'completed'),
        deployment(18, 'failed'),
        deployment(19, 'cancelled'),
        failed,
      ],
      failed
    )
    expect(target?.id).toBe(15)
  })

  test('accepts superseded (stopped) deployments', () => {
    expect(
      lastSuccessfulDeployment([deployment(9, 'stopped'), failed], failed)?.id
    ).toBe(9)
  })

  test('ignores other environments and later deployments', () => {
    expect(
      lastSuccessfulDeployment(
        [
          deployment(16, 'completed', { environment_id: 2 }),
          deployment(25, 'completed'),
        ],
        failed
      )
    ).toBe(undefined)
  })

  test('returns nothing when the environment never succeeded', () => {
    expect(lastSuccessfulDeployment([failed], failed)).toBe(undefined)
    expect(lastSuccessfulDeployment(undefined, failed)).toBe(undefined)
  })
})

describe('findLastSuccessfulDeployment', () => {
  // Newest-first history, paged like the API: ids 300 down to 1.
  function history(statusOf: (id: number) => string) {
    const all = Array.from({ length: 300 }, (_, i) => 300 - i).map((id) =>
      deployment(id, statusOf(id))
    )
    const pages: number[] = []
    const fetchPage = async (page: number, perPage: number) => {
      pages.push(page)
      return all.slice((page - 1) * perPage, page * perPage)
    }
    return { all, pages, fetchPage }
  }

  test('reads past a page of newer deployments to find an older target', async () => {
    // The failure is old: every deployment on the first page is newer.
    const { all, pages, fetchPage } = history((id) =>
      id === 40 ? 'completed' : 'failed'
    )
    const failed = all.find((d) => d.id === 120)!
    const target = await findLastSuccessfulDeployment(fetchPage, failed, {
      perPage: 50,
    })
    expect(target?.id).toBe(40)
    expect(pages).toEqual([1, 2, 3, 4, 5, 6])
  })

  test('finds the target after a long run of failed attempts', async () => {
    const { all, fetchPage } = history((id) =>
      id === 10 ? 'completed' : 'failed'
    )
    const target = await findLastSuccessfulDeployment(fetchPage, all[0], {
      perPage: 100,
    })
    expect(target?.id).toBe(10)
  })

  test('stops at the first page that has a target', async () => {
    const { all, pages, fetchPage } = history(() => 'completed')
    const target = await findLastSuccessfulDeployment(fetchPage, all[0], {
      perPage: 100,
    })
    expect(target?.id).toBe(299)
    expect(pages).toEqual([1])
  })

  test('returns null when history runs out or the page cap is reached', async () => {
    const exhausted = history(() => 'failed')
    expect(
      await findLastSuccessfulDeployment(
        exhausted.fetchPage,
        exhausted.all[0],
        {
          perPage: 100,
        }
      )
    ).toBeNull()
    expect(exhausted.pages).toEqual([1, 2, 3, 4])

    const capped = history((id) => (id === 1 ? 'completed' : 'failed'))
    expect(
      await findLastSuccessfulDeployment(capped.fetchPage, capped.all[0], {
        perPage: 50,
        maxPages: 2,
      })
    ).toBeNull()
    expect(capped.pages).toEqual([1, 2])
  })
})
