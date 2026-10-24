// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import {
  adminAttachEnvironmentVolume,
  adminCreateEnvironmentVolume,
  adminDeleteEnvironmentVolume,
  adminDetachEnvironmentVolume,
  adminEnvironmentPinningRecommendation,
  adminEnvironmentVolumeStatus,
  adminExplainEnvironmentPlacement,
  adminGetEnvironmentWorkloadIsolation,
  adminGetEnvironmentWorkloadMode,
  adminListEnvironmentVolumes,
  adminSetEnvironmentWorkloadIsolation,
  adminSetEnvironmentWorkloadMode,
} from '@/api/client'
import type {
  AttachEnvironmentVolumeRequest,
  CreateEnvironmentVolumeRequest,
  DeletedEnvironmentVolumeResponse,
  EnvironmentVolumeListResponse,
  EnvironmentVolumeResponse,
  EnvironmentVolumeStatusResponse,
  EnvironmentWorkloadIsolation,
  EnvironmentWorkloadMode,
  NodeExclusionDto,
  NodeRecommendationDto,
  PinningRecommendationResponse,
  PlacementExplanationResponse,
  VolumeAttachmentDto,
  WorkloadIsolationResponse,
  WorkloadModeResponse,
} from '@/api/client'
import { httpStatusOf } from '@/lib/http-error-status'

/** Which scheduler places an environment's deployments (ADR-055 Phase 5). */
export type { EnvironmentWorkloadMode }

/** The isolation an environment's plane-run deployments request (Phase 10). */
export type { EnvironmentWorkloadIsolation }

/** An environment's workload mode. */
export type EnvironmentWorkloadModeSettings = WorkloadModeResponse

/** An environment's isolation; only `enforce` deployments honour it. */
export type EnvironmentWorkloadIsolationSettings = WorkloadIsolationResponse

/** An eligible node and why. */
export type NodeRecommendation = NodeRecommendationDto

/** A node left out (or kept with a warning when `excluded` is false). */
export type NodeExclusion = NodeExclusionDto

/** Where the environment's replicas would go now, and why. */
export type PlacementExplanation = PlacementExplanationResponse

/** The recommended `target_nodes`. Recommended, never applied. */
export type PinningRecommendation = PinningRecommendationResponse

/** Where a volume is mounted. */
export type VolumeAttachment = VolumeAttachmentDto

/** An environment volume; `attachment` is absent when detached. */
export type EnvironmentVolume = EnvironmentVolumeResponse

/** A page of an environment's volumes. */
export type EnvironmentVolumeList = EnvironmentVolumeListResponse

export type CreateEnvironmentVolume = CreateEnvironmentVolumeRequest

export type AttachEnvironmentVolume = AttachEnvironmentVolumeRequest

/** Whether the next deploy mounts the attached volumes, and why not. */
export type EnvironmentVolumeStatus = EnvironmentVolumeStatusResponse

/** The deleted volume, and whether its backing data was removed. */
export type DeletedEnvironmentVolume = DeletedEnvironmentVolumeResponse

/** The control plane's node id in every placement response. */
export const CONTROL_PLANE_NODE_ID = 0

export const DEFAULT_RECOMMENDATION_WINDOW_DAYS = 7

/** DOM id of the workload-plane section, so other cards can link to it. */
export const WORKLOAD_PLANE_SECTION_ID = 'environment-workload-plane'

// ── Query keys ──────────────────────────────────────────────────────────────

export function workloadModeQueryKey(environmentId: number) {
  return ['environment-workload-mode', environmentId] as const
}

export function workloadIsolationQueryKey(environmentId: number) {
  return ['environment-workload-isolation', environmentId] as const
}

export function placementExplanationQueryKey(environmentId: number) {
  return ['environment-placement-explain', environmentId] as const
}

export function pinningRecommendationQueryKey(
  environmentId: number,
  windowDays: number
) {
  return [
    'environment-placement-recommendation',
    environmentId,
    windowDays,
  ] as const
}

/** Prefix of every volume query of an environment (list pages and status). */
export function environmentVolumesQueryKey(environmentId: number) {
  return ['environment-volumes', environmentId] as const
}

export function environmentVolumeListQueryKey(
  environmentId: number,
  page: number,
  pageSize: number
) {
  return ['environment-volumes', environmentId, 'list', page, pageSize] as const
}

