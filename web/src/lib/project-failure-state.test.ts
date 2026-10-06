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
