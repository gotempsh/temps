// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import { client } from '@/api/client/client.gen'
import { httpStatusOf } from '@/lib/http-error-status'

/**
 * Which scheduler places an environment's deployments (ADR-055 Phase 5).
 * Mirrors `EnvironmentWorkloadMode` (serde `snake_case`).
 */
export type EnvironmentWorkloadMode = 'legacy' | 'shadow' | 'enforce'

/**
 * The isolation an environment's plane-run deployments request (ADR-055
 * Phase 10). Mirrors `EnvironmentWorkloadIsolation` (serde `snake_case`).
 */
export type EnvironmentWorkloadIsolation = 'process' | 'micro_vm'

/** Mirrors `WorkloadModeResponse` in `workload_placement.rs`. */
export interface EnvironmentWorkloadModeSettings {
  environment_id: number
  workload_mode: EnvironmentWorkloadMode
}

/** Mirrors `WorkloadIsolationResponse`. */
export interface EnvironmentWorkloadIsolationSettings {
  environment_id: number
  workload_isolation: EnvironmentWorkloadIsolation
  /** Only `enforce` deployments run through the plane and honour it. */
  applies: boolean
}

/** Mirrors `NodeRecommendationDto`. */
export interface NodeRecommendation {
  node_id: number
  node_name: string
  score: number
  reasons: string[]
}

/** Mirrors `NodeExclusionDto`. */
export interface NodeExclusion {
  node_id: number
  node_name: string
  /** The `Scheduled` condition reason, e.g. `NodeDraining`. */
  reason: string
  message: string
  /** `false` when the node was kept with a warning. */
  excluded: boolean
}

/** Mirrors `PlacementExplanationResponse`. */
export interface PlacementExplanation {
  environment_id: number
  /** Eligible nodes, best first. */
  recommended: NodeRecommendation[]
  excluded: NodeExclusion[]
  /** Where each replica would go now (`0` = the control plane). */
  placement: number[] | null
  unschedulable: string | null
  unschedulable_reason: string | null
  warnings: string[]
  /** Why the environment is pinned by its volumes, `null` without any. */
  pinned_by: string | null
}

/** Mirrors `PinningRecommendationResponse`. */
export interface PinningRecommendation {
  environment_id: number
  window_days: number
  nodes_used: number[]
  moved_between_nodes: boolean
  /** The recommended `target_nodes`. Recommended, never applied. */
  recommended_nodes: number[]
  reasons: string[]
  current_nodes: number[]
  changes_placement: boolean
}

/** Mirrors `VolumeAttachmentDto` in `workload_volumes.rs`. */
export interface VolumeAttachment {
  mount_path: string
  read_only: boolean
}

/** Mirrors `EnvironmentVolumeResponse`. */
export interface EnvironmentVolume {
  id: number
  environment_id: number
  name: string
  /** `named` (a Docker named volume). */
  backing: string
  /** `null` until first created, `0` for the control plane. */
  node_id: number | null
  size_mb: number | null
  retain_on_delete: boolean
  /** `null` when detached. */
  attachment: VolumeAttachment | null
  pin_reason: string | null
  created_at: string
  updated_at: string
}

/** Mirrors `EnvironmentVolumeListResponse`. */
export interface EnvironmentVolumeList {
  volumes: EnvironmentVolume[]
  total: number
  page: number
  page_size: number
}

/** Mirrors `CreateEnvironmentVolumeRequest`. */
export interface CreateEnvironmentVolume {
  name: string
  size_mb?: number | null
  retain_on_delete?: boolean
}

/** Mirrors `AttachEnvironmentVolumeRequest`. */
export interface AttachEnvironmentVolume {
  mount_path: string
  read_only?: boolean
}

/** Mirrors `EnvironmentVolumeStatusResponse`. */
export interface EnvironmentVolumeStatus {
  environment_id: number
  workload_mode: EnvironmentWorkloadMode
  replicas: number
  attached: number
  /** Whether the next deploy mounts the attached volumes. */
  active: boolean
  /** Why they would not be mounted (the next deploy fails with it). */
  reason: string | null
  setup_path: string | null
}

/** Mirrors `DeletedEnvironmentVolumeResponse`. */
export interface DeletedEnvironmentVolume {
  volume: EnvironmentVolume
  backing_removed: boolean
  backing_kept_reason: string | null
}

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const
const JSON_HEADERS = { 'Content-Type': 'application/json' }

