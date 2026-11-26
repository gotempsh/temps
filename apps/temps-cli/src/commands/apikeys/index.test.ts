// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Command } from 'commander'
import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test'
import {
  registerApiKeysCommands,
  computeExpiryIso,
  groupPermissionsByCategory,
  parseApiKeyScope,
  roleTypeForPermissions,
} from './index.js'

describe('computeExpiryIso', () => {
  test('advances by the given number of days from the reference date', () => {
    const from = new Date('2026-01-01T00:00:00.000Z')
    expect(computeExpiryIso('30', from)).toBe('2026-01-31T00:00:00.000Z')
  })

  test('rejects non-numeric input rather than producing an invalid date', () => {
    expect(computeExpiryIso('never')).toBeNull()
  })

  test('rejects zero and negative day counts', () => {
    expect(computeExpiryIso('0')).toBeNull()
    expect(computeExpiryIso('-5')).toBeNull()
  })
})

describe('roleTypeForPermissions', () => {
  test('a chosen permission list makes the key custom', () => {
    expect(roleTypeForPermissions('user', ['projects:read'])).toBe('custom')
  })

  test('no chosen permissions keep the picked role', () => {
    expect(roleTypeForPermissions('user', null)).toBe('user')
    expect(roleTypeForPermissions('reader', [])).toBe('reader')
  })
})

describe('groupPermissionsByCategory', () => {
  test('buckets permissions under their category', () => {
    const permissions = [
      { name: 'a', category: 'Deploy' },
      { name: 'b', category: 'Deploy' },
      { name: 'c', category: 'Monitor' },
    ]
    const grouped = groupPermissionsByCategory(permissions)
    expect([...grouped.keys()]).toEqual(['Deploy', 'Monitor'])
    expect(grouped.get('Deploy')?.map((p) => p.name)).toEqual(['a', 'b'])
    expect(grouped.get('Monitor')?.map((p) => p.name)).toEqual(['c'])
  })

  test('falls back permissions with no category into "Other"', () => {
    // The server may not tag every permission with a category — losing
    // ungrouped permissions from the printed list would hide them entirely.
    const permissions = [{ name: 'legacy', category: null }, { name: 'legacy2' }]
    const grouped = groupPermissionsByCategory(permissions)
    expect(grouped.get('Other')?.map((p) => p.name)).toEqual(['legacy', 'legacy2'])
  })
})


describe('parseApiKeyScope', () => {
  test('keeps optional resource boundaries explicit', () => {
    expect(parseApiKeyScope()).toEqual({project_id:null,environment_id:null})
    expect(parseApiKeyScope('7')).toEqual({project_id:7,environment_id:null})
    expect(parseApiKeyScope('7','9')).toEqual({project_id:7,environment_id:9})
  })
  test('rejects environment without project, invalid and overflowing IDs', () => {
    for (const [project,environment] of [[undefined,'9'],['0',undefined],['7','-1'],['7suffix','9'],['2147483648',undefined]]) {
      expect(parseApiKeyScope(project,environment)).toBeNull()
    }
  })
})

test('invalid scope fails before authentication', async () => {
  const previous = process.exitCode
  try {
    const program = new Command()
    registerApiKeysCommands(program)
    await program.parseAsync(['apikeys', 'create', '--environment-id', '9'], { from: 'user' })
    expect(process.exitCode).toBe(1)
  } finally { process.exitCode = previous }
})

describe('apikeys create --yes', () => {
  const realFetch = globalThis.fetch
  const savedEnv = { token: process.env.TEMPS_TOKEN, url: process.env.TEMPS_API_URL }
  const spies: { mockRestore: () => void }[] = []
  let bodies: Record<string, unknown>[] = []
  let printed: string[] = []

  beforeEach(() => {
    bodies = []
    printed = []
    process.env.TEMPS_TOKEN = 'tk_test'
    process.env.TEMPS_API_URL = 'http://temps.test'
    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      const request = new Request(input, init)
      if (request.method !== 'POST' || !new URL(request.url).pathname.endsWith('/api-keys')) {
        return new Response('no route', { status: 599 })
      }
      const body = (await request.json()) as Record<string, unknown>
      bodies.push(body)
      return new Response(
        JSON.stringify({
          id: 1,
          name: body.name,
          api_key: 'tk_created',
          key_prefix: 'tk_crea',
          role_type: body.role_type,
          permissions: body.permissions,
          created_at: '2026-01-01T00:00:00Z',
          expires_at: '2027-01-01T00:00:00Z',
        }),
        { status: 201, headers: { 'Content-Type': 'application/json' } },
      )
    }) as typeof fetch
    const capture = (...args: unknown[]) => { printed.push(args.map(String).join(' ')) }
    spies.push(spyOn(console, 'log').mockImplementation(capture))
    spies.push(spyOn(console, 'warn').mockImplementation(capture))
    spies.push(spyOn(console, 'error').mockImplementation(capture))
  })

  afterEach(() => {
    globalThis.fetch = realFetch
    for (const s of spies.splice(0)) s.mockRestore()
    if (savedEnv.token === undefined) delete process.env.TEMPS_TOKEN
    else process.env.TEMPS_TOKEN = savedEnv.token
    if (savedEnv.url === undefined) delete process.env.TEMPS_API_URL
    else process.env.TEMPS_API_URL = savedEnv.url
  })

  async function create(...argv: string[]) {
    const program = new Command()
    program.exitOverride()
    registerApiKeysCommands(program)
    await program.parseAsync(['apikeys', 'create', '--yes', ...argv], { from: 'user' })
  }

  test('--permissions with a predefined role creates a custom key with exactly those permissions', async () => {
    await create('--name', 'ci', '--role', 'user', '--permissions', 'projects:read, deployments:read')
    expect(bodies).toHaveLength(1)
    expect(bodies[0]!.role_type).toBe('custom')
    expect(bodies[0]!.permissions).toEqual(['projects:read', 'deployments:read'])
    expect(printed.join('\n')).toContain('Using the custom role')
  })

  test('a --permissions list with an empty entry creates nothing', async () => {
    const previous = process.exitCode
    try {
      await create('--name', 'ci', '--role', 'user', '--permissions', 'projects:read,,')
      expect(bodies).toEqual([])
      expect(process.exitCode).toBe(1)
      expect(printed.join('\n')).toContain('--permissions has an empty entry')
    } finally {
      process.exitCode = previous
    }
  })

  test('a predefined role without --permissions is sent as chosen', async () => {
    await create('--name', 'ci', '--role', 'reader')
    expect(bodies).toHaveLength(1)
    expect(bodies[0]!.role_type).toBe('reader')
    expect(bodies[0]!.permissions ?? null).toBeNull()
    expect(printed.join('\n')).not.toContain('Using the custom role')
  })
})
