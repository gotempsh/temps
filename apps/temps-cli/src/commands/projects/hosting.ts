// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getProjectBySlug, setProjectHosting } from '../../api/sdk.gen.js'
import type { Hosting } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { colors, info, json, keyValue, newline, success } from '../../ui/output.js'

const CLI = 'bunx @temps-sdk/cli'

interface HostingOptions {
  project?: string
  json?: boolean
}

/** `hosted`/`external`, with `elsewhere` accepted for `external`. */
export function parseHosting(value: string): Hosting {
  const normalized = value.trim().toLowerCase()
  if (normalized === 'hosted') return 'hosted'
  if (normalized === 'external' || normalized === 'elsewhere') return 'external'
  throw new Error(`Unknown hosting "${value}". Expected hosted or external.`)
}

/**
 * Show or change whether Temps runs a project's app (`hosted`) or the app
 * runs elsewhere and only sends Temps its telemetry (`external`).
 */
export async function projectHostingAction(
  value: string | undefined,
  options: HostingOptions
): Promise<void> {
  await requireAuth()
  await setupClient()
  const resolved = await requireProjectSlug(options.project)
  const project = await withSpinner('Fetching project...', async () => {
    const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
    if (error || !data) throw new Error(getErrorMessage(error) ?? `Project "${resolved.slug}" not found`)
    return data
  })

  if (value === undefined) {
    if (options.json) {
      json({ project: project.slug, hosting: project.hosting })
      return
    }
    newline()
    keyValue('Project', project.slug)
    keyValue('Hosting', project.hosting === 'external' ? 'external (runs outside Temps)' : 'hosted (Temps runs it)')
    newline()
    info(`Change it: ${CLI} projects hosting <hosted|external> --project ${project.slug}`)
    return
  }

  const hosting = parseHosting(value)
  const updated = await withSpinner('Updating hosting...', async () => {
    const { data, error } = await setProjectHosting({
      client,
      path: { id: project.id },
      body: { hosting },
    })
    if (error || !data) throw new Error(getErrorMessage(error) ?? 'Failed to update hosting')
    return data
  })
  if (options.json) {
    json({ project: updated.slug, hosting: updated.hosting })
    return
  }
  if (updated.hosting === 'external') {
    success(`${updated.slug} runs outside Temps; Temps will not deploy it`)
    info(`Send its analytics with an ingest key: ${CLI} analytics keys create --project ${updated.slug}`)
  } else {
    success(`Temps can deploy ${colors.bold(updated.slug)} again`)
  }
}
