// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  ConnectorNodeView,
  ConnectorView,
  CreateConnectorRequest,
  ManagedDomainResponse,
} from '@/api/client/types.gen'
import {
  type IngressSetupApi,
  ingressSetupUrl,
  runTunnelSetup,
  type SetupPlan,
  type SetupStep,
  subdomainLabelError,
  tunnelDomainFor,
  hostSuffixFor,
  schemeLabelError,
  exampleHost,
  uniqueProviderName,
  uniqueTunnelName,
  unavailableWorker,
  workerEligibility,
} from './ingress-setup'

function managed(
  overrides: Partial<ManagedDomainResponse> = {}
): ManagedDomainResponse {
  return {
    auto_manage: true,
    created_at: '2026-01-01T00:00:00Z',
    domain: 'example.com',
    generated_hostname_mode: 'standard',
    id: 3,
    provider_id: 1,
    proxied_by_default: false,
    sync_generated_records: false,
    updated_at: '2026-01-01T00:00:00Z',
    verified: true,
    ...overrides,
  }
}

function connector(
  nodes: Partial<ConnectorNodeView>[],
  overrides: Partial<ConnectorView> = {}
): ConnectorView {
  return {
    auto_publish: true,
    created_at: '2026-01-01T00:00:00Z',
    domain: 'example.com',
    edge_port: 8080,
    environment_hosts: false,
    id: 9,
    name: 'example-com',
    provider: 'cloudflare_tunnel',
    publications: [],
    status: 'ready',
    nodes: nodes.map((n) => ({
      node_id: 1,
      node_name: 'worker-a',
      state: 'pending',
      ...n,
    })),
    ...overrides,
  }
}

/** A fake API that records every call, with overridable answers. */
function fakeApi(overrides: Partial<IngressSetupApi> = {}) {
  const calls: string[] = []
  const api: IngressSetupApi = {
    addManagedDomain: async (id, domain) => {
      calls.push(`add ${id} ${domain}`)
      return managed({ domain, verified: false })
    },
    enableAutoManage: async (id, domain, on) => {
      calls.push(`auto ${id} ${domain} ${on}`)
    },
    verifyManagedDomain: async (id, domain) => {
      calls.push(`verify ${id} ${domain}`)
      return managed({ domain })
    },
    removeManagedDomain: async (id, domain) => {
      calls.push(`remove-domain ${id} ${domain}`)
    },
    listNodes: async () => {
      calls.push('nodes')
      return [{ id: 1, name: 'worker-a', role: 'worker', status: 'active' }]
    },
    createConnector: async (request) => {
      calls.push(`create ${request.name} ${request.node_ids.join(',')}`)
      return connector([{ node_id: 1, state: 'pending' }])
    },
    findConnectorByName: async (name) => {
      calls.push(`find ${name}`)
      return undefined
    },
    getConnector: async (id) => {
      calls.push(`get ${id}`)
      return connector([{ node_id: 1, state: 'ready', ready_connections: 4 }])
    },
    deleteConnector: async (id) => {
      calls.push(`delete-connector ${id}`)
    },
    deleteDnsProvider: async (id) => {
      calls.push(`delete-provider ${id}`)
    },
    sleep: async () => {},
    ...overrides,
  }
  return { api, calls }
}

const plan: SetupPlan = {
  provider: { id: 1, name: 'Cloudflare', createdThisRun: false },
  zone: 'example.com',
  managed: undefined,
  tunnelDomain: 'example.com',
  tunnelName: 'example-com',
  nodes: [{ id: 1, name: 'worker-a' }],
}

