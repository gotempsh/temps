// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { localExecutor, runTunnel, type TunnelApi } from './tunnel.js'
test('only accepts a loopback port, never a forwarding URL', () => {
  expect(localExecutor('3000')).toBe('http://127.0.0.1:3000/.well-known/temps/workflows')
  for (const port of ['0', '65536', 'localhost:3000', '3000/path', '-1', '1.5']) expect(() => localExecutor(port)).toThrow()
})
test('forwards signed bytes and closes the session without disclosing management credentials', async () => {
  const stop = new AbortController()
  const replies: unknown[] = []
  let closed = false
  let polls = 0
  const api: TunnelApi = {
    open: async () => ({ session_id: 'session', manifest_token: 'manifest-proof' }),
    configure: async (_, manifest) => { expect(manifest).toEqual({ protocol_version: 1 }) },
    poll: async () => ({ request: polls++ === 0 ? { id: 'request', token: 'invoke-proof', body: '{"signed": true}' } : null }),
    respond: async (_session, id, reply) => { expect(id).toBe('request'); replies.push(reply); stop.abort() },
    close: async () => { closed = true },
  }
  const fetcher = (async (url: string | URL | Request, options?: RequestInit) => {
    expect(options?.redirect).toBe('error')
    const headers = new Headers(options?.headers)
    if (String(url).endsWith('/manifest')) { expect(headers.get('authorization')).toBe('Bearer manifest-proof'); return Response.json({ protocol_version: 1 }) }
    expect(options?.body).toBe('{"signed": true}')
    expect(headers.get('authorization')).toBe('Bearer invoke-proof')
    return Response.json({ outcome: 'completed', output: 1 })
  }) as typeof fetch
  await runTunnel(api, '3000', stop.signal, () => {}, fetcher, 1)
  expect(closed).toBe(true)
  expect(replies).toEqual([{ status: 200, outcome: { outcome: 'completed', output: 1 } }])
})
test('closes when the local executor cannot serve a manifest', async () => {
  let closed = false
  const unexpected = async () => { throw new Error('Manifest failure must prevent forwarding') }
  const api: TunnelApi = { open: async () => ({ session_id: 's', manifest_token: 't' }), close: async () => { closed = true }, configure: unexpected, poll: unexpected, respond: unexpected }
  await expect(runTunnel(api, '3000', new AbortController().signal, () => {}, Object.assign(async () => new Response(null, { status: 401 }), { preconnect: fetch.preconnect }))).rejects.toThrow('HTTP 401')
  expect(closed).toBe(true)
})
