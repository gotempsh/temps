// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
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
