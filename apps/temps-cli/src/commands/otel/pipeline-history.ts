// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getPipelineHistory } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, json as jsonOut, colors } from '../../ui/output.js'
import { parsePeriod } from '../analytics/period.js'
import type { PipelineSeries } from '../../api/types.gen.js'

interface PipelineHistoryOptions {
  period?: string
  startTime?: string
  endTime?: string
  json?: boolean
}

/** Preset windows the endpoint accepts directly, bypassing client-side parsing. */
const SERVER_PRESETS = new Set(['1h', '6h', '24h', '7d'])

/** Per-series summary — a terminal can't usefully show every bucket. */
interface SeriesSummary {
  name: string
  buckets: number
  peak: number
  mean: number
  last: number
}

function summarize(series: PipelineSeries): SeriesSummary {
  const values = series.points.map((p) => p.value)
  if (values.length === 0) {
    return { name: series.name, buckets: 0, peak: 0, mean: 0, last: 0 }
  }
  const sum = values.reduce((a, b) => a + b, 0)
  return {
    name: series.name,
    buckets: values.length,
    peak: Math.max(...values),
    mean: sum / values.length,
    last: values[values.length - 1] ?? 0,
  }
}

/** Trim trailing zeros so a whole number doesn't render as "12.00". */
function formatValue(v: number): string {
  return Number.isInteger(v) ? String(v) : v.toFixed(2)
}

export async function otelPipelineHistory(
  options: PipelineHistoryOptions
): Promise<void> {
  await requireAuth()
  await setupClient()

  // Explicit --start-time/--end-time win over --period, matching
  // "temps metrics query". A preset the server already understands is passed
  // straight through as `range`; anything else (e.g. "today", "3d") is
  // resolved client-side into explicit bounds by the shared period parser.
  const query: {
    range?: string
    start_time?: string
    end_time?: string
  } = {}
  let periodLabel: string

  if (options.startTime || options.endTime) {
    if (!options.startTime || !options.endTime) {
      throw new Error(
        '--start-time and --end-time must be provided together (or use --period).'
      )
    }
    query.start_time = options.startTime
    query.end_time = options.endTime
    periodLabel = 'custom range'
  } else {
    const period = options.period ?? '24h'
    if (SERVER_PRESETS.has(period)) {
      query.range = period
      periodLabel = period
    } else {
      const parsed = parsePeriod(period)
      query.start_time = parsed.startDate
      query.end_time = parsed.endDate
      periodLabel = parsed.label
    }
  }

  const data = await withSpinner('Fetching pipeline history...', async () => {
    const { data, error } = await getPipelineHistory({ client, query })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    jsonOut(data)
    return
  }

  const series = data?.series ?? []
  const interval = data?.sample_interval_seconds ?? 60

  header(`OTel pipeline history (${periodLabel})`)

  if (series.length === 0 || series.every((s) => s.points.length === 0)) {
    newline()
    console.log(colors.muted('  No samples recorded in this window.'))
    console.log(
      colors.muted(
        `  The pipeline sampler writes every ${interval}s, so a server started recently has no history yet.`
      )
    )
    newline()
    return
  }

  const summaries = series.map(summarize)

  const columns: TableColumn<SeriesSummary>[] = [
    { header: 'Metric', accessor: (s) => s.name, align: 'left' },
    { header: 'Peak', accessor: (s) => formatValue(s.peak), align: 'right' },
    { header: 'Avg', accessor: (s) => formatValue(s.mean), align: 'right' },
    { header: 'Last', accessor: (s) => formatValue(s.last), align: 'right' },
    { header: 'Buckets', accessor: (s) => s.buckets, align: 'right' },
  ]

  printTable(summaries, columns, { style: 'minimal' })
  newline()
  console.log(
    colors.muted(
      `  Window ${data?.start_time} to ${data?.end_time}, ${data?.step_seconds}s buckets.`
    )
  )
  console.log(
    colors.muted(
      `  Values are counts per ${interval}s sample, not window totals. Use --json for every bucket.`
    )
  )
  newline()
}