export function environmentVolumeStatusQueryKey(environmentId: number) {
  return ['environment-volumes', environmentId, 'status'] as const
}

// ── Requests ────────────────────────────────────────────────────────────────

export async function getWorkloadMode(
  environmentId: number
): Promise<EnvironmentWorkloadModeSettings> {
  const { data } = await adminGetEnvironmentWorkloadMode({
    path: { environment_id: environmentId },
    throwOnError: true,
  })
  return data
}

export async function setWorkloadMode(
  environmentId: number,
  workloadMode: EnvironmentWorkloadMode
): Promise<EnvironmentWorkloadModeSettings> {
  const { data } = await adminSetEnvironmentWorkloadMode({
    path: { environment_id: environmentId },
    body: { workload_mode: workloadMode },
    throwOnError: true,
  })
  return data
}

export async function getWorkloadIsolation(
  environmentId: number
): Promise<EnvironmentWorkloadIsolationSettings> {
  const { data } = await adminGetEnvironmentWorkloadIsolation({
    path: { environment_id: environmentId },
    throwOnError: true,
  })
  return data
}

export async function setWorkloadIsolation(
  environmentId: number,
  workloadIsolation: EnvironmentWorkloadIsolation
): Promise<EnvironmentWorkloadIsolationSettings> {
  const { data } = await adminSetEnvironmentWorkloadIsolation({
    path: { environment_id: environmentId },
    body: { workload_isolation: workloadIsolation },
    throwOnError: true,
  })
  return data
}

export async function explainPlacement(
  environmentId: number
): Promise<PlacementExplanation> {
  const { data } = await adminExplainEnvironmentPlacement({
    path: { environment_id: environmentId },
    throwOnError: true,
  })
  return data
}

export async function getPinningRecommendation(
  environmentId: number,
  windowDays: number = DEFAULT_RECOMMENDATION_WINDOW_DAYS
): Promise<PinningRecommendation> {
  const { data } = await adminEnvironmentPinningRecommendation({
    path: { environment_id: environmentId },
    query: { window_days: windowDays },
    throwOnError: true,
  })
  return data
}

export async function listEnvironmentVolumes(
  environmentId: number,
  page: number,
  pageSize: number
): Promise<EnvironmentVolumeList> {
  const { data } = await adminListEnvironmentVolumes({
    path: { environment_id: environmentId },
    query: { page, page_size: pageSize },
    throwOnError: true,
  })
  return data
}

export async function getEnvironmentVolumeStatus(
  environmentId: number
): Promise<EnvironmentVolumeStatus> {
  const { data } = await adminEnvironmentVolumeStatus({
    path: { environment_id: environmentId },
    throwOnError: true,
  })
  return data
}

export async function createEnvironmentVolume(
  environmentId: number,
  request: CreateEnvironmentVolume
): Promise<EnvironmentVolume> {
  const { data } = await adminCreateEnvironmentVolume({
    path: { environment_id: environmentId },
    body: createVolumeBody(request),
    throwOnError: true,
  })
  return data
}

export async function attachEnvironmentVolume(
  environmentId: number,
  volumeId: number,
  request: AttachEnvironmentVolume
): Promise<EnvironmentVolume> {
  const { data } = await adminAttachEnvironmentVolume({
    path: { environment_id: environmentId, volume_id: volumeId },
    body: {
      mount_path: request.mount_path.trim(),
      read_only: request.read_only ?? false,
    },
    throwOnError: true,
  })
  return data
}

export async function detachEnvironmentVolume(
  environmentId: number,
  volumeId: number
): Promise<EnvironmentVolume> {
  const { data } = await adminDetachEnvironmentVolume({
    path: { environment_id: environmentId, volume_id: volumeId },
    throwOnError: true,
  })
  return data
}

export async function deleteEnvironmentVolume(
  environmentId: number,
  volumeId: number
): Promise<DeletedEnvironmentVolume> {
  const { data } = await adminDeleteEnvironmentVolume({
    path: { environment_id: environmentId, volume_id: volumeId },
    throwOnError: true,
  })
  return data
}

// ── Pure helpers ────────────────────────────────────────────────────────────

