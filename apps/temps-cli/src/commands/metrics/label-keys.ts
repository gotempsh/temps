// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProjectBySlug, listMetricLabelKeys } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { header, list, newline, json as jsonOut, colors, info } from '../../ui/output.js'

interface LabelKeysOptions {
  project?: string
  metricName: string
  startTime?: string
  endTime?: string
  json?: boolean
}

export async function metricsLabelKeys(options: LabelKeysOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })
  if (projectError || !projectData) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }

  const keys = await withSpinner('Fetching label keys...', async () => {
    const { data, error } = await listMetricLabelKeys({
      client,
      query: {
        project_id: projectData.id,
        metric_name: options.metricName,
        start_time: options.startTime,
        end_time: options.endTime,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data?.keys ?? []
  })

  if (options.json) {
    jsonOut({ project: resolved.slug, metric_name: options.metricName, keys })
    return
  }

  header(`Label Keys — ${options.metricName} (${resolved.slug})`)
  newline()
  if (keys.length === 0) {
    console.log(colors.muted('  No labels observed on this metric in the window.'))
    newline()
    return
  }
  list(keys)
  newline()
}
