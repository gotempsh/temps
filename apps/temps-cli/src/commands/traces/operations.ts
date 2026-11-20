// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getDeploymentRegressions,
  getOperationBreakdown,
  getOperationTrend,
  getPageBackend,
} from '../../api/sdk.gen.js'
import type {
  AttributeSliceResponse,
  BackendRequestResponse,
  BreakdownGroupResponse,
  OperationChangeResponse,
  OperationTrendPointResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, info, warning, keyValue } from '../../ui/output.js'

/** The widest trend the server keeps, in days. */
export const MAX_TREND_DAYS = 30

/** Parse `--days`: a whole number from 1 to {@link MAX_TREND_DAYS}. */
export function parseTrendDays(value: string): { ok: true; days: number } | { ok: false; error: string } {
  const days = Number(value)
  if (!Number.isInteger(days) || days < 1 || days > MAX_TREND_DAYS) {
    return { ok: false, error: `Invalid --days: ${value}. Use a whole number from 1 to ${MAX_TREND_DAYS}` }
  }
  return { ok: true, days }
}

export interface DaySummary {
  day: string
  calls: number
  errors: number
  /** The median of the day's hourly p95s. */
  p95Ms: number
  hours: number
}

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b)
  const mid = Math.floor(sorted.length / 2)
  return sorted.length % 2 ? sorted[mid]! : (sorted[mid - 1]! + sorted[mid]!) / 2
}

/** One row per UTC day, oldest first, from hourly points. */
export function summarizeByDay(points: OperationTrendPointResponse[]): DaySummary[] {
  const days = new Map<string, OperationTrendPointResponse[]>()
  for (const point of points) {
    const day = point.bucket_start.slice(0, 10)
    days.set(day, [...(days.get(day) ?? []), point])
  }
  return [...days.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([day, hours]) => ({
      day,
      calls: hours.reduce((sum, h) => sum + h.count, 0),
      errors: hours.reduce((sum, h) => sum + h.error_count, 0),
      p95Ms: median(hours.map((h) => h.p95_ms)),
      hours: hours.length,
    }))
}

/** "+40%" or "-30%". */
export function formatChange(ratio: number | null | undefined): string {
  if (ratio == null || !Number.isFinite(ratio)) return '-'
  const pct = Math.round(ratio * 100)
  return pct > 0 ? `+${pct}%` : `${pct}%`
}

/** A share from 0 to 1 as a whole percent; tiny shares stay visible. */
export function formatSharePercent(share: number | null | undefined): string {
  if (share == null || !Number.isFinite(share)) return '-'
  const pct = share * 100
  if (pct > 0 && pct < 1) return '<1%'
  return `${Math.round(pct)}%`
}

export const BREAKDOWN_SAMPLES = ['recent', 'slow']

function formatMs(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return '-'
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  return `${ms.toFixed(1)}ms`
}

interface TrendOptions {
  projectId?: string
  service?: string
  operation?: string
  days: string
  json?: boolean
}

interface BreakdownOptions {
  projectId?: string
  service?: string
  operation?: string
  sample: string
  attribute?: string
  json?: boolean
}

interface PageOptions {
  projectId?: string
  path?: string
  json?: boolean
}

interface RegressionsOptions {
  projectId?: string
  deploymentId?: string
  json?: boolean
}

export function registerOperationTrendCommands(traces: Command): void {
  traces
    .command('trend')
    .description('Show how one operation\'s latency moved, day by day, with deployments')
    .option('--project-id <id>', 'Project ID')
    .option('--service <name>', 'Service of the operation')
    .option('--operation <name>', 'The operation (span name)')
    .option('--days <n>', `Days to show (max ${MAX_TREND_DAYS})`, '14')
    .option('--json', 'Output the hourly points in JSON format')
    .addHelpText(
      'after',
      `
Examples:
  $ temps traces trend --project-id 4 --service checkout-api --operation "GET /cart"
  $ temps traces trend --project-id 4 --service checkout-api --operation "GET /cart" --days 3 --json
`,
    )
    .action(trendAction)

  traces
    .command('regressions')
    .description('List the operations that got slower or faster after a deployment')
    .option('--project-id <id>', 'Project ID')
    .option('--deployment-id <id>', 'Deployment to compare across')
    .option('--json', 'Output in JSON format')
    .addHelpText(
      'after',
      `
Each operation's p95 in the day after the deployment went live is compared with
the week before it. An operation needs 8 hours of history before and 2 complete
hours after to be judged.

Examples:
  $ temps traces regressions --project-id 4 --deployment-id 812
`,
    )
    .action(regressionsAction)

  traces
    .command('breakdown')
    .description('Show where the time inside one operation goes: queries, downstream calls, its own code')
    .option('--project-id <id>', 'Project ID')
    .option('--service <name>', 'Service of the operation')
    .option('--operation <name>', 'The operation (span name)')
    .option('--sample <kind>', `Calls to sample (${BREAKDOWN_SAMPLES.join(', ')})`, 'recent')
    .option('--attribute <key>', 'Also split the calls by this attribute, e.g. http.route')
    .option('--json', 'Output in JSON format')
    .addHelpText(
      'after',
      `
Reads at most 20 traces of the last 24 hours that contain the operation and
charges every span under it only the time its own children do not cover.

Examples:
  $ temps traces breakdown --project-id 4 --service checkout-api --operation "GET /cart"
  $ temps traces breakdown --project-id 4 --service checkout-api --operation "GET /cart" --sample slow
  $ temps traces breakdown --project-id 4 --service checkout-api --operation "GET /cart" --attribute tenant.id
`,
    )
    .action(breakdownAction)

  traces
    .command('page')
    .description("Compare a page's web vitals with the backend requests that served it")
    .option('--project-id <id>', 'Project ID')
    .option('--path <path>', 'The page path, e.g. /checkout')
    .option('--json', 'Output in JSON format')
    .addHelpText(
      'after',
      `
Page loads carry no trace id, so backend requests are matched by path (the
url.path or http.target attribute of server spans) and time, over the last
24 hours.

Examples:
  $ temps traces page --project-id 4 --path /checkout
`,
    )
    .action(pageAction)
}

