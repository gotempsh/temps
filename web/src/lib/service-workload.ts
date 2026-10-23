// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'

/**
 * How a database's container is built (ADR-055 Phase 4). Mirrors
 * `ServiceWorkloadMode` (serde `snake_case`).
 */
export type ServiceWorkloadMode = 'legacy' | 'shadow' | 'enforce'

/** Mirrors `WorkloadSupport` in `temps-providers/src/workload/mod.rs`. */
export interface ServiceWorkloadSupport {
  supported: boolean
  /** Why the database stays on its engine's own path; omitted when supported. */
  reason?: string
}

/** Mirrors `WorkloadModeResponse` in `workload_handlers.rs`. */
export interface ServiceWorkloadModeStatus {
  service_id: number
  service_name: string
  service_type: string
  mode: ServiceWorkloadMode
  /** Omitted when the database runs on the control plane. */
  node_id?: number
  support: ServiceWorkloadSupport
}

/** Mirrors `WorkloadChange`. */
export interface ServiceWorkloadChange {
  /** `labels`, `restart_policy`, `pids_limit`, `healthcheck`, ... */
  field: string
  /** What the running container has. Secrets are shown as `***`. */
  current: string
  /** What the container will have once recreated from its spec. */
  after_recreate: string
}

/** Mirrors `PendingChanges`. */
export interface ServiceWorkloadPendingChanges {
  service_id: number
  service_name: string
  mode: ServiceWorkloadMode
  /** Omitted for the control plane. */
  node_id?: number
  container_name?: string
  support: ServiceWorkloadSupport
  /** Whether the database's container exists to compare against. */
  container_found: boolean
  changes: ServiceWorkloadChange[]
  /** Identifies `changes`: sent back to confirm a recreate. */
  fingerprint: string
  computed_at: string
}

/** Mirrors `WorkloadModeChangeResponse`. */
export interface ServiceWorkloadModeChange {
  previous_mode: ServiceWorkloadMode
  workload: ServiceWorkloadModeStatus
  /** Omitted in `legacy`, or when the container could not be read. */
  pending_changes?: ServiceWorkloadPendingChanges
}

/** Mirrors `RecreateStatus` (serde `snake_case`). */
export type ServiceRecreateStatus = 'confirmation_required' | 'recreated'

/** Mirrors `RecreateFromWorkloadSpecResponse`. */
export interface ServiceRecreateResult {
  status: ServiceRecreateStatus
  /** The changes to confirm, or the changes that were applied. */
  pending_changes: ServiceWorkloadPendingChanges
  /** After a recreate: what still differs (empty when it worked). */
  remaining_changes?: ServiceWorkloadPendingChanges
}

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const
const JSON_HEADERS = { 'Content-Type': 'application/json' }

export function serviceWorkloadUrl(serviceId: number): string {
  return `/external-services/${serviceId}/workload-mode`
}

export function serviceWorkloadModeQueryKey(serviceId: number) {
  return ['external-service-workload-mode', serviceId] as const
}

export function serviceWorkloadPendingQueryKey(serviceId: number) {
  return ['external-service-workload-pending', serviceId] as const
}

export async function getServiceWorkloadMode(
  serviceId: number
): Promise<ServiceWorkloadModeStatus> {
  const { data } = await client.get<ServiceWorkloadModeStatus, unknown, true>({
    security: [...BEARER_SECURITY],
    url: serviceWorkloadUrl(serviceId),
    throwOnError: true,
  })
  return data
}

export async function setServiceWorkloadMode(
  serviceId: number,
  mode: ServiceWorkloadMode
): Promise<ServiceWorkloadModeChange> {
  const { data } = await client.put<ServiceWorkloadModeChange, unknown, true>({
    security: [...BEARER_SECURITY],
    url: serviceWorkloadUrl(serviceId),
    body: { mode },
    headers: JSON_HEADERS,
    throwOnError: true,
  })
  return data
}

export async function getServiceWorkloadPendingChanges(
  serviceId: number
): Promise<ServiceWorkloadPendingChanges> {
  const { data } = await client.get<
    ServiceWorkloadPendingChanges,
    unknown,
    true
  >({
    security: [...BEARER_SECURITY],
    url: `${serviceWorkloadUrl(serviceId)}/pending-changes`,
    throwOnError: true,
  })
  return data
}

/**
 * Recreate the container from its spec. The server only recreates with the
 * fingerprint of the changes the administrator was shown; without one it
 * returns them for confirmation and changes nothing.
 */
export async function recreateServiceFromWorkloadSpec(
  serviceId: number,
  confirmedFingerprint: string
): Promise<ServiceRecreateResult> {
  const { data } = await client.post<ServiceRecreateResult, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `${serviceWorkloadUrl(serviceId)}/recreate`,
    body: recreateBody(confirmedFingerprint),
    headers: JSON_HEADERS,
    throwOnError: true,
  })
  return data
}

/** The body that confirms a recreate of exactly the changes shown. */
export function recreateBody(confirmedFingerprint: string): {
  confirmed_fingerprint: string
} {
  return { confirmed_fingerprint: confirmedFingerprint }
}

/** Where the database runs, for a sentence. */
export function serviceLocation(nodeId: number | undefined): string {
  return nodeId === undefined ? 'the control plane' : `node ${nodeId}`
}

/** What the saved mode does right now. */
export function serviceWorkloadModeSummary(
  status: ServiceWorkloadModeStatus
): string {
  switch (status.mode) {
    case 'legacy':
      return 'The database engine builds its own container request, exactly as before the workload plane.'
    case 'shadow':
      return 'The engine still builds the container; a background pass compares the running container with its workload spec and logs every difference.'
    case 'enforce':
      return 'The container is built from its admitted workload spec the next time it is created or recreated. The running container is unchanged until then.'
  }
}

/** What switching to `mode` would do, for the confirmation dialog. */
export function serviceModeChangeConsequence(
  mode: ServiceWorkloadMode
): string {
  switch (mode) {
    case 'legacy':
      return 'The engine builds its own container request again. Nothing running changes.'
    case 'shadow':
      return 'Nothing running changes. A background pass starts comparing the running container with its workload spec and logs the differences.'
    case 'enforce':
      return 'Nothing running changes now. The next time the container is created or recreated it is built from its workload spec through the Docker driver, with the changes listed below.'
  }
}

/**
 * Whether a recreate from the spec can be offered now, and why not. The
 * server enforces the same rules and explains any refusal.
 */
export function recreateAvailability(
  status: ServiceWorkloadModeStatus,
  pending: ServiceWorkloadPendingChanges | undefined
): { allowed: boolean; reason: string | null } {
  if (!status.support.supported) {
    return {
      allowed: false,
      reason:
        status.support.reason ?? 'This database cannot leave the legacy mode.',
    }
  }
  if (status.mode !== 'enforce') {
    return {
      allowed: false,
      reason:
        'A recreate from the workload spec is only available in Enforce mode.',
    }
  }
  if (!pending) {
    return { allowed: false, reason: 'Loading the pending changes…' }
  }
  if (!pending.container_found) {
    return {
      allowed: false,
      reason: `No container was found for ${pending.service_name} on ${serviceLocation(pending.node_id)} to compare against or recreate.`,
    }
  }
  if (pending.changes.length === 0) {
    return {
      allowed: false,
      reason:
        'Nothing to change: the running container already matches its workload spec.',
    }
  }
  return { allowed: true, reason: null }
}
