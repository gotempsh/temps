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

/** A canned body, or a function of the request URL (and JSON body) returning one.
 * A `Response` is returned as is, e.g. to answer with an error status. */
export type MockRoute = unknown | ((url: URL, body: unknown) => unknown)

export interface MockRun {
  /** 0 unless the command set a failure code. */
  exitCode: number
  stdout: string
  stderr: string
  /** `METHOD /path?query` of every request, in order (without the `/api` prefix). */
  requests: string[]
  /** Bodies sent, keyed like `requests`: parsed JSON, or the text of a non-JSON body. */
  bodies: Array<{ request: string; body: unknown }>
  error?: Error
}

export interface MockRunOptions {
  /**
   * What the command reads from stdin. A string or bytes stand for a pipe;
   * `'tty'` for an interactive terminal with nothing piped. Unset leaves the
   * runner's own stdin in place.
   */
  stdin?: string | Uint8Array | 'tty'
  /** The API base URL the command sees (default: an address nothing listens on). */
  apiUrl?: string
}

/** A stand-in for `process.stdin`: a pipe yielding `input`, or an empty terminal. */
function fakeStdin(input: string | Uint8Array | 'tty'): { isTTY: boolean; [Symbol.asyncIterator](): AsyncGenerator<Uint8Array> } {
  const isTTY = input === 'tty'
  const bytes = typeof input === 'string' ? new TextEncoder().encode(isTTY ? '' : input) : input
  return {
    isTTY,
    async *[Symbol.asyncIterator]() {
      if (!isTTY && bytes.length > 0) yield bytes
    },
  }
}

/** Keys are `METHOD /path` without the `/api` prefix, e.g. `GET /projects/by-slug/web`. */
export async function runWithApi(
  register: (program: Command) => void,
  argv: string[],
  routes: Record<string, MockRoute>,
  options: MockRunOptions = {}
): Promise<MockRun> {
  const savedStdin = Object.getOwnPropertyDescriptor(process, 'stdin')
  if (options.stdin !== undefined) {
    Object.defineProperty(process, 'stdin', { value: fakeStdin(options.stdin), configurable: true, writable: true })
  }
  const saved = { url: process.env.TEMPS_API_URL, token: process.env.TEMPS_TOKEN, exitCode: process.exitCode }
  process.env.TEMPS_API_URL = options.apiUrl ?? 'http://127.0.0.1:9'
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
  const fetchSpy = spyOn(globalThis, 'fetch').mockImplementation((async (input: Request | string | URL, init?: RequestInit) => {
    // `init` carries the method, headers and body of a raw `fetch(url, { ... })` call.
    const request = input instanceof Request && !init ? input : new Request(input instanceof Request ? input : String(input), init)
    const url = new URL(request.url)
    const path = url.pathname.replace(/^\/api/, '')
    const key = `${request.method} ${path}`
    requests.push(`${key}${url.search}`)
    const text = request.body ? await request.text() : ''
    // A binary upload is recorded as its text; anything else as parsed JSON.
    let body: unknown
    try {
      body = text ? JSON.parse(text) : undefined
    } catch {
      body = text
    }
    if (body !== undefined) bodies.push({ request: key, body })
    const route = routes[key]
    if (route === undefined) {
      return Response.json({ title: 'Not mocked', detail: `No canned response for ${key}` }, { status: 500 })
    }
    const payload = typeof route === 'function' ? (route as (url: URL, body: unknown) => unknown)(url, body) : route
    // A Response is sent as is, so a route can answer with an error status.
    return payload instanceof Response ? payload : Response.json(payload)
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
    if (options.stdin !== undefined && savedStdin) Object.defineProperty(process, 'stdin', savedStdin)
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
