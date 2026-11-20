// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildInstantiateRequest, parseDatabase, parseServiceNames, parseVariable, resolveTarget } from './create.js'

const single = { name: 'Plausible', kind: 'app' as const, databases: ['postgres'], service_definitions: [{ name: 'plausible' }] }
const multi = {
  name: 'Secrets manager',
  kind: 'app' as const,
  databases: ['postgres', 'redis'],
  service_definitions: [{ name: 'backend' }, { name: 'frontend' }],
}

// Only the fields these helpers read.
type Template = Parameters<typeof resolveTarget>[0] & Parameters<typeof buildInstantiateRequest>[0]
const asTemplate = (value: unknown): Template => value as Template

describe('resolveTarget', () => {
  test('one service stands alone and several go into a new project by default', () => {
    expect(resolveTarget(asTemplate(single), {})).toEqual({ type: 'no_project' })
    expect(resolveTarget(asTemplate(multi), {})).toEqual({ type: 'new_project', name: 'Secrets manager' })
  })

  test('an explicit choice wins, and only one may be given', () => {
    expect(resolveTarget(asTemplate(single), { project: 'Analytics' })).toEqual({ type: 'new_project', name: 'Analytics' })
    expect(resolveTarget(asTemplate(multi), { into: 'shop' }, 7)).toEqual({ type: 'existing_project', group_id: 7 })
    expect(() => resolveTarget(asTemplate(multi), { project: 'a', into: 'b' }, 7)).toThrow(/one of/)
  })

  test('several services cannot stand alone', () => {
    expect(() => resolveTarget(asTemplate(multi), { standalone: true })).toThrow(/go into a project/)
  })
})

describe('parseVariable', () => {
  const services = new Set(['backend', 'frontend'])

  test('a known service prefix assigns the variable to that service', () => {
    expect(parseVariable('backend:SMTP_HOST=mail.example.test', services, false)).toEqual({
      name: 'SMTP_HOST',
      value: 'mail.example.test',
      is_secret: false,
      service: 'backend',
    })
  })

  test('values keep their colons and equals signs', () => {
    expect(parseVariable('DATABASE_URL=postgres://u:p@h/db?a=b', services, true)).toEqual({
      name: 'DATABASE_URL',
      value: 'postgres://u:p@h/db?a=b',
      is_secret: true,
      service: null,
    })
  })

  test('rejects input without a name', () => {
    expect(() => parseVariable('=value', services, false)).toThrow(/KEY=VALUE/)
  })
})

describe('parseDatabase', () => {
  test('creates, creates with a name, or links by id', () => {
    expect(parseDatabase('postgres=new')).toEqual(['postgres', { type: 'create' }])
    expect(parseDatabase('Redis=new:cache')).toEqual(['redis', { type: 'create', name: 'cache' }])
    expect(parseDatabase('postgres=12')).toEqual(['postgres', { type: 'existing', service_id: 12 }])
    expect(() => parseDatabase('postgres=twelve')).toThrow(/postgres=new/)
  })
})

describe('parseServiceNames', () => {
  test('a bare name only works for one service', () => {
    expect(parseServiceNames(['analytics'], asTemplate(single))).toEqual({ plausible: 'analytics' })
    expect(() => parseServiceNames(['api'], asTemplate(multi))).toThrow(/which service/)
    expect(parseServiceNames(['backend=api'], asTemplate(multi))).toEqual({ backend: 'api' })
    expect(() => parseServiceNames(['worker=w'], asTemplate(multi))).toThrow(/no service "worker"/)
  })
})

describe('buildInstantiateRequest', () => {
  test('every database defaults to a new one unless linked', () => {
    const request = buildInstantiateRequest(asTemplate(multi), { database: ['postgres=4'] }, { type: 'no_project' }, 'key-1')
    expect(request.databases).toEqual({ postgres: { type: 'existing', service_id: 4 }, redis: { type: 'create' } })
    expect(request.fork).toBeNull()
    expect(request.idempotency_key).toBe('key-1')
  })

  test('a fork needs a repository name', () => {
    expect(() =>
      buildInstantiateRequest(asTemplate(single), { forkConnection: '3' }, { type: 'no_project' }, 'k')
    ).toThrow(/--repo-name/)
    const request = buildInstantiateRequest(
      asTemplate(single),
      { forkConnection: '3', repoName: 'my-app', autoDeploy: true },
      { type: 'no_project' },
      'k'
    )
    expect(request.fork).toEqual({
      git_provider_connection_id: 3,
      repository_name: 'my-app',
      repository_owner: null,
      private: true,
      automatic_deploy: true,
    })
  })
})
