// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  capabilityRemedy,
  describeCapability,
  describeConnection,
  joinCommand,
  strandedPublicNodes,
} from './index.js'
import type {
  NodeCapabilityResponse,
  WireguardMeshNodeStatus,
  WireguardMeshStatusResponse,
} from '../../api/types.gen.js'

function makeCapability(overrides: Partial<NodeCapabilityResponse> = {}): NodeCapabilityResponse {
  return {
    local_workloads: true,
    active_worker_nodes: 0,
    schedulable: true,
    reason: null,
    setup_path: '/settings/nodes',
    can_manage_nodes: true,
    ...overrides,
  }
}

describe('capabilityRemedy', () => {
  test('tells an operator who can manage nodes how to add one', () => {
    const remedy = capabilityRemedy(
      makeCapability({ local_workloads: false, schedulable: false })
    )
    expect(remedy).toContain('temps join')
    expect(remedy).toContain('/settings/nodes')
  })

  test('does not send a credential without node permissions to a page that refuses it', () => {
    const remedy = capabilityRemedy(
      makeCapability({
        local_workloads: false,
        schedulable: false,
        can_manage_nodes: false,
      })
    )
    expect(remedy).toContain('Ask an administrator')
    expect(remedy).not.toContain('temps join')
  })
})

describe('describeCapability', () => {
  test('a single-binary install runs workloads on this host', () => {
    expect(describeCapability(makeCapability())).toBe('this host')
  })

  test('a control plane with workers names the workers only', () => {
    expect(
      describeCapability(
        makeCapability({ local_workloads: false, active_worker_nodes: 2 })
      )
    ).toBe('2 worker nodes')
  })

  test('one worker is not pluralized', () => {
    expect(
      describeCapability(
        makeCapability({ local_workloads: false, active_worker_nodes: 1 })
      )
    ).toBe('1 worker node')
  })

  test('both targets are listed when both exist', () => {
    expect(describeCapability(makeCapability({ active_worker_nodes: 3 }))).toBe(
      'this host + 3 worker nodes'
    )
  })

  test('an unschedulable install says so instead of listing nothing', () => {
    // A blank value here would read as "loading"; the operator needs to see
    // that nothing can run, not an empty cell.
    expect(
      describeCapability(
        makeCapability({ local_workloads: false, schedulable: false })
      )
    ).toBe('nowhere — no schedulable target')
  })
})

function makeMeshNode(overrides: Partial<WireguardMeshNodeStatus> = {}): WireguardMeshNodeStatus {
  return {
    node_id: 1,
    name: 'worker-1',
    node_status: 'active',
    registered_address: '203.0.113.10',
    registered_on_private_network: false,
    mesh_address: null,
    endpoint: null,
    data_address: '203.0.113.10',
    connection: 'mesh_off',
    last_handshake_at: null,
    rx_bytes: null,
    tx_bytes: null,
    ...overrides,
  }
}

function makeMesh(overrides: Partial<WireguardMeshStatusResponse> = {}): WireguardMeshStatusResponse {
  return {
    state: 'disabled',
    reason: null,
    cidr: null,
    listen_port: 51820,
    control_plane: null,
    can_enable: true,
    enable_blocker: null,
    enable_command: 'temps network setup-multi-node --wireguard',
    join_url: 'https://temps.example.com',
    handshake_error: null,
    nodes: [],
    ...overrides,
  }
}

describe('joinCommand', () => {
  test('points a node at the configured external URL', () => {
    expect(joinCommand(makeMesh(), true)).toBe(
      'temps join https://temps.example.com <join-token> --private-address <worker-public-ip>'
    )
  })

  test('uses a placeholder rather than a wrong URL when none is configured', () => {
    expect(joinCommand(makeMesh({ join_url: null }), false)).toContain(
      '<control-plane-url> <join-token> --private-address <worker-private-ip>'
    )
  })
})

describe('strandedPublicNodes', () => {
  const nodes = [
    makeMeshNode({ name: 'public' }),
    makeMeshNode({ name: 'private', registered_on_private_network: true }),
  ]

  test('flags publicly-joined nodes while the mesh is not carrying traffic', () => {
    expect(strandedPublicNodes(makeMesh({ nodes })).map((node) => node.name)).toEqual(['public'])
    expect(
      strandedPublicNodes(makeMesh({ state: 'starting', nodes })).map((node) => node.name)
    ).toEqual(['public'])
  })

  test('flags nothing once the mesh is ready', () => {
    expect(strandedPublicNodes(makeMesh({ state: 'ready', nodes }))).toEqual([])
  })
})

describe('describeConnection', () => {
  test('points at the usual cause of a node that never connected', () => {
    expect(describeConnection('never_connected')).toContain('UDP port')
  })
})