describe('runTunnelSetup', () => {
  test('adds and verifies the domain, creates the tunnel, waits for the worker', async () => {
    const { api, calls } = fakeApi()
    const reports: SetupStep[][] = []
    const outcome = await runTunnelSetup(plan, api, (s) => reports.push(s))
    expect(outcome.kind).toBe('done')
    expect(calls).toEqual([
      'nodes',
      'add 1 example.com',
      'verify 1 example.com',
      'create example-com 1',
      'get 9',
    ])
    const last = reports[reports.length - 1]
    expect(last.map((s) => s.key)).toEqual([
      'account',
      'workers',
      'domain',
      'tunnel',
      'node-1',
    ])
    expect(last.map((s) => s.status)).toEqual([
      'done',
      'done',
      'done',
      'done',
      'done',
    ])
    expect(last[4].detail).toBe('4 connections to Cloudflare')
  })

  test('a one-level label is sent as the tunnel host suffix, and only then', async () => {
    const requests: CreateConnectorRequest[] = []
    const record = async (request: CreateConnectorRequest) => {
      requests.push(request)
      return connector([{ node_id: 1, state: 'ready', ready_connections: 4 }])
    }
    const { api } = fakeApi({ createConnector: record })
    await runTunnelSetup(
      { ...plan, managed: managed(), hostSuffix: 'lab' },
      api,
      () => {}
    )
    await runTunnelSetup({ ...plan, managed: managed() }, api, () => {})
    expect(requests[0].values).toEqual({
      dns_provider_id: 1,
      domain: 'example.com',
    })
    expect(requests[0].host_suffix).toBe('lab')
    expect('host_suffix' in requests[1]).toBe(false)
  })

  test('a verified managed domain is used as it is', async () => {
    const { api, calls } = fakeApi()
    await runTunnelSetup({ ...plan, managed: managed() }, api, () => {})
    expect(calls.slice(0, 2)).toEqual(['nodes', 'create example-com 1'])
  })

  test('a domain the token cannot manage fails with its reason and undoes only what the run added', async () => {
    const { api, calls } = fakeApi({
      verifyManagedDomain: async () =>
        managed({
          verified: false,
          zone_access_error: 'The token has no access to zone example.com',
        }),
    })
    const outcome = await runTunnelSetup(
      { ...plan, provider: { ...plan.provider, createdThisRun: true } },
      api,
      () => {}
    )
    expect(outcome.kind).toBe('failed')
    if (outcome.kind !== 'failed') return
    expect(outcome.error).toBe('The token has no access to zone example.com')
    expect(calls).toContain('remove-domain 1 example.com')
    expect(calls).toContain('delete-provider 1')
    expect(calls.some((c) => c.startsWith('create'))).toBe(false)
    expect(outcome.undo.every((u) => u.ok)).toBe(true)
  })

  test('a pre-existing account and domain are never undone', async () => {
    const { api, calls } = fakeApi({
      createConnector: async () => {
        throw new Error('Cloudflare refused: missing Cloudflare Tunnel Edit')
      },
    })
    const outcome = await runTunnelSetup(
      { ...plan, managed: managed() },
      api,
      () => {}
    )
    expect(outcome.kind).toBe('failed')
    // Only the lookup for a half-saved tunnel: nothing pre-existing is touched.
    expect(calls).toEqual(['nodes', 'find example-com'])
  })

  test('a worker that went offline stops the run before anything is created', async () => {
    const { api, calls } = fakeApi({
      listNodes: async () => {
        calls.push('nodes')
        return [{ id: 1, name: 'worker-a', role: 'worker', status: 'offline' }]
      },
    })
    const reports: SetupStep[][] = []
    const outcome = await runTunnelSetup(plan, api, (s) => reports.push(s))
    expect(outcome.kind).toBe('failed')
    if (outcome.kind !== 'failed') return
    expect(outcome.error).toBe(
      "Worker 'worker-a' is offline, so it cannot run a tunnel. Start its agent on that machine (sudo systemctl start temps-agent with the systemd service, otherwise `temps agent` under its supervisor) or untick it under Workers, then try again."
    )
    // No domain, no tunnel: nothing to roll back.
    expect(calls).toEqual(['nodes'])
    expect(outcome.undo).toEqual([])
    expect(outcome.stoppedBeforeChanges).toBe(true)
    const last = reports[reports.length - 1]
    expect(last.find((s) => s.key === 'workers')?.status).toBe('failed')
    expect(last.find((s) => s.key === 'domain')?.status).toBe('pending')
  })

  test('a run whose page went away creates nothing after the workers check', async () => {
    const controller = new AbortController()
    const { api, calls } = fakeApi({
      listNodes: async () => {
        calls.push('nodes')
        // The page unmounts while the check is in flight.
        controller.abort()
        return [{ id: 1, name: 'worker-a', role: 'worker', status: 'active' }]
      },
    })
    const outcome = await runTunnelSetup(plan, api, () => {}, controller.signal)
    expect(outcome.kind).toBe('aborted')
    expect(calls).toEqual(['nodes'])
  })

  test('a worker the run cannot read the nodes for creates nothing either', async () => {
    const { api, calls } = fakeApi({
      listNodes: async () => {
        calls.push('nodes')
        throw new Error('Could not list nodes (HTTP 503)')
      },
    })
    const outcome = await runTunnelSetup(plan, api, () => {})
    expect(outcome.kind).toBe('failed')
    if (outcome.kind !== 'failed') return
    expect(outcome.error).toBe('Could not list nodes (HTTP 503)')
    expect(calls).toEqual(['nodes'])
  })

  test('a token added on this page is kept when the workers check fails, for a retry', async () => {
    const { api, calls } = fakeApi({
      listNodes: async () => [],
    })
    const outcome = await runTunnelSetup(
      { ...plan, provider: { ...plan.provider, createdThisRun: true } },
      api,
      () => {}
    )
    expect(outcome.kind).toBe('failed')
    if (outcome.kind !== 'failed') return
    expect(outcome.error).toContain(
      "Worker 'worker-a' is no longer in this cluster"
    )
    expect(outcome.stoppedBeforeChanges).toBe(true)
    expect(outcome.undo).toEqual([])
    expect(calls).toEqual([])
  })

  test('a tunnel saved before its setup failed is found by name and deleted', async () => {
    const { api, calls } = fakeApi({
      createConnector: async () => {
        throw new Error('The worker assignment failed')
      },
      findConnectorByName: async () => connector([], { id: 42 }),
    })
    const outcome = await runTunnelSetup(
      { ...plan, managed: managed() },
      api,
      () => {}
    )
    expect(outcome.kind).toBe('failed')
    expect(calls).toContain('delete-connector 42')
  })

  test('turns automatic DNS on for an existing domain and back off on failure', async () => {
    const { api, calls } = fakeApi({
      getConnector: async () =>
        connector([], { status: 'error', last_error: 'Tunnel quota reached' }),
    })
    const outcome = await runTunnelSetup(
      { ...plan, managed: managed({ auto_manage: false }) },
      api,
      () => {}
    )
    expect(outcome.kind).toBe('failed')
    if (outcome.kind !== 'failed') return
    expect(outcome.error).toBe('Tunnel quota reached')
    expect(calls).toEqual([
      'nodes',
      'auto 1 example.com true',
      'create example-com 1',
      'delete-connector 9',
      'auto 1 example.com false',
    ])
  })

  test('a refused worker fails the run when no other worker connected', async () => {
    const { api } = fakeApi({
      getConnector: async () =>
        connector([
          { node_id: 1, state: 'refused', last_error: 'ingress is off' },
        ]),
    })
    const outcome = await runTunnelSetup(plan, api, () => {})
    expect(outcome.kind).toBe('failed')
    if (outcome.kind !== 'failed') return
    expect(outcome.error).toContain('ingress is off')
  })

  test('a slow worker hands over with everything kept', async () => {
    let clock = 0
    const { api, calls } = fakeApi({
      getConnector: async () => connector([{ node_id: 1, state: 'starting' }]),
      sleep: async () => {
        clock += 60_000
      },
    })
    const outcome = await runTunnelSetup(
      { ...plan, managed: managed() },
      api,
      () => {},
      undefined,
      () => clock
    )
    expect(outcome.kind).toBe('waiting')
    expect(calls.some((c) => c.startsWith('delete'))).toBe(false)
  })
})

