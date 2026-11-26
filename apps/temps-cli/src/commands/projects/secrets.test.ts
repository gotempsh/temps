// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import type { Command } from 'commander'
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { registerProjectSecretsCommands } from './secrets.js'
import { runWithApi } from '../../lib/mock-api.js'

const SECRET = 's3cr3t-test-value'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
})

async function fixtureFile(contents: string | Uint8Array, mode = 0o600): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-secrets-test-'))
  temporaryDirectories.push(directory)
  const path = join(directory, 'auth.json')
  await writeFile(path, contents)
  await chmod(path, mode)
  return path
}

function register(program: Command): void {
  registerProjectSecretsCommands(program.command('projects'))
}

const secret = (body: Record<string, unknown> = {}) => ({
  id: 5,
  key: 'CODEX_AUTH',
  environments: [],
  compose_services: [],
  include_in_preview: false,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  ...body,
})

const routes = {
  'GET /projects/by-slug/app': { id: 4, slug: 'app' },
  'POST /projects/4/secrets': (_url: URL, body: unknown) => secret(body as Record<string, unknown>),
  // One page, as the list endpoint answers: the secret the tests update is
  // on page 2, so a lookup by key has to read past the first page.
  'GET /projects/4/secrets': (url: URL) =>
    url.searchParams.get('page') === '2'
      ? { items: [secret({ compose_services: ['worker'] })], total: 101, page: 2, page_size: 100 }
      : {
          items: Array.from({ length: 100 }, (_, i) => secret({ id: 100 + i, key: `OTHER_${i}` })),
          total: 101,
          page: 1,
          page_size: 100,
        },
  'PUT /projects/4/secrets/5': (_url: URL, body: unknown) => secret(body as Record<string, unknown>),
}

function sentBody(run: Awaited<ReturnType<typeof runWithApi>>, request: string): Record<string, unknown> {
  const sent = run.bodies.find((b) => b.request === request)
  expect(sent).toBeDefined()
  return sent!.body as Record<string, unknown>
}

describe('projects secrets create', () => {
  test('--value-file sends the file content byte-for-byte', async () => {
    const contents = '{"token":"abc"}\n'
    const path = await fixtureFile(contents)
    const run = await runWithApi(register, ['projects', 'secrets', 'create', '-p', 'app', '-k', 'CODEX_AUTH', '--value-file', path], routes)
    expect(run.error).toBeUndefined()
    expect(sentBody(run, 'POST /projects/4/secrets')).toEqual({ key: 'CODEX_AUTH', value: contents })
    // A mounted file normally ends in a newline: no hint for project secrets.
    expect(run.stderr).not.toContain('hint')
  })

  test('--value-stdin sends the piped value and never prints it', async () => {
    const run = await runWithApi(register, ['projects', 'secrets', 'create', '-p', 'app', '-k', 'DB_PASSWORD', '--value-stdin'], routes, {
      stdin: SECRET,
    })
    expect(run.error).toBeUndefined()
    expect(sentBody(run, 'POST /projects/4/secrets').value).toBe(SECRET)
    expect(`${run.stdout}\n${run.stderr}`).not.toContain(SECRET)
  })

  test('-v @path keeps working, with a deprecation note pointing to --value-file', async () => {
    const path = await fixtureFile(SECRET)
    const run = await runWithApi(register, ['projects', 'secrets', 'create', '-p', 'app', '-k', 'K', '-v', `@${path}`], routes)
    expect(run.error).toBeUndefined()
    expect(sentBody(run, 'POST /projects/4/secrets').value).toBe(SECRET)
    expect(run.stderr).toContain('deprecated; use --value-file <path>')
  })

  test('-v @value --literal sends the value itself', async () => {
    const run = await runWithApi(register, ['projects', 'secrets', 'create', '-p', 'app', '-k', 'K', '-v', `@${SECRET}`, '--literal'], routes)
    expect(run.error).toBeUndefined()
    expect(sentBody(run, 'POST /projects/4/secrets').value).toBe(`@${SECRET}`)
  })

  test('a missing -v @file fails before any request and does not echo the would-be value', async () => {
    const run = await runWithApi(register, ['projects', 'secrets', 'create', '-p', 'app', '-k', 'K', '-v', `@${SECRET}`], routes)
    expect(run.error?.message).toContain('--literal')
    expect(run.error?.message).not.toContain(SECRET)
    expect(run.stderr).not.toContain(SECRET)
    expect(run.requests).toEqual([])
  })

  test('a world-readable file is sent, with a permission warning', async () => {
    const path = await fixtureFile(SECRET, 0o644)
    const run = await runWithApi(register, ['projects', 'secrets', 'create', '-p', 'app', '-k', 'K', '--value-file', path], routes)
    expect(run.error).toBeUndefined()
    if (process.platform !== 'win32') expect(run.stderr).toContain('readable by other users (mode 644)')
  })

  test('no value source fails before any request', async () => {
    const run = await runWithApi(register, ['projects', 'secrets', 'create', '-p', 'app', '-k', 'K'], routes)
    expect(run.error?.message).toBe('Missing the value for secret K: pass --value-file <path>, --value-stdin, or -v/--value')
    expect(run.requests).toEqual([])
  })
})

describe('projects secrets update', () => {
  test('--value-file - reads the new value from stdin and keeps the service scope', async () => {
    const run = await runWithApi(register, ['projects', 'secrets', 'update', '-p', 'app', '-k', 'CODEX_AUTH', '--value-file', '-'], routes, {
      stdin: `${SECRET}\n`,
    })
    expect(run.error).toBeUndefined()
    expect(sentBody(run, 'PUT /projects/4/secrets/5')).toEqual({ value: `${SECRET}\n`, compose_services: ['worker'] })
  })

  test('--value-file with --trim-trailing-newline drops one newline', async () => {
    const path = await fixtureFile(`${SECRET}\r\n`)
    const run = await runWithApi(
      register,
      ['projects', 'secrets', 'update', '-p', 'app', '-k', 'CODEX_AUTH', '--value-file', path, '--trim-trailing-newline'],
      routes,
    )
    expect(run.error).toBeUndefined()
    expect(sentBody(run, 'PUT /projects/4/secrets/5').value).toBe(SECRET)
  })

  test('without a value source the stored value is kept (no value in the body)', async () => {
    const run = await runWithApi(register, ['projects', 'secrets', 'update', '-p', 'app', '-k', 'CODEX_AUTH', '--include-in-preview'], routes)
    expect(run.error).toBeUndefined()
    expect(sentBody(run, 'PUT /projects/4/secrets/5')).toEqual({ compose_services: ['worker'], include_in_preview: true })
    expect(run.requests.filter((r) => r.startsWith('GET /projects/4/secrets'))).toEqual([
      'GET /projects/4/secrets?page=1&page_size=100',
      'GET /projects/4/secrets?page=2&page_size=100',
    ])
  })

  test('--value-stdin on a terminal fails before any request', async () => {
    const run = await runWithApi(register, ['projects', 'secrets', 'update', '-p', 'app', '-k', 'CODEX_AUTH', '--value-stdin'], routes, {
      stdin: 'tty',
    })
    expect(run.error?.message).toContain('stdin is a terminal')
    expect(run.requests).toEqual([])
  })

  test('a directory is refused', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'temps-secrets-dir-'))
    temporaryDirectories.push(directory)
    const run = await runWithApi(register, ['projects', 'secrets', 'update', '-p', 'app', '-k', 'CODEX_AUTH', '--value-file', directory], routes)
    expect(run.error?.message).toContain('it is a directory, not a file')
    expect(run.requests).toEqual([])
  })
})
