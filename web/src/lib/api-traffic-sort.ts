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
