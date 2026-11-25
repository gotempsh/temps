// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { initialRows } from './env'
import {
  detectedFrom,
  linkAliases,
  projectNameFrom,
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
