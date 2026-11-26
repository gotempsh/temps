// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getTrace } from '../../api/sdk.gen.js'
import type { SpanRecord } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { colors, header, icons, json, keyValue, newline } from '../../ui/output.js'
import { CliError } from '../../utils/errors.js'

interface ShowTraceOptions {
  projectId?: string
  startTime?: string
  endTime?: string
  json?: boolean
}

/** A trace ID is 32 hex digits (W3C Trace Context). */
const TRACE_ID = /^[0-9a-f]{32}$/i

export interface TraceLookup {
  projectId: number
  traceId: string
  /** Both or neither: the server rejects half a window. */
  window?: { start_time: string; end_time: string }
}

/**
 * Validate what `traces show` was given before any request is made. Throws
 * with the offending value, so a wrong argument exits non-zero instead of
 * returning an empty trace that reads as "nothing happened".
 */
export function resolveTraceLookup(traceId: string, options: ShowTraceOptions): TraceLookup {
  if (!TRACE_ID.test(traceId)) {
    throw new CliError(
      `"${traceId}" is not a trace ID: expected 32 hexadecimal characters.`,
      'INVALID_TRACE_ID',
    )
  }
  if (!options.projectId || !/^[1-9]\d*$/.test(options.projectId)) {
    throw new CliError(
      `--project-id must be the numeric ID of the project the trace belongs to, got "${options.projectId ?? ''}".`,
      'INVALID_PROJECT_ID',
    )
  }
  const lookup: TraceLookup = {
    projectId: Number(options.projectId),
    traceId: traceId.toLowerCase(),
  }
  if (options.startTime === undefined && options.endTime === undefined) return lookup
  if (options.startTime === undefined || options.endTime === undefined) {
    throw new CliError(
      '--start-time and --end-time narrow the lookup together: pass both or neither.',
      'INVALID_TIME_WINDOW',
    )
  }
  const start = Date.parse(options.startTime)
  const end = Date.parse(options.endTime)
  if (Number.isNaN(start) || Number.isNaN(end)) {
    throw new CliError(
      `--start-time and --end-time must be ISO 8601 timestamps, got "${options.startTime}" and "${options.endTime}".`,
      'INVALID_TIME_WINDOW',
    )
  }
  if (end <= start) {
    throw new CliError(
      `--end-time (${options.endTime}) must be after --start-time (${options.startTime}).`,
      'INVALID_TIME_WINDOW',
    )
  }
  lookup.window = {
    start_time: new Date(start).toISOString(),
    end_time: new Date(end).toISOString(),
  }
  return lookup
}

export interface TraceSummary {
  spans: number
  errorSpans: number
  services: string[]
  /** The root span's name, or the earliest span's when no root was recorded. */
  root: string | null
  startTime: string | null
  durationMs: number | null
}

/** What the human view prints above the span table. */
export function summarizeTrace(spans: SpanRecord[]): TraceSummary {
  if (spans.length === 0) {
    return { spans: 0, errorSpans: 0, services: [], root: null, startTime: null, durationMs: null }
  }
  const ordered = [...spans].sort((a, b) => Date.parse(a.start_time) - Date.parse(b.start_time))
  const first = ordered[0] as SpanRecord
  const root = ordered.find((s) => !s.parent_span_id) ?? first
  const start = Date.parse(first.start_time)
  const end = Math.max(...spans.map((s) => Date.parse(s.end_time)))
  const services = new Set<string>()
  for (const span of spans) {
    const name = span.resource?.service_name
    if (name) services.add(name)
  }
  return {
    spans: spans.length,
    errorSpans: spans.filter((s) => String(s.status_code).toUpperCase() === 'ERROR').length,
    services: [...services].sort(),
    root: root.name,
    startTime: first.start_time,
    durationMs: Number.isNaN(end - start) ? null : end - start,
  }
}

async function showTraceAction(traceId: string, options: ShowTraceOptions): Promise<void> {
  const lookup = resolveTraceLookup(traceId, options)
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching trace...', async () => {
    const { data, error } = await getTrace({
      client,
      path: { project_id: lookup.projectId, trace_id: lookup.traceId },
      query: lookup.window,
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const spans = result?.data ?? []
  if (spans.length === 0) {
    // An empty answer is not a trace. Fail so a script or an agent does not
    // read "no spans" as "nothing went wrong in this request".
    throw new CliError(
      `No spans found for trace ${lookup.traceId} in project ${lookup.projectId}` +
        (lookup.window
          ? ` between ${lookup.window.start_time} and ${lookup.window.end_time}.`
          : '. If it is older than the default lookup window, pass --start-time and --end-time.'),
      'TRACE_NOT_FOUND',
    )
  }

  if (options.json) {
    json(result)
    return
  }

  const summary = summarizeTrace(spans)
  newline()
  header(`${icons.info} Trace ${lookup.traceId}`)
  keyValue('Root', summary.root ?? '-')
  keyValue('Started', summary.startTime ?? '-')
  keyValue('Duration', summary.durationMs == null ? '-' : `${summary.durationMs}ms`)
  keyValue('Spans', `${summary.spans} (${summary.errorSpans} with errors)`)
  keyValue('Services', summary.services.join(', ') || '-')
  newline()
  const columns: TableColumn<SpanRecord>[] = [
    { header: 'Start', accessor: (s) => s.start_time },
    { header: 'Span', accessor: (s) => s.name, color: (v) => colors.bold(v) },
    { header: 'Service', accessor: (s) => s.resource?.service_name ?? '-' },
    { header: 'Kind', accessor: (s) => String(s.kind) },
    { header: 'Duration', accessor: (s) => `${s.duration_ms}ms`, align: 'right' },
    {
      header: 'Status',
      accessor: (s) => String(s.status_code),
      color: (v) => (v.toUpperCase() === 'ERROR' ? colors.error(v) : v),
    },
  ]
  printTable(
    [...spans].sort((a, b) => Date.parse(a.start_time) - Date.parse(b.start_time)),
    columns,
    { style: 'minimal' },
  )
  newline()
}

export function registerShowTraceCommand(traces: Command): void {
  traces
    .command('show <trace-id>')
    .alias('get')
    .description('Show every span of one trace: timing, service, status, attributes and events')
    .requiredOption('--project-id <id>', 'Project the trace belongs to')
    .option('--start-time <iso>', 'Look for the trace from this time (ISO 8601); pair with --end-time')
    .option('--end-time <iso>', 'Look for the trace up to this time (ISO 8601); pair with --start-time')
    .option('--json', 'Output the spans in JSON format')
    .addHelpText(
      'after',
      `
Examples:
  # Read one trace, with its span attributes and events
  $ temps traces show 4bf92f3577b34da6a3ce929d0e0e4736 --project-id 4 --json

  # An older trace: say when it ran (the window may span up to 31 days)
  $ temps traces show 4bf92f3577b34da6a3ce929d0e0e4736 --project-id 4 \\
      --start-time 2026-11-25T14:02:00Z --end-time 2026-11-25T14:07:00Z
`,
    )
    .action(showTraceAction)
}
