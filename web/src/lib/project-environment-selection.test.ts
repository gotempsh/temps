// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  ALL_ENVIRONMENTS,
  environmentLiveStatus,
  legacyEnvironmentSlug,
  resolveDefaultEnvironment,
  resolveEnvironmentSelection,
  withReachability,
} from './project-environment-selection'

const staging = { id: 1, slug: 'staging', is_preview: false }
const production = { id: 2, slug: 'production', is_preview: false }
const preview = { id: 3, slug: 'pr-12', is_preview: true }
const environments = [staging, production, preview]

const select = (
  overrides: Partial<Parameters<typeof resolveEnvironmentSelection>[0]>
) =>
  resolveEnvironmentSelection({
    environments,
    urlParam: null,
    remembered: null,
    preferredDefault: null,
    allowAll: false,
    ...overrides,
  })

describe('resolveEnvironmentSelection', () => {
  test('defaults to production, not the first environment returned', () => {
    expect(select({})).toEqual({
      environment: production,
      all: false,
      source: 'fallback',
    })
  })

  test('the URL wins over the remembered and default environment', () => {
    const selection = select({
      urlParam: 'pr-12',
      remembered: 'staging',
      preferredDefault: 'staging',
    })
    expect(selection.environment).toBe(preview)
    expect(selection.source).toBe('url')
  })

  test('the remembered environment wins over the project default', () => {
    const selection = select({
      remembered: 'pr-12',
      preferredDefault: 'staging',
    })
    expect(selection.environment).toBe(preview)
    expect(selection.source).toBe('remembered')
  })

  test('uses the project default when nothing else is set', () => {
    const selection = select({ preferredDefault: 'staging' })
    expect(selection.environment).toBe(staging)
    expect(selection.source).toBe('default')
  })

  test('skips a slug that no longer exists', () => {
    const selection = select({ urlParam: 'deleted', remembered: 'staging' })
    expect(selection.environment).toBe(staging)
    expect(selection.source).toBe('remembered')
  })

  test('selects all environments only where the page allows it', () => {
    expect(select({ urlParam: ALL_ENVIRONMENTS, allowAll: true })).toEqual({
      environment: undefined,
      all: true,
      source: 'url',
    })
    const single = select({ urlParam: ALL_ENVIRONMENTS, allowAll: false })
    expect(single.all).toBe(false)
    expect(single.environment).toBe(production)
  })

  test('selects nothing for a project without environments', () => {
    const selection = resolveEnvironmentSelection({
      environments: [],
      urlParam: 'production',
      remembered: null,
      preferredDefault: null,
      allowAll: false,
    })
    expect(selection.environment).toBeUndefined()
    expect(selection.all).toBe(false)
  })
})

describe('resolveDefaultEnvironment', () => {
  test('prefers the stored default and falls back to production', () => {
    expect(resolveDefaultEnvironment(environments, 'staging')).toBe(staging)
    expect(resolveDefaultEnvironment(environments, 'gone')).toBe(production)
  })
})

describe('environmentLiveStatus', () => {
  const live = { sleeping: false, current_deployment_id: 4 }
  const label = (
    environment: { sleeping: boolean; current_deployment_id: number | null },
    signals?: Parameters<typeof environmentLiveStatus>[1]
  ) => {
    const status = environmentLiveStatus(environment, signals)
    return [status.tone, status.label, status.explainedAt]
  }

  test('distinguishes live, sleeping and never-deployed environments', () => {
    expect(label(live)).toEqual(['ok', 'Live', undefined])
    expect(label({ ...live, sleeping: true })).toEqual([
      'idle',
      'Sleeping',
      undefined,
    ])
    expect(label({ ...live, current_deployment_id: null })).toEqual([
      'idle',
      'Not deployed',
      undefined,
    ])
  })

  test('a failed latest deployment reads Failed even while an older one serves', () => {
    const failed = { latestDeployment: { status: 'failed' } }
    expect(label(live, failed)).toEqual(['error', 'Failed', 'deployment'])
    expect(environmentLiveStatus(live, failed).detail).toContain(
      'previous version is still serving'
    )
    expect(label({ ...live, current_deployment_id: null }, failed)).toEqual([
      'error',
      'Failed',
      'deployment',
    ])
  })

  test('a running deployment reads Deploying', () => {
    expect(label(live, { latestDeployment: { status: 'running' } })).toEqual([
      'running',
      'Deploying',
      'deployment',
    ])
  })

  test('crashing containers and failing monitors are problems, in that order', () => {
    expect(label(live, { unhealthyContainers: 2 })).toEqual([
      'warn',
      'Degraded',
      'environment',
    ])
    expect(
      label(live, { unhealthyContainers: 1, monitorStatus: 'down' })
    ).toEqual(['warn', 'Degraded', 'environment'])
    expect(label(live, { monitorStatus: 'down' })).toEqual([
      'error',
      'Down',
      'monitors',
    ])
    expect(label(live, { monitorStatus: 'degraded' })).toEqual([
      'warn',
      'Degraded',
      'monitors',
    ])
    expect(label(live, { monitorStatus: 'operational' })).toEqual([
      'ok',
      'Live',
      undefined,
    ])
  })

  test('a sleeping environment is not degraded for having stopped containers', () => {
    expect(
      label({ ...live, sleeping: true }, { unhealthyContainers: 3 })
    ).toEqual(['idle', 'Sleeping', undefined])
  })
})

test('a legacy ?environment= value is read as an id or a slug', () => {
  const environments = [
    { id: 3, slug: 'production' },
    { id: 5, slug: 'staging' },
  ]
  expect(legacyEnvironmentSlug(environments, '5')).toBe('staging')
  expect(legacyEnvironmentSlug(environments, 'production')).toBe('production')
  expect(legacyEnvironmentSlug(environments, '99')).toBeNull()
  expect(legacyEnvironmentSlug(environments, null)).toBeNull()
})

describe('withReachability', () => {
  const live = {
    tone: 'ok' as const,
    label: 'Live',
    detail: 'Serving its current deployment.',
  }

  test('a live environment keeps its label and names where it is public', () => {
    expect(
      withReachability(live, {
        kind: 'public',
        address: 'app.example.test',
        url: 'https://app.example.test',
      })
    ).toEqual({ ...live, detail: 'Public at app.example.test.' })
  })

  test('a live environment nobody can reach is a warning that leads to the fix', () => {
    expect(
      withReachability(live, {
        kind: 'not_reachable',
        reason: 'No address points at it.',
        canFix: true,
      })
    ).toEqual({
      tone: 'warn',
      label: 'Not reachable',
      detail: 'No address points at it.',
      explainedAt: 'reachability',
    })
  })

  test('a failed deployment still says Failed, whatever the address', () => {
    const failed = {
      tone: 'error' as const,
      label: 'Failed',
      detail: 'The latest deployment failed.',
      explainedAt: 'deployment' as const,
    }
    expect(
      withReachability(failed, {
        kind: 'not_reachable',
        reason: 'No address points at it.',
        canFix: false,
      })
    ).toBe(failed)
  })

  test('while the check runs, the status is unchanged', () => {
    expect(
      withReachability(live, { kind: 'checking', reason: 'Checking…' })
    ).toBe(live)
  })
})
