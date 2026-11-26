// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, afterEach } from 'bun:test'
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { buildCreateSecretBody, buildUpdateSecretBody, registerSecretsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const SECRET = 's3cr3t-test-value'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
})

async function fixtureFile(contents: string | Uint8Array): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-secrets-test-'))
  temporaryDirectories.push(directory)
  const path = join(directory, 'creds.json')
  await writeFile(path, contents)
  await chmod(path, 0o600)
  return path
}

describe('buildCreateSecretBody', () => {
  test('defaults to env type and resolves a plain value', () => {
    const body = buildCreateSecretBody({ name: 'my_key', value: 'secret-value' })
    expect(body).toEqual({ name: 'my_key', secret_type: 'env', value: 'secret-value' })
  })

  test('rejects an invalid --type instead of silently coercing it', () => {
    expect(() =>
      buildCreateSecretBody({ name: 'my_key', value: 'v', type: 'bogus' }),
    ).toThrow(/Invalid --type: must be "env" or "file"/)
  })

  test('requires --mount-path for file-type secrets', () => {
    expect(() =>
      buildCreateSecretBody({ name: 'creds', value: 'v', type: 'file' }),
    ).toThrow(/--mount-path is required when --type is "file"/)
  })

  test('carries mount_path and description through for file-type secrets', () => {
    const body = buildCreateSecretBody({
      name: 'creds',
      value: 'v',
      type: 'FILE',
      mountPath: '/run/secrets/creds.json',
      description: 'GSC creds',
    })
    expect(body).toEqual({
      name: 'creds',
      secret_type: 'file',
      value: 'v',
      mount_path: '/run/secrets/creds.json',
      description: 'GSC creds',
    })
  })

  test('sends an already-resolved value as is, even one starting with @', () => {
    const body = buildCreateSecretBody({ name: 'my_key', value: '@not-a-path' })
    expect(body.value).toBe('@not-a-path')
  })
})

describe('buildUpdateSecretBody', () => {
  const current = {
    id: 1,
    name: 'my_key',
    secret_type: 'env' as const,
    value: '***',
    mount_path: null,
    description: 'old description',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  }

  test('requires --value since the server never echoes the existing one', () => {
    expect(() => buildUpdateSecretBody({ name: 'my_key' }, current)).toThrow(
      /A value is required when updating/,
    )
  })

  test('falls back to the current type and description when not overridden', () => {
    const body = buildUpdateSecretBody({ name: 'my_key', value: 'new-value' }, current)
    expect(body).toEqual({
      name: 'my_key',
      secret_type: 'env',
      value: 'new-value',
      description: 'old description',
    })
  })

  test('rejects an invalid --type override', () => {
    expect(() =>
      buildUpdateSecretBody({ name: 'my_key', value: 'v', type: 'bogus' }, current),
    ).toThrow(/Invalid --type: must be "env" or "file"/)
  })

  test('requires a mount path when switching an existing secret to file type', () => {
    expect(() =>
      buildUpdateSecretBody({ name: 'my_key', value: 'v', type: 'file' }, current),
    ).toThrow(/--mount-path is required when type is "file"/)
  })

  test('keeps the existing mount path for a file-type secret when not overridden', () => {
    const fileSecret = { ...current, secret_type: 'file' as const, mount_path: '/run/secrets/x.json' }
    const body = buildUpdateSecretBody({ name: 'my_key', value: 'v' }, fileSecret)
    expect(body.mount_path).toBe('/run/secrets/x.json')
  })

  test('overrides the mount path when a new one is provided', () => {
    const fileSecret = { ...current, secret_type: 'file' as const, mount_path: '/run/secrets/old.json' }
    const body = buildUpdateSecretBody(
      { name: 'my_key', value: 'v', mountPath: '/run/secrets/new.json' },
      fileSecret,
    )
    expect(body.mount_path).toBe('/run/secrets/new.json')
  })
})

