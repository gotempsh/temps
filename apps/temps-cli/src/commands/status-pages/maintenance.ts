// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Maintenance windows of a status page (ADR 108 D2).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listStatusPageMaintenances,
  createStatusPageMaintenance,
  updateStatusPageMaintenance,
  cancelStatusPageMaintenance,
} from '../../api/sdk.gen.js'
import type {
  CreateMaintenanceRequest,
  MaintenanceResponse,
  UpdateMaintenanceRequest,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, json, colors, success, info, warning, formatDate } from '../../ui/output.js'

/** The longest window the server accepts. */
export const MAX_WINDOW_MS = 7 * 24 * 60 * 60 * 1000

export interface WindowOptions {
  title?: string
  description?: string
  starts?: string
  ends?: string
  duration?: string
  monitor?: string[]
}

interface ScheduleOptions extends WindowOptions {
  id: string
  json?: boolean
}

interface UpdateOptions extends WindowOptions {
  id: string
  maintenanceId: string
  json?: boolean
}

interface ListOptions {
  id: string
  page?: string
  pageSize?: string
  json?: boolean
}

interface CancelOptions {
  id: string
  maintenanceId: string
  yes?: boolean
}

/** `30m`, `2h` or `1d` in milliseconds, or null. */
export function parseDuration(value: string): number | null {
  const match = /^(\d+)\s*([mhd])$/.exec(value.trim())
  if (!match) return null
  const amount = parseInt(match[1] ?? '', 10)
  const unit = { m: 60_000, h: 3_600_000, d: 86_400_000 }[match[2] as 'm' | 'h' | 'd']
  return amount > 0 ? amount * unit : null
}

/** `now` or an ISO 8601 date with a time zone, as a Date, or null. */
export function parseWhen(value: string, now: Date): Date | null {
  const trimmed = value.trim()
  if (trimmed === 'now') return now
  // A date without a zone would be read in this machine's zone, which is
  // rarely what an operator scheduling for a team means.
  if (!/T.*(Z|[+-]\d{2}:?\d{2})$/i.test(trimmed)) return null
  const parsed = new Date(trimmed)
  return isNaN(parsed.getTime()) ? null : parsed
}

function monitorIds(values: string[]): number[] | string {
  const ids = values.flatMap((value) => value.split(',')).map((value) => value.trim()).filter(Boolean)
  const parsed = ids.map((value) => Number(value))
  if (parsed.some((id) => !Number.isInteger(id) || id <= 0)) {
    return `Invalid monitor ID in: ${ids.join(', ')}`
  }
  return [...new Set(parsed)]
}

/** The window's start and end, from --starts with --ends or --duration. */
function window(
  options: WindowOptions,
  now: Date,
): { starts?: Date; ends?: Date } | string {
  let starts: Date | undefined
  if (options.starts !== undefined) {
    const parsed = parseWhen(options.starts, now)
    if (!parsed) return `Invalid --starts "${options.starts}": use now or an ISO 8601 date with a zone, like 2026-12-01T22:00:00Z`
    starts = parsed
  }
  if (options.ends !== undefined && options.duration !== undefined) {
    return 'Pass either --ends or --duration, not both'
  }
  let ends: Date | undefined
  if (options.ends !== undefined) {
    const parsed = parseWhen(options.ends, now)
    if (!parsed) return `Invalid --ends "${options.ends}": use an ISO 8601 date with a zone, like 2026-12-01T23:00:00Z`
    ends = parsed
  }
  if (options.duration !== undefined) {
    const ms = parseDuration(options.duration)
    if (ms === null) return `Invalid --duration "${options.duration}": use minutes, hours or days, like 90m, 2h or 1d`
    if (!starts) return '--duration needs --starts'
    ends = new Date(starts.getTime() + ms)
  }
  if (starts && ends) {
    if (ends.getTime() <= starts.getTime()) return 'The window must end after it starts'
    if (ends.getTime() - starts.getTime() > MAX_WINDOW_MS) return 'A maintenance window lasts at most 7 days'
  }
  return { starts, ends }
}

