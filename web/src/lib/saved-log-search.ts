// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  LogLevel,
  LogSourceKind,
  SavedSearchQuery,
  SavedSearchResponse,
} from '@/api/client/types.gen'
import {
  quickTimeRange,
  type DateTimeRangeValue,
  type QuickTimeRange,
} from '@/lib/date-time-range'

const LEVELS: LogLevel[] = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR']

/** The time ranges the server stores, in the order a picker shows them. */
export const SAVED_SEARCH_TIME_RANGES = [
  '15m',
  '1h',
  '6h',
  '24h',
  '7d',
  '30d',
] as const
export type SavedSearchTimeRange = (typeof SAVED_SEARCH_TIME_RANGES)[number]

const RANGE_HOURS: Record<SavedSearchTimeRange, number> = {
  '15m': 0.25,
  '1h': 1,
  '6h': 6,
  '24h': 24,
  '7d': 168,
  '30d': 720,
}

/** The page's presets, under the server's names. */
const PRESET_TO_RANGE: Record<QuickTimeRange, SavedSearchTimeRange> = {
  '1h': '1h',
  '6h': '6h',
  '1d': '24h',
  '7d': '7d',
}

/**
 * The relative range a saved search keeps. A custom (absolute) window is
 * saved as the preset closest to its length, since a saved search is a
 * question to ask again later, not a fixed slice of the past.
 */
export function savedRangeFromWindow(
  window: DateTimeRangeValue
): SavedSearchTimeRange {
  if (window.preset !== 'custom') return PRESET_TO_RANGE[window.preset]
  const hours = (Date.parse(window.to) - Date.parse(window.from)) / 3_600_000
  if (!Number.isFinite(hours) || hours <= 0) return '1h'
  let best: SavedSearchTimeRange = '1h'
  for (const range of SAVED_SEARCH_TIME_RANGES) {
    if (
      Math.abs(Math.log(RANGE_HOURS[range] / hours)) <
      Math.abs(Math.log(RANGE_HOURS[best] / hours))
    )
      best = range
  }
  return best
}

/** The window a saved search opens with, ending now. */
export function windowFromSavedRange(
  range: string,
  now = Date.now()
): DateTimeRangeValue {
  const preset = (Object.keys(PRESET_TO_RANGE) as QuickTimeRange[]).find(
    (key) => PRESET_TO_RANGE[key] === range
  )
  if (preset) return quickTimeRange(preset, now)
  const hours = RANGE_HOURS[range as SavedSearchTimeRange] ?? 1
  return {
    preset: 'custom',
    from: new Date(now - hours * 3_600_000).toISOString(),
    to: new Date(now).toISOString(),
  }
}

function positive(value: string | null): number | undefined {
  if (!value || !/^\d+$/.test(value)) return undefined
  const parsed = Number(value)
  return parsed > 0 ? parsed : undefined
}

/** The filters on the logs page, as a saved search stores them. */
export function savedQueryFromParams(
  params: URLSearchParams
): SavedSearchQuery {
  const source = params.get('source')
  const level = LEVELS.find((value) => value === params.get('level'))
  const env = params.get('env')
  const node = positive(params.get('node_id'))
  const deploy = positive(params.get('deploy_id'))
  const text = params.get('q')?.trim()
  return {
    source:
      source === 'application' || source === 'service'
        ? (source as LogSourceKind)
        : 'collected',
    levels: level ? [level] : [],
    envs: env ? [env] : [],
    node_ids: node ? [node] : [],
    deploy_id: deploy ?? null,
    text: text || null,
    attrs: params.getAll('attr'),
    services: [],
    container_ids: [],
    external_service_ids: [],
  }
}

export interface SavedSearchView {
  /** Single-value filters for the page's URL; `undefined` clears one. */
  patch: Record<string, string | undefined>
  attrs: string[]
  window: DateTimeRangeValue
  /**
   * Filters the search holds that the logs page has no control for (a
   * second level, a container id). They still apply to alerts and charts
   * built from the search; the page says it is showing a wider view.
   */
  notShown: string[]
}

/** What opening a saved search does to the logs page. */
export function viewFromSavedSearch(
  saved: { project_id: number; query: SavedSearchQuery; time_range: string },
  now = Date.now()
): SavedSearchView {
  const query = saved.query
  const notShown: string[] = []
  const first = <T>(values: T[] | undefined, label: string) => {
    if ((values?.length ?? 0) > 1) notShown.push(label)
    return values?.[0]
  }
  const level = first(query.levels, 'more than one level')
  const env = first(query.envs, 'more than one environment')
  const node = first(query.node_ids, 'more than one node')
  if (query.services?.length) notShown.push('services')
  if (query.container_ids?.length) notShown.push('containers')
  if (query.external_service_ids?.length) notShown.push('databases')
  return {
    patch: {
      project_id: String(saved.project_id),
      source:
        query.source && query.source !== 'collected' ? query.source : undefined,
      level: level ?? undefined,
      env: env ?? undefined,
      node_id: node != null ? String(node) : undefined,
      deploy_id: query.deploy_id != null ? String(query.deploy_id) : undefined,
      q: query.text || undefined,
    },
    attrs: query.attrs ?? [],
    window: windowFromSavedRange(saved.time_range, now),
    notShown,
  }
}

/** Where "Alert on this search" leads: a new alert rule with the search chosen. */
export function alertOnSearchHref(
  projectSlug: string,
  savedSearchId: number
): string {
  return `/projects/${encodeURIComponent(projectSlug)}/metrics/alerts/new?saved_search_id=${savedSearchId}`
}

/** Pinned first, then by name, so the list reads the same every time. */
export function orderSavedSearches(
  searches: SavedSearchResponse[]
): SavedSearchResponse[] {
  return [...searches].sort(
    (a, b) =>
      Number(b.pinned) - Number(a.pinned) || a.name.localeCompare(b.name)
  )
}
