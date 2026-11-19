// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Data helpers for the events the tracker records by itself (ADR 071 D1):
 * outbound link clicks, file downloads and "not found" pages.
 *
 * Outbound links and downloads are ordinary events ranked by one of their
 * properties through `GET /analytics/event-properties`. Not-found pages come
 * from two places: the 404 responses Temps itself answered (`proxy_logs`,
 * through the API traffic aggregation) and the `not_found` events pages
 * report with `trackNotFound()` or the `data-temps-404` marker.
 */

import type {
  TrafficAggregationRequest,
  TrafficAggregationResponse,
  TrafficFilter,
  TrafficMetric,
} from '@/api/client/types.gen'

/** Event names the tracker emits; they match `@temps-sdk/analytics-core`. */
export const OUTBOUND_CLICK_EVENT = 'outbound_click'
export const FILE_DOWNLOAD_EVENT = 'file_download'
export const NOT_FOUND_EVENT = 'not_found'

/**
 * Widest window the API traffic aggregation serves (`MAX_TRAFFIC_WINDOW_DAYS`
 * in `crates/temps-proxy/src/traffic_aggregation.rs`). A longer selection is
 * narrowed to its most recent 30 days and the card says so.
 */
export const MAX_NOT_FOUND_WINDOW_DAYS = 30

/** Paths ranked in the not-found report. */
export const NOT_FOUND_PATH_LIMIT = 10

const DAY_MS = 24 * 60 * 60 * 1000

/** One value of an event property, as `GET /analytics/event-properties` returns it. */
export interface EventPropertyValue {
  value: string | null
  count: number
  unique_visitors: number
  percentage: number
}

/** Response of `GET /analytics/event-properties`. */
export interface EventPropertiesResponse {
  event_name: string
  property: string
  total_count: number
  items: EventPropertyValue[]
}

/**
 * The aggregation request with the `referrer` dimension the server accepts
 * from this release on. Kept local until the generated client is refreshed
 * from a server that has it.
 */
export type NotFoundReferrerRequest = Omit<
  TrafficAggregationRequest,
  'dimensions' | 'filters' | 'order_by'
> & {
  dimensions: Array<'path' | 'referrer'>
  filters: Array<
    | TrafficFilter
    | { dimension: 'referrer'; operator: 'not_eq'; values: string[] }
  >
  order_by: Array<{
    field: { kind: 'metric'; field: TrafficMetric }
    direction: 'desc'
  }>
}

/** One row of the not-found report. */
export interface NotFoundPathRow {
  path: string
  hits: number
  lastSeen: string | null
  /** The page that most often linked to `path`, when the server knows it. */
  topReferrer: string | null
}

export interface NotFoundWindow {
  start: Date
  end: Date
  /** True when the selection was longer than the aggregation allows. */
  narrowed: boolean
}

/** Narrow a selected period to what the API traffic aggregation serves. */
export function notFoundWindow(start: Date, end: Date): NotFoundWindow {
  const earliest = new Date(end.getTime() - MAX_NOT_FOUND_WINDOW_DAYS * DAY_MS)
  return start < earliest
    ? { start: earliest, end, narrowed: true }
    : { start, end, narrowed: false }
}

/** Filters shared by both not-found queries: real visitors' 404s only. */
function notFoundFilters(): TrafficFilter[] {
  return [
    { dimension: 'status_code', operator: 'eq', values: ['404'] },
    { dimension: 'is_bot', operator: 'eq', values: ['false'] },
  ]
}

/** Most-hit 404 paths answered by the proxy in the window. */
export function notFoundPathsRequest(
  window: NotFoundWindow,
  environmentId: number | undefined,
  limit = NOT_FOUND_PATH_LIMIT
): TrafficAggregationRequest {
  return {
    start_time: window.start.toISOString(),
    end_time: window.end.toISOString(),
    environment_id: environmentId ?? null,
    dimensions: ['path'],
    metrics: ['requests', 'last_seen'],
    filters: notFoundFilters(),
    order_by: [
      { field: { kind: 'metric', field: 'requests' }, direction: 'desc' },
    ],
    page: 1,
    page_size: limit,
  }
}

/**
 * Referrers of the given 404 paths, most frequent first. `null` when there
 * are no paths, so the caller skips the request. The `in` filter takes at
 * most 20 values, which `NOT_FOUND_PATH_LIMIT` stays under.
 */
export function notFoundReferrersRequest(
  window: NotFoundWindow,
  environmentId: number | undefined,
  paths: string[]
): NotFoundReferrerRequest | null {
  if (paths.length === 0) return null
  return {
    start_time: window.start.toISOString(),
    end_time: window.end.toISOString(),
    environment_id: environmentId ?? null,
    dimensions: ['path', 'referrer'],
    metrics: ['requests'],
    filters: [
      ...notFoundFilters(),
      { dimension: 'path', operator: 'in', values: paths.slice(0, 20) },
      { dimension: 'referrer', operator: 'not_eq', values: [''] },
    ],
    order_by: [
      { field: { kind: 'metric', field: 'requests' }, direction: 'desc' },
    ],
    page: 1,
    page_size: 100,
  }
}

function dimensionValue(
  row: TrafficAggregationResponse['rows'][number],
  dimension: string
): string | null {
  return (
    row.dimensions.find((value) => value.dimension === dimension)?.value ?? null
  )
}

/**
 * Join the path ranking with the referrer breakdown. The referrer response is
 * optional: a server that predates the `referrer` dimension rejects that
 * query, and the report still lists paths and hits without it.
 */
export function mergeNotFoundRows(
  paths: TrafficAggregationResponse,
  referrers: TrafficAggregationResponse | null
): NotFoundPathRow[] {
  const topReferrer = new Map<string, string>()
  // Rows arrive ordered by requests, so the first one seen per path wins.
  for (const row of referrers?.rows ?? []) {
    const path = dimensionValue(row, 'path')
    const referrer = dimensionValue(row, 'referrer')
    if (path !== null && referrer !== null && !topReferrer.has(path)) {
      topReferrer.set(path, referrer)
    }
  }
  return paths.rows.flatMap((row) => {
    const path = dimensionValue(row, 'path')
    if (path === null) return []
    return [
      {
        path,
        hits: row.metrics.requests ?? 0,
        lastSeen: row.metrics.last_seen ?? null,
        topReferrer: topReferrer.get(path) ?? null,
      },
    ]
  })
}

/** Text shown for a property value; events sent without it read "(not set)". */
export function propertyValueLabel(value: string | null): string {
  return value === null || value === '' ? '(not set)' : value
}

/** A URL without its scheme, for compact display: `example.org/pricing`. */
export function compactUrl(url: string): string {
  return url.replace(/^https?:\/\//, '').replace(/\/$/, '')
}