async function breakdownAction(options: BreakdownOptions): Promise<void> {
  if (!options.projectId || !options.service || !options.operation) {
    warning('Provide --project-id, --service and --operation')
    return
  }
  if (!BREAKDOWN_SAMPLES.includes(options.sample)) {
    warning(`Invalid --sample: ${options.sample}. Available: ${BREAKDOWN_SAMPLES.join(', ')}`)
    return
  }
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Breaking the operation down...', async () => {
    const { data, error } = await getOperationBreakdown({
      client,
      query: {
        project_id: parseInt(options.projectId!, 10),
        service_name: options.service!,
        span_name: options.operation!,
        sample: options.sample,
        ...(options.attribute && { attribute: options.attribute }),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  if (!result) return
  newline()
  header(`${icons.info} Where the time of ${options.operation} goes (${result.sample} calls)`)
  if (result.instances === 0) {
    info('No call of this operation was traced in the last 24 hours.')
    newline()
    return
  }
  keyValue(
    'Sample',
    `${result.instances} calls in ${result.traces} traces (at most ${result.max_traces})` +
      (result.slow_threshold_ms != null ? `, each ${formatMs(result.slow_threshold_ms)} or slower` : ''),
  )
  keyValue('Own time', formatSharePercent(result.self_share))
  const columns: TableColumn<BreakdownGroupResponse>[] = [
    { header: 'Kind', key: 'category' },
    { header: 'Call', key: 'span_name', color: (v) => colors.bold(v) },
    { header: 'Service', key: 'service_name' },
    { header: 'Per call', accessor: (r) => formatMs(r.avg_ms), align: 'right' },
    { header: 'Calls each', accessor: (r) => r.calls_per_instance.toFixed(1), align: 'right' },
    { header: 'Share', accessor: (r) => formatSharePercent(r.share), align: 'right' },
  ]
  printTable(result.groups, columns, { style: 'minimal' })
  if (result.attribute && result.by_attribute.length > 0) {
    newline()
    header(`By ${result.attribute}`)
    const sliceColumns: TableColumn<AttributeSliceResponse>[] = [
      { header: 'Value', accessor: (r) => r.value ?? '(not set)' },
      { header: 'Calls', accessor: (r) => r.calls.toString(), align: 'right' },
      { header: 'Avg', accessor: (r) => formatMs(r.avg_ms), align: 'right' },
      { header: 'Max', accessor: (r) => formatMs(r.max_ms), align: 'right' },
      { header: 'Share', accessor: (r) => formatSharePercent(r.share), align: 'right' },
    ]
    printTable(result.by_attribute, sliceColumns, { style: 'minimal' })
  }
  info('Parallel calls each keep their own time, so shares can add up to more than 100%.')
  newline()
}

async function pageAction(options: PageOptions): Promise<void> {
  if (!options.projectId || !options.path) {
    warning('Provide --project-id and --path')
    return
  }
  if (!options.path.startsWith('/')) {
    warning(`Invalid --path: ${options.path}. A page path starts with /`)
    return
  }
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Matching backend requests to the page...', async () => {
    const { data, error } = await getPageBackend({
      client,
      query: { project_id: parseInt(options.projectId!, 10), path: options.path! },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  if (!result) return
  newline()
  header(`${icons.info} ${result.path}, last 24 hours`)
  keyValue('Page loads', result.vitals.page_loads.toLocaleString())
  keyValue('TTFB p75', formatMs(result.vitals.ttfb_p75_ms))
  keyValue('LCP p75', formatMs(result.vitals.lcp_p75_ms))
  if (!result.configured) {
    info(result.reason ?? 'No backend request for this path was traced.')
    newline()
    return
  }
  keyValue(
    'Backend',
    `${result.requests} requests, p50 ${formatMs(result.p50_ms)}, p75 ${formatMs(result.p75_ms)}, p95 ${formatMs(result.p95_ms)}` +
      (result.errors > 0 ? `, ${result.errors} failed` : ''),
  )
  if (result.backend_share_of_ttfb != null) {
    keyValue('Backend share of TTFB (p75)', formatSharePercent(Math.min(result.backend_share_of_ttfb, 1)))
  }
  const columns: TableColumn<BackendRequestResponse>[] = [
    { header: 'Trace', key: 'trace_id' },
    { header: 'Request', key: 'span_name' },
    { header: 'Service', key: 'service_name' },
    {
      header: 'Duration',
      accessor: (r) => formatMs(r.duration_ms),
      align: 'right',
      color: (v, r) => (r.error ? colors.error(v) : v),
    },
  ]
  printTable(result.slowest, columns, { style: 'minimal' })
  newline()
}

async function trendAction(options: TrendOptions): Promise<void> {
  if (!options.projectId || !options.service || !options.operation) {
    warning('Provide --project-id, --service and --operation')
    return
  }
  const days = parseTrendDays(options.days)
  if (!days.ok) {
    warning(days.error)
    return
  }
  await requireAuth()
  await setupClient()
  const end = new Date()
  const start = new Date(end.getTime() - days.days * 86_400_000)
  const result = await withSpinner('Loading the operation trend...', async () => {
    const { data, error } = await getOperationTrend({
      client,
      query: {
        project_id: parseInt(options.projectId!, 10),
        service_name: options.service!,
        span_name: options.operation!,
        start_time: start.toISOString(),
        end_time: end.toISOString(),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.info} ${options.operation} (${options.service}), last ${days.days} days`)
  if (!result?.tracking.configured) {
    info(result?.tracking.reason ?? 'No operation history yet.')
    newline()
    return
  }
  const rows = summarizeByDay(result.points)
  if (rows.length === 0) {
    info('No calls of this operation were rolled up in this window.')
  } else {
    const columns: TableColumn<DaySummary>[] = [
      { header: 'Day', key: 'day' },
      { header: 'Calls', accessor: (r) => r.calls.toLocaleString(), align: 'right' },
      {
        header: 'Errors',
        accessor: (r) => (r.errors > 0 ? r.errors.toLocaleString() : '-'),
        align: 'right',
        color: (v) => (v === '-' ? colors.muted(v) : colors.error(v)),
      },
      { header: 'p95 (median hour)', accessor: (r) => formatMs(r.p95Ms), align: 'right' },
      { header: 'Hours', accessor: (r) => r.hours.toString(), align: 'right' },
    ]
    printTable(rows, columns, { style: 'minimal' })
  }
  for (const deployment of result.deployments) {
    keyValue(`Deployment #${deployment.deployment_id}`, `live at ${deployment.live_at} (${deployment.slug})`)
  }
  if (result.tracking.overflow_operations > 0) {
    warning(
      `The last hour had ${result.tracking.overflow_operations} operations over the ${result.tracking.max_tracked_operations} kept per hour; those have no history for it.`,
    )
  }
  newline()
}

async function regressionsAction(options: RegressionsOptions): Promise<void> {
  if (!options.projectId || !options.deploymentId) {
    warning('Provide --project-id and --deployment-id')
    return
  }
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Comparing operations across the deployment...', async () => {
    const { data, error } = await getDeploymentRegressions({
      client,
      query: {
        project_id: parseInt(options.projectId!, 10),
        deployment_id: parseInt(options.deploymentId!, 10),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  if (!result) return
  newline()
  header(`${icons.info} Deployment #${result.deployment.deployment_id}, live at ${result.deployment.live_at}`)
  keyValue('Compared', `${result.after_start} → ${result.after_end} against the week before`)
  keyValue(
    'Operations',
    `${result.regressed} slower, ${result.improved} faster, ${result.unchanged} unchanged, ` +
      `${result.insufficient_baseline} without enough history, ${result.insufficient_after} waiting for more hours`,
  )
  if (!result.tracking.configured) {
    info(result.tracking.reason ?? 'No operation history yet.')
  } else if (result.changes.length > 0) {
    const columns: TableColumn<OperationChangeResponse>[] = [
      { header: 'Service', key: 'service_name' },
      { header: 'Operation', key: 'span_name', color: (v) => colors.bold(v) },
      { header: 'p95 before', accessor: (r) => formatMs(r.baseline_p95_ms), align: 'right' },
      { header: 'p95 after', accessor: (r) => formatMs(r.after_p95_ms), align: 'right' },
      {
        header: 'Change',
        accessor: (r) => formatChange(r.change_ratio),
        align: 'right',
        color: (v, r) => (r.change === 'regressed' ? colors.error(v) : colors.success(v)),
      },
    ]
    printTable(result.changes, columns, { style: 'minimal' })
  }
  newline()
}
