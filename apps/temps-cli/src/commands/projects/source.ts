// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  changeProjectSource,
  getProject,
  getProjectBySlug,
  setAlternateSources,
} from '../../api/sdk.gen.js'
import type { ProjectResponse, SourceType } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, json, colors, success, info, keyValue } from '../../ui/output.js'

/**
 * Source types this command can switch a project *to*.
 *
 * `git` is absent on purpose: switching to Git also needs a repository and a
 * provider connection, so the API routes that through the Git settings
 * endpoint. `projects git` is the command for it.
 */
const SWITCHABLE: SourceType[] = [
  'docker_image',
  'static_files',
  'uploaded_source',
  'manual',
]

interface SourceOptions {
  project?: string
  type?: string
  allowAlternate?: boolean
  json?: boolean
}

async function fetchProject(reference: string): Promise<ProjectResponse> {
  const id = Number.parseInt(reference, 10)
  const result = Number.isNaN(id)
    ? await getProjectBySlug({ client, path: { slug: reference } })
    : await getProject({ client, path: { id } })
  if (result.error || !result.data) {
    throw new Error(
      getErrorMessage(result.error) ?? `Project "${reference}" not found`
    )
  }
  return result.data
}

function printProject(project: ProjectResponse, asJson?: boolean): void {
  if (asJson) {
    json(project)
    return
  }
  newline()
  keyValue('Project', project.slug)
  keyValue('Source', project.source_type)
  keyValue(
    'Alternate sources',
    project.allow_alternate_sources ? 'allowed' : 'not allowed'
  )
}

/**
 * Show or change how a project is deployed.
 *
 * Two independent things live here because they answer two different
 * questions: `--type` changes the project's *primary* source, while
 * `--allow-alternate` leaves that alone and only decides whether the project
 * will *also* accept an uploaded source archive from `drop`.
 */
export async function projectSourceAction(options: SourceOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const project = await withSpinner('Fetching project...', () =>
    fetchProject(resolved.slug)
  )

  // No mutating flag: report current state and how to change it.
  if (!options.type && options.allowAlternate === undefined) {
    printProject(project, options.json)
    if (!options.json) {
      newline()
      info(`Change the primary source:  --type <${SWITCHABLE.join('|')}>`)
      info('Also accept `drop` uploads:  --allow-alternate')
    }
    return
  }

  let updated = project

  if (options.type) {
    if (options.type === 'git') {
      throw new Error(
        'Switching to a Git source needs a repository and a provider connection.\n' +
          `Use: bunx @temps-sdk/cli projects git --project ${project.slug}`
      )
    }
    if (!SWITCHABLE.includes(options.type as SourceType)) {
      throw new Error(
        `Unknown source type "${options.type}". Expected one of: ${SWITCHABLE.join(', ')}`
      )
    }
    updated = await withSpinner(
      `Changing source to ${options.type}...`,
      async () => {
        const { data, error } = await changeProjectSource({
          client,
          path: { id: project.id },
          body: { source_type: options.type as SourceType },
        })
        if (error || !data) {
          throw new Error(
            getErrorMessage(error) ?? 'Failed to change deployment source'
          )
        }
        return data
      }
    )
    success(`Source changed to ${options.type}`)
  }

  if (options.allowAlternate !== undefined) {
    const allow = options.allowAlternate
    updated = await withSpinner(
      allow ? 'Allowing alternate sources...' : 'Restricting to configured source...',
      async () => {
        const { data, error } = await setAlternateSources({
          client,
          path: { id: project.id },
          body: { allow_alternate_sources: allow },
        })
        if (error || !data) {
          throw new Error(
            getErrorMessage(error) ?? 'Failed to update alternate sources'
          )
        }
        return data
      }
    )
    if (allow) {
      success(
        `${updated.slug} still deploys from ${updated.source_type} and now also accepts uploaded source`
      )
      info(`Try: bunx @temps-sdk/cli drop ./ --project ${updated.slug}`)
    } else {
      success(`${updated.slug} now only deploys from ${updated.source_type}`)
    }
  }

  printProject(updated, options.json)
}
