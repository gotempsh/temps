// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, afterEach, beforeAll, describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import { getCurrentUser, getProjects } from '../api/sdk.gen.js'
import {
  ApiResponseError,
  client,
  getErrorMessage,
  requireApiShape,
  setupClient,
} from './api-client.js'
import { announceCommandMaturity, __resetMaturityNotices } from './feature-maturity.js'
import { isUserObject } from '../commands/auth/login.js'
import { promptConfirm } from '../ui/prompts.js'

/**
 * A reply that is not the Temps API must fail loudly instead of reading as
 * "no data". The stub answers every path with whatever `reply` currently holds,
 * which is what a console page, a captive portal or a CDN block page does.
 */
type Reply = { status: number; body?: string; contentType?: string }

let reply: Reply = { status: 200, body: '{}', contentType: 'application/json' }
let server: ReturnType<typeof Bun.serve>
let baseUrl: string

beforeAll(() => {
  server = Bun.serve({
    port: 0,
    hostname: '127.0.0.1',
    fetch: () => {
      const headers: Record<string, string> = {}
      if (reply.contentType) headers['content-type'] = reply.contentType
      return new Response(reply.body ?? null, { status: reply.status, headers })
    },
  })
  baseUrl = `http://127.0.0.1:${server.port}`
})

afterAll(() => {
  server.stop(true)
})

afterEach(async () => {
  await setupClient('http://localhost:3000', undefined)
})

async function callProjects() {
  await setupClient(baseUrl, 'tk_test')
  return getProjects({ client })
}

describe('a 2xx that is not the Temps API', () => {
  test('an HTML page is an error naming the URL and status, not data', async () => {
    reply = { status: 200, body: '<html>sign in</html>', contentType: 'text/html; charset=utf-8' }
    const { data, error } = await callProjects()
    expect(data).toBeUndefined()
    expect(error).toBeInstanceOf(ApiResponseError)
    const message = getErrorMessage(error)
    expect(message).toContain(baseUrl)
    expect(message).toContain('HTTP 200')
    expect(message).toContain('text/html')
  })

  test('a GET answered 200 with no body and no content type is an error', async () => {
    reply = { status: 200 }
    const { error } = await callProjects()
    expect(error).toBeInstanceOf(ApiResponseError)
    expect(getErrorMessage(error)).toContain('empty body')
  })

  test('a real JSON reply is passed through untouched', async () => {
    reply = { status: 200, body: JSON.stringify({ projects: [] }), contentType: 'application/json' }
    const { data, error } = await callProjects()
    expect(error).toBeUndefined()
    expect(data).toEqual({ projects: [] })
  })

  test('a problem+json success-class reply is still JSON', async () => {
    reply = {
      status: 200,
      body: JSON.stringify({ projects: [] }),
      contentType: 'application/problem+json',
    }
    const { error } = await callProjects()
    expect(error).toBeUndefined()
  })

  test('a text/plain reply is not rejected (downloads and logs stay usable)', async () => {
    reply = { status: 200, body: 'line one\nline two', contentType: 'text/plain' }
    const { data, error } = await callProjects()
    expect(error).toBeUndefined()
    expect(data).toBe('line one\nline two')
  })
})

