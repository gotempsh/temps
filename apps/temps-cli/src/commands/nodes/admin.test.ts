// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildLimitsRequest, ingressSettingsRequest } from './admin.js'
import { registerNodesCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('node admin helpers', () => {
  const limits = { build_timeout_seconds: 1800, concurrency: 1, queue_wait_seconds: 300 }

  test('buildLimitsRequest keeps unset limits and validates given ones', () => {
    expect(buildLimitsRequest(limits, {})).toBeNull()
    expect(buildLimitsRequest(limits, { concurrency: '4' })).toEqual({ ...limits, concurrency: 4 })
    expect(() => buildLimitsRequest(limits, { timeout: '0' })).toThrow(/--timeout/)
  })

  test('ingressSettingsRequest merges flags; an empty address means automatic', () => {
    const current = { enabled: false, http_port: 80, https_port: 443, listen_address: '0.0.0.0', public_address: null }
    expect(ingressSettingsRequest(current, {})).toBeNull()
    expect(ingressSettingsRequest(current, { enable: true, httpsPort: '8443', listenAddress: '' })).toEqual({
      enabled: true,
      http_port: 80,
      https_port: 8443,
      listen_address: null,
      public_address: null,
    })
    expect(() => ingressSettingsRequest(current, { enable: true, disable: true })).toThrow(/not both/)
    expect(() => ingressSettingsRequest(current, { httpPort: '70000' })).toThrow(/--http-port/)
  })
})

describe('temps nodes (admin)', () => {
  const routes = {
    'GET /internal/nodes/3': { id: 3, name: 'worker-a' },
    'GET /internal/nodes/3/containers': { containers: [] },
    'GET /internal/nodes/3/drain': { complete: false },
    'POST /internal/nodes/3/drain': { status: 'draining' },
    'DELETE /internal/nodes/3/drain': { status: 'active' },
    'DELETE /internal/nodes/3': { removed: true },
    'GET /internal/nodes/3/build-limits': { build_timeout_seconds: 1800, concurrency: 1, queue_wait_seconds: 300 },
    'PUT /internal/nodes/3/build-limits': { build_timeout_seconds: 1800, concurrency: 2, queue_wait_seconds: 300 },
    'GET /internal/nodes/3/public-ingress/settings': { enabled: true, http_port: 80, https_port: 443 },
    'PUT /internal/nodes/3/public-ingress/settings': { enabled: true },
    'PATCH /internal/nodes/3/public-ingress': { enabled: false },
    'POST /internal/nodes/3/public-ingress/check': { reachable: true },
    'GET /internal/environments/9/workload-mode': { workload_mode: 'legacy' },
    'PUT /internal/environments/9/workload-mode': { workload_mode: 'shadow' },
  }

  test.each([
    [['show', '3'], ['GET /internal/nodes/3']],
    [['containers', '3'], ['GET /internal/nodes/3/containers']],
    [['drain', '3', '--status'], ['GET /internal/nodes/3/drain']],
    [['drain', '3', '--yes'], ['POST /internal/nodes/3/drain']],
    [['undrain', '3'], ['DELETE /internal/nodes/3/drain']],
    [['remove', '3', '--force', '--yes'], ['DELETE /internal/nodes/3?force=true']],
    [['build-limits', '3'], ['GET /internal/nodes/3/build-limits']],
    [['build-limits', '3', '--concurrency', '2'], ['GET /internal/nodes/3/build-limits', 'PUT /internal/nodes/3/build-limits']],
    [['public-ingress', '3'], ['GET /internal/nodes/3/public-ingress/settings']],
    [['public-ingress', '3', '--https-port', '8443'], ['GET /internal/nodes/3/public-ingress/settings', 'PUT /internal/nodes/3/public-ingress/settings']],
    [['public-ingress', '3', '--disable', '--toggle-only', '--yes'], ['PATCH /internal/nodes/3/public-ingress']],
    [['public-ingress', '3', '--check'], ['POST /internal/nodes/3/public-ingress/check']],
    [['workload-mode', '9'], ['GET /internal/environments/9/workload-mode']],
    [['workload-mode', '9', 'shadow', '--yes'], ['PUT /internal/environments/9/workload-mode']],
  ])('nodes %p', async (argv, requests) => {
    const run = await runWithApi(registerNodesCommands, ['nodes', ...argv, '--json'], routes)
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(requests)
  })

  test('drain and remove refuse without a terminal', async () => {
    for (const argv of [['drain', '3'], ['remove', '3']]) {
      const run = await runWithApi(registerNodesCommands, ['nodes', ...argv], routes, { stdin: '' })
      expect(run.error?.message).toMatch(/not an interactive terminal/)
      expect(run.requests).toEqual([])
    }
  })

  test('an unknown workload mode or node id fails before calling the API', async () => {
    const mode = await runWithApi(registerNodesCommands, ['nodes', 'workload-mode', '9', 'strict'], routes)
    expect(mode.error?.message).toMatch(/Unknown workload mode/)
    const node = await runWithApi(registerNodesCommands, ['nodes', 'show', 'worker-a'], routes)
    expect(node.error?.message).toMatch(/<node-id> must be/)
    expect([...mode.requests, ...node.requests]).toEqual([])
  })

  test('public-ingress refuses --enable with --disable on every branch, before calling the API', async () => {
    for (const extra of [[], ['--toggle-only'], ['--check']]) {
      const run = await runWithApi(registerNodesCommands, ['nodes', 'public-ingress', '3', '--enable', '--disable', ...extra, '--yes'], routes)
      expect(run.error?.message).toMatch(/not both/)
      expect(run.requests).toEqual([])
    }
  })

  test('ssh list passes paging', async () => {
    const run = await runWithApi(registerNodesCommands, ['nodes', 'ssh', '--page', '2', '--per-page', '50', '--json'], {
      'GET /nodes/ssh/enrollments': { enrollments: [] },
    })
    expect(run.requests).toEqual(['GET /nodes/ssh/enrollments?page=2&per_page=50'])
  })
})
