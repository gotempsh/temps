// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ClusterNetworkStatus } from '@/lib/cluster-network'
import { ClusterNetworkIdSection } from './NetworkIdAction'

const status = (
  overrides: Partial<ClusterNetworkStatus> = {}
): ClusterNetworkStatus => ({
  can_change: true,
  can_change_network_id: true,
  change_blocked_by: [],
  compute_pool_cidr: '10.202.0.0/16',
  control_plane: { overlay_ready: true },
  default_compute_pool_cidr: '10.202.0.0/16',
  mesh_enabled: false,
  mesh_pool_cidr: '10.201.0.0/16',
  network_id_change_blocked_by: [],
  nodes: [],
  overlay_network_name: 'temps0',
  pool_confirmed: true,
  subnet_prefix_len: 24,
  vxlan_network_id: 42,
  vxlan_port: 4789,
  ...overrides,
})

function render(value: ClusterNetworkStatus) {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <ClusterNetworkIdSection status={value} />
    </QueryClientProvider>
  )
}

// ADR 062: the network ID is shown even when nothing is wrong, so an
// operator can find the change before an overlay collides.
test('the cluster network ID and port are always shown, with the change', () => {
  const html = render(status())
  expect(html).toContain('Network ID 42')
  expect(html).toContain('UDP port 4789')
  expect(html).toContain('Give this cluster its own network ID')
  expect(html).toContain('pauses for up to 30 seconds')
})

test('a node that cannot follow is named in place of the button', () => {
  const html = render(
    status({
      can_change_network_id: false,
      network_id_change_blocked_by: [
        "node 'worker-1' has not reported a network ID; run sudo temps upgrade on it",
      ],
    })
  )
  expect(html).toContain('cannot get its own network ID yet')
  expect(html).toContain('worker-1')
  expect(html).not.toContain('Give this cluster its own network ID</button>')
})

test('a control plane without a network ID shows nothing', () => {
  expect(render(status({ vxlan_network_id: 0 }))).toBe('')
})
