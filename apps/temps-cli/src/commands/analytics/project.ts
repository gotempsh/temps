// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Resolve the `--project` of an analytics command, which takes a slug or a
 * numeric project ID, to both.
 *
 * Slugs may be numeric, so a number is a slug first: an explicit
 * `--project 42` means project ID 42 only when no project has the slug `42`,
 * and a slug saved by `temps link` or the environment is never read as an ID.
 */

import { client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug, type ProjectSource } from '../../config/resolve-project.js'
import { getProject, getProjectBySlug } from '../../api/sdk.gen.js'
import { colors, info } from '../../ui/output.js'

/** The project ID a `--project` value could name, or `undefined` if it cannot be one. */
export function projectIdFromRef(ref: string): number | undefined {
  if (!/^[1-9][0-9]{0,9}$/.test(ref)) return undefined
  const id = Number(ref)
  return id <= 2_147_483_647 ? id : undefined
}

/**
 * Whether to look `ref` up as a project ID after no project had it as its
 * slug: only for a value typed as `--project`.
 */
export function mayBeProjectId(ref: string, source: ProjectSource): boolean {
  return source === 'flag' && projectIdFromRef(ref) !== undefined
}

export async function resolveAnalyticsProject(
  flagValue?: string
): Promise<{ id: number; slug: string }> {
  const resolved = await requireProjectSlug(flagValue)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const bySlug = await getProjectBySlug({ client, path: { slug: resolved.slug } })
  if (bySlug.data) return { id: bySlug.data.id, slug: resolved.slug }

  const id = projectIdFromRef(resolved.slug)
  if (
    bySlug.response?.status === 404 &&
    id !== undefined &&
    mayBeProjectId(resolved.slug, resolved.source)
  ) {
    const byId = await getProject({ client, path: { id } })
    if (byId.data) return { id: byId.data.id, slug: byId.data.slug }
    throw new Error(
      getErrorMessage(byId.error) || `No project has the slug "${resolved.slug}" or the ID ${id}`
    )
  }
  throw new Error(getErrorMessage(bySlug.error) || `Project "${resolved.slug}" not found`)
}
