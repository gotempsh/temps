// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Drives the sandbox commands end to end against a stubbed `fetch`: the
 * request bodies they send and what they print. Credentials come from the
 * documented env overrides, so no context store is touched.
 */

import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test'
import { Command } from 'commander'
import { registerSandboxCommands } from './index.js'

const API = 'http://temps.test'

interface Recorded {
  method: string
  path: string
  body: unknown
}

type Route = (req: Recorded) => { status: number; body?: unknown } | undefined

let requests: Recorded[] = []
let route: Route = () => undefined
let stdout: string[] = []
let stderr: string[] = []
const realFetch = globalThis.fetch
const savedEnv = { token: process.env.TEMPS_TOKEN, url: process.env.TEMPS_API_URL }
const spies: { mockRestore: () => void }[] = []

// eslint-disable-next-line no-control-regex
const strip = (s: string) => s.replace(/\u001b\[[0-9;]*m/g, '')
const out = () => strip(stdout.join('\n'))
const err = () => strip(stderr.join('\n'))

beforeEach(() => {
  requests = []
  stdout = []
  stderr = []
  process.env.TEMPS_TOKEN = 'tk_test'
  process.env.TEMPS_API_URL = API
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(typeof input === 'string' ? input : input.toString())
    const req: Recorded = {
      method: init?.method ?? 'GET',
      // The configured URL is normalised to end in `/api`.
      path: url.pathname.replace(/^\/api(?=\/)/, '') + url.search,
      body: typeof init?.body === 'string' ? JSON.parse(init.body) : undefined,
    }
    requests.push(req)
    const res = route(req)
    if (!res) return new Response('no route', { status: 599 })
    return new Response(res.body === undefined ? null : JSON.stringify(res.body), {
      status: res.status,
      headers: { 'Content-Type': 'application/problem+json' },
    })
  }) as typeof fetch
  const capture = (sink: string[]) => (...args: unknown[]) => {
    sink.push(args.map(String).join(' '))
  }
  spies.push(spyOn(console, 'log').mockImplementation(capture(stdout)))
  spies.push(spyOn(console, 'warn').mockImplementation(capture(stderr)))
  spies.push(spyOn(console, 'error').mockImplementation(capture(stderr)))
})

afterEach(() => {
  globalThis.fetch = realFetch
  for (const s of spies.splice(0)) s.mockRestore()
  if (savedEnv.token === undefined) delete process.env.TEMPS_TOKEN
  else process.env.TEMPS_TOKEN = savedEnv.token
  if (savedEnv.url === undefined) delete process.env.TEMPS_API_URL
  else process.env.TEMPS_API_URL = savedEnv.url
})

async function run(...argv: string[]): Promise<void> {
  const program = new Command()
  program.exitOverride()
  registerSandboxCommands(program)
  await program.parseAsync(['sandbox', ...argv], { from: 'user' })
}

const controlPlane = {
  id: 0,
  name: 'control-plane',
  is_control_plane: true,
  status: 'active',
  allowed: true,
  eligible: true,
  reason: null,
  live_sandboxes: 0,
}
const worker = { ...controlPlane, id: 3, name: 'worker-1', is_control_plane: false }

function placementRoutes(saved: number[] | null): Route {
  return (req) => {
    if (req.path !== '/v1/sandboxes/placement') return undefined
    if (req.method === 'GET') {
      return { status: 200, body: { allowed_node_ids: saved, nodes: [controlPlane, worker] } }
    }
    const next = (req.body as { allowed_node_ids: number[] | null }).allowed_node_ids
    return { status: 200, body: { allowed_node_ids: next, nodes: [controlPlane, worker] } }
  }
}

function sandboxInner(extra: Record<string, unknown> = {}) {
  return {
    id: 'sbx_1',
    name: 'demo',
    status: 'running',
    image: null,
    cwd: '/workspace',
    createdAt: 0,
    timeout: 60_000,
    lifecycle: 'ephemeral',
    node_id: 3,
    node_name: 'worker-1',
    ...extra,
  }
}

