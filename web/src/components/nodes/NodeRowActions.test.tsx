// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { NodeRowActions } from './NodeRowActions'

const NOW = Date.parse('2026-10-23T12:00:00Z')

function node(overrides: Partial<NodeInfoResponse> = {}): NodeInfoResponse {
  return {
    id: 4,
    name: 'edge-4',
    address: 'https://10.201.0.4:3100',
    private_address: '10.201.0.4',
    role: 'worker',
    status: 'active',
    labels: {},
    capacity: {},
    build_limits: {} as NodeInfoResponse['build_limits'],
    last_heartbeat: new Date(NOW - 10_000).toISOString(),
    created_at: new Date(NOW - 86_400_000).toISOString(),
    public_ingress_enabled: false,
    public_ingress_unsupported_reasons: [],
    serving_containers: 0,
    live_sandboxes: 0,
    ...overrides,
  }
}

function render(info: NodeInfoResponse) {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <NodeRowActions node={info} now={NOW} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an empty reachable node offers Remove in one step', () => {
  const html = render(node())
  expect(html).toContain('aria-label="Remove node edge-4"')
  expect(html).not.toContain('Drain first')
})

test('a node with workloads says to drain it first, with the count', () => {
  const html = render(node({ serving_containers: 2 }))
  expect(html).toContain('Drain first — it has 2 containers')
  expect(html).not.toContain('aria-label="Remove node edge-4"')
})

test('an unreachable node keeps the remove (and force) flow', () => {
  const html = render(
    node({
      serving_containers: 3,
      last_heartbeat: new Date(NOW - 3_600_000).toISOString(),
    })
  )
  expect(html).toContain('aria-label="Remove node edge-4"')
})

test('a server without the counts offers no row action', () => {
  const html = render(
    node({ serving_containers: undefined, live_sandboxes: undefined })
  )
  expect(html).not.toContain('Remove node')
  expect(html).not.toContain('Drain first')
})
