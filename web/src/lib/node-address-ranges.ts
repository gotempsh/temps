// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { WireguardMeshStatusResponse } from '@/api/client/types.gen'

/**
 * The private network's range to explain on the Worker Nodes page, and
 * whether nodes already have their addresses from it. Nothing while the
 * mesh is off or unreadable: no machine has an address from it then.
 */
export function meshRangeToShow(
  mesh: Pick<WireguardMeshStatusResponse, 'state' | 'cidr'> | undefined
): { cidr: string; ready: boolean } | undefined {
  if (!mesh || mesh.state === 'disabled' || !mesh.cidr) return undefined
  return { cidr: mesh.cidr, ready: mesh.state === 'ready' }
}
