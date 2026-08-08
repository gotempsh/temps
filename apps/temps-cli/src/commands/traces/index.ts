import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { querySpanStats } from '../../api/sdk.gen.js'
import type { SpanStats } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  info,
  warning,
  keyValue,
  formatRelativeTime,
} from '../../ui/output.js'

interface SpanStatsOptions {
  projectId?: string
  projectIds?: string
  since?: string
  startTime?: string
  endTime?: string
  service?: string
  operation?: string
  search?: string
  kind?: string
  status?: string
  environmentId?: string
  deploymentId?: string
  attributes?: string
  minDurationMs?: string
  minCount?: string
  sortBy?: string
  sortOrder?: string
  limit?: string
  offset?: string
  json?: boolean
}

/** Sort keys accepted by `--sort-by`, mirroring `SpanStatsSortField::parse`. */
const SORT_FIELDS = [
  'total_time',
  'p50',
  'p95',
  'p99',
  'max',
  'avg',
  'stddev',
  'count',
  'errors',
  'error_rate',
  'variability',
  'tail_ratio',
]

const SPAN_KINDS = ['server', 'client', 'internal', 'producer', 'consumer', 'unspecified']
const SPAN_STATUSES = ['ok', 'error', 'unset']

/** Server-side caps, mirrored so the CLI can explain them before a round trip. */
const MAX_PROJECTS = 50
export const MAX_WINDOW_DAYS = 31

/**
 * Parse a relative duration like `30m`, `24h`, `7d` into milliseconds.
 * Returns `null` for anything that isn't one of those shapes, so the caller can
 * complain with the actual input rather than silently defaulting.
 */
export function parseSince(since: string): number | null {
  const match = /^(\d+)\s*([mhd])$/i.exec(since.trim())
  if (!match) return null
  const amount = parseInt(match[1]!, 10)
  switch (match[2]!.toLowerCase()) {
    case 'm':
      return amount * 60_000
    case 'h':
      return amount * 3_600_000
    case 'd':
      return amount * 86_400_000
    default:
      return null
  }
}

/** Format a millisecond duration for a terminal column. */
export function formatMs(ms: number): string {
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  if (ms >= 1) return `${ms.toFixed(1)}ms`
  return `${ms.toFixed(2)}ms`
}

