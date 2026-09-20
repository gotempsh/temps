// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'

/**
 * Can this installation actually run a workload right now?
 *
 * Mirrors the Rust `NodeCapabilityResponse` served by
 * `GET /api/nodes/capability`. A control plane that was started without a
 * local Docker daemon runs nothing itself, so every build, deployment and
 * managed service needs a worker node to have joined.
 *
 * TEMPORARY: hand-written because the endpoint is newer than the committed
 * generated client. Delete this module and import the generated
 * `getNodeCapability` / `getNodeCapabilityOptions` once `bun run openapi-ts`
 * has been re-run against a server carrying the endpoint — the shape below is
 * deliberately identical so only the import sites change.
 */
export interface NodeCapability {
  /** True when the control plane can run containers itself. */
  local_workloads: boolean
  /** Worker nodes currently online and accepting work. */
  active_worker_nodes: number
  /** True when *something* (local or worker) can run a workload. */
  schedulable: boolean
  /** Why nothing is schedulable, when `schedulable` is false. */
  reason: string | null
  /** Console path that fixes it. */
  setup_path: string
}

export const nodeCapabilityQueryKey = ['nodes', 'capability'] as const

/**
 * Read the scheduling capability.
 *
 * Fails *open*: a server that does not serve this endpoint yet (404), or any
 * other read failure, resolves to "schedulable" so the console never tells an
 * operator their platform is broken on the strength of a missing endpoint.
 * A genuinely unschedulable control plane always answers.
 */
export async function fetchNodeCapability(): Promise<NodeCapability> {
  const response = await client.get({
    url: '/nodes/capability' as never,
  })
  const data = response.data as NodeCapability | undefined
  if (!data || typeof data.schedulable !== 'boolean') {
    return {
      local_workloads: true,
      active_worker_nodes: 0,
      schedulable: true,
      reason: null,
      setup_path: '/settings/nodes',
    }
  }
  return data
}

/** Query options for the capability read, shaped like the generated SDK's. */
export function nodeCapabilityOptions() {
  return {
    queryKey: nodeCapabilityQueryKey,
    queryFn: fetchNodeCapability,
    // Operator-level state that only changes when a node joins or leaves.
    staleTime: 60_000,
    retry: false,
  }
}
