import { test, expect, describe } from 'bun:test'
import { buildStatusJson, countRunningContainers, filterEnvironments } from './index.js'
import type { EnvironmentResponse } from '../../api/types.gen.js'

function makeEnv(overrides: Partial<EnvironmentResponse> = {}): EnvironmentResponse {
  return {
    id: 1,
    name: 'production',
    slug: 'production',
    main_url: 'https://app.example.com',
    is_preview: false,
    project_id: 1,
    created_at: 0,
    ai_write_actions_enabled: false,
    ...overrides,
  } as EnvironmentResponse
}

describe('filterEnvironments', () => {
  test('returns everything when no filter is given', () => {
    const envs = [makeEnv({ id: 1, name: 'production' }), makeEnv({ id: 2, name: 'staging' })]
    expect(filterEnvironments(envs, undefined)).toEqual(envs)
  })

  test('matches by name case-insensitively', () => {
    const envs = [makeEnv({ id: 1, name: 'Production', slug: 'production' })]
    expect(filterEnvironments(envs, 'production')).toEqual(envs)
    expect(filterEnvironments(envs, 'PRODUCTION')).toEqual(envs)
  })

  test('matches by exact slug', () => {
    const envs = [makeEnv({ id: 1, name: 'PR Preview', slug: 'pr-123' })]
    expect(filterEnvironments(envs, 'pr-123')).toEqual(envs)
  })

  test('returns an empty list rather than falling back when nothing matches', () => {
    // A silent fallback to the full list would print the wrong environment's
    // status under a filter that looks like it worked.
    const envs = [makeEnv({ id: 1, name: 'production' })]
    expect(filterEnvironments(envs, 'nope')).toEqual([])
  })
})

describe('buildStatusJson', () => {
  test('shapes the payload for --json consumers', () => {
    const project = { id: 5, name: 'My App', slug: 'my-app', main_branch: 'main' }
    const envs = [
      makeEnv({ id: 1, name: 'production', slug: 'production', main_url: 'https://my-app.temps.sh', is_preview: false }),
    ]
    expect(buildStatusJson(project, envs)).toEqual({
      project: { id: 5, name: 'My App', slug: 'my-app', main_branch: 'main' },
      environments: [
        { id: 1, name: 'production', slug: 'production', url: 'https://my-app.temps.sh', is_preview: false },
      ],
    })
  })

  test('produces an empty environments array, not an omitted key, when none matched', () => {
    const project = { id: 5, name: 'My App', slug: 'my-app', main_branch: 'main' }
    const result = buildStatusJson(project, [])
    expect(result.environments).toEqual([])
  })
})

describe('countRunningContainers', () => {
  test('counts case-insensitively', () => {
    expect(countRunningContainers([{ status: 'Running' }, { status: 'stopped' }, { status: 'RUNNING' }])).toBe(2)
  })

  test('treats missing status as not running', () => {
    expect(countRunningContainers([{ status: undefined }, { status: null }])).toBe(0)
  })

  test('returns 0 for an empty list', () => {
    expect(countRunningContainers([])).toBe(0)
  })
})
