// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getWorkloadMode,
  getWorkloadPendingChanges,
  recreateFromWorkloadSpec,
  updateWorkloadMode,
} from '@/api/client'
import type {
  PendingChanges,
  RecreateFromWorkloadSpecRequest,
  RecreateFromWorkloadSpecResponse,
  RecreateStatus,
  ServiceWorkloadMode,
  ServiceWorkloadModeResponse,
  WorkloadChange,
  WorkloadModeChangeResponse,
  WorkloadSupport,
} from '@/api/client'

/** How a database's container is built (ADR-055 Phase 4). */
export type { ServiceWorkloadMode }

/** Whether a database can leave `legacy`, and why not. */
export type ServiceWorkloadSupport = WorkloadSupport

/** A database's workload mode, where it runs and whether it can move. */
export type ServiceWorkloadModeStatus = ServiceWorkloadModeResponse

/** One field a recreate from the spec would change. Secrets are `***`. */
export type ServiceWorkloadChange = WorkloadChange

/** The changes a recreate would apply; `fingerprint` confirms them. */
export type ServiceWorkloadPendingChanges = PendingChanges

/** The previous mode, the new status and what a recreate would change. */
export type ServiceWorkloadModeChange = WorkloadModeChangeResponse

/** Whether a recreate needs confirmation or happened. */
export type ServiceRecreateStatus = RecreateStatus

/** The changes to confirm, or those applied and what still differs. */
export type ServiceRecreateResult = RecreateFromWorkloadSpecResponse

export function serviceWorkloadModeQueryKey(serviceId: number) {
  return ['external-service-workload-mode', serviceId] as const
}

export function serviceWorkloadPendingQueryKey(serviceId: number) {
  return ['external-service-workload-pending', serviceId] as const
}

export async function getServiceWorkloadMode(
  serviceId: number
): Promise<ServiceWorkloadModeStatus> {
  const { data } = await getWorkloadMode({
    path: { id: serviceId },
    throwOnError: true,
  })
  return data
}

export async function setServiceWorkloadMode(
  serviceId: number,
  mode: ServiceWorkloadMode
): Promise<ServiceWorkloadModeChange> {
  const { data } = await updateWorkloadMode({
    path: { id: serviceId },
    body: { mode },
    throwOnError: true,
  })
  return data
}

export async function getServiceWorkloadPendingChanges(
  serviceId: number
): Promise<ServiceWorkloadPendingChanges> {
  const { data } = await getWorkloadPendingChanges({
    path: { id: serviceId },
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
  const { data } = await recreateFromWorkloadSpec({
    path: { id: serviceId },
    body: recreateBody(confirmedFingerprint),
    throwOnError: true,
  })
  return data
}

/** The body that confirms a recreate of exactly the changes shown. */
export function recreateBody(
  confirmedFingerprint: string
): RecreateFromWorkloadSpecRequest & { confirmed_fingerprint: string } {
  return { confirmed_fingerprint: confirmedFingerprint }
}

/** Where the database runs, for a sentence. */
export function serviceLocation(nodeId: number | null | undefined): string {
  return nodeId == null ? 'the control plane' : `node ${nodeId}`
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
