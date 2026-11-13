// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { TrafficAggregationRequest } from '@/api/client/types.gen'

export type TrafficSortDirection = 'asc' | 'desc'

export interface TrafficSort<TMetric extends string> {
  metric: TMetric
  direction: TrafficSortDirection
}

/**
 * Start a newly selected metric in descending order, then toggle direction
 * on subsequent clicks. The returned direction is sent to the aggregation
 * API, so sorting remains correct across server-paginated results.
 */
export function nextTrafficSort<TMetric extends string>(
  current: TrafficSort<TMetric>,
  metric: TMetric
): TrafficSort<TMetric> {
  if (current.metric !== metric) {
    return { metric, direction: 'desc' }
  }

  return {
    metric,
    direction: current.direction === 'desc' ? 'asc' : 'desc',
  }
}

export function trafficPageCount(total: number, pageSize: number): number {
  return Math.max(1, Math.ceil(total / pageSize))
}

/**
 * The page a table should be on once it has answered: the same page while it
 * still has rows, else its last one. Refresh keeps the page, and a moved
 * window can have fewer pages; an empty page past the end has no pagination
 * to get back with.
 */
export function trafficPageInRange(
  page: number,
  answer: { rows: readonly unknown[]; total_pages: number } | undefined
): number {
  if (!answer || page === 0 || answer.rows.length > 0) return page
  return Math.max(0, Math.min(page, answer.total_pages - 1))
}

export function trafficQueryNeedsCompatibilityRetry(
  body: TrafficAggregationRequest,
  error: unknown
): boolean {
  const hasOptionalCrawlerMetric = body.metrics.some(
    (metric) => metric === 'bot_requests' || metric === 'robots_txt_requests'
  )
  if (!hasOptionalCrawlerMetric || !error) return false

  return /unknown variant [`'](?:bot_requests|robots_txt_requests)[`']/.test(
    JSON.stringify(error)
  )
}

/**
 * Query key of one paged, sorted API traffic table (routes or callers). The
 * window is a named object so Refresh, which moves only the window, keeps the
 * current rows on screen (`keepPreviousWindow`), while a new page, sort or
 * environment still loads from scratch.
 */
export function trafficTableQueryKey<TMetric extends string>(table: {
  projectId: number
  startIso: string | undefined
  endIso: string | undefined
  environmentId: number | undefined
  kind: 'routes' | 'callers'
  page: number
  sort: TrafficSort<TMetric>
}) {
  return [
    'api-traffic-query',
    table.projectId,
    { start_time: table.startIso, end_time: table.endIso },
    table.environmentId,
    table.kind,
    table.page,
    table.sort.metric,
    table.sort.direction,
  ] as const
}
