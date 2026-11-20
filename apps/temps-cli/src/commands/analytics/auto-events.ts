// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps analytics outbound | downloads | not-found`: the events the tracker
 * records by itself (ADR 109 D1).
 *
 * - `outbound` and `downloads` rank the `url` property of the `outbound_click`
 *   and `file_download` events through `GET /analytics/event-properties`.
 * - `not-found` ranks the 404s Temps answered to real visitors (API traffic
 *   aggregation over the request log), plus the paths pages reported with
 *   `trackNotFound()` / `data-temps-404` (`not_found` events).
 */

import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { resolveAnalyticsProject } from './project.js'
import {
  aggregateApiTraffic,
  getEventProperties,
  getPropertyBreakdown,
} from '../../api/sdk.gen.js'
import type {
  TrafficAggregationRequest,
  TrafficAggregationResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, json as jsonOut, colors, info } from '../../ui/output.js'
import { parsePeriod } from './period.js'

export const OUTBOUND_CLICK_EVENT = 'outbound_click'
export const FILE_DOWNLOAD_EVENT = 'file_download'
export const NOT_FOUND_EVENT = 'not_found'

/** Widest window the API traffic aggregation serves. */
export const MAX_NOT_FOUND_WINDOW_DAYS = 30

interface AutoEventOptions {
  project?: string
  period?: string
  limit?: string
  json?: boolean
}

/** Parse `--limit`, defaulting to 20 and refusing anything outside 1..100. */
export function parseLimit(raw: string | undefined): number {
  if (raw === undefined) return 20
  const limit = Number(raw)
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) {
    throw new Error(`--limit must be a whole number from 1 to 100, got "${raw}"`)
  }
  return limit
}

/** Narrow a start date so the window stays within the aggregation limit. */
export function clampNotFoundStart(
  startDate: string,
  endDate: string,
): { startDate: string; narrowed: boolean } {
  const earliest = new Date(
    new Date(endDate).getTime() - MAX_NOT_FOUND_WINDOW_DAYS * 24 * 60 * 60 * 1000,
  )
  return new Date(startDate) < earliest
    ? { startDate: earliest.toISOString(), narrowed: true }
    : { startDate, narrowed: false }
}

/** Request body for the 404 paths Temps answered to non-bot visitors. */
export function notFoundRequest(
  startDate: string,
  endDate: string,
  limit: number,
): TrafficAggregationRequest {
  return {
    start_time: startDate,
    end_time: endDate,
    dimensions: ['path'],
    metrics: ['requests', 'last_seen'],
    filters: [
      { dimension: 'status_code', operator: 'eq', values: ['404'] },
      { dimension: 'is_bot', operator: 'eq', values: ['false'] },
    ],
    order_by: [{ field: { kind: 'metric', field: 'requests' }, direction: 'desc' }],
    page: 1,
    page_size: limit,
  }
}

function printTable(
  title: string,
  subtitle: string,
  header: [string, string, string],
  rows: Array<[string, string, string]>,
  empty: string,
): void {
  const line = chalk.cyan('━'.repeat(72))
  newline()
  console.log(line)
  console.log(`   ${chalk.bold.white(title)} ${chalk.gray(`— ${subtitle}`)}`)
  console.log(line)
  newline()
  if (rows.length === 0) {
    console.log(`  ${chalk.gray(empty)}`)
  } else {
    console.log(
      `  ${chalk.gray(header[0].padEnd(48))}${chalk.gray(header[1].padStart(10))}${chalk.gray(header[2].padStart(12))}`,
    )
    console.log(`  ${chalk.gray('─'.repeat(70))}`)
    for (const [value, count, extra] of rows) {
      const display = value.length > 46 ? `${value.slice(0, 43)}...` : value
      console.log(`  ${chalk.white(display.padEnd(48))}${count.padStart(10)}${extra.padStart(12)}`)
    }
  }
  newline()
  console.log(line)
  newline()
}

