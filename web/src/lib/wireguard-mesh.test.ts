// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  NodePairingResponse,
  NodeSshEnrollmentResponse,
  WireguardMeshLink,
  WireguardMeshNodeStatus,
  WireguardMeshStatusResponse,
} from '@/api/client/types.gen'
import {
  defaultInternetMethod,
  defaultJoinPath,
  enrollmentProgress,
  hubCandidates,
  hubOptionValue,
  hubTargetFromOption,
  linksNeedingAttention,
  joinUrlReachableFromOutside,
  meshProblems,
  pairingProgress,
  pendingPairings,
  joinCommand,
  joinUrl,
  meshConnectionLabel,
  meshLinkLabel,
  pairingAsOf,
  pairingEnded,
  SSH_ENROLLMENT_STEPS,
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
    checks: [],
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
    hub: null,
    links: [],
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

describe('join URL reachability', () => {
  test('addresses that resolve to the worker itself or a private network do not count', () => {
    for (const url of [
      'http://localhost:3000',
      'https://app.localho.st',
      'http://127.0.0.1:8080',
      'http://10.0.0.5',
      'http://192.168.1.10',
      'http://172.20.0.2',
      'http://100.99.0.10',
      'http://[::1]:8080',
      'not a url',
    ]) {
      expect(joinUrlReachableFromOutside(url)).toBe(false)
    }
  })

  test('public names and addresses count', () => {
    expect(joinUrlReachableFromOutside('https://temps.example.com')).toBe(true)
    expect(joinUrlReachableFromOutside('https://203.0.113.10')).toBe(true)
  })
})

describe('default internet join method', () => {
  test('pairs when workers could not reach the join URL', () => {
    expect(defaultInternetMethod(mesh(), 'https://app.localho.st')).toBe('pair')
  })

  test('pairs when the control plane has no endpoint to dial', () => {
    expect(
      defaultInternetMethod(
        mesh({
          control_plane: {
            address: '10.201.0.1',
            endpoint: null,
            endpoint_is_private: false,
          },
        }),
        'https://temps.example.com'
      )
    ).toBe('pair')
  })

  test('uses the join URL for a reachable control plane', () => {
    expect(defaultInternetMethod(mesh(), 'https://temps.example.com')).toBe(
      'url'
    )
  })
})

function pairing(
  overrides: Partial<NodePairingResponse> = {}
): NodePairingResponse {
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

describe('pairing progress', () => {
  test('surfaces why the node has not been reached', () => {
    const progress = pairingProgress(
      pairing({ last_error: 'No answer from 198.51.100.7:51820 yet.' })
    )
    expect(progress.tone).toBe('warn')
    expect(progress.hint).toContain('No answer')
  })

  test('keeps a refusal visible over later "no answer" attempts', () => {
    const progress = pairingProgress(
      pairing({
        last_error: 'No answer from 198.51.100.7:51820 yet.',
        last_rejection: 'Its WireGuard key already belongs to another node.',
      })
    )
    expect(progress.tone).toBe('error')
    expect(progress.label).toBe('Refused')
    expect(progress.hint).toContain('another node')
  })

  test('only waiting and registering pairings are pending', () => {
    const all = ['waiting', 'key_received', 'completed', 'expired'].map(
      (status, id) => pairing({ id, status })
    )
    expect(pendingPairings(all).map((p) => p.status)).toEqual([
      'waiting',
      'key_received',
    ])
  })
})

describe('mesh problems', () => {
  test('keeps checks that need action, failures first', () => {
    const problems = meshProblems([
      { label: 'Agent', status: 'pass', detail: 'reporting', fix: null },
      { label: 'Handshake', status: 'warn', detail: 'stale', fix: 'open UDP' },
      {
        label: 'Mesh key',
        status: 'fail',
        detail: 'missing',
        fix: 'run agent',
      },
      { label: 'Other', status: 'info', detail: 'fyi', fix: null },
    ])
    expect(problems.map((check) => check.label)).toEqual([
      'Mesh key',
      'Handshake',
    ])
    expect(meshProblems(undefined)).toEqual([])
  })
})

describe('enrollmentProgress', () => {
  const enrollment = (
    overrides: Partial<NodeSshEnrollmentResponse>
  ): NodeSshEnrollmentResponse => ({
    id: 1,
    name: 'worker-1',
    host: 'node.example.com',
    ssh_address: '198.51.100.7:22',
    ssh_user: 'root',
    auth_method: 'password',
    host_key_fingerprint: `SHA256:${'A'.repeat(43)}`,
    pairing_id: 2,
    status: 'running',
    step: 'pairing',
    log: '',
    error: null,
    agent_mode: null,
    node_id: null,
    created_at: '2026-09-29T10:00:00Z',
    finished_at: null,
    ...overrides,
  })

  test('shows the step while running', () => {
    expect(enrollmentProgress(enrollment({}))).toEqual({
      label: 'pairing',
      tone: 'muted',
    })
  })

  test('flags an agent that will not survive a reboot', () => {
    expect(
      enrollmentProgress(
        enrollment({ status: 'succeeded', agent_mode: 'detached' })
      ).tone
    ).toBe('warn')
    expect(
      enrollmentProgress(
        enrollment({ status: 'succeeded', agent_mode: 'service' })
      ).tone
    ).toBe('ok')
    expect(enrollmentProgress(enrollment({ status: 'failed' })).tone).toBe(
      'error'
    )
  })
})

function meshLink(
  overrides: Partial<WireguardMeshLink> = {}
): WireguardMeshLink {
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

describe('mesh hub', () => {
  test('a hub choice round-trips through the select value', () => {
    for (const target of [
      { kind: 'none' as const },
      { kind: 'control_plane' as const },
      { kind: 'node' as const, node_id: 4 },
    ]) {
      expect(hubTargetFromOption(hubOptionValue(target))).toEqual(target)
    }
    expect(hubTargetFromOption('node:abc')).toEqual({ kind: 'none' })
  })

  test('only members on the mesh can be the hub', () => {
    const status = mesh({
      nodes: [
        meshNode({ node_id: 4, name: 'worker-3', mesh_address: '10.201.0.4' }),
        meshNode({ node_id: 9, name: 'worker-9', mesh_address: null }),
      ],
    })
    expect(hubCandidates(status).map((option) => option.value)).toEqual([
      'control-plane',
      'node:4',
    ])
  })

  test('links needing attention leave out direct pairs, broken ones first', () => {
    const links = [
      meshLink({ b: 'worker-2', state: 'via_hub' }),
      meshLink(),
      meshLink({ b: 'worker-5', state: 'unreachable' }),
    ]
    expect(linksNeedingAttention(links).map((link) => link.b)).toEqual([
      'worker-5',
      'worker-2',
    ])
  })
})

describe('mesh link label', () => {
  test('names every link state, with only broken pairs as errors', () => {
    expect(meshLinkLabel('direct')).toEqual({ label: 'Direct', tone: 'ok' })
    expect(meshLinkLabel('via_hub')).toEqual({
      label: 'Through the hub',
      tone: 'ok',
    })
    expect(meshLinkLabel('connecting')).toEqual({
      label: 'Connecting',
      tone: 'muted',
    })
    expect(meshLinkLabel('unreachable')).toEqual({
      label: 'Cannot connect',
      tone: 'error',
    })
  })
})

describe('pairing expiry', () => {
  const expiresAt = Date.parse('2026-09-29T12:30:00Z')

  test('a waiting pairing past its expiry shows as expired', () => {
    const waiting = pairing({ status: 'waiting' })
    expect(pairingAsOf(waiting, expiresAt - 1).status).toBe('waiting')
    const expired = pairingAsOf(waiting, expiresAt)
    expect(expired.status).toBe('expired')
    expect(pairingEnded(expired)).toBe(true)
    expect(pairingProgress(expired).label).toBe('Expired')
  })

  test('a pairing the node answered is not expired by the clock', () => {
    for (const status of ['key_received', 'completed']) {
      expect(pairingAsOf(pairing({ status }), expiresAt + 60_000).status).toBe(
        status
      )
    }
  })

  test('only expired and cancelled pairings have ended', () => {
    expect(
      ['waiting', 'key_received', 'completed', 'expired', 'cancelled'].filter(
        (status) => pairingEnded(pairing({ status }))
      )
    ).toEqual(['expired', 'cancelled'])
  })
})

describe('SSH enrollment steps', () => {
  test('match the steps the server reports, in order', () => {
    // The `progress.step(...)` calls in
    // crates/temps-deployments/src/services/node_ssh.rs (`enroll`) and
    // the heartbeat wait in crates/temps-deployments/src/services/node_ssh_enrollment.rs.
    // Update both sides together: an unknown step shows no progress.
    expect(SSH_ENROLLMENT_STEPS).toEqual([
      'connecting',
      'authenticating',
      'checking the server',
      'installing temps',
      'pairing',
      'starting the agent',
      'waiting for the first heartbeat',
    ])
  })
})