/** Whether the server refused the call for lack of permission (HTTP 403). */
export function isPermissionDenied(error: unknown): boolean {
  return httpStatusOf(error) === 403
}

/**
 * The body of a create request: a trimmed name, and `size_mb` only when it
 * is a positive whole number (the server takes it as an optional `u64`).
 */
export function createVolumeBody(
  request: CreateEnvironmentVolume
): CreateEnvironmentVolume {
  const size = request.size_mb
  return {
    name: request.name.trim(),
    size_mb:
      typeof size === 'number' && Number.isInteger(size) && size > 0
        ? size
        : null,
    retain_on_delete: request.retain_on_delete ?? false,
  }
}

/**
 * Why a volume name would be refused, mirroring `validate_name` in
 * `temps-workload-volumes`, or `null` when it is acceptable. The server is
 * the authority; this only spares a round trip.
 */
export function volumeNameError(name: string): string | null {
  const value = name.trim()
  if (value.length < 2 || value.length > 64) {
    return 'Use 2 to 64 characters.'
  }
  if (!/^[a-z0-9][a-z0-9_.-]*$/.test(value)) {
    return 'Use lowercase letters, digits, "_", "." or "-", starting with a letter or digit.'
  }
  return null
}

/**
 * Why a mount path would be refused (absolute, without `.` or `..`
 * segments), or `null`. Reserved paths such as `/proc` are left to the
 * server, whose refusal names the rule.
 */
export function mountPathError(path: string): string | null {
  const value = path.trim()
  if (!value.startsWith('/'))
    return 'The mount path must be absolute, e.g. /data.'
  if (value === '/')
    return 'A volume cannot be mounted over the root directory.'
  if (value.split('/').some((segment) => segment === '.' || segment === '..')) {
    return 'The mount path must not contain "." or ".." segments.'
  }
  return null
}

/** What the saved deployment workload mode does right now. */
export function workloadModeSummary(mode: EnvironmentWorkloadMode): string {
  switch (mode) {
    case 'legacy':
      return 'Deployments are placed and run exactly as before the workload plane.'
    case 'shadow':
      return 'Deployments are placed and run as before; the workload plane scheduler runs alongside and every disagreement is logged.'
    case 'enforce':
      return 'The workload plane scheduler places deployments, and replicas on the control plane are created and run by the workload plane.'
  }
}

/** What the saved isolation does, given whether the mode honours it. */
export function isolationSummary(
  settings: EnvironmentWorkloadIsolationSettings
): string {
  const name =
    settings.workload_isolation === 'micro_vm'
      ? 'a Firecracker microVM'
      : 'a container'
  if (!settings.applies) {
    return `Recorded as ${name}, but ignored until the workload mode is Enforce: deployments keep running in a container.`
  }
  return settings.workload_isolation === 'micro_vm'
    ? 'Each deployment runs in a Firecracker microVM, only on a node whose runtime provides it.'
    : 'Each deployment runs in a container sharing the host kernel.'
}

/** Node names from an explanation, so bare node ids can be shown by name. */
export function nodeNamesOf(
  explanation: PlacementExplanation | undefined
): Map<number, string> {
  const names = new Map<number, string>()
  if (!explanation) return names
  for (const node of explanation.recommended) {
    names.set(node.node_id, node.node_name)
  }
  for (const node of explanation.excluded) {
    if (!names.has(node.node_id)) names.set(node.node_id, node.node_name)
  }
  return names
}

/** A node's display name: its name when known, the control plane for 0. */
export function nodeLabel(nodeId: number, names: Map<number, string>): string {
  const name = names.get(nodeId)
  if (nodeId === CONTROL_PLANE_NODE_ID) {
    return name ? `${name} (control plane)` : 'Control plane'
  }
  return name ?? `Node ${nodeId}`
}

/** "Where would the next deployment run", in one sentence. */
export function placementSummary(explanation: PlacementExplanation): string {
  if (explanation.unschedulable) {
    return `The next deployment could not be placed: ${explanation.unschedulable}`
  }
  const placement = explanation.placement
  if (!placement || placement.length === 0) {
    return 'The scheduler did not return a placement for the next deployment.'
  }
  const names = nodeNamesOf(explanation)
  const counts = new Map<number, number>()
  for (const node of placement) counts.set(node, (counts.get(node) ?? 0) + 1)
  const parts = [...counts.entries()].map(([node, count]) =>
    count > 1
      ? `${nodeLabel(node, names)} (${count} replicas)`
      : nodeLabel(node, names)
  )
  return placement.length === 1
    ? `The next deployment would run on ${parts[0]}.`
    : `The next deployment's ${placement.length} replicas would run on ${parts.join(', ')}.`
}

