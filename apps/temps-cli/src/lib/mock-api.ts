// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Test helper: run a CLI command against canned API responses. Requests go
 * to a closed local port through a mocked `fetch`, so nothing reaches a real
 * server; a request with no canned response fails the command with a 500
 * naming the route, and `process.exit` throws instead of ending the runner.
 */

import { spyOn } from 'bun:test'
import { Command } from 'commander'
import { setQuietMode } from '../ui/spinner.js'

/** A canned body, or a function of the request URL (and JSON body) returning one. */
export type MockRoute = unknown | ((url: URL, body: unknown) => unknown)

export interface MockRun {
  /** 0 unless the command set a failure code. */
  exitCode: number
  stdout: string
  stderr: string
  /** `METHOD /path?query` of every request, in order (without the `/api` prefix). */
  requests: string[]
  /** JSON bodies sent, keyed like `requests`. */
  bodies: Array<{ request: string; body: unknown }>
  error?: Error
}

/** Keys are `METHOD /path` without the `/api` prefix, e.g. `GET /projects/by-slug/web`. */
export async function runWithApi(
  register: (program: Command) => void,
  argv: string[],
  routes: Record<string, MockRoute>
): Promise<MockRun> {
  const saved = { url: process.env.TEMPS_API_URL, token: process.env.TEMPS_TOKEN, exitCode: process.exitCode }
  process.env.TEMPS_API_URL = 'http://127.0.0.1:9'
  process.env.TEMPS_TOKEN = 'test-token-never-sent'
  // Bun ignores `process.exitCode = undefined`, so 0 marks "not set".
  process.exitCode = 0
  const stdout: string[] = []
  const stderr: string[] = []
  const requests: string[] = []
  const bodies: MockRun['bodies'] = []
  const logSpy = spyOn(console, 'log').mockImplementation((...args: unknown[]) => {
    stdout.push(args.join(' '))
  })
  const errSpy = spyOn(console, 'error').mockImplementation((...args: unknown[]) => {
    stderr.push(args.join(' '))
  })
  const warnSpy = spyOn(console, 'warn').mockImplementation((...args: unknown[]) => {
    stderr.push(args.join(' '))
  })
  const stderrSpy = spyOn(process.stderr, 'write').mockImplementation(((chunk: unknown) => {
    stderr.push(String(chunk))
    return true
  }) as typeof process.stderr.write)
  const exitSpy = spyOn(process, 'exit').mockImplementation(((code?: number) => {
    throw new Error(`process.exit(${code}) called`)
  }) as typeof process.exit)
  const fetchSpy = spyOn(globalThis, 'fetch').mockImplementation((async (input: Request | string | URL) => {
    const request = input instanceof Request ? input : new Request(String(input))
    const url = new URL(request.url)
    const path = url.pathname.replace(/^\/api/, '')
    const key = `${request.method} ${path}`
    requests.push(`${key}${url.search}`)
    const text = request.body ? await request.text() : ''
    const body = text ? JSON.parse(text) : undefined
    if (body !== undefined) bodies.push({ request: key, body })
    const route = routes[key]
    if (route === undefined) {
      return Response.json({ title: 'Not mocked', detail: `No canned response for ${key}` }, { status: 500 })
    }
    const payload = typeof route === 'function' ? (route as (url: URL, body: unknown) => unknown)(url, body) : route
    return Response.json(payload)
  }) as typeof fetch)
  let error: Error | undefined
  try {
    const program = new Command().exitOverride()
    register(program)
    await program.parseAsync(argv, { from: 'user' })
  } catch (err) {
    error = err instanceof Error ? err : new Error(String(err))
  } finally {
    logSpy.mockRestore()
    errSpy.mockRestore()
    warnSpy.mockRestore()
    stderrSpy.mockRestore()
    exitSpy.mockRestore()
    fetchSpy.mockRestore()
    setQuietMode(false)
  }
  const run: MockRun = {
    exitCode: Number(process.exitCode ?? 0),
    stdout: stdout.join('\n'),
    stderr: stderr.join('\n'),
    requests,
    bodies,
    error,
  }
  process.exitCode = saved.exitCode ?? 0
  if (saved.url === undefined) delete process.env.TEMPS_API_URL
  else process.env.TEMPS_API_URL = saved.url
  if (saved.token === undefined) delete process.env.TEMPS_TOKEN
  else process.env.TEMPS_TOKEN = saved.token
  return run
}
