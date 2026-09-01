// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  listFunnels,
  getFunnelMetrics,
} from '../../api/sdk.gen.js'
import type { StepConversionResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, json as jsonOut, colors, info, keyValue } from '../../ui/output.js'
import { parsePeriod } from './period.js'

interface FunnelsOptions {
  project?: string
  period?: string
  json?: boolean
}

function formatRate(rate: number): string {
  return `${rate.toFixed(1)}%`
}

export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds.toFixed(1)}s`
  if (seconds < 3600) return `${(seconds / 60).toFixed(1)}m`
  return `${(seconds / 3600).toFixed(1)}h`
}

export function renderConversionBar(rate: number, width = 20): string {
  const filled = Math.round((rate / 100) * width)
  const empty = width - filled
  const bar = '█'.repeat(filled) + '░'.repeat(empty)

  if (rate >= 50) return chalk.green(bar)
  if (rate >= 20) return chalk.yellow(bar)
  return chalk.red(bar)
}

export async function funnelsOverview(options: FunnelsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '7d'
  const { startDate, endDate, label } = parsePeriod(period)

  const resolved = await requireProjectSlug(options.project)

  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  // Resolve project ID from slug
  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })

  if (projectError || !projectData) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }

  const projectId = projectData.id

  // Fetch funnels list
  const funnelsData = await withSpinner('Fetching funnels...', async () => {
    const { data, error } = await listFunnels({
      client,
      path: { project_id: projectId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data ?? []
  })

  if (funnelsData.length === 0) {
    newline()
    info('No funnels configured for this project.')
    info(`Create one with: temps funnels create --project-id ${projectId}`)
    newline()
    return
  }

  // Fetch metrics for all funnels concurrently
  const metricsResults = await withSpinner(
    `Fetching metrics for ${funnelsData.length} funnel(s)...`,
    async () => {
      return Promise.all(
        funnelsData.map(async (funnel) => {
          try {
            const { data, error } = await getFunnelMetrics({
              client,
              path: { project_id: projectId, funnel_id: funnel.id },
              query: { start_date: startDate, end_date: endDate },
            })
            if (error || !data) return { funnel, metrics: null }
            return { funnel, metrics: data }
          } catch {
            return { funnel, metrics: null }
          }
        })
      )
    }
  )

  if (options.json) {
    jsonOut(
      metricsResults.map(({ funnel, metrics }) => ({
        funnel,
        metrics,
        period: { start: startDate, end: endDate, label },
      }))
    )
    return
  }

  // Pretty output
  const line = chalk.cyan('━'.repeat(64))

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white('Funnels:')} ${chalk.bold.cyan(resolved.slug)} ${chalk.gray(`(${label})`)}`
  )
  console.log(line)

  for (const { funnel, metrics } of metricsResults) {
    newline()
    console.log(
      `  ${chalk.bold.white(funnel.name)} ${chalk.gray(`#${funnel.id}`)}` +
        (funnel.is_active ? '' : chalk.gray(' (inactive)'))
    )

    if (!metrics) {
      console.log(chalk.gray('    No metrics available'))
      continue
    }

    keyValue('  Total entries', metrics.total_entries)
    keyValue('  Overall conversion', formatRate(metrics.overall_conversion_rate))
    keyValue('  Avg completion time', formatDuration(metrics.average_completion_time_seconds))

    if (metrics.step_conversions && metrics.step_conversions.length > 0) {
      newline()

      const columns: TableColumn<StepConversionResponse>[] = [
        { header: '#', accessor: (s) => s.step_order.toString(), width: 4 },
        {
          header: 'Step',
          key: 'step_name',
          color: (v) => colors.bold(v),
        },
        {
          header: 'Completions',
          accessor: (s) => s.completions.toLocaleString('en-US'),
        },
        {
          header: 'Conversion',
          accessor: (s) => `${renderConversionBar(s.conversion_rate)} ${formatRate(s.conversion_rate)}`,
        },
        {
          header: 'Drop-off',
          accessor: (s) => formatRate(s.drop_off_rate),
          color: (v) => {
            const rate = parseFloat(v)
            return rate >= 50 ? colors.error(v) : rate >= 20 ? colors.warning(v) : colors.success(v)
          },
        },
        {
          header: 'Avg Time',
          accessor: (s) => formatDuration(s.average_time_to_complete_seconds),
        },
      ]

      printTable(metrics.step_conversions, columns, { style: 'compact' })
    }
  }

  newline()
  console.log(line)
  newline()
}