/** The pinning recommendation, in one sentence. */
export function recommendationSummary(
  recommendation: PinningRecommendation,
  names: Map<number, string>
): string {
  const window = `the last ${recommendation.window_days} day${
    recommendation.window_days === 1 ? '' : 's'
  }`
  if (
    recommendation.changes_placement &&
    recommendation.recommended_nodes.length > 0
  ) {
    const nodes = recommendation.recommended_nodes
      .map((node) => nodeLabel(node, names))
      .join(', ')
    return `Recommended: pin this environment to ${nodes}. It moved between nodes in ${window}.`
  }
  if (recommendation.nodes_used.length === 0) {
    return `No deployments ran in ${window}, so there is nothing to recommend.`
  }
  return `No change recommended: the current placement already fits what ran in ${window}.`
}

// ── Volume forms ────────────────────────────────────────────────────────────

export const createVolumeFormSchema = z.object({
  name: z
    .string()
    .trim()
    .superRefine((value, ctx) => {
      const error = volumeNameError(value)
      if (error) ctx.addIssue({ code: 'custom', message: error })
    }),
  sizeMb: z
    .string()
    .trim()
    .refine(
      (value) => value === '' || (/^\d+$/.test(value) && Number(value) > 0),
      'Size must be a whole number of megabytes, or empty for no limit'
    ),
  retainOnDelete: z.boolean(),
})

export type CreateVolumeFormValues = z.infer<typeof createVolumeFormSchema>

export function toCreateVolumeRequest(
  values: CreateVolumeFormValues
): CreateEnvironmentVolume {
  const size = values.sizeMb.trim()
  return createVolumeBody({
    name: values.name,
    size_mb: size === '' ? null : Number(size),
    retain_on_delete: values.retainOnDelete,
  })
}

export const attachVolumeFormSchema = z.object({
  mountPath: z
    .string()
    .trim()
    .superRefine((value, ctx) => {
      const error = mountPathError(value)
      if (error) ctx.addIssue({ code: 'custom', message: error })
    }),
  readOnly: z.boolean(),
})

export type AttachVolumeFormValues = z.infer<typeof attachVolumeFormSchema>

/** Where a volume's data lives, for a sentence. */
export function volumeLocation(volume: EnvironmentVolume): string {
  if (volume.node_id == null) return 'not created on a node yet'
  return volume.node_id === CONTROL_PLANE_NODE_ID
    ? 'on the control plane'
    : `on node ${volume.node_id}`
}

/**
 * What deleting a volume does to its data, for the confirmation dialog:
 * whether the backing data is retained (`retain_on_delete`) or removed.
 */
export function volumeDeleteConsequence(volume: EnvironmentVolume): string {
  const mounted = volume.attachment
    ? ` It is attached at ${volume.attachment.mount_path}: if a running replica still mounts it, the delete is refused until you detach it and redeploy.`
    : ''
  if (volume.retain_on_delete) {
    return `The volume record is removed, but its data is retained (retain on delete is set) and stays ${volumeLocation(volume)}.${mounted}`
  }
  if (volume.node_id == null) {
    return `The volume was never created on a node, so there is no data to remove.${mounted}`
  }
  if (volume.node_id === CONTROL_PLANE_NODE_ID) {
    return `Its Docker volume and all the data in it are removed from the control plane.${mounted}`
  }
  return `The record is removed, but its data on node ${volume.node_id} is kept: this release cannot remove it there, so remove the Docker volume on that node by hand.${mounted}`
}

/** The toast after a delete: whether the backing data went with it. */
export function volumeDeletedMessage(result: DeletedEnvironmentVolume): string {
  if (result.backing_removed) {
    return `Deleted volume ${result.volume.name} and the data in it`
  }
  return result.backing_kept_reason
    ? `Deleted volume ${result.volume.name}; its data was kept: ${result.backing_kept_reason}`
    : `Deleted volume ${result.volume.name}`
}
