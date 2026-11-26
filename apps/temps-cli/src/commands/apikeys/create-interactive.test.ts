// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Drives `apikeys create` without `--yes` against a stubbed `fetch` and
 * scripted prompt answers: the request it sends, or that it sends none.
 */

import { afterEach, beforeEach, describe, expect, mock, spyOn, test } from 'bun:test'
import { Command } from 'commander'

// Scripted answers, consumed in order. Unscripted prompts fall through to the
// real ones, so other test files that import the module are unaffected.
const answers: { confirm: boolean[]; text: string[] } = { confirm: [], text: [] }
// Copied before mocking: Bun rewrites the module's live namespace to the
// mock, so falling back through it would call the wrapper again.
const realPrompts = { ...(await import('../../ui/prompts.js')) }
mock.module('../../ui/prompts.js', () => ({
  ...realPrompts,
  promptConfirm: async (options: Parameters<typeof realPrompts.promptConfirm>[0]) =>
    answers.confirm.length > 0 ? answers.confirm.shift()! : realPrompts.promptConfirm(options),
  promptText: async (options: Parameters<typeof realPrompts.promptText>[0]) =>
    answers.text.length > 0 ? answers.text.shift()! : realPrompts.promptText(options),
}))
const { registerApiKeysCommands } = await import('./index.js')
const prompts = await import('../../ui/prompts.js')

const PERMISSIONS = ['projects:read', 'deployments:read', 'deployments:write']

const realFetch = globalThis.fetch
const savedEnv = { token: process.env.TEMPS_TOKEN, url: process.env.TEMPS_API_URL }
const spies: { mockRestore: () => void }[] = []
let created: Record<string, unknown>[] = []
let listed = 0
let printed: string[] = []
let exitCode: typeof process.exitCode

beforeEach(() => {
  created = []
  listed = 0
  printed = []
  answers.confirm = []
  answers.text = []
  exitCode = process.exitCode
  process.env.TEMPS_TOKEN = 'tk_test'
  process.env.TEMPS_API_URL = 'http://temps.test'
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const request = new Request(input, init)
    const path = new URL(request.url).pathname
    if (request.method === 'GET' && path.endsWith('/api-keys/permissions')) {
      listed += 1
      return Response.json({
        permissions: PERMISSIONS.map(name => ({ name, description: name, category: 'Test' })),
      })
    }
    if (request.method === 'POST' && path.endsWith('/api-keys')) {
      const body = (await request.json()) as Record<string, unknown>
      created.push(body)
      return Response.json(
        {
          id: 1,
          name: body.name,
          api_key: 'tk_created',
          key_prefix: 'tk_crea',
          role_type: body.role_type,
          permissions: body.permissions,
          created_at: '2026-01-01T00:00:00Z',
          expires_at: '2027-01-01T00:00:00Z',
        },
        { status: 201 },
      )
    }
    return new Response('no route', { status: 599 })
  }) as typeof fetch
  const capture = (...args: unknown[]) => { printed.push(args.map(String).join(' ')) }
  spies.push(spyOn(console, 'log').mockImplementation(capture))
  spies.push(spyOn(console, 'warn').mockImplementation(capture))
  spies.push(spyOn(console, 'error').mockImplementation(capture))
})

afterEach(() => {
  globalThis.fetch = realFetch
  for (const s of spies.splice(0)) s.mockRestore()
  process.exitCode = exitCode
  if (savedEnv.token === undefined) delete process.env.TEMPS_TOKEN
  else process.env.TEMPS_TOKEN = savedEnv.token
  if (savedEnv.url === undefined) delete process.env.TEMPS_API_URL
  else process.env.TEMPS_API_URL = savedEnv.url
})

async function create(...argv: string[]) {
  const program = new Command()
  program.exitOverride()
  registerApiKeysCommands(program)
  await program.parseAsync(['apikeys', 'create', ...argv], { from: 'user' })
}

test('an unscripted prompt falls back to the real prompt, not the stub', () => {
  // The stub is installed, and its fallback is the original function.
  expect(realPrompts.promptConfirm).not.toBe(prompts.promptConfirm)
  expect(realPrompts.promptText).not.toBe(prompts.promptText)
})

describe('apikeys create, interactive', () => {
  test('--permissions is the key\'s permissions without asking for them', async () => {
    answers.confirm = [false] // no expiry
    await create('--name', 'ci', '--role', 'user', '--permissions', 'deployments:read')
    expect(listed).toBe(0)
    expect(created).toEqual([
      expect.objectContaining({ role_type: 'custom', permissions: ['deployments:read'] }),
    ])
  })

  test('picked numbers become the key\'s permissions', async () => {
    answers.confirm = [false, true] // no expiry; customize
    answers.text = ['3, 1']
    await create('--name', 'ci', '--role', 'user')
    expect(created).toEqual([
      expect.objectContaining({
        role_type: 'custom',
        permissions: ['deployments:write', 'projects:read'],
      }),
    ])
  })

  test('a selection with a number not on the list creates nothing', async () => {
    for (const input of ['9', '1, 9', '1, x', '']) {
      created = []
      printed = []
      process.exitCode = 0
      answers.confirm = [false, true]
      answers.text = [input]
      await create('--name', 'ci', '--role', 'user')
      expect(created).toEqual([])
      expect(process.exitCode).toBe(1)
      expect(printed.join('\n')).toContain('No API key was created')
    }
  })

  test('declining to customize keeps the chosen role and its permissions', async () => {
    answers.confirm = [false, false]
    await create('--name', 'ci', '--role', 'reader')
    expect(created).toEqual([expect.objectContaining({ role_type: 'reader' })])
    expect(created[0]!.permissions ?? null).toBeNull()
  })
})