describe('helpers', () => {
  test('tunnel domain and its subdomain scheme', () => {
    expect(tunnelDomainFor('Example.com.', { kind: 'zone' })).toBe(
      'example.com'
    )
    expect(
      tunnelDomainFor('example.com', { kind: 'subdomain', label: ' Lab ' })
    ).toBe('lab.example.com')
    expect(
      tunnelDomainFor('example.com', { kind: 'suffix', label: 'lab' })
    ).toBe('example.com')
    expect(hostSuffixFor({ kind: 'suffix', label: ' Lab ' })).toBe('lab')
    expect(hostSuffixFor({ kind: 'subdomain', label: 'lab' })).toBeUndefined()
    expect(hostSuffixFor({ kind: 'zone' })).toBeUndefined()
    expect(schemeLabelError({ kind: 'suffix', label: 'lab.eu' })).toBeDefined()
    expect(
      schemeLabelError({ kind: 'subdomain', label: 'lab.eu' })
    ).toBeUndefined()
    expect(schemeLabelError({ kind: 'zone' })).toBeUndefined()
    expect(
      exampleHost('shop', 'example.com', { kind: 'suffix', label: 'lab' })
    ).toBe('shop-lab.example.com')
    expect(
      exampleHost('shop', 'example.com', { kind: 'subdomain', label: 'lab' })
    ).toBe('shop.lab.example.com')
    expect(subdomainLabelError('')).toBeDefined()
    expect(subdomainLabelError('lab_1')).toBeDefined()
    expect(subdomainLabelError('lab')).toBeUndefined()
  })

  test('names avoid the ones taken', () => {
    expect(uniqueTunnelName('example.com', [])).toBe('example-com')
    expect(uniqueTunnelName('example.com', ['example-com'])).toBe(
      'example-com-2'
    )
    expect(uniqueProviderName('Cloudflare', ['cloudflare'])).toBe(
      'Cloudflare 2'
    )
  })

  test('workers default on only when they can take a tunnel and have no way in', () => {
    const node = {
      id: 1,
      role: 'worker',
      status: 'active',
      public_ingress_enabled: false,
    }
    expect(workerEligibility(node, 'delegated', []).defaultOn).toBe(true)
    expect(workerEligibility(node, null, []).defaultOn).toBe(true)
    const fixed = workerEligibility(node, 'fixed', [])
    expect(fixed.selectable).toBe(false)
    expect(fixed.command).toBe('temps agent ingress delegated')
    const offline = workerEligibility(
      { ...node, status: 'offline' },
      'delegated',
      []
    )
    expect(offline.selectable).toBe(false)
    expect(offline.reason).toContain('It is offline, so it cannot run a tunnel')
    expect(offline.command).toBe('sudo systemctl start temps-agent')
    // Starting the agent does not bring back a drained worker.
    const drained = workerEligibility(
      { ...node, status: 'drained' },
      'delegated',
      []
    )
    expect(drained.selectable).toBe(false)
    expect(drained.command).toBeUndefined()
    expect(drained.reason).toContain('Bring it back from its page')
    const served = workerEligibility(node, 'delegated', [
      { provider: 'cloudflare_tunnel', nodes: [{ node_id: 1 }] },
    ])
    expect(served).toMatchObject({ selectable: true, defaultOn: false })
  })

  test('the page URL carries the worker and the way in', () => {
    expect(ingressSetupUrl()).toBe('/settings/nodes/ingress')
    expect(ingressSetupUrl({ worker: 4, via: 'ports' })).toBe(
      '/settings/nodes/ingress?via=ports&worker=4'
    )
  })
})

