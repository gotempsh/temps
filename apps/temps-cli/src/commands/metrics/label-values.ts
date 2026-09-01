// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProjectBySlug, listMetricLabelValues } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { header, list, newline, json as jsonOut, colors, info } from '../../ui/output.js'

interface LabelValuesOptions {
  project?: string
  metricName: string
  labelKey: string
  startTime?: string
  endTime?: string
  json?: boolean
}

export async function metricsLabelValues(options: LabelValuesOptions): Promise<void> {
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

  const values = await withSpinner('Fetching label values...', async () => {
    const { data, error } = await listMetricLabelValues({
      client,
      query: {
        project_id: projectData.id,
        metric_name: options.metricName,
        label_key: options.labelKey,
        start_time: options.startTime,
        end_time: options.endTime,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data?.values ?? []
  })

  if (options.json) {
    jsonOut({
      project: resolved.slug,
      metric_name: options.metricName,
      label_key: options.labelKey,
      values,
    })
    return
  }

  header(`Label Values — ${options.labelKey} on ${options.metricName} (${resolved.slug})`)
  newline()
  if (values.length === 0) {
    console.log(colors.muted('  No values observed for this label in the window.'))
    newline()
    return
  }
  list(values)
  newline()
}
