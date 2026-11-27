// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { initialRows } from './env'
import {
  detectedFrom,
  linkAliases,
  hasLiveEnvironment,
  newProjects,
  projectNameFrom,
  projectsBaseline,
  projectVariables,
  providedNames,
  rootPreset,
  suggestedDatabase,
  type DatabaseOptionLike,
} from './ship'

const postgres: DatabaseOptionLike = {
  serviceType: 'postgres',
  label: 'PostgreSQL',
  suggested: true,
  reason: '`.env.example` asks for `DATABASE_URL`.',
  provides: ['POSTGRES_URL', 'POSTGRES_HOST'],
  envAliases: { DATABASE_URL: 'POSTGRES_URL' },
}
const redis: DatabaseOptionLike = {
  serviceType: 'redis',
  label: 'Redis',
  suggested: false,
  provides: ['REDIS_URL'],
  envAliases: {},
}

describe('ship', () => {
  const detected = detectedFrom([
    { key: 'DATABASE_URL', looksSecret: true },
    { key: 'POSTGRES_HOST' },
    { key: 'SESSION_SECRET', looksSecret: true, canGenerate: true },
    { key: 'LOG_LEVEL', defaultValue: 'info' },
  ])

  test('a database fills the names it injects and the ones aliased onto it', () => {
    expect(providedNames(postgres, detected)).toEqual([
      'DATABASE_URL',
      'POSTGRES_HOST',
    ])
    expect(providedNames(redis, detected)).toEqual([])
    expect(providedNames(null, detected)).toEqual([])
    expect(suggestedDatabase([redis, postgres])).toBe(postgres)
    expect(suggestedDatabase([redis])).toBeNull()
  })

  test('variables: typed values as sent, generated ones asked of the server, database ones left out', () => {
    const rows = initialRows(detected, {
      label: 'PostgreSQL',
      names: providedNames(postgres, detected),
    })
    expect(projectVariables(rows)).toEqual([
      { key: 'LOG_LEVEL', value: 'info', is_secret: false },
      { key: 'SESSION_SECRET', generate: { kind: 'secret', bytes: 32 } },
    ])
  })

  test('aliases go on the new link only when there are some', () => {
    expect(linkAliases(7, postgres)).toEqual({
      '7': { DATABASE_URL: 'POSTGRES_URL' },
    })
    expect(linkAliases(7, redis)).toBeUndefined()
    expect(linkAliases(null, postgres)).toBeUndefined()
  })

  test('names and presets', () => {
    expect(projectNameFrom('My_Cool.App')).toBe('my-cool-app')
    expect(projectNameFrom('---')).toBe('app')
    expect(rootPreset([{ path: 'apps/web' }, { path: './' }])).toEqual({
      path: './',
    })
    expect(rootPreset([{ path: 'apps/web' }])).toEqual({ path: 'apps/web' })
    expect(rootPreset([])).toBeNull()
  })
})

describe('new projects', () => {
  const at = (id: number, created: string) => ({ id, created_at: created })
  const opened = 'Thu, 26 Nov 2026 10:00:00 GMT'

  test('an older project redeployed onto the page is not new; one created after the step opened is', () => {
    const baseline = projectsBaseline([at(1, '2026-11-01T00:00:00Z')], opened)
    const later = [
      at(30, '2026-11-26T10:00:04Z'),
      // Created long before, listed now because it was just redeployed.
      at(21, '2026-10-01T00:00:00Z'),
      at(1, '2026-11-01T00:00:00Z'),
    ]
    expect(newProjects(baseline, later).map((p) => p.id)).toEqual([30])
  })

  test('the server clock decides, so a browser clock that is off changes nothing', () => {
    // The step opened at 10:00 server time; the browser's clock is not read.
    const baseline = projectsBaseline([], opened)
    expect(newProjects(baseline, [at(2, '2026-11-26T09:59:59Z')])).toEqual([])
    expect(newProjects(baseline, [at(3, '2026-11-26T10:00:00Z')])).toHaveLength(
      1
    )
  })

  test('without a Date header, a project must be newer than every one listed when the step opened', () => {
    const baseline = projectsBaseline(
      [at(5, '2026-11-20T00:00:00Z'), at(4, '2026-11-01T00:00:00Z')],
      null
    )
    expect(newProjects(baseline, [at(9, '2026-11-10T00:00:00Z')])).toEqual([])
    expect(
      newProjects(baseline, [at(10, '2026-11-21T00:00:00Z')])
    ).toHaveLength(1)
    // With nothing listed, any project that appears is new.
    expect(
      newProjects(projectsBaseline([], null), [at(1, '2026-01-01T00:00:00Z')])
    ).toHaveLength(1)
  })

  test('nothing is new before the step has its first listing', () => {
    expect(newProjects(null, [at(1, '2026-11-26T10:00:00Z')])).toEqual([])
  })
})

describe('live environments', () => {
  test('only an environment serving a deployment counts', () => {
    expect(hasLiveEnvironment([])).toBe(false)
    expect(hasLiveEnvironment([{ current_deployment_id: null }, {}])).toBe(
      false
    )
    expect(
      hasLiveEnvironment([
        { current_deployment_id: null },
        { current_deployment_id: 7 },
      ])
    ).toBe(true)
  })
})