describe('control-plane exclusion warning', () => {
  it('fires when deny takes the control plane out', async () => {
    route = placementRoutes(null)
    await run('nodes', 'deny', 'control-plane')
    const put = requests.find((r) => r.method === 'PUT')
    expect(put?.body).toEqual({ allowed_node_ids: [3] })
    expect(err()).toContain('The control plane will no longer take new sandboxes')
    expect(err()).toContain('Fleet')
  })

  it('fires for deny-all, and still sends allowed_node_ids explicitly', async () => {
    route = placementRoutes(null)
    await run('nodes', 'deny-all')
    const put = requests.find((r) => r.method === 'PUT')
    expect(put?.body).toEqual({ allowed_node_ids: [] })
    expect(err()).toContain('No node, including the control plane, will take new sandboxes')
  })

  it('goes to stderr under --json, leaving stdout parseable', async () => {
    route = placementRoutes([0, 3])
    await run('nodes', 'set', 'worker-1', '--json')
    expect(err()).toContain('The control plane will no longer take new sandboxes')
    expect(JSON.parse(out())).toEqual({
      allowed_node_ids: [3],
      nodes: [controlPlane, worker],
    })
  })

  it('stays quiet when the control plane remains allowed or was already out', async () => {
    route = placementRoutes(null)
    await run('nodes', 'deny', 'worker-1')
    expect(err()).not.toContain('control plane')

    stderr = []
    route = placementRoutes([3])
    await run('nodes', 'deny-all')
    expect(err()).not.toContain('control plane')
  })

  it('allow-all sends an explicit null', async () => {
    route = placementRoutes([3])
    await run('nodes', 'allow-all')
    expect(requests.find((r) => r.method === 'PUT')?.body).toEqual({ allowed_node_ids: null })
  })
})

describe('sandbox nodes evict', () => {
  const evictPath = '/v1/sandboxes/placement/nodes/worker-1/evict'

  it('prints what was destroyed and the cleanup commands on success', async () => {
    route = (req) =>
      req.path === evictPath
        ? {
            status: 200,
            body: {
              node: worker,
              destroyed: ['sbx_a', 'sbx_b'],
              containers_unconfirmed: [
                {
                  sandbox_id: 'sbx_b',
                  reason: 'node did not answer',
                  cleanup_command: 'docker rm -f temps-sandbox-sbx_b',
                },
              ],
            },
          }
        : undefined
    await run('nodes', 'evict', 'worker-1', '--force')
    expect(out()).toContain('Destroyed 2 sandbox(es) on worker-1.')
    expect(out()).toContain('docker rm -f temps-sandbox-sbx_b')
    expect(out()).toContain('no longer block removing this node')
  })

  it('reports a partial eviction (503) from the problem members and fails', async () => {
    route = (req) =>
      req.path === evictPath
        ? {
            status: 503,
            body: {
              type: 'https://temps.sh/probs/sandbox-node-eviction-incomplete',
              title: 'Sandbox Node Eviction Incomplete',
              detail: 'long sentence',
              destroyed: ['sbx_a'],
              containers_unconfirmed: [
                {
                  sandbox_id: 'sbx_a',
                  reason: 'timed out',
                  cleanup_command: 'docker rm -f temps-sandbox-sbx_a',
                },
              ],
              failed: [{ sandbox_id: 'sbx_c', reason: 'database busy' }],
            },
          }
        : undefined
    await expect(run('nodes', 'evict', 'worker-1', '--force')).rejects.toThrow(
      'Eviction of node worker-1 is incomplete: 1 sandbox(es) could not be destroyed.',
    )
    expect(err()).toContain('Destroyed 1 sandbox(es) on worker-1, but not all of them.')
    expect(err()).toContain('Could not destroy 1 sandbox(es):')
    expect(out()).toContain('sbx_c: database busy')
    expect(out()).toContain('docker rm -f temps-sandbox-sbx_a')
    expect(out()).not.toContain('long sentence')
  })

  it('falls back to the detail text when the partial problem has no members', async () => {
    route = (req) =>
      req.path === evictPath
        ? {
            status: 503,
            body: {
              type: 'https://temps.sh/probs/sandbox-node-eviction-incomplete',
              title: 'Incomplete',
              detail: 'Destroyed 1, 1 left: sbx_c',
            },
          }
        : undefined
    await expect(run('nodes', 'evict', 'worker-1', '--force')).rejects.toThrow('incomplete')
    expect(out()).toContain('Destroyed 1, 1 left: sbx_c')
  })

  it('does not report any other 503 as a partial eviction', async () => {
    route = (req) =>
      req.path === evictPath
        ? { status: 503, body: { title: 'Sandbox Unavailable', detail: 'sandbox subsystem is down' } }
        : undefined
    await expect(run('nodes', 'evict', 'worker-1', '--force')).rejects.toThrow('sandbox subsystem is down')
    expect(out()).not.toContain('Destroyed')
  })

  it('prints the partial report as JSON on stdout under --json', async () => {
    route = (req) =>
      req.path === evictPath
        ? {
            status: 503,
            body: {
              type: 'https://temps.sh/probs/sandbox-node-eviction-incomplete',
              destroyed: ['sbx_a'],
              failed: [{ sandbox_id: 'sbx_c', reason: 'x' }],
            },
          }
        : undefined
    await expect(run('nodes', 'evict', 'worker-1', '--force', '--json')).rejects.toThrow()
    expect(JSON.parse(out())).toMatchObject({
      node: 'worker-1',
      destroyed: ['sbx_a'],
      failed: [{ sandbox_id: 'sbx_c', reason: 'x' }],
      containers_unconfirmed: [],
      partial: true,
    })
  })

  it('explains a 409 when another eviction is running', async () => {
    route = (req) =>
      req.path === evictPath
        ? { status: 409, body: { title: 'Conflict', detail: 'eviction in progress' } }
        : undefined
    await expect(run('nodes', 'evict', 'worker-1', '--force')).rejects.toThrow(
      'already being destroyed by another eviction',
    )
  })
})