describe('unavailableWorker', () => {
  const nodes = [
    { id: 1, name: 'worker-a', role: 'worker', status: 'active' },
    { id: 2, name: 'worker-b', role: 'worker', status: 'offline' },
    { id: 3, name: 'cp', role: 'control_plane', status: 'active' },
  ]

  test('accepts active workers', () => {
    expect(
      unavailableWorker([{ id: 1, name: 'worker-a' }], nodes)
    ).toBeUndefined()
  })

  test('names the worker that is not active, removed or not a worker', () => {
    expect(
      unavailableWorker(
        [
          { id: 1, name: 'worker-a' },
          { id: 2, name: 'worker-b' },
        ],
        nodes
      )
    ).toContain("Worker 'worker-b' is offline")
    expect(unavailableWorker([{ id: 9, name: 'gone' }], nodes)).toContain(
      "Worker 'gone' is no longer in this cluster"
    )
    expect(unavailableWorker([{ id: 3, name: 'cp' }], nodes)).toContain(
      'not a worker'
    )
    const draining = unavailableWorker(
      [{ id: 4, name: 'worker-d' }],
      [{ id: 4, name: 'worker-d', role: 'worker', status: 'draining' }]
    )
    expect(draining).toContain("Worker 'worker-d' is draining")
    expect(draining).toContain('Bring it back from its page')
    expect(draining).not.toContain('systemctl')
  })
})
