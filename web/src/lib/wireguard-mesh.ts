// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  NodePairingResponse,
  NodeSshEnrollmentSummary,
  WireguardMeshCheck,
  WireguardMeshHubTarget,
  WireguardMeshLink,
  WireguardMeshLinkState,
  WireguardMeshNodeConnection,
  WireguardMeshNodeStatus,
  WireguardMeshStatusResponse,
} from '@/api/client/types.gen'

/** How a worker reaches this control plane when it joins. */
export type JoinPath = 'private' | 'internet'

/**
 * The URL `temps join` should point at: the configured external URL, which
 * is what a worker on another network can reach. The browser's own origin is
 * only a fallback — it is often `localhost` or an internal hostname, so the
 * caller is told when it is used.
 */
export function joinUrl(
  mesh: Pick<WireguardMeshStatusResponse, 'join_url'> | undefined,
  browserOrigin: string
): { url: string; configured: boolean } {
  const configured = mesh?.join_url?.trim()
  if (configured) return { url: configured, configured: true }
  return { url: browserOrigin, configured: false }
}

export function joinCommand(
  url: string,
  token: string | null,
  path: JoinPath
): string {
  const address =
    path === 'internet' ? '<worker-public-ip>' : '<worker-private-ip>'
  return `temps join ${url} ${token ?? '<join-token>'} --private-address ${address}`
}

/** Label and tone for a node's mesh connection. */
export function meshConnectionLabel(connection: WireguardMeshNodeConnection): {
  label: string
  tone: 'ok' | 'warn' | 'error' | 'muted'
  hint?: string
} {
  switch (connection) {
    case 'connected':
      return { label: 'Connected', tone: 'ok' }
    case 'stale':
      return {
        label: 'Stale',
        tone: 'warn',
        hint: 'No handshake with the control plane in the last 3 minutes.',
      }
    case 'never_connected':
      return {
        label: 'Not connected',
        tone: 'error',
        hint: 'Registered but never reached the control plane — check that the mesh UDP port is open on both ends.',
      }
    case 'not_registered':
      return {
        label: 'Waiting for agent',
        tone: 'muted',
        hint: 'The node joins the mesh once its agent is running (temps agent).',
      }
    case 'waiting_for_control_plane':
      return {
        label: 'Waiting for control plane',
        tone: 'muted',
        hint: 'The node moves onto the mesh once the control plane brings its end up.',
      }
    case 'unknown':
      return { label: 'Unknown', tone: 'muted' }
    case 'mesh_off':
      return { label: 'Mesh off', tone: 'muted' }
  }
}

/**
 * Nodes that joined with a public address while the mesh is not carrying
 * traffic: they have no private path to the control plane or other nodes,
 * so cross-node networking does not work for them.
 */
export function strandedPublicNodes(
  mesh: WireguardMeshStatusResponse | undefined
): WireguardMeshNodeStatus[] {
  if (!mesh || mesh.state === 'ready') return []
  return mesh.nodes.filter((node) => !node.registered_on_private_network)
}

/** Which join path to open first: the internet one once the mesh is in use. */
export function defaultJoinPath(
  mesh: WireguardMeshStatusResponse | undefined
): JoinPath {
  return mesh && mesh.state !== 'disabled' ? 'internet' : 'private'
}

/** How a worker over the internet gets onto the mesh. */
export type InternetJoinMethod = 'url' | 'pair' | 'ssh'

/**
 * Whether a machine elsewhere on the internet could reach `url`: not
 * loopback, not a name that resolves to the machine itself
 * (`*.localho.st`, `localhost`), not a private or link-local address.
 */
export function joinUrlReachableFromOutside(url: string): boolean {
  let host: string
  try {
    host = new URL(url).hostname.toLowerCase().replace(/^\[|\]$/g, '')
  } catch {
    return false
  }
  if (
    host === 'localhost' ||
    host.endsWith('.localhost') ||
    host === 'localho.st' ||
    host.endsWith('.localho.st') ||
    host.endsWith('.local') ||
    host.endsWith('.internal')
  ) {
    return false
  }
  const v4 = host.match(/^(\d+)\.(\d+)\.(\d+)\.(\d+)$/)
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])]
    return !(
      a === 10 ||
      a === 127 ||
      a === 0 ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 169 && b === 254) ||
      (a === 100 && b >= 64 && b <= 127)
    )
  }
  if (host.includes(':')) {
    return !(
      host === '::1' ||
      host.startsWith('fc') ||
      host.startsWith('fd') ||
      host.startsWith('fe80')
    )
  }
  return true
}

/**
 * Which internet join to show first: pairing (this server dials the worker)
 * when workers could not reach the join URL, or when the control plane has
 * no endpoint nodes can dial.
 */
export function defaultInternetMethod(
  mesh: Pick<WireguardMeshStatusResponse, 'control_plane'> | undefined,
  url: string
): InternetJoinMethod {
  if (!joinUrlReachableFromOutside(url)) return 'pair'
  if (mesh?.control_plane && !mesh.control_plane.endpoint) return 'pair'
  return 'url'
}