describe('sandbox create / list with nodes', () => {
  it('sends --node in the create body', async () => {
    route = (req) =>
      req.method === 'POST' && req.path === '/v1/sandboxes'
        ? { status: 201, body: { sandbox: sandboxInner(), routes: [] } }
        : undefined
    await run('create', '--node', 'worker-1', '--json')
    expect(requests[0]?.body).toEqual({ node: 'worker-1' })
  })

  it('omits node when not given, so Temps places the sandbox', async () => {
    route = (req) =>
      req.method === 'POST' ? { status: 201, body: { sandbox: sandboxInner() } } : undefined
    await run('create', '--json')
    expect(requests[0]?.body).toEqual({})
  })

  it('shows a Node column in sandbox list', async () => {
    route = (req) =>
      req.path === '/v1/sandboxes'
        ? {
            status: 200,
            body: {
              sandboxes: [
                sandboxInner(),
                sandboxInner({ id: 'sbx_2', node_id: null, node_name: 'control-plane' }),
              ],
              pagination: { count: 2, next: null, prev: null },
            },
          }
        : undefined
    await run('list')
    expect(out()).toContain('Node')
    expect(out()).toContain('worker-1')
    expect(out()).toContain('control-plane')
  })
})

describe('unsupported on a worker node (422)', () => {
  it('prints the server detail and the way around it', async () => {
    route = () => ({
      status: 422,
      body: {
        type: 'https://temps.sh/probs/sandbox-unsupported-on-worker-node',
        title: 'Not Supported On Worker Nodes',
        detail: 'Snapshots are not available yet for sandboxes on worker nodes',
      },
    })
    await expect(run('snapshot', 'sbx_1')).rejects.toThrow(
      'Not Supported On Worker Nodes — Snapshots are not available yet for sandboxes on worker nodes',
    )
    await expect(run('snapshot', 'sbx_1')).rejects.toThrow('--node control-plane')
  })
})
