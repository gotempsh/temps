// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  capabilityRemedy,
  describeCapability,
  describeConnection,
  describePairing,
  hubTargetFor,
  joinCommand,
  linksNeedingAttention,
  meshDoctorFindings,
  pendingPairings,
  strandedPublicNodes,
} from './index.js'
import type {
  NodeCapabilityResponse,
  NodePairingResponse,
  WireguardMeshLink,
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
    checks: [],
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
    hub: null,
    links: [],
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

function makePairing(overrides: Partial<NodePairingResponse> = {}): NodePairingResponse {
  return {
    id: 1,
    name: 'worker-1',
    node_endpoint: '198.51.100.7:51820',
    mesh_address: '10.201.0.5',
    status: 'waiting',
    last_error: null,
    last_rejection: null,
    last_attempt_at: null,
    expires_at: '2026-09-29T12:30:00Z',
    node_id: null,
    created_at: '2026-09-29T12:00:00Z',
    ...overrides,
  }
}

describe('describePairing', () => {
  test('shows why the control plane has not reached the node yet', () => {
    expect(describePairing(makePairing())).toContain('run the pairing command')
    expect(
      describePairing(makePairing({ last_error: 'No answer from 198.51.100.7:51820 yet.' }))
    ).toContain('No answer')
  })

  test('keeps a refusal visible over later "no answer" attempts', () => {
    expect(
      describePairing(
        makePairing({
          last_error: 'No answer from 198.51.100.7:51820 yet.',
          last_rejection: 'its WireGuard key already belongs to another node',
        })
      )
    ).toBe('refused: its WireGuard key already belongs to another node')
  })

  test('names the node a finished pairing enrolled', () => {
    expect(describePairing(makePairing({ status: 'completed', node_id: 7 }))).toBe(
      'joined as node 7'
    )
  })
})

describe('pendingPairings', () => {
  test('keeps only pairings still in progress', () => {
    const pairings = ['waiting', 'key_received', 'completed', 'expired', 'cancelled'].map(
      (status, id) => makePairing({ id, status })
    )
    expect(pendingPairings(pairings).map((pairing) => pairing.status)).toEqual([
      'waiting',
      'key_received',
    ])
  })
})

describe('meshDoctorFindings', () => {
  test('a mesh that is off says how to turn it on', () => {
    const findings = meshDoctorFindings(makeMesh(), [])
    expect(findings).toHaveLength(1)
    expect(findings[0]?.status).toBe('info')
    expect(findings[0]?.fix).toContain('nodes mesh enable')
  })

  test('lists node checks, a private control-plane endpoint and refused pairings', () => {
    const mesh = makeMesh({
      state: 'ready',
      control_plane: { address: '10.201.0.1', endpoint: '10.0.0.5:51820', endpoint_is_private: true },
      nodes: [
        makeMeshNode({
          connection: 'never_connected',
          checks: [
            { label: 'Agent', status: 'pass', detail: 'reporting', fix: null },
            {
              label: 'Handshake',
              status: 'fail',
              detail: 'never handshook with this server',
              fix: 'open UDP 51820 inbound on worker-1',
            },
          ],
        }),
      ],
    })
    const pairings = [
      makePairing({ name: 'worker-9', last_rejection: 'its key belongs to another node' }),
      makePairing({ id: 2, name: 'worker-8', status: 'completed' }),
    ]
    const findings = meshDoctorFindings(mesh, pairings)
    expect(findings.map((finding) => `${finding.scope}/${finding.label}/${finding.status}`)).toEqual([
      'cluster/Control plane/pass',
      'cluster/Control plane endpoint/warn',
      'worker-1/Agent/pass',
      'worker-1/Handshake/fail',
      'pairing worker-9/Pairing/fail',
    ])
    expect(findings[1]?.fix).toContain('nodes pair create')
  })

  test('a control plane that has not brought its end up fails', () => {
    const findings = meshDoctorFindings(makeMesh({ state: 'starting', reason: 'waiting' }), [])
    expect(findings[0]).toMatchObject({ label: 'Mesh', status: 'fail', detail: 'waiting' })
  })
})

function makeLink(overrides: Partial<WireguardMeshLink> = {}): WireguardMeshLink {
  return {
    a: 'worker-1',
    b: 'worker-4',
    a_node_id: 3,
    b_node_id: 5,
    state: 'direct',
    last_handshake_at: null,
    detail: null,
    ...overrides,
  }
}

describe('hubTargetFor', () => {
  const mesh = makeMesh({
    nodes: [makeMeshNode({ node_id: 4, name: 'worker-3' })],
  })

  test('control-plane names the control plane', () => {
    expect(hubTargetFor('control-plane', mesh)).toEqual({ kind: 'control_plane' })
  })

  test('a node is found by name or id', () => {
    expect(hubTargetFor('worker-3', mesh)).toEqual({ kind: 'node', node_id: 4 })
    expect(hubTargetFor('4', mesh)).toEqual({ kind: 'node', node_id: 4 })
  })

  test('an unknown member lists the ones to choose from', () => {
    expect(() => hubTargetFor('worker-9', mesh)).toThrow('control-plane, worker-3')
  })
})

describe('mesh links', () => {
  test('only pairs that are not simply direct need attention', () => {
    const links = [
      makeLink(),
      makeLink({ b: 'worker-5', state: 'via_hub' }),
      makeLink({ b: 'worker-2', state: 'unreachable' }),
    ]
    expect(linksNeedingAttention(links).map((link) => link.b)).toEqual(['worker-5', 'worker-2'])
  })

  test('the doctor fails a mesh with unreachable pairs and no hub, and says how to fix it', () => {
    const findings = meshDoctorFindings(
      makeMesh({ state: 'ready', links: [makeLink({ state: 'unreachable' })] }),
      []
    )
    const hub = findings.find((finding) => finding.label === 'Hub')
    expect(hub?.status).toBe('fail')
    expect(hub?.fix).toContain('nodes mesh hub set')
  })

  test('the doctor names the hub once one is set', () => {
    const findings = meshDoctorFindings(
      makeMesh({
        state: 'ready',
        hub: { target: { kind: 'control_plane' }, name: 'control-plane' },
        links: [makeLink({ state: 'via_hub' })],
      }),
      []
    )
    expect(findings.find((finding) => finding.label === 'Hub')?.status).toBe('info')
  })
})
