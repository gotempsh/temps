// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Whether a managed service placed like this is reached through a
 * `*.temps.local` name, which resolves only with Cluster DNS on.
 *
 * A standalone service on a worker is reached that way by every app on
 * another node (its port binds to loopback on its own host), and a cluster
 * whose members sit on more than one machine stores those names as its
 * member hostnames. A service on the control plane, or a cluster entirely on
 * one machine, keeps plain container names and needs nothing.
 */
export function placementNeedsClusterDns(
  placement:
    | { topology: 'standalone'; nodeId: number | null }
    | { topology: 'cluster'; memberNodeIds: (number | null | undefined)[] }
): boolean {
  if (placement.topology === 'standalone') return placement.nodeId !== null
  const machines = new Set(placement.memberNodeIds.map((id) => id ?? null))
  return machines.size > 1
}
