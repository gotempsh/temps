// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface WorkerIngressState {
  status: string
  last_heartbeat?: string | null
  public_ingress_enabled: boolean
  public_ingress_running?: boolean | null
  public_ingress_last_error?: string | null
  public_ingress_certificate_count?: number | null
  public_ingress_route_count?: number | null
  public_ingress_unsupported_route_count?: number | null
}

export function workerIngressStatus(
  node: WorkerIngressState,
  now = Date.now()
) {
  if (!node.public_ingress_enabled) {
    return {
      label: 'Disabled',
      description:
        'Enable public ingress to receive application traffic on this worker.',
    }
  }
  const heartbeat = node.last_heartbeat ? Date.parse(node.last_heartbeat) : NaN
  if (
    node.status === 'offline' ||
    !Number.isFinite(heartbeat) ||
    now - heartbeat > 90_000
  ) {
    return {
      label: 'Unconfirmed',
      description:
        'No recent worker heartbeat. Check worker connectivity before pointing DNS here.',
    }
  }
  if (node.public_ingress_last_error) {
    return {
      label: 'Needs attention',
      description: node.public_ingress_last_error,
    }
  }
  if (node.public_ingress_running == null) {
    return {
      label: 'Waiting for worker',
      description:
        'This worker has not reported ingress support yet. Choose where it listens below; a worker on an older Temps needs an upgrade first.',
    }
  }
  if (!node.public_ingress_running) {
    return {
      label: 'Starting',
      description:
        'Waiting for the worker to start its public listeners and apply configuration.',
    }
  }
  if (node.public_ingress_route_count === 0) {
    return {
      label: 'No public routes',
      description:
        'The listeners are running, but no supported application routes have synchronized. Check your deployment and domain before changing DNS.',
    }
  }
  if (!node.public_ingress_certificate_count) {
    return {
      label: 'Waiting for certificates',
      description:
        'Public listeners are running. Configure a domain and certificate before sending HTTPS traffic.',
    }
  }
  if ((node.public_ingress_unsupported_route_count ?? 0) > 0) {
    return {
      label: 'Some routes unavailable',
      description:
        'The worker has public listeners, but some application routes require features it cannot serve. Review the exclusions below before changing DNS.',
    }
  }
  return {
    label: 'Listeners running',
    description:
      'The worker reports public listeners and certificates. Verify your domain from outside the cluster before switching production traffic.',
  }
}

export type AddressScope = 'public' | 'private' | 'cgnat' | 'local'

/** Who can reach an address; mirrors `temps_core::worker_ingress::address_scope`. */
export function addressScope(address: string): AddressScope | undefined {
  const v4 = address
    .trim()
    .match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/)
  if (v4) {
    const [a, b, c] = v4.slice(1, 4).map(Number)
    if (v4.slice(1).some((part) => Number(part) > 255)) return undefined
    if (
      a === 0 ||
      a === 127 ||
      (a === 169 && b === 254) ||
      a >= 224 ||
      (a === 192 && b === 0 && c === 2) ||
      (a === 198 && b === 51 && c === 100) ||
      (a === 203 && b === 0 && c === 113)
    )
      return 'local'
    if (a === 100 && b >= 64 && b < 128) return 'cgnat'
    if (
      a === 10 ||
      (a === 172 && b >= 16 && b < 32) ||
      (a === 192 && b === 168)
    )
      return 'private'
    return 'public'
  }
  if (!address.includes(':')) return undefined
  const lower = address.trim().toLowerCase()
  if (lower === '::1' || lower === '::' || lower.startsWith('ff'))
    return 'local'
  if (/^fe[89ab]/.test(lower) || lower.startsWith('2001:db8')) return 'local'
  if (/^f[cd]/.test(lower)) return 'private'
  return 'public'
}

export function scopeLabel(scope: AddressScope): string {
  switch (scope) {
    case 'public':
      return 'public'
    case 'private':
      return 'private (behind NAT)'
    case 'cgnat':
      return 'carrier-grade NAT'
    case 'local':
      return 'local only'
  }
}

/**
 * What a new worker allows, chosen in Add worker and passed to `temps join`.
 * Only the worker can change it afterwards (`temps agent ingress`).
 */
export type JoinIngressChoice = 'off' | 'delegated' | 'fixed'

export function joinIngressFlag(choice: JoinIngressChoice): string {
  return choice === 'off' ? '' : ` --ingress ${choice}`
}

/** The commands, run on the worker, that let it take public traffic. */
export const ALLOW_INGRESS_COMMANDS = {
  delegated: 'temps agent ingress delegated',
  fixed: 'temps agent ingress fixed --listen auto',
} as const

/** How visitors get into a worker, at a glance: the node list and header. */
export type WorkerIngressWay =
  | { kind: 'tunnel'; providers: string[] }
  | { kind: 'direct'; running: boolean }
  | { kind: 'none' }

/**
 * Tunnels first: with one carrying the worker, open ports are a second way
 * in. `connectors` is every tunnel in the cluster (`allTunnelsOptions`).
 */
export function workerIngressWay(
  node: {
    id: number
    public_ingress_enabled: boolean
    public_ingress_running?: boolean | null
  },
  connectors: { provider: string; nodes: { node_id: number }[] }[]
): WorkerIngressWay {
  const providers = [
    ...new Set(
      connectors
        .filter((c) => c.nodes.some((n) => n.node_id === node.id))
        .map((c) => c.provider)
    ),
  ]
  if (providers.length > 0) return { kind: 'tunnel', providers }
  if (node.public_ingress_enabled) {
    return { kind: 'direct', running: node.public_ingress_running === true }
  }
  return { kind: 'none' }
}