/** The request body for `maintenance schedule`, or an error message. */
export function scheduleBody(options: WindowOptions, now: Date): CreateMaintenanceRequest | string {
  const title = options.title?.trim() ?? ''
  if (!title) return 'Pass --title: it is shown on the public page'
  if (options.starts === undefined) return 'Pass --starts (now, or an ISO 8601 date)'
  if (options.ends === undefined && options.duration === undefined) return 'Pass --ends or --duration'
  const span = window(options, now)
  if (typeof span === 'string') return span
  const ids = monitorIds(options.monitor ?? [])
  if (typeof ids === 'string') return ids
  if (ids.length === 0) return 'Pass --monitor <id> for each monitor the window covers'
  const { starts, ends } = span
  if (!starts || !ends) return 'Pass --starts and --ends or --duration'
  return {
    title,
    description: options.description?.trim() ? options.description.trim() : null,
    // `now` is sent as the current time; the server allows a few minutes of slack.
    starts_at: starts.toISOString(),
    ends_at: ends.toISOString(),
    monitor_ids: ids,
  }
}

/** The request body for `maintenance update`, or an error message. */
export function updateBody(options: WindowOptions, now: Date): UpdateMaintenanceRequest | string {
  const span = window(options, now)
  if (typeof span === 'string') return span
  const body: UpdateMaintenanceRequest = {}
  if (options.title !== undefined) body.title = options.title
  if (options.description !== undefined) body.description = options.description
  if (span.starts) body.starts_at = span.starts.toISOString()
  if (span.ends) body.ends_at = span.ends.toISOString()
  if (options.monitor !== undefined) {
    const ids = monitorIds(options.monitor)
    if (typeof ids === 'string') return ids
    body.monitor_ids = ids
  }
  if (Object.keys(body).length === 0) {
    return 'Nothing to change: pass --title, --description, --starts, --ends, --duration or --monitor'
  }
  return body
}

const STATE_COLORS: Record<MaintenanceResponse['state'], (value: string) => string> = {
  scheduled: colors.info,
  in_progress: colors.warning,
  completed: colors.muted,
  cancelled: colors.muted,
}

export function stateLabel(state: MaintenanceResponse['state']): string {
  return state === 'in_progress' ? 'in progress' : state
}

function parseId(value: string, what: string): number | null {
  const id = parseInt(value, 10)
  if (isNaN(id)) {
    warning(`Invalid ${what}`)
    return null
  }
  return id
}

async function run<T>(label: string, call: () => Promise<{ data?: T; error?: unknown }>): Promise<T> {
  return withSpinner(label, async () => {
    const { data, error } = await call()
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    if (data === undefined) {
      throw new Error('The server returned an empty response')
    }
    return data
  })
}

function printWindow(item: MaintenanceResponse): void {
  info(`${colors.bold(item.title)} (${stateLabel(item.state)}): ${formatDate(item.starts_at)} to ${formatDate(item.ends_at)}, monitors ${item.monitor_ids.join(', ')}`)
}

function collect(value: string, previous: string[] = []): string[] {
  return [...previous, value]
}

