// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  NodePairingResponse,
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
export type InternetJoinMethod = 'url' | 'pair'

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

/** Pairings still in progress. */
export function pendingPairings(
  pairings: NodePairingResponse[] | undefined
): NodePairingResponse[] {
  return (pairings ?? []).filter(
    (pairing) =>
      pairing.status === 'waiting' || pairing.status === 'key_received'
  )
}