/** Total wall-clock, which is often minutes or hours rather than milliseconds. */
export function formatTotal(ms: number): string {
  if (ms >= 3_600_000) return `${(ms / 3_600_000).toFixed(2)}h`
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)}m`
  return formatMs(ms)
}

export type TimeWindowResult =
  | { ok: true; startTime: Date; endTime: Date }
  | { ok: false; error: string }

/**
 * Resolve the query window from --since/--start-time/--end-time and enforce
 * MAX_WINDOW_DAYS client-side, mirroring the server's own validation so a bad
 * window is reported with a message that names the flag to fix, instead of a
 * bare 400 after a round trip.
 */
export function resolveTimeWindow(options: {
  since?: string
  startTime?: string
  endTime?: string
}): TimeWindowResult {
  const endTime = options.endTime ? new Date(options.endTime) : new Date()
  if (Number.isNaN(endTime.getTime())) {
    return { ok: false, error: `Invalid --end-time: ${options.endTime}` }
  }

  let startTime: Date
  if (options.startTime) {
    startTime = new Date(options.startTime)
    if (Number.isNaN(startTime.getTime())) {
      return { ok: false, error: `Invalid --start-time: ${options.startTime}` }
    }
  } else {
    const sinceMs = parseSince(options.since ?? '24h')
    if (sinceMs === null) {
      return { ok: false, error: `Invalid --since: ${options.since}. Use a form like 30m, 24h or 7d` }
    }
    startTime = new Date(endTime.getTime() - sinceMs)
  }

  const windowDays = (endTime.getTime() - startTime.getTime()) / 86_400_000
  if (windowDays > MAX_WINDOW_DAYS) {
    return {
      ok: false,
      error:
        `Window is ${windowDays.toFixed(1)} days; the maximum is ${MAX_WINDOW_DAYS}. ` +
        `Narrow --since, or --start-time/--end-time.`,
    }
  }

  return { ok: true, startTime, endTime }
}

export function registerTracesCommands(program: Command): void {
  const traces = program
    .command('traces')
    .alias('trace')
    .description('Inspect distributed traces and operation latency')

  traces
    .command('span-stats')
    .alias('operations')
    .alias('ops')
    .description('Rank operations by time spent, latency percentiles, or inconsistency')
    .option('--project-id <id>', 'Project ID')
    .option('--project-ids <ids>', `Comma-separated project IDs to rank across, e.g. 4,5,6 (max ${MAX_PROJECTS})`)
    .option('--since <duration>', `Relative window: 30m, 24h, 7d (max ${MAX_WINDOW_DAYS}d)`, '24h')
    .option('--start-time <iso>', 'Window start (ISO 8601); overrides --since')
    .option('--end-time <iso>', 'Window end (ISO 8601); defaults to now')
    .option('--service <name>', 'Only this service')
    .option('--operation <name>', 'Only this operation (exact span name)')
    .option('--search <text>', 'Only operations whose name contains this text')
    .option('--kind <kind>', `Span kind (${SPAN_KINDS.join(', ')})`)
    .option('--status <status>', `Span status (${SPAN_STATUSES.join(', ')})`)
    .option('--environment-id <id>', 'Only this environment')
    .option('--deployment-id <id>', 'Only this deployment')
    .option('--attributes <pairs>', 'Span attribute filters, e.g. db.system=postgresql')
    .option('--min-duration-ms <ms>', 'Ignore spans faster than this')
    .option('--min-count <n>', 'Drop operations with fewer samples than this')
    .option('--sort-by <field>', `Ranking (${SORT_FIELDS.join(', ')})`, 'total_time')
    .option('--sort-order <order>', 'asc or desc', 'desc')
    .option('--limit <n>', 'Rows to show (max 100)', '20')
    .option('--offset <n>', 'Page offset')
    .option('--json', 'Output in JSON format')
    .addHelpText(
      'after',
      `
Examples:
  # Where does the wall-clock actually go?
  $ temps traces span-stats --project-id 4 --since 24h

  # What do users feel?
  $ temps traces span-stats --project-id 4 --sort-by p95

  # Which operations are erratic — fast most of the time, awful sometimes?
  $ temps traces span-stats --project-id 4 --sort-by variability --min-count 20

  # How slow did one operation ever get?
  $ temps traces span-stats --project-id 4 --operation payments.charge

  # Rank across several projects at once
  $ temps traces span-stats --project-ids 4,5,6 --sort-by p99

  # How slow are the failures specifically?
  $ temps traces span-stats --project-id 4 --status error --sort-by p95
