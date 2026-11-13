// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Only signed workflow messages are forwarded, to one loopback port.
 * The management credential is retained by the API adapter, never sent locally. */
export const TUNNEL_LIMIT = 8 * 1024 * 1024
export interface TunnelRequest { id: string; token: string; body: string }
export interface TunnelApi {
  open(): Promise<{ session_id: string; manifest_token: string }>
  configure(session: string, manifest: unknown): Promise<void>
  poll(session: string): Promise<{ request?: TunnelRequest | null }>
  respond(session: string, request: string, reply: { status: number; outcome?: unknown }): Promise<void>
  close(session: string): Promise<void>
}
export function localExecutor(port: string): string {
  if (!/^[0-9]+$/.test(port) || Number(port) < 1 || Number(port) > 65535) throw new Error('Local executor port must be an integer from 1 to 65535')
  return `http://127.0.0.1:${Number(port)}/.well-known/temps/workflows`
}
async function jsonBody(response: Response): Promise<unknown> {
  if (!response.body) throw new Error('Local executor returned an empty response')
  const reader = response.body.getReader()
  const chunks: Uint8Array[] = []
  let bytes = 0
  try {
    for (;;) {
      const next = await reader.read()
      if (next.done) break
      bytes += next.value.byteLength
      if (bytes > TUNNEL_LIMIT) throw new Error('Local executor response exceeds the 8 MiB development limit')
      chunks.push(next.value)
    }
  } finally { await reader.cancel().catch(() => {}) }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'))
}
export async function runTunnel(api: TunnelApi, port: string, signal: AbortSignal, ready: () => void, fetcher: typeof fetch = fetch, pollMs = 1000): Promise<void> {
  const base = localExecutor(port)
  const session = await api.open()
  const local = new AbortController()
  const stop = () => local.abort()
  signal.addEventListener('abort', stop, { once: true })
  if (signal.aborted) stop()
  const jobs = new Set<Promise<void>>()
  let failure: unknown
  try {
    const manifest = await fetcher(`${base}/manifest`, { headers: { authorization: `Bearer ${session.manifest_token}` }, redirect: 'error', signal: AbortSignal.any([local.signal, AbortSignal.timeout(10000)]) })
    if (!manifest.ok) throw new Error(`Local workflow manifest returned HTTP ${manifest.status}; check the SDK's Temps URL and workflow credential`)
    await api.configure(session.session_id, await jsonBody(manifest))
    ready()
    while (!local.signal.aborted) {
      const { request } = await api.poll(session.session_id)
      if (request) {
        if (jobs.size >= 4) throw new Error('Server exceeded the four-invocation development tunnel limit')
        const job = (async () => {
          const response = await fetcher(`${base}/invoke`, {
            method: 'POST', redirect: 'error',
            headers: { authorization: `Bearer ${request.token}`, 'content-type': 'application/json', 'temps-workflows-protocol': '1' },
            body: request.body, signal: AbortSignal.any([local.signal, AbortSignal.timeout(3600000)]),
          })
          if (response.status === 200) await api.respond(session.session_id, request.id, { status: 200, outcome: await jsonBody(response) })
          else {
            await response.body?.cancel()
            await api.respond(session.session_id, request.id, { status: [202, 409, 429].includes(response.status) ? response.status : 503 })
          }
        })().catch((error: unknown) => { if (!signal.aborted) failure = error; stop() })
        jobs.add(job)
        void job.finally(() => jobs.delete(job))
      }
      await new Promise<void>((resolve) => {
        if (local.signal.aborted) return resolve()
        const done = () => { clearTimeout(timer); local.signal.removeEventListener('abort', done); resolve() }
        const timer = setTimeout(done, pollMs)
        local.signal.addEventListener('abort', done, { once: true })
      })
    }
    if (failure) throw failure
  } finally {
    stop()
    await Promise.all(jobs)
    signal.removeEventListener('abort', stop)
    await api.close(session.session_id)
  }
}
