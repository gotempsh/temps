// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import {
  adminAttachEnvironmentVolume,
  adminCreateEnvironmentVolume,
  adminDeleteEnvironmentVolume,
  adminDetachEnvironmentVolume,
  adminEnvironmentVolumeStatus,
  adminListEnvironmentVolumes,
} from '@/api/client'
import type {
  AttachEnvironmentVolumeRequest,
  CreateEnvironmentVolumeRequest,
  DeletedEnvironmentVolumeResponse,
  EnvironmentVolumeListResponse,
  EnvironmentVolumeResponse,
  EnvironmentVolumeStatusResponse,
} from '@/api/client'
import { httpStatusOf } from '@/lib/http-error-status'

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

/** The control plane's node id, where a volume lives by default. */
export const CONTROL_PLANE_NODE_ID = 0

// ── Query keys ──────────────────────────────────────────────────────────────

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

/** What the next deployment needs before it mounts attached volumes. */
export const VOLUMES_REQUIREMENT =
  'Volumes are mounted only when the environment runs one replica and the data lives on the control plane.'

/**
 * Why the next deployment would not mount volumes, in the user's terms. The
 * workload mode is an internal migration setting the console does not show,
 * so the server's reason, which names it, is not repeated here.
 */
export function volumeBlockerText(status: EnvironmentVolumeStatus): string {
  if (status.workload_mode !== 'enforce') {
    return "This environment's deployments do not mount volumes yet."
  }
  return status.reason ?? VOLUMES_REQUIREMENT
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
