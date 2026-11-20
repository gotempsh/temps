// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'
import { attachHttpStatus } from '@/lib/http-error-status'
import {
  registerMockRestore,
  restoreAllMockApis,
  unregisterMockRestore,
} from './mock-registry'

export { restoreAllMockApis }

type Handler = (request: Request) => Response | Promise<Response>

export interface RecordedCall {
  method: string
  path: string
}

export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

/**
 * Replaces `globalThis.fetch` for the generated API client with a route table
 * keyed `"METHOD /path"` (path without the `/api` prefix and query string).
 * Unmatched requests answer 404 and are recorded, so a test can assert that
 * a request was *not* sent as well as that it was. Call `restore()` when done; anything
 * left installed is restored after each test.
 */
export function mockApi(routes: Record<string, Handler>) {
  const calls: RecordedCall[] = []
  const previous = client.getConfig()
  // Installed on the client itself rather than on `globalThis.fetch`: other
  // tests in the shared `bun test` process may already have given the client
  // its own fetch implementation.
  const mockedFetch = (async (
    input: Request | string | URL,
    init?: RequestInit
  ) => {
    const request = input instanceof Request ? input : new Request(input, init)
    const url = new URL(request.url)
    const path = url.pathname.replace(/^\/api/, '')
    calls.push({ method: request.method, path })
    const handler = routes[`${request.method} ${path}`]
    if (!handler) return jsonResponse({ title: 'Not mocked' }, 404)
    return handler(request)
  }) as typeof fetch
  // Same as App.tsx: pages tell a 404 from a 403/500 by the status copied onto
  // the thrown body.
  if (!client.interceptors.error.exists(attachHttpStatus)) {
    client.interceptors.error.use(attachHttpStatus)
  }
  client.setConfig({ baseUrl: 'http://localhost/api', fetch: mockedFetch })
  const restore = () => {
    if (!unregisterMockRestore(restore)) return
    client.setConfig({ baseUrl: previous.baseUrl, fetch: previous.fetch })
  }
  registerMockRestore(restore)
  return {
    calls,
    called: (method: string, path: string) =>
      calls.some((call) => call.method === method && call.path === path),
    restore,
  }
}