async function eventPropertyTop(
  eventName: string,
  title: string,
  empty: string,
  options: AutoEventOptions,
): Promise<void> {
  const limit = parseLimit(options.limit)
  await requireAuth()
  await setupClient()
  const period = options.period ?? '7d'
  const { startDate, endDate, label } = parsePeriod(period)
  const project = await resolveAnalyticsProject(options.project)

  const data = await withSpinner(`Fetching ${title.toLowerCase()}...`, async () => {
    const { data, error } = await getEventProperties({
      client,
      query: {
        project_id: project.id,
        event_name: eventName,
        property: 'url',
        start_date: startDate,
        end_date: endDate,
        limit,
      },
    })
    if (error || !data) {
      throw new Error(
        `Could not load ${eventName} for project ${project.slug}: ${getErrorMessage(error)}`,
      )
    }
    return data
  })

  if (options.json) {
    jsonOut({ project: project.slug, period, ...data })
    return
  }

  printTable(
    title,
    `${project.slug} (${label}), ${data.total_count.toLocaleString('en-US')} in total`,
    ['URL', 'Clicks', 'Visitors'],
    data.items.map((item) => [
      item.value ?? '(not set)',
      item.count.toLocaleString('en-US'),
      item.unique_visitors.toLocaleString('en-US'),
    ]),
    empty,
  )
}

export function outboundLinks(options: AutoEventOptions): Promise<void> {
  return eventPropertyTop(
    OUTBOUND_CLICK_EVENT,
    'Outbound links',
    'No outbound link clicks in this period. The tracker records them without code changes.',
    options,
  )
}

export function fileDownloads(options: AutoEventOptions): Promise<void> {
  return eventPropertyTop(
    FILE_DOWNLOAD_EVENT,
    'File downloads',
    'No file downloads in this period. The tracker records them without code changes.',
    options,
  )
}

export async function notFound(options: AutoEventOptions): Promise<void> {
  const limit = parseLimit(options.limit)
  await requireAuth()
  await setupClient()
  const period = options.period ?? '7d'
  const parsed = parsePeriod(period)
  const { startDate, narrowed } = clampNotFoundStart(parsed.startDate, parsed.endDate)
  const project = await resolveAnalyticsProject(options.project)

  const [answered, reported] = await withSpinner('Fetching not-found pages...', async () => {
    const answered = await aggregateApiTraffic({
      client,
      path: { project_id: project.id },
      body: notFoundRequest(startDate, parsed.endDate, limit),
    })
    if (answered.error || !answered.data) {
      throw new Error(
        `Could not load 404 responses for project ${project.slug}: ${getErrorMessage(answered.error)}`,
      )
    }
    const reported = await getPropertyBreakdown({
      client,
      path: { project_id: project.id },
      query: {
        start_date: parsed.startDate,
        end_date: parsed.endDate,
        group_by: 'pathname',
        event_name: NOT_FOUND_EVENT,
        aggregation_level: 'events',
        limit,
      },
    })
    if (reported.error || !reported.data) {
      throw new Error(
        `Could not load not_found events for project ${project.slug}: ${getErrorMessage(reported.error)}`,
      )
    }
    return [answered.data, reported.data] as const
  })

  const answeredRows = (answered as TrafficAggregationResponse).rows.map((row) => ({
    path: row.dimensions[0]?.value ?? '(unknown)',
    hits: row.metrics.requests ?? 0,
    last_seen: row.metrics.last_seen ?? null,
  }))
  const reportedRows = reported.items.map((item) => ({
    path: item.value || '/',
    reports: item.count,
  }))

  if (options.json) {
    jsonOut({
      project: project.slug,
      period,
      answered_window_narrowed: narrowed,
      answered: answeredRows,
      reported: reportedRows,
    })
    return
  }

  if (narrowed) {
    info(`404 responses cover the last ${MAX_NOT_FOUND_WINDOW_DAYS} days of the period.`)
  }
  printTable(
    'Answered 404',
    `${project.slug} (${parsed.label}), requests Temps served`,
    ['Path', 'Hits', 'Last seen'],
    answeredRows.map((row) => [
      row.path,
      row.hits.toLocaleString('en-US'),
      row.last_seen ? new Date(row.last_seen).toISOString().slice(0, 10) : '-',
    ]),
    'No 404 responses to visitors in this period.',
  )
  printTable(
    'Reported by the page',
    `${project.slug} (${parsed.label}), not_found events`,
    ['Path', 'Reports', ''],
    reportedRows.map((row) => [row.path, row.reports.toLocaleString('en-US'), '']),
    'No pages reported as missing. Add data-temps-404 to your 404 view, or call trackNotFound().',
  )
}
