// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  adminGetNodeResources,
  adminSetNodeResources,
  nodeStateStatus,
} from '@/api/client'
import type {
  NodeResourcePolicy,
  NodeResourcesResponse,
  NodeStateStatusResponse,
  SetNodeResourcesRequest,
  SystemReserved,
} from '@/api/client'

/** Node `0` is the control plane in every per-node workload route. */
export const CONTROL_PLANE_NODE_ID = 0

// ── Resource policy and system reserve (ADR-055 §5) ────────────────────────

/** Whether the scheduler counts reservations against a node's capacity. */
export type { NodeResourcePolicy }

/** What Temps keeps for itself on a node. */
export type { SystemReserved }

/** A node's resource policy and system reserve. Node `0` is the control plane. */
export type NodeResources = NodeResourcesResponse

/** At least one field must be set. */
export type NodeResourcesUpdate = SetNodeResourcesRequest

/** Upper bounds the server validates against (`MAX_RESERVED_*`). */
export const MAX_RESERVED_CPU_MILLIS = 256_000
export const MAX_RESERVED_MEMORY_MB = 4 * 1024 * 1024

/** The ADR-055 §5 defaults, mirrored from `SystemReserved::*`. */
export const SYSTEM_RESERVED_DEFAULTS = {
  worker: { cpu_millis: 250, memory_mb: 256 },
  controlPlaneLocalDatabase: { cpu_millis: 500, memory_mb: 1536 },
  controlPlaneExternalDatabase: { cpu_millis: 250, memory_mb: 512 },
} as const satisfies Record<string, SystemReserved>

export function nodeResourcesQueryKey(nodeId: number) {
  return ['node-resources', nodeId] as const
}

export async function getNodeResources(nodeId: number): Promise<NodeResources> {
  const { data } = await adminGetNodeResources({
    path: { node_id: nodeId },
    throwOnError: true,
  })
  return data
}

export async function setNodeResources(
  nodeId: number,
  update: NodeResourcesUpdate
): Promise<NodeResources> {
  const { data } = await adminSetNodeResources({
    path: { node_id: nodeId },
    body: update,
    throwOnError: true,
  })
  return data
}

/** `500m` / `1.5 GB` style text for a reserve. */
export function formatSystemReserved(reserved: SystemReserved): string {
  const cpu =
    reserved.cpu_millis % 1000 === 0
      ? `${reserved.cpu_millis / 1000} vCPU`
      : `${reserved.cpu_millis}m CPU`
  const memory =
    reserved.memory_mb >= 1024 && reserved.memory_mb % 512 === 0
      ? `${reserved.memory_mb / 1024} GB`
      : `${reserved.memory_mb} MB`
  return `${cpu}, ${memory} memory`
}

/** The default reserve sentence shown next to the inputs for a node. */
export function systemReservedDefaultsText(nodeId: number): string {
  if (nodeId === CONTROL_PLANE_NODE_ID) {
    return `Default: ${formatSystemReserved(SYSTEM_RESERVED_DEFAULTS.controlPlaneLocalDatabase)} with the database on this host, or ${formatSystemReserved(SYSTEM_RESERVED_DEFAULTS.controlPlaneExternalDatabase)} with an external database.`
  }
  return `Default for a worker: ${formatSystemReserved(SYSTEM_RESERVED_DEFAULTS.worker)} (agent, resolver and egress proxy).`
}

/**
 * Parse the two reserve inputs. Returns the reserve, or the sentence that
 * explains why it cannot be saved (mirroring the server's validation).
 */
export function parseSystemReserved(
  cpuMillis: string,
  memoryMb: string
): { ok: true; value: SystemReserved } | { ok: false; error: string } {
  const cpu = Number(cpuMillis.trim())
  const memory = Number(memoryMb.trim())
  if (cpuMillis.trim() === '' || !Number.isInteger(cpu) || cpu < 0) {
    return {
      ok: false,
      error: 'Reserved CPU must be a whole number of millicores (0 or more).',
    }
  }
  if (memoryMb.trim() === '' || !Number.isInteger(memory) || memory < 0) {
    return {
      ok: false,
      error: 'Reserved memory must be a whole number of MB (0 or more).',
    }
  }
  if (cpu > MAX_RESERVED_CPU_MILLIS) {
    return {
      ok: false,
      error: `Reserved CPU ${cpu}m exceeds the maximum of ${MAX_RESERVED_CPU_MILLIS}m.`,
    }
  }
  if (memory > MAX_RESERVED_MEMORY_MB) {
    return {
      ok: false,
      error: `Reserved memory ${memory} MB exceeds the maximum of ${MAX_RESERVED_MEMORY_MB} MB.`,
    }
  }
  return { ok: true, value: { cpu_millis: cpu, memory_mb: memory } }
}

export function resourcePolicySummary(resources: NodeResources): string {
  const reserve = formatSystemReserved(resources.system_reserved)
  return resources.resource_policy === 'enforced'
    ? `A placement that does not fit this node's capacity minus ${reserve} is refused.`
    : `Workloads are placed by observed load (up to 90%) over the capacity left after ${reserve}; reservations are not checked.`
}

// ── Node state stream (ADR-055 §7) ─────────────────────────────────────────

/**
 * Whether the node state stream runs in this process (`reason` says why
 * not), and how far its visibility watermark lags behind.
 */
export type NodeStateStatus = NodeStateStatusResponse

export const nodeStateStatusQueryKey = ['node-state-status'] as const

export async function getNodeStateStatus(): Promise<NodeStateStatus> {
  const { data } = await nodeStateStatus({ throwOnError: true })
  return data
}

/** A watermark lag above this is worth a warning: an old transaction holds it. */
export const NODE_STATE_LAG_WARNING_MS = 5_000

export type NodeStateTone = 'ok' | 'warning' | 'off'

/** One-glance verdict for the stream's status line. */
export function nodeStateStreamSummary(status: NodeStateStatus): {
  tone: NodeStateTone
  label: string
  detail: string
} {
  if (!status.configured) {
    return {
      tone: 'off',
      label: 'Not running',
      detail:
        status.reason ??
        'The workload plane does not run in this process, so nodes receive no desired-state stream.',
    }
  }
  const lag = formatLag(status.watermark_lag_ms)
  const counts = `${status.reads.toLocaleString()} reads, ${status.full_snapshots.toLocaleString()} full snapshots`
  if (status.watermark_lag_ms >= NODE_STATE_LAG_WARNING_MS) {
    return {
      tone: 'warning',
      label: `Lagging ${lag}`,
      detail: `The visibility watermark is ${lag} behind: an old open transaction is holding changes back from nodes. ${counts}.`,
    }
  }
  return {
    tone: 'ok',
    label: 'Streaming',
    detail: `Watermark lag ${lag}. ${counts}.`,
  }
}

export function formatLag(ms: number): string {
  if (ms < 1000) return `${ms} ms`
  const seconds = ms / 1000
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)} s`
  return `${Math.round(seconds / 60)} min`
}
