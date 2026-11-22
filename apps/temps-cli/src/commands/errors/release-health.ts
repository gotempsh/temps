// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps errors releases` and `temps errors release <release>`: crash-free
// sessions and users, adoption and the deployment of each release, from
// the sessions Sentry SDKs send (ADR 111, D3).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getReleaseHealthSeries, listReleaseHealth } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { header, info, json, keyValue, warning } from '../../ui/output.js'

export const MAX_HEALTH_DAYS = 14

/** Check a `--days` value: a whole number of days from 1 to 14. */
export function parseHealthDays(raw: string): { ok: true; days: number } | { ok: false; error: string } {
  const days = Number(raw)
  if (!Number.isInteger(days) || days < 1 || days > MAX_HEALTH_DAYS) {
    return { ok: false, error: `--days must be a whole number from 1 to ${MAX_HEALTH_DAYS}, got "${raw}"` }
  }
  return { ok: true, days }
}

/** A rate as a percentage that never rounds below-100% up to 100%. */
export function formatRate(rate: number | null | undefined): string {
  if (rate == null || !Number.isFinite(rate)) return '-'
  if (rate >= 1) return '100%'
  const percent = rate * 100
  const digits = percent >= 99 ? 2 : 1
  const factor = 10 ** digits
  return `${(Math.floor(percent * factor) / factor).toFixed(digits)}%`
}

/** A commit SHA release shortened to 12 characters; other names unchanged. */
export function releaseLabel(release: string): string {
  return /^[0-9a-f]{40}$/i.test(release) ? release.slice(0, 12) : release
}

interface ListOptions {
  projectId: string
  environment?: string
  days: string
  page: string
  json?: boolean
}

interface SeriesOptions {
  projectId: string
  environment?: string
  days: string
  json?: boolean
}

function projectIdOf(raw: string): number | null {
  const id = parseInt(raw, 10)
  if (isNaN(id)) {
    warning('Invalid project ID')
    return null
  }
  return id
}

function daysOf(raw: string): number | null {
  const parsed = parseHealthDays(raw)
  if (!parsed.ok) {
    warning(parsed.error)
    return null
  }
  return parsed.days
}

export function registerReleaseHealthCommands(errors: Command): void {
  errors
    .command('releases')
    .description('Show the health of each release: crash-free sessions and users, adoption and deployment')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--environment <name>', 'SDK environment (as the SDK sends it)')
    .option('--days <n>', `Window in days, 1 to ${MAX_HEALTH_DAYS}`, '7')
    .option('--page <n>', 'Page', '1')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  errors
    .command('release <release>')
    .description('Show one release\'s sessions and crash-free rate per hour')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--environment <name>', 'SDK environment (as the SDK sends it)')
    .option('--days <n>', `Window in days, 1 to ${MAX_HEALTH_DAYS}`, '7')
    .option('--json', 'Output in JSON format')
    .action(seriesAction)
}

async function listAction(options: ListOptions): Promise<void> {
  const projectId = projectIdOf(options.projectId)
  const days = daysOf(options.days)
  if (projectId === null || days === null) return
  await requireAuth()
  await setupClient()
  const page = await withSpinner('Loading release health...', async () => {
    const { data, error } = await listReleaseHealth({
      client,
      path: { project_id: projectId },
      query: {
        days,
        page: Math.max(1, parseInt(options.page, 10) || 1),
        per_page: 20,
        ...(options.environment ? { environment: options.environment } : {}),
      },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(page)
    return
  }
  header(`Release health, last ${days} ${days === 1 ? 'day' : 'days'}`)
  if (!page.tracking.configured) {
    warning(page.tracking.reason ?? 'No sessions received yet')
    info("For example: Sentry.init({ dsn, release: 'my-app@1.0.0' }) sends a session on every page load")
    return
  }
  if (page.items.length === 0) {
    info('No session started in this window')
    return
  }
  printTable(page.items, [
    { header: 'Release', accessor: (row) => releaseLabel(row.release) },
    { header: 'Crash-free sessions', accessor: (row) => formatRate(row.crash_free_sessions) },
    { header: 'Crash-free users', accessor: (row) => formatRate(row.crash_free_users) },
    { header: 'Adoption 24h', accessor: (row) => formatRate(row.adoption) },
    { header: 'Sessions', accessor: (row) => row.sessions.toLocaleString() },
    { header: 'Crashed', accessor: (row) => row.crashed_sessions.toLocaleString() },
    { header: 'Users (est.)', accessor: (row) => row.users.toLocaleString() },
    {
      header: 'Deployment',
      accessor: (row) =>
        row.deployment
          ? `#${row.deployment.deployment_id}${row.deployment.environment_name ? ` ${row.deployment.environment_name}` : ''}`
          : '-',
    },
  ])
  if (page.total > page.items.length) {
    info(`Showing ${page.items.length} of ${page.total} releases; use --page for more`)
  }
  info('Alert on a crash-free drop: a metric alert on release.crash_free_sessions, grouped by release')
}

async function seriesAction(release: string, options: SeriesOptions): Promise<void> {
  const projectId = projectIdOf(options.projectId)
  const days = daysOf(options.days)
  if (projectId === null || days === null) return
  await requireAuth()
  await setupClient()
  const series = await withSpinner('Loading release trend...', async () => {
    const { data, error } = await getReleaseHealthSeries({
      client,
      path: { project_id: projectId },
      query: {
        release,
        days,
        ...(options.environment ? { environment: options.environment } : {}),
      },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(series)
    return
  }
  header(`Release ${releaseLabel(series.release)}`)
  if (series.points.length === 0) {
    info('No session of this release started in the window')
    return
  }
  const sessions = series.points.reduce((sum, p) => sum + p.sessions, 0)
  const crashed = series.points.reduce((sum, p) => sum + p.crashed_sessions, 0)
  keyValue('Sessions', sessions.toLocaleString())
  keyValue('Crashed', crashed.toLocaleString())
  keyValue('Crash-free sessions', formatRate(sessions > 0 ? 1 - crashed / sessions : null))
  printTable(series.points, [
    { header: 'Hour (UTC)', accessor: (row) => row.bucket_start.replace('T', ' ').slice(0, 16) },
    { header: 'Sessions', accessor: (row) => row.sessions.toLocaleString() },
    { header: 'Errored', accessor: (row) => row.errored_sessions.toLocaleString() },
    { header: 'Crashed', accessor: (row) => row.crashed_sessions.toLocaleString() },
    { header: 'Crash-free', accessor: (row) => formatRate(row.crash_free_sessions) },
  ])
}
