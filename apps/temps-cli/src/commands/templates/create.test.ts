// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildInstantiateRequest, parseDatabase, parseServiceNames, parseVariable, resolveSecretFiles, resolveTarget } from './create.js'
import { registerTemplatesCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

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

describe('--secret-file', () => {
  const SECRET = 's3cr3t-test-value'

  async function secretPath(contents: string): Promise<string> {
    const { mkdtemp, writeFile, chmod } = await import('node:fs/promises')
    const { tmpdir } = await import('node:os')
    const { join } = await import('node:path')
    const path = join(await mkdtemp(join(tmpdir(), 'temps-template-secret-')), 'secret.txt')
    await writeFile(path, contents)
    await chmod(path, 0o600)
    return path
  }

  test('a malformed --secret entry is rejected without echoing it', () => {
    expect(() => parseVariable(SECRET, new Set(), true)).toThrow('A --secret entry is not [service:]KEY=VALUE')
    try {
      parseVariable(SECRET, new Set(), true)
    } catch (err) {
      expect((err as Error).message).not.toContain(SECRET)
    }
  })

  test('reads each KEY=PATH into a KEY=VALUE entry, keeping the service prefix', async () => {
    const path = await secretPath(`${SECRET}\n`)
    expect(await resolveSecretFiles([`backend:API_KEY=${path}`], false)).toEqual([`backend:API_KEY=${SECRET}\n`])
    expect(await resolveSecretFiles([`API_KEY=${path}`], true)).toEqual([`API_KEY=${SECRET}`])
  })

  test('rejects an entry without a path, and more than one stdin entry', async () => {
    await expect(resolveSecretFiles(['API_KEY='], false)).rejects.toThrow('is not [service:]KEY=PATH')
    await expect(resolveSecretFiles(['A=-', 'B=-'], false)).rejects.toThrow('Only one --secret-file entry can read stdin')
  })

  test('templates create sends the file content as a secret variable', async () => {
    const path = await secretPath(SECRET)
    const run = await runWithApi(registerTemplatesCommands,
      ['templates', 'create', 'plausible', '--standalone', '--secret-file', `API_KEY=${path}`, '--no-wait', '--json'],
      {
        'GET /templates/plausible': single,
        'POST /templates/plausible/instantiate': { id: 1, status: 'pending', template_slug: 'plausible' },
      })
    const sent = run.bodies.find((b) => b.request === 'POST /templates/plausible/instantiate')
    expect(sent).toBeDefined()
    expect((sent!.body as { environment_variables: unknown[] }).environment_variables).toEqual([
      { name: 'API_KEY', value: SECRET, is_secret: true, service: null },
    ])
    expect(run.stderr).not.toContain(SECRET)
  })
})