describe('secrets create/update value sources', () => {
  const saved = (body: unknown) => ({
    id: 1,
    value: '***',
    mount_path: null,
    description: null,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...(body as object),
  })
  const routes = {
    'POST /settings/secrets': (_url: URL, body: unknown) => saved(body),
    'GET /settings/secrets': {
      items: [saved({ name: 'gsc_creds', secret_type: 'file', mount_path: '/home/temps/gsc.json' })],
      total: 1,
    },
  }

  function postedValue(run: Awaited<ReturnType<typeof runWithApi>>): unknown {
    const sent = run.bodies.find((b) => b.request === 'POST /settings/secrets')
    expect(sent).toBeDefined()
    return (sent!.body as Record<string, unknown>).value
  }

  test('create --value-file sends a PEM file byte-for-byte, without a newline hint', async () => {
    const pem = '-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\n'
    const path = await fixtureFile(pem)
    const run = await runWithApi(registerSecretsCommands, [
      'secrets', 'create', '-n', 'tls_key', '-t', 'file', '-m', '/home/temps/key.pem', '--value-file', path,
    ], routes)
    expect(run.error).toBeUndefined()
    expect(postedValue(run)).toBe(pem)
    expect(run.stderr).not.toContain('hint')
  })

  test('create --value-stdin sends the piped value, which never reaches stdout or stderr', async () => {
    const run = await runWithApi(registerSecretsCommands, [
      'secrets', 'create', '-n', 'my_api_key', '--value-stdin',
    ], routes, { stdin: SECRET })
    expect(run.error).toBeUndefined()
    expect(postedValue(run)).toBe(SECRET)
    expect(`${run.stdout}\n${run.stderr}`).not.toContain(SECRET)
  })

  test('create -v @path still reads the file, with a deprecation note', async () => {
    const path = await fixtureFile(SECRET)
    const run = await runWithApi(registerSecretsCommands, ['secrets', 'create', '-n', 'k', '-v', `@${path}`], routes)
    expect(run.error).toBeUndefined()
    expect(postedValue(run)).toBe(SECRET)
    expect(run.stderr).toContain('deprecated')
    expect(run.stderr).toContain('--value-file')
  })

  test('create -v @value --literal sends the @ value itself', async () => {
    const run = await runWithApi(registerSecretsCommands, ['secrets', 'create', '-n', 'k', '-v', `@${SECRET}`, '--literal'], routes)
    expect(run.error).toBeUndefined()
    expect(postedValue(run)).toBe(`@${SECRET}`)
  })

  test('create with no value source fails before any request', async () => {
    const run = await runWithApi(registerSecretsCommands, ['secrets', 'create', '-n', 'k'], routes)
    expect(run.error?.message).toBe(
      'Missing the value for secret k: pass --value-file <path>, --value-stdin, or -v/--value',
    )
    expect(run.requests).toEqual([])
  })

  test('create -v and --value-file together is a conflict that does not echo the value', async () => {
    const path = await fixtureFile(SECRET)
    const run = await runWithApi(registerSecretsCommands, ['secrets', 'create', '-n', 'k', '-v', SECRET, '--value-file', path], routes)
    expect(run.error?.message).toBe('Pass the value for secret k from only one source, got -v/--value and --value-file')
    expect(run.requests).toEqual([])
  })

  test('create refuses a binary file instead of mangling it', async () => {
    const path = await fixtureFile(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0xff]))
    const run = await runWithApi(registerSecretsCommands, ['secrets', 'create', '-n', 'k', '-t', 'file', '-m', '/x', '--value-file', path], routes)
    expect(run.error?.message).toContain('is not valid UTF-8 text')
    expect(run.requests).toEqual([])
  })

  test('update --value-file re-sends the full body with the file content exactly', async () => {
    const json = '{\n  "type": "service_account"\n}\n'
    const path = await fixtureFile(json)
    const run = await runWithApi(registerSecretsCommands, ['secrets', 'update', '-n', 'gsc_creds', '--value-file', path], routes)
    expect(run.error).toBeUndefined()
    const sent = run.bodies.find((b) => b.request === 'POST /settings/secrets')!.body
    expect(sent).toEqual({ name: 'gsc_creds', secret_type: 'file', value: json, mount_path: '/home/temps/gsc.json' })
  })

  test('update --value-stdin reads the new value from a pipe', async () => {
    const run = await runWithApi(registerSecretsCommands, ['secrets', 'update', '-n', 'gsc_creds', '--value-stdin'], routes, { stdin: SECRET })
    expect(run.error).toBeUndefined()
    expect(postedValue(run)).toBe(SECRET)
  })

  test('update without a value source fails before any request', async () => {
    const run = await runWithApi(registerSecretsCommands, ['secrets', 'update', '-n', 'gsc_creds', '-d', 'new'], routes)
    expect(run.error?.message).toContain('A value is required when updating')
    expect(run.requests).toEqual([])
  })
})

describe('project-scoped secrets', () => {
  test('create, update and delete pass the project only when --project is given', async () => {
    const created = await runWithApi(
      registerSecretsCommands,
      ['secrets', 'create', '-n', 'db', '--value-stdin', '-p', 'web'],
      { 'GET /projects/by-slug/web': { id: 7, slug: 'web' }, 'POST /settings/secrets': { name: 'db', secret_type: 'env' } },
      { stdin: 'pw' },
    )
    expect(created.error).toBeUndefined()
    expect(created.bodies.at(-1)?.body).toMatchObject({ name: 'db', value: 'pw', project_id: 7 })
    const removed = await runWithApi(registerSecretsCommands, ['secrets', 'delete', 'db', '-p', '7', '--yes'], {
      'DELETE /settings/secrets/db': new Response(null, { status: 204 }),
    })
    expect(removed.requests).toEqual(['DELETE /settings/secrets/db?project_id=7'])
    const instanceWide = await runWithApi(registerSecretsCommands, ['secrets', 'delete', 'db', '--yes'], {
      'DELETE /settings/secrets/db': new Response(null, { status: 204 }),
    })
    expect(instanceWide.requests).toEqual(['DELETE /settings/secrets/db'])
  })
})
