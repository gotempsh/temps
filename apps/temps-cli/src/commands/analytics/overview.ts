// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getUniqueCounts,
  getPagePaths,
  getEventsCount,
  getPropertyBreakdown,
  getAggregatedBuckets,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, json as jsonOut, colors, info } from '../../ui/output.js'
import { parsePeriod } from './period.js'

interface OverviewOptions {
  project?: string
  period?: string
  json?: boolean
}

interface LocationCount {
  country: string
  count: number
  percentage: number
}

export const SPARKLINE_WIDTH = 48

/**
 * Pick a server-side bucket size that renders the *whole* requested period in
 * at most SPARKLINE_WIDTH columns.
 *
 * The overview used to always request hourly buckets and then keep only the
 * last 48 of them, so `--period 7d` and `--period 30d` both silently drew the
 * same last-two-days sparkline under a header claiming a much longer range.
 */
export function bucketSizeForRange(startDate: string, endDate: string): string {
  const hours = (Date.parse(endDate) - Date.parse(startDate)) / 3_600_000

  if (hours <= SPARKLINE_WIDTH) return '1 hour'
  if (hours <= SPARKLINE_WIDTH * 6) return '6 hours'
  if (hours <= SPARKLINE_WIDTH * 24) return '1 day'
  if (hours <= SPARKLINE_WIDTH * 24 * 7) return '1 week'
  return '1 month'
}

function formatNumber(n: number): string {
  return n.toLocaleString('en-US')
}

export function renderSparkline(data: { count: number }[]): string {
  const blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
  const max = Math.max(...data.map((d) => d.count), 1)

  // The bucket size is chosen so the full period already fits; trimming here is
  // only a guard against a server returning more buckets than we asked for.
  const points = data.length > SPARKLINE_WIDTH ? data.slice(-SPARKLINE_WIDTH) : data

  return points
    .map((d) => {
      const idx = Math.min(Math.floor((d.count / max) * 7), 7)
      return blocks[idx]
    })
    .join('')
}

