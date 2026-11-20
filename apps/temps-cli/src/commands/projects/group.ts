// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `projects group`: which project (group of services, as the console calls
 * it) a service belongs to.
 */

import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getProjectBySlug, getProjectGroupMembership } from '../../api/sdk.gen.js'
import type { ProjectGroupResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { colors, info, json, keyValue, newline } from '../../ui/output.js'
import { describeMembership } from '../project-groups/index.js'

const CLI = 'bunx @temps-sdk/cli'

interface GroupOptions {
  project?: string
  json?: boolean
}

/** The project (group) a service is in; null when it is standalone. */
export async function loadMembership(projectId: number): Promise<ProjectGroupResponse | null> {
  const { data, error } = await getProjectGroupMembership({ client, path: { project_id: projectId } })
  if (error || !data) throw new Error(getErrorMessage(error) || 'Could not load the service\'s project')
  return data.group ?? null
}

export async function projectGroupAction(options: GroupOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag' && !options.json) {
    info(`Using service ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const { service, group } = await withSpinner('Fetching project...', async () => {
    const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
    if (error || !data) throw new Error(getErrorMessage(error) || `Service "${resolved.slug}" not found`)
    return { service: data, group: await loadMembership(data.id) }
  })

  if (options.json) {
    json({ service: service.slug, group })
    return
  }
  newline()
  keyValue('Service', service.slug)
  keyValue('Project', describeMembership(group))
  if (group) {
    const others = group.projects.filter((p) => p.id !== service.id).map((p) => p.slug)
    keyValue('Other services', others.length ? others.join(', ') : '(none)')
    newline()
    info(`Shared variables it receives: ${CLI} project-groups vars inherited ${service.slug}`)
  } else {
    newline()
    info(`Add it to a project: ${CLI} project-groups add <project> ${service.slug}`)
  }
}