describe('error replies that carry no usable body', () => {
  test('an empty 404 reports the status and URL instead of {}', async () => {
    reply = { status: 404 }
    const { error } = await callProjects()
    const message = getErrorMessage(error)
    expect(message).toContain('HTTP 404')
    expect(message).toContain(baseUrl)
    expect(message).not.toBe('{}')
  })

  test('an HTML 404 does not dump the page', async () => {
    reply = {
      status: 404,
      body: '<html><body><h1>404 Not Found</h1></body></html>',
      contentType: 'text/html',
    }
    const { error } = await callProjects()
    const message = getErrorMessage(error)
    expect(message).toContain('HTTP 404')
    expect(message).not.toContain('<html>')
  })

  test('a plain-text 502 keeps its text, with the status', async () => {
    reply = { status: 502, body: 'upstream unavailable', contentType: 'text/plain' }
    const { error } = await callProjects()
    const message = getErrorMessage(error)
    expect(message).toContain('HTTP 502')
    expect(message).toContain('upstream unavailable')
  })

  test('a problem+json body keeps the server detail', async () => {
    reply = {
      status: 403,
      body: JSON.stringify({ title: 'Forbidden', detail: 'Missing projects:read' }),
      contentType: 'application/problem+json',
    }
    const { error } = await callProjects()
    expect(getErrorMessage(error)).toBe('Missing projects:read')
  })

  test('a transport failure names the URL that could not be reached', async () => {
    const closed = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: () => new Response('') })
    const deadUrl = `http://127.0.0.1:${closed.port}`
    closed.stop(true)
    await setupClient(deadUrl, 'tk_test')
    const { error } = await getProjects({ client })
    expect(error).toBeInstanceOf(ApiResponseError)
    expect(getErrorMessage(error)).toContain(`Unable to reach ${deadUrl}`)
  })
})

describe('requireApiShape and isUserObject', () => {
  const hasProjects = (value: unknown): value is { projects: unknown[] } =>
    typeof value === 'object' &&
    value !== null &&
    'projects' in value &&
    Array.isArray(value.projects)

  test('accepts the documented shape', () => {
    expect(requireApiShape({ projects: [1] }, 'listing projects', hasProjects)).toEqual({
      projects: [1],
    })
  })

  test.each([['a page string', '<html></html>'], ['another shape', { items: [] }], ['null', null]])(
    'rejects %s',
    (_label, value) => {
      expect(() => requireApiShape(value, 'listing projects', hasProjects)).toThrow(
        ApiResponseError,
      )
    },
  )

  test('a user needs a numeric id', () => {
    expect(isUserObject({ id: 7, email: 'dev@example.com' })).toBe(true)
    expect(isUserObject('<html></html>')).toBe(false)
    expect(isUserObject({})).toBe(false)
    expect(isUserObject(null)).toBe(false)
  })

  test('login validation endpoint: HTML page is not a validated user', async () => {
    reply = { status: 200, body: '<html>sign in</html>', contentType: 'text/html' }
    await setupClient(baseUrl, 'tk_test')
    const { data, error } = await getCurrentUser({ client })
    expect(data).toBeUndefined()
    expect(error).toBeInstanceOf(ApiResponseError)
  })
})

describe('feature maturity lookup', () => {
  const saved = {
    url: process.env.TEMPS_API_URL,
    token: process.env.TEMPS_API_TOKEN,
  }

  afterEach(() => {
    if (saved.url === undefined) delete process.env.TEMPS_API_URL
    else process.env.TEMPS_API_URL = saved.url
    if (saved.token === undefined) delete process.env.TEMPS_API_TOKEN
    else process.env.TEMPS_API_TOKEN = saved.token
    __resetMaturityNotices()
  })

  test('a non-array reply never aborts the command', async () => {
    reply = { status: 200, body: JSON.stringify({ not: 'a registry' }), contentType: 'application/json' }
    process.env.TEMPS_API_URL = baseUrl
    process.env.TEMPS_API_TOKEN = 'tk_test'
    const root = new Command('temps')
    const teams = root.command('teams')
    const list = teams.command('list')
    await expect(announceCommandMaturity(list, root)).resolves.toBeUndefined()
  })
})

describe('confirmation prompt without a terminal', () => {
  test('fails with a message that says to pass --yes', async () => {
    const stdinTty = process.stdin.isTTY
    const stdoutTty = process.stdout.isTTY
    process.stdin.isTTY = false
    process.stdout.isTTY = false
    try {
      await expect(promptConfirm({ message: 'Delete project foo?' })).rejects.toThrow(/--yes/)
    } finally {
      process.stdin.isTTY = stdinTty
      process.stdout.isTTY = stdoutTty
    }
  })
})