/** The control plane's node id in every placement response. */
export const CONTROL_PLANE_NODE_ID = 0

export const DEFAULT_RECOMMENDATION_WINDOW_DAYS = 7

/** DOM id of the workload-plane section, so other cards can link to it. */
export const WORKLOAD_PLANE_SECTION_ID = 'environment-workload-plane'

// ── URLs and query keys ─────────────────────────────────────────────────────

export function environmentWorkloadUrl(environmentId: number): string {
  return `/internal/environments/${environmentId}`
}

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
  const { data } = await client.get<
    EnvironmentWorkloadModeSettings,
    unknown,
    true
  >({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/workload-mode`,
    throwOnError: true,
  })
  return data
}

export async function setWorkloadMode(
  environmentId: number,
  workloadMode: EnvironmentWorkloadMode
): Promise<EnvironmentWorkloadModeSettings> {
  const { data } = await client.put<
    EnvironmentWorkloadModeSettings,
    unknown,
    true
  >({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/workload-mode`,
    body: { workload_mode: workloadMode },
    headers: JSON_HEADERS,
    throwOnError: true,
  })
  return data
}

export async function getWorkloadIsolation(
  environmentId: number
): Promise<EnvironmentWorkloadIsolationSettings> {
  const { data } = await client.get<
    EnvironmentWorkloadIsolationSettings,
    unknown,
    true
  >({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/workload-isolation`,
    throwOnError: true,
  })
  return data
}

export async function setWorkloadIsolation(
  environmentId: number,
  workloadIsolation: EnvironmentWorkloadIsolation
): Promise<EnvironmentWorkloadIsolationSettings> {
  const { data } = await client.put<
    EnvironmentWorkloadIsolationSettings,
    unknown,
    true
  >({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/workload-isolation`,
    body: { workload_isolation: workloadIsolation },
    headers: JSON_HEADERS,
    throwOnError: true,
  })
  return data
}

export async function explainPlacement(
  environmentId: number
): Promise<PlacementExplanation> {
  const { data } = await client.get<PlacementExplanation, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/placement/explain`,
    throwOnError: true,
  })
  return data
}

export async function getPinningRecommendation(
  environmentId: number,
  windowDays: number = DEFAULT_RECOMMENDATION_WINDOW_DAYS
): Promise<PinningRecommendation> {
  const { data } = await client.get<PinningRecommendation, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/placement/recommendation`,
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
  const { data } = await client.get<EnvironmentVolumeList, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/volumes`,
    query: { page, page_size: pageSize },
    throwOnError: true,
  })
  return data
}

export async function getEnvironmentVolumeStatus(
  environmentId: number
): Promise<EnvironmentVolumeStatus> {
  const { data } = await client.get<EnvironmentVolumeStatus, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/volumes/status`,
    throwOnError: true,
  })
  return data
}

export async function createEnvironmentVolume(
  environmentId: number,
  request: CreateEnvironmentVolume
): Promise<EnvironmentVolume> {
  const { data } = await client.post<EnvironmentVolume, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/volumes`,
    body: createVolumeBody(request),
    headers: JSON_HEADERS,
    throwOnError: true,
  })
  return data
}

export async function attachEnvironmentVolume(
  environmentId: number,
  volumeId: number,
  request: AttachEnvironmentVolume
): Promise<EnvironmentVolume> {
  const { data } = await client.put<EnvironmentVolume, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/volumes/${volumeId}/attachment`,
    body: {
      mount_path: request.mount_path.trim(),
      read_only: request.read_only ?? false,
    },
    headers: JSON_HEADERS,
    throwOnError: true,
  })
  return data
}

export async function detachEnvironmentVolume(
  environmentId: number,
  volumeId: number
): Promise<EnvironmentVolume> {
  const { data } = await client.delete<EnvironmentVolume, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `${environmentWorkloadUrl(environmentId)}/volumes/${volumeId}/attachment`,
    throwOnError: true,
  })
  return data
}

export async function deleteEnvironmentVolume(
  environmentId: number,
  volumeId: number
): Promise<DeletedEnvironmentVolume> {
  const { data } = await client.delete<DeletedEnvironmentVolume, unknown, true>(
    {
      security: [...BEARER_SECURITY],
      url: `${environmentWorkloadUrl(environmentId)}/volumes/${volumeId}`,
      throwOnError: true,
    }
  )
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
  if (volume.node_id === null) return 'not created on a node yet'
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
  if (volume.node_id === null) {
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
