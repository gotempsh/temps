// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getIdenticalBuilds, getProject, getProjectBySlug, updateIdenticalBuilds } from '../../api/sdk.gen.js'
import type { IdenticalBuildMode, IdenticalBuildScope, IdenticalBuildsResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { colors, info, json, keyValue, newline, success } from '../../ui/output.js'

const CLI = 'bunx @temps-sdk/cli'

const MODES: readonly IdenticalBuildMode[] = ['parallel', 'queue', 'share']
const SCOPES: readonly IdenticalBuildScope[] = ['source', 'fingerprint']

interface IdenticalBuildsOptions {
  project?: string
  mode?: string
  scope?: string
  json?: boolean
}

/** `value` as a mode, or an error listing the accepted ones. */
export function parseMode(value: string): IdenticalBuildMode {
  const mode = value.trim().toLowerCase()
  if (!(MODES as readonly string[]).includes(mode)) {
    throw new Error(`"${value}" is not an identical build mode; use one of ${MODES.join(', ')}`)
  }
  return mode as IdenticalBuildMode
}

/** `value` as a scope, or an error listing the accepted ones. */
export function parseScope(value: string): IdenticalBuildScope {
  const scope = value.trim().toLowerCase()
  if (!(SCOPES as readonly string[]).includes(scope)) {
    throw new Error(`"${value}" is not an identical build scope; use one of ${SCOPES.join(', ')}`)
  }
  return scope as IdenticalBuildScope
}

function minutes(seconds: number): string {
  return `${Math.round(seconds / 60)} minutes`
}

function describe(data: IdenticalBuildsResponse, slug: string): void {
  const mode = data.modes.find((option) => option.id === data.mode)
  const scope = data.scopes.find((option) => option.id === data.scope)
  newline()
  keyValue('Project', slug)
  keyValue('Mode', `${data.mode}${mode ? ` (${mode.label})` : ''}`)
  keyValue('Grouped by', `${data.scope}${scope ? ` (${scope.label})` : ''}`)
  keyValue(
    'Last 30 days',
    `${data.activity.reused} reused, ${data.activity.built} built, ${data.activity.waiting} waiting now`
  )
  if (mode) {
    newline()
    info(mode.description)
    info(`Example: ${mode.example}`)
  }
  if (data.mode === 'share') {
    info(`A shared image is offered for ${minutes(data.reuse_window_seconds)} after it is built.`)
  }
  if (data.mode !== 'parallel') {
    info(`A build waits at most ${minutes(data.max_wait_seconds)}.`)
  }
}

/**
 * Show or change what a project's builds do when an identical build (same
 * repository, commit, build path and build-time values) is already running.
 */
export async function projectIdenticalBuildsAction(options: IdenticalBuildsOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const mode = options.mode === undefined ? undefined : parseMode(options.mode)
  const scope = options.scope === undefined ? undefined : parseScope(options.scope)
  const resolved = await requireProjectSlug(options.project)
  const project = await withSpinner('Fetching project...', async () => {
    // `--project` takes a slug or an ID: an all-digit value given there is
    // the ID. A linked, environment or default project is always a slug.
    const { data, error } = resolved.source === 'flag' && /^\d+$/.test(resolved.slug)
      ? await getProject({ client, path: { id: Number(resolved.slug) } })
      : await getProjectBySlug({ client, path: { slug: resolved.slug } })
    if (error || !data) throw new Error(getErrorMessage(error) ?? `Project "${resolved.slug}" not found`)
    return data
  })
  const current = await withSpinner('Reading identical build settings...', async () => {
    const { data, error } = await getIdenticalBuilds({ client, path: { id: project.id } })
    if (error || !data) throw new Error(getErrorMessage(error) ?? 'Failed to read identical build settings')
    return data
  })

  if (mode === undefined && scope === undefined) {
    if (options.json) {
      json({ project: project.slug, ...current })
      return
    }
    describe(current, project.slug)
    newline()
    info(`Build once and share: ${CLI} projects identical-builds --mode share --project ${project.slug}`)
    return
  }

  const updated = await withSpinner('Saving identical build settings...', async () => {
    const { data, error } = await updateIdenticalBuilds({
      client,
      path: { id: project.id },
      body: { mode: mode ?? current.mode, scope: scope ?? current.scope },
    })
    if (error || !data) throw new Error(getErrorMessage(error) ?? 'Failed to save identical build settings')
    return data
  })
  if (options.json) {
    json({ project: project.slug, ...updated })
    return
  }
  success(`${colors.bold(project.slug)} identical builds: ${updated.mode}, grouped by ${updated.scope}`)
  info('Applies from the next build.')
}