export async function overview(options: OverviewOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
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

  const bucketSize = bucketSizeForRange(startDate, endDate)

  // Fetch all data concurrently
  const data = await withSpinner('Fetching analytics...', async () => {
    const [visitorsRes, sessionsRes, pageViewsRes, pagesRes, eventsRes, locationsRes, bucketsRes] =
      await Promise.all([
        getUniqueCounts({
          client,
          path: { project_id: projectId },
          query: { start_date: startDate, end_date: endDate, metric: 'visitors' },
        }),
        getUniqueCounts({
          client,
          path: { project_id: projectId },
          query: { start_date: startDate, end_date: endDate, metric: 'sessions' },
        }),
        getUniqueCounts({
          client,
          path: { project_id: projectId },
          query: { start_date: startDate, end_date: endDate, metric: 'page_views' },
        }),
        getPagePaths({
          client,
          query: { project_id: projectId, start_date: startDate, end_date: endDate, limit: 10 },
        }),
        getEventsCount({
          client,
          path: { project_id: projectId },
          query: {
            start_date: startDate,
            end_date: endDate,
            limit: 10,
            custom_events_only: true,
          },
        }),
        // Locations must come from the server-side country aggregate. The
        // previous implementation listed raw visitor rows and tallied their
        // countries client-side, which silently reported the same numbers for
        // every period: /analytics/visitors caps at 100 rows and orders by
        // last_seen DESC, so 24h, 7d and 30d all got the same 100 most-recent
        // visitors.
        getPropertyBreakdown({
          client,
          path: { project_id: projectId },
          query: {
            start_date: startDate,
            end_date: endDate,
            group_by: 'country',
            aggregation_level: 'visitors',
            limit: 10,
          },
        }),
        getAggregatedBuckets({
          client,
          path: { project_id: projectId },
          query: {
            start_date: startDate,
            end_date: endDate,
            aggregation_level: 'visitors',
            bucket_size: bucketSize,
          },
        }),
      ])

    if (visitorsRes.error) throw new Error(getErrorMessage(visitorsRes.error))
    if (sessionsRes.error) throw new Error(getErrorMessage(sessionsRes.error))
    if (pageViewsRes.error) throw new Error(getErrorMessage(pageViewsRes.error))
    if (pagesRes.error) throw new Error(getErrorMessage(pagesRes.error))
    if (eventsRes.error) throw new Error(getErrorMessage(eventsRes.error))
    if (locationsRes.error) throw new Error(getErrorMessage(locationsRes.error))
    if (bucketsRes.error) throw new Error(getErrorMessage(bucketsRes.error))

    const topLocations: LocationCount[] = ((locationsRes.data as any)?.items ?? []).map(
      (item: any) => ({
        country: item.value,
        count: item.count,
        percentage: item.percentage,
      })
    )

    return {
      uniqueVisitors: (visitorsRes.data as any)?.count ?? 0,
      totalSessions: (sessionsRes.data as any)?.count ?? 0,
      pageViews: (pageViewsRes.data as any)?.count ?? 0,
      topPages: (pagesRes.data as any)?.page_paths ?? [],
      topEvents: (eventsRes.data as any) ?? [],
      topLocations,
      // Echo back the bucket size the server actually used, so the rendered
      // label always describes the data rather than what we asked for.
      bucketSize: (bucketsRes.data as any)?.bucket_size ?? bucketSize,
      buckets: (bucketsRes.data as any)?.items ?? [],
    }
  })

  if (options.json) {
    jsonOut({
      project: resolved.slug,
      period,
      ...data,
    })
    return
  }

  // Pretty output
  const line = chalk.cyan('━'.repeat(64))

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white('Analytics:')} ${chalk.bold.cyan(resolved.slug)} ${chalk.gray(`(${label})`)}`
  )
  console.log(line)
  newline()

  // Key metrics
  console.log(`  ${chalk.white('Unique Visitors')}${' '.repeat(7)}${chalk.bold.green(formatNumber(data.uniqueVisitors))}`)
  console.log(`  ${chalk.white('Total Sessions')}${' '.repeat(8)}${chalk.bold.green(formatNumber(data.totalSessions))}`)
  console.log(`  ${chalk.white('Page Views')}${' '.repeat(12)}${chalk.bold.green(formatNumber(data.pageViews))}`)

  // Sparkline — spans the full requested period, one column per bucket
  if (data.buckets.length > 0) {
    const max = Math.max(...data.buckets.map((d: any) => d.count), 1)
    newline()
    console.log(`  ${chalk.bold.white(`Visitors (${label}, per ${data.bucketSize})`)}`)
    console.log(
      `  ${chalk.cyan(renderSparkline(data.buckets))} ${chalk.gray(`(max: ${formatNumber(max)})`)}`
    )
  }

  // Top Pages
  if (data.topPages.length > 0) {
    newline()
    console.log(`  ${chalk.bold.white('Top Pages')}`)
    console.log(`  ${chalk.gray('─'.repeat(60))}`)
    console.log(
      `  ${chalk.gray('#'.padEnd(4))}${chalk.gray('Path'.padEnd(40))}${chalk.gray('Sessions'.padStart(10))}${chalk.gray('Views'.padStart(8))}`
    )

    data.topPages.forEach((page: any, i: number) => {
      const path = page.page_path.length > 38 ? page.page_path.slice(0, 35) + '...' : page.page_path
      console.log(
        `  ${chalk.gray(String(i + 1).padEnd(4))}${chalk.white(path.padEnd(40))}${formatNumber(page.session_count).padStart(10)}${formatNumber(page.page_view_count).padStart(8)}`
      )
    })
  }

  // Top Events
  if (data.topEvents.length > 0) {
    newline()
    console.log(`  ${chalk.bold.white('Top Events')}`)
    console.log(`  ${chalk.gray('─'.repeat(60))}`)
    console.log(
      `  ${chalk.gray('#'.padEnd(4))}${chalk.gray('Event'.padEnd(40))}${chalk.gray('Count'.padStart(10))}${chalk.gray('%'.padStart(8))}`
    )

    data.topEvents.forEach((event: any, i: number) => {
      const name = event.event_name.length > 38 ? event.event_name.slice(0, 35) + '...' : event.event_name
      console.log(
        `  ${chalk.gray(String(i + 1).padEnd(4))}${chalk.white(name.padEnd(40))}${formatNumber(event.count).padStart(10)}${(event.percentage.toFixed(1) + '%').padStart(8)}`
      )
    })
  }

  // Top Locations
  if (data.topLocations.length > 0) {
    newline()
    console.log(`  ${chalk.bold.white('Top Locations')}`)
    console.log(`  ${chalk.gray('─'.repeat(60))}`)
    console.log(
      `  ${chalk.gray('#'.padEnd(4))}${chalk.gray('Country'.padEnd(40))}${chalk.gray('Visitors'.padStart(10))}${chalk.gray('%'.padStart(8))}`
    )

    data.topLocations.forEach((loc, i) => {
      console.log(
        `  ${chalk.gray(String(i + 1).padEnd(4))}${chalk.white(loc.country.padEnd(40))}${formatNumber(loc.count).padStart(10)}${(loc.percentage.toFixed(1) + '%').padStart(8)}`
      )
    })
  }

  newline()
  console.log(line)
  newline()
}