/** A pairing's progress, for the pending-pairings list. */
export function pairingProgress(pairing: NodePairingResponse): {
  label: string
  tone: 'ok' | 'warn' | 'error' | 'muted'
  hint?: string
} {
  switch (pairing.status) {
    case 'waiting':
      if (pairing.last_rejection)
        return { label: 'Refused', tone: 'error', hint: pairing.last_rejection }
      return pairing.last_error
        ? { label: 'Not reached yet', tone: 'warn', hint: pairing.last_error }
        : {
            label: 'Waiting for the node',
            tone: 'muted',
            hint: 'Run the pairing command on the node.',
          }
    case 'key_received':
      return {
        label: 'Registering',
        tone: 'ok',
        hint: 'The node answered and is registering over the mesh.',
      }
    case 'completed':
      return { label: 'Joined', tone: 'ok' }
    case 'expired':
      return {
        label: 'Expired',
        tone: 'error',
        hint: 'The node never answered. Create a new pairing.',
      }
    case 'cancelled':
      return { label: 'Cancelled', tone: 'muted' }
    default:
      return { label: pairing.status, tone: 'muted' }
  }
}

/**
 * The steps of adding a server over SSH, in order, as the server names them
 * (`crates/temps-deployments/src/services/node_ssh.rs` and, for the last one,
 * `node_ssh_enrollment.rs`). Before the first step the server reports
 * `queued`, and `done` once it succeeded.
 */
export const SSH_ENROLLMENT_STEPS = [
  'connecting',
  'authenticating',
  'checking the server',
  'installing temps',
  'pairing',
  'starting the agent',
  'waiting for the first heartbeat',
]

/** An "add server over SSH" in a word, for the recent list. */
/** Accepts a list summary or a full enrollment, which carries the same fields. */
export function enrollmentProgress(enrollment: NodeSshEnrollmentSummary): {
  label: string
  tone: 'ok' | 'warn' | 'error' | 'muted'
} {
  switch (enrollment.status) {
    case 'running':
      return { label: enrollment.step, tone: 'muted' }
    case 'succeeded':
      return enrollment.agent_mode === 'detached'
        ? { label: 'Added, no service', tone: 'warn' }
        : { label: 'Added', tone: 'ok' }
    case 'failed':
      return { label: 'Failed', tone: 'error' }
    default:
      return { label: enrollment.status, tone: 'muted' }
  }
}

/** A node's mesh checks that need action, failures first. */
export function meshProblems(
  checks: WireguardMeshCheck[] | undefined
): WireguardMeshCheck[] {
  const rank = { fail: 0, warn: 1 } as const
  return (checks ?? [])
    .filter(
      (check): check is WireguardMeshCheck & { status: 'fail' | 'warn' } =>
        check.status === 'fail' || check.status === 'warn'
    )
    .sort((a, b) => rank[a.status] - rank[b.status])
}

/**
 * A pairing as of `now`: one still waiting past its `expires_at` is shown as
 * expired, since its command no longer works even before the server marks
 * it so.
 */
export function pairingAsOf(
  pairing: NodePairingResponse,
  now: number
): NodePairingResponse {
  const expiresAt = Date.parse(pairing.expires_at)
  if (pairing.status === 'waiting' && !Number.isNaN(expiresAt)) {
    if (expiresAt <= now) return { ...pairing, status: 'expired' }
  }
  return pairing
}

/** Whether a pairing's command can no longer be used: expired or cancelled. */
export function pairingEnded(pairing: NodePairingResponse): boolean {
  return pairing.status === 'expired' || pairing.status === 'cancelled'
}

/** Pairings still in progress. */
export function pendingPairings(
  pairings: NodePairingResponse[] | undefined
): NodePairingResponse[] {
  return (pairings ?? []).filter(
    (pairing) =>
      pairing.status === 'waiting' || pairing.status === 'key_received'
  )
}

/** Label and tone for how a pair of mesh members is connected. */
export function meshLinkLabel(state: WireguardMeshLinkState): {
  label: string
  tone: 'ok' | 'warn' | 'error' | 'muted'
} {
  switch (state) {
    case 'direct':
      return { label: 'Direct', tone: 'ok' }
    case 'via_hub':
      return { label: 'Through the hub', tone: 'ok' }
    case 'connecting':
      return { label: 'Connecting', tone: 'muted' }
    case 'unreachable':
      return { label: 'Cannot connect', tone: 'error' }
  }
}

/** Pairs that are not simply direct, the broken ones first. */
export function linksNeedingAttention(
  links: WireguardMeshLink[] | undefined
): WireguardMeshLink[] {
  const rank = { unreachable: 0, connecting: 1, via_hub: 2, direct: 3 } as const
  return (links ?? [])
    .filter((link) => link.state !== 'direct')
    .sort((a, b) => rank[a.state] - rank[b.state])
}

/** A hub choice as a select value, and back. */
export function hubOptionValue(
  target: WireguardMeshHubTarget | undefined
): string {
  if (!target || target.kind === 'none') return 'none'
  if (target.kind === 'control_plane') return 'control-plane'
  return `node:${target.node_id}`
}

export function hubTargetFromOption(value: string): WireguardMeshHubTarget {
  if (value === 'control-plane') return { kind: 'control_plane' }
  const nodeId = Number.parseInt(value.replace(/^node:/, ''), 10)
  if (value.startsWith('node:') && Number.isInteger(nodeId)) {
    return { kind: 'node', node_id: nodeId }
  }
  return { kind: 'none' }
}

/** Members that can be the hub: the control plane and every node on the mesh. */
export function hubCandidates(
  mesh: Pick<WireguardMeshStatusResponse, 'nodes'>
): { value: string; label: string }[] {
  return [
    { value: 'control-plane', label: 'Control plane' },
    ...mesh.nodes
      .filter((node) => node.mesh_address)
      .map((node) => ({ value: `node:${node.node_id}`, label: node.name })),
  ]
}