`,
    )
    .action(spanStatsAction)
}

async function spanStatsAction(options: SpanStatsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.projectId && !options.projectIds) {
    warning('Provide --project-id, or --project-ids with a comma-separated list')
    return
  }
  if (options.projectIds) {
    const count = options.projectIds.split(',').filter((s) => s.trim()).length
    if (count > MAX_PROJECTS) {
      warning(`--project-ids accepts at most ${MAX_PROJECTS} projects, got ${count}`)
      return
    }
  }
  if (options.sortBy && !SORT_FIELDS.includes(options.sortBy)) {
    warning(`Invalid --sort-by: ${options.sortBy}. Available: ${SORT_FIELDS.join(', ')}`)
    return
  }
  if (options.kind && !SPAN_KINDS.includes(options.kind)) {
    warning(`Invalid --kind: ${options.kind}. Available: ${SPAN_KINDS.join(', ')}`)
    return
  }
  if (options.status && !SPAN_STATUSES.includes(options.status)) {
    warning(`Invalid --status: ${options.status}. Available: ${SPAN_STATUSES.join(', ')}`)
    return
  }

  // Resolve the window client-side so `--since` is a real convenience rather
  // than a second, subtly different defaulting rule on the server.
  const window = resolveTimeWindow(options)
  if (!window.ok) {
    warning(window.error)
    return
  }
  const { startTime, endTime } = window

  const result = await withSpinner('Aggregating operation latency...', async () => {
    const { data, error } = await querySpanStats({
      client,
      query: {
        ...(options.projectIds
          ? { project_ids: options.projectIds }
          : { project_id: parseInt(options.projectId!, 10) }),
        start_time: startTime.toISOString(),
        end_time: endTime.toISOString(),
        ...(options.service && { service_name: options.service }),
        ...(options.operation && { span_name: options.operation }),
        ...(options.search && { name_pattern: options.search }),
        ...(options.kind && { kind: options.kind }),
        ...(options.status && { status: options.status }),
        ...(options.environmentId && { environment_id: parseInt(options.environmentId, 10) }),
        ...(options.deploymentId && { deployment_id: parseInt(options.deploymentId, 10) }),
        ...(options.attributes && { attributes: options.attributes }),
        ...(options.minDurationMs && { min_duration_ms: parseFloat(options.minDurationMs) }),
        ...(options.minCount && { min_count: parseInt(options.minCount, 10) }),
        ...(options.sortBy && { sort_by: options.sortBy }),
        ...(options.sortOrder && { sort_order: options.sortOrder }),
        ...(options.limit && { limit: parseInt(options.limit, 10) }),
        ...(options.offset && { offset: parseInt(options.offset, 10) }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  const rows = result?.data ?? []
  const total = result?.total ?? rows.length

  newline()
  header(`${icons.info} Operations by ${options.sortBy ?? 'total_time'} (${total} total)`)
  keyValue('Window', `${startTime.toISOString()} → ${endTime.toISOString()}`)

  if (rows.length === 0) {
    // Distinguish "nothing matched" from "nothing was ever ingested" — the
    // fixes are completely different, and a bare "no results" sends people
    // hunting in the wrong place.
    info('No operations matched.')
    info(
      'If you expected data: widen --since, drop --min-count/--service filters, ' +
        'or confirm the app is exporting traces (temps traces span-stats --since 7d).',
    )
    newline()
    return
  }

  // Multi-project reports need the project column; a single-project one does
  // not, and dropping it keeps the table readable in a narrow terminal.
  const multiProject = new Set(rows.map((r) => r.project_id)).size > 1

  const columns: TableColumn<SpanStats>[] = [
    ...(multiProject
      ? [{ header: 'Proj', accessor: (r: SpanStats) => r.project_id.toString(), width: 5 }]
      : []),
    { header: 'Service', key: 'service_name' },
    { header: 'Operation', key: 'span_name', color: (v) => colors.bold(v) },
    { header: 'Calls', accessor: (r) => r.count.toLocaleString(), align: 'right' },
    {
      header: 'Errors',
      accessor: (r) => (r.error_count > 0 ? `${(r.error_rate * 100).toFixed(1)}%` : '-'),
      align: 'right',
      color: (v) => (v === '-' ? colors.muted(v) : colors.error(v)),
    },
    { header: 'Total', accessor: (r) => formatTotal(r.total_duration_ms), align: 'right' },
    { header: 'p50', accessor: (r) => formatMs(r.p50_duration_ms), align: 'right' },
    { header: 'p95', accessor: (r) => formatMs(r.p95_duration_ms), align: 'right' },
    { header: 'p99', accessor: (r) => formatMs(r.p99_duration_ms), align: 'right' },
    { header: 'Max', accessor: (r) => formatMs(r.max_duration_ms), align: 'right' },
    {
      header: 'p99/p50',
      accessor: (r) => (r.tail_ratio > 0 ? `${r.tail_ratio.toFixed(1)}x` : '-'),
      align: 'right',
      // A tail ratio above 10 means the bad case is an order of magnitude
      // worse than the typical one — the thing this report exists to surface.
      color: (v, r) => (r.tail_ratio >= 10 ? colors.error(v) : r.tail_ratio >= 4 ? colors.warning(v) : v),
    },
    { header: 'Last Seen', accessor: (r) => formatRelativeTime(r.last_seen) },
  ]

  printTable(rows, columns, { style: 'minimal' })

  const shown = rows.length
  const offset = options.offset ? parseInt(options.offset, 10) : 0
  if (total > shown + offset) {
    info(`Showing ${offset + 1}–${offset + shown} of ${total} (use --limit / --offset)`)
  }
  newline()
}
