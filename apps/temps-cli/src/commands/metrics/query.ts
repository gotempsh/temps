// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProjectBySlug, queryMetrics } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, json as jsonOut, colors, info, warning } from '../../ui/output.js'
import { parsePeriod } from '../analytics/period.js'
import type { MetricBucket } from '../../api/types.gen.js'

interface QueryOptions {
  project?: string
  metricName?: string
  serviceName?: string
  environment?: string
  period?: string
  startTime?: string
  endTime?: string
  bucketInterval?: string
  aggregation?: string
  metricType?: string
  labelFilters?: string
  groupBy?: string
  limit?: string
  json?: boolean
}

function seriesLabel(bucket: MetricBucket): string {
  if (!bucket.series_key || bucket.series_key.length === 0) return ''
  return bucket.series_key.map(([k, v]) => `${k}=${v}`).join(',')
}

export async function metricsQuery(options: QueryOptions): Promise<void> {
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
  const projectId = projectData.id

  // Explicit --start-time/--end-time win over --period; --period defaults to 24h.
  let startTime = options.startTime
  let endTime = options.endTime
  let periodLabel = 'custom range'
  if (!startTime && !endTime) {
    const period = options.period ?? '24h'
    const parsed = parsePeriod(period)
    startTime = parsed.startDate
    endTime = parsed.endDate
    periodLabel = parsed.label
  }

  if (!options.metricName) {
    warning(
      'No --metric-name given — this returns every metric bucketed together. Run "temps metrics names" to discover available names.'
    )
  }

  const limit = options.limit ? parseInt(options.limit, 10) : 500

  const data = await withSpinner('Querying metrics...', async () => {
    const { data, error } = await queryMetrics({
      client,
      query: {
        project_id: projectId,
        metric_name: options.metricName,
        service_name: options.serviceName,
        environment: options.environment,
        start_time: startTime,
        end_time: endTime,
        bucket_interval: options.bucketInterval,
        aggregation: options.aggregation,
        metric_type: options.metricType,
        label_filters: options.labelFilters,
        group_by: options.groupBy,
        limit,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const buckets = data?.data ?? []

  if (options.json) {
    jsonOut({
      project: resolved.slug,
      metric_name: options.metricName,
      start_time: startTime,
      end_time: endTime,
      aggregation: options.aggregation ?? 'avg',
      count: data?.count ?? 0,
      data: buckets,
    })
    return
  }

  header(
    `Metrics${options.metricName ? `: ${options.metricName}` : ''} — ${resolved.slug} (${periodLabel})`
  )

  if (buckets.length === 0) {
    newline()
    console.log(colors.muted('  No metric data in this window.'))
    console.log(
      colors.muted(
        '  Check the metric name with "temps metrics names -p ' + resolved.slug + '", or widen --period.'
      )
    )
    newline()
    return
  }

  const hasSeries = buckets.some((b) => b.series_key && b.series_key.length > 0)

  const columns: TableColumn<MetricBucket>[] = [
    { header: 'Bucket', accessor: (b) => new Date(b.bucket).toLocaleString(), align: 'left' },
    ...(hasSeries ? [{ header: 'Series', accessor: seriesLabel, align: 'left' as const }] : []),
    { header: 'Avg', accessor: (b) => b.avg_value.toFixed(4), align: 'right' },
    { header: 'Min', accessor: (b) => b.min_value.toFixed(4), align: 'right' },
    { header: 'Max', accessor: (b) => b.max_value.toFixed(4), align: 'right' },
    { header: 'Count', accessor: (b) => b.count, align: 'right' },
  ]

  printTable(buckets, columns, { style: 'minimal' })
  newline()
  console.log(colors.muted(`  ${buckets.length} bucket(s)`))
  newline()
}
