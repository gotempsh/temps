// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Resolve the `--project` of an analytics command, which takes a slug or a
 * numeric project ID, to both.
 */

import { client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProject, getProjectBySlug } from '../../api/sdk.gen.js'
import { colors, info } from '../../ui/output.js'

/** The project ID a `--project` value names, or `undefined` for a slug. */
export function projectIdFromRef(ref: string): number | undefined {
  if (!/^[1-9][0-9]{0,9}$/.test(ref)) return undefined
  const id = Number(ref)
  return id <= 2_147_483_647 ? id : undefined
}

export async function resolveAnalyticsProject(
  flagValue?: string
): Promise<{ id: number; slug: string }> {
  const resolved = await requireProjectSlug(flagValue)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const id = projectIdFromRef(resolved.slug)
  if (id !== undefined) {
    const { data, error } = await getProject({ client, path: { id } })
    if (error || !data) {
      throw new Error(getErrorMessage(error) || `Project ${id} not found`)
    }
    return { id: data.id, slug: data.slug }
  }
  const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
  if (error || !data) {
    throw new Error(getErrorMessage(error) || `Project "${resolved.slug}" not found`)
  }
  return { id: data.id, slug: resolved.slug }
}