export function registerMaintenanceCommands(pages: Command): void {
  const maintenance = pages
    .command('maintenance')
    .description('Schedule maintenance windows: shown on the page, no outage alerts, no downtime counted')

  maintenance
    .command('list')
    .alias('ls')
    .description('List maintenance windows, newest first')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--page <n>', 'Page number', '1')
    .option('--page-size <n>', 'Items per page (max 100)', '20')
    .option('--json', 'Output in JSON format')
    .action(async (options: ListOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const result = await run('Fetching maintenance windows...', () =>
        listStatusPageMaintenances({
          client,
          path: { status_page_id: id },
          query: {
            page: parseInt(options.page ?? '1', 10) || 1,
            page_size: parseInt(options.pageSize ?? '20', 10) || 20,
          },
        }))
      if (options.json) {
        json(result)
        return
      }
      newline()
      if (result.items.length === 0) {
        info(`No maintenance windows. Schedule one with: temps status-pages maintenance schedule --id ${id} --title "Database upgrade" --starts 2026-12-01T22:00:00Z --duration 1h --monitor <monitor-id>`)
        newline()
        return
      }
      printTable(result.items, [
        { header: 'ID', key: 'id', width: 8 },
        { header: 'Title', key: 'title', color: (v) => colors.bold(v) },
        { header: 'State', accessor: (m) => STATE_COLORS[m.state](stateLabel(m.state)) },
        { header: 'Starts', accessor: (m) => formatDate(m.starts_at) },
        { header: 'Ends', accessor: (m) => formatDate(m.ends_at) },
        { header: 'Monitors', accessor: (m) => m.monitor_ids.join(', ') },
      ], { style: 'minimal' })
      info(`Page ${result.page} of ${Math.max(1, Math.ceil(result.total / result.page_size))} (${result.total} total)`)
      newline()
    })

  maintenance
    .command('schedule')
    .description('Schedule a window; subscribers and webhooks are told once')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--title <title>', 'Public title (at most 120 characters)')
    .option('--description <text>', 'Public description (at most 2000 characters)')
    .requiredOption('--starts <when>', 'now, or an ISO 8601 date with a zone')
    .option('--ends <when>', 'ISO 8601 date with a zone')
    .option('--duration <span>', 'Instead of --ends: 90m, 2h, 1d (at most 7d)')
    .option('--monitor <id>', 'Monitor on this page the window covers (repeat, or comma-separate)', collect)
    .option('--json', 'Output in JSON format')
    .action(async (options: ScheduleOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const body = scheduleBody(options, new Date())
      if (typeof body === 'string') {
        warning(body)
        return
      }
      const created = await run('Scheduling maintenance...', () =>
        createStatusPageMaintenance({ client, path: { status_page_id: id }, body }))
      if (options.json) {
        json(created)
        return
      }
      success(`Scheduled maintenance ${created.id}`)
      printWindow(created)
    })

  maintenance
    .command('update')
    .description('Change a window; once it has started only the title, description and end can change')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--maintenance-id <id>', 'Maintenance ID (see `maintenance list`)')
    .option('--title <title>', 'Public title')
    .option('--description <text>', 'Public description ("" removes it)')
    .option('--starts <when>', 'now, or an ISO 8601 date with a zone')
    .option('--ends <when>', 'ISO 8601 date with a zone')
    .option('--duration <span>', 'With --starts, instead of --ends: 90m, 2h, 1d')
    .option('--monitor <id>', 'Replace the covered monitors (repeat, or comma-separate)', collect)
    .option('--json', 'Output in JSON format')
    .action(async (options: UpdateOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      const maintenanceId = parseId(options.maintenanceId, 'maintenance ID')
      if (id === null || maintenanceId === null) return
      const body = updateBody(options, new Date())
      if (typeof body === 'string') {
        warning(body)
        return
      }
      const updated = await run('Saving maintenance...', () =>
        updateStatusPageMaintenance({
          client,
          path: { status_page_id: id, maintenance_id: maintenanceId },
          body,
        }))
      if (options.json) {
        json(updated)
        return
      }
      success(`Updated maintenance ${updated.id}`)
      printWindow(updated)
    })

  maintenance
    .command('cancel')
    .description('Cancel a scheduled window, or end a running one now')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--maintenance-id <id>', 'Maintenance ID (see `maintenance list`)')
    .option('-y, --yes', 'Skip the confirmation prompt')
    .action(async (options: CancelOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      const maintenanceId = parseId(options.maintenanceId, 'maintenance ID')
      if (id === null || maintenanceId === null) return
      if (!options.yes) {
        const confirmed = await promptConfirm({
          message: `Cancel maintenance ${maintenanceId}? Outage alerts resume for its monitors.`,
          default: false,
        })
        if (!confirmed) {
          info('Kept')
          return
        }
      }
      const cancelled = await run('Cancelling maintenance...', () =>
        cancelStatusPageMaintenance({
          client,
          path: { status_page_id: id, maintenance_id: maintenanceId },
        }))
      success(`Cancelled maintenance ${cancelled.id}`)
    })
}
