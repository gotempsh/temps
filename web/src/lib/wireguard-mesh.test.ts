// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  WireguardMeshNodeStatus,
  WireguardMeshStatusResponse,
} from '@/api/client/types.gen'
import {
  defaultJoinPath,
  joinCommand,
  joinUrl,
  meshConnectionLabel,
  strandedPublicNodes,
} from './wireguard-mesh'

function meshNode(
  overrides: Partial<WireguardMeshNodeStatus> = {}
): WireguardMeshNodeStatus {
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

function mesh(
  overrides: Partial<WireguardMeshStatusResponse> = {}
): WireguardMeshStatusResponse {
  return {
    state: 'disabled',
    reason: null,
    cidr: null,
    listen_port: 51820,
    control_plane: null,
    can_enable: true,
    enable_blocker: null,
    enable_command: 'temps network setup-multi-node --wireguard',
    join_url: null,
    handshake_error: null,
    nodes: [],
    ...overrides,
  }
}

describe('join URL', () => {
  test('prefers the configured external URL over the browser origin', () => {
    expect(
      joinUrl(
        mesh({ join_url: 'https://temps.example.com' }),
        'http://localhost:3000'
      )
    ).toEqual({ url: 'https://temps.example.com', configured: true })
  })

  test('falls back to the browser origin and says so', () => {
    expect(joinUrl(mesh(), 'http://localhost:3000')).toEqual({
      url: 'http://localhost:3000',
      configured: false,
    })
    expect(joinUrl(undefined, 'http://localhost:3000').configured).toBe(false)
  })
})

describe('join command', () => {
  test('asks for the address that matches the join path', () => {
    expect(joinCommand('https://t.example.com', 'tok', 'internet')).toBe(
      'temps join https://t.example.com tok --private-address <worker-public-ip>'
    )
    expect(joinCommand('https://t.example.com', null, 'private')).toBe(
      'temps join https://t.example.com <join-token> --private-address <worker-private-ip>'
    )
  })
})

describe('stranded public nodes', () => {
  const nodes = [
    meshNode({ name: 'public' }),
    meshNode({ name: 'private', registered_on_private_network: true }),
  ]

  test('are publicly-joined nodes while the mesh is off or starting', () => {
    expect(strandedPublicNodes(mesh({ nodes })).map((n) => n.name)).toEqual([
      'public',
    ])
    expect(
      strandedPublicNodes(mesh({ state: 'starting', nodes })).map((n) => n.name)
    ).toEqual(['public'])
  })

  test('are none once the mesh is ready', () => {
    expect(strandedPublicNodes(mesh({ state: 'ready', nodes }))).toEqual([])
  })
})

describe('mesh connection label', () => {
  test('a node that never connected points at the firewall', () => {
    const label = meshConnectionLabel('never_connected')
    expect(label.tone).toBe('error')
    expect(label.hint).toContain('UDP port')
  })
})

describe('default join path', () => {
  test('opens the internet path once the mesh is on', () => {
    expect(defaultJoinPath(mesh())).toBe('private')
    expect(defaultJoinPath(mesh({ state: 'starting' }))).toBe('internet')
    expect(defaultJoinPath(undefined)).toBe('private')
  })
})
