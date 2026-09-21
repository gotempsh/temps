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
        'This worker has not reported ingress support. Configure its public listener using the steps below; older workers also need an agent upgrade and ingress key enrollment.',
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
