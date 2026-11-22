// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse, PropertyColumn } from '@/api/client/types.gen'
import { segmentSupportsVisitors } from './SegmentVisitors-shared'

export type DimensionKey =
  | 'events'
  | 'referrers'
  | 'browsers'
  | 'operating_systems'
  | 'devices'
  | 'countries'
  | 'regions'
  | 'cities'
  | 'channels'
  | 'languages'
  | 'pages'
  | 'utm_source'
  | 'utm_medium'
  | 'utm_campaign'
  | 'utm_term'
  | 'utm_content'

export interface DimensionConfig {
  title: string
  singular: string
  plural: string
  /** When set, use the property-breakdown API with this group_by column. */
  groupBy?: PropertyColumn
  /** When set, use the dedicated events-count endpoint instead. */
  useEventsCount?: boolean
}

export const DIMENSIONS: Record<DimensionKey, DimensionConfig> = {
  events: {
    title: 'Events',
    singular: 'event',
    plural: 'events',
    useEventsCount: true,
  },
  referrers: {
    title: 'Referrers',
    singular: 'referrer',
    plural: 'referrers',
    groupBy: 'referrer_hostname',
  },
  browsers: {
    title: 'Browsers',
    singular: 'browser',
    plural: 'browsers',
    groupBy: 'browser',
  },
  operating_systems: {
    title: 'Operating systems',
    singular: 'operating system',
    plural: 'operating systems',
    groupBy: 'operating_system',
  },
  devices: {
    title: 'Devices',
    singular: 'device',
    plural: 'devices',
    groupBy: 'device_type',
  },
  countries: {
    title: 'Countries',
    singular: 'country',
    plural: 'countries',
    groupBy: 'country',
  },
  regions: {
    title: 'Regions',
    singular: 'region',
    plural: 'regions',
    groupBy: 'region',
  },
  cities: {
    title: 'Cities',
    singular: 'city',
    plural: 'cities',
    groupBy: 'city',
  },
  channels: {
    title: 'Traffic channels',
    singular: 'channel',
    plural: 'channels',
    groupBy: 'channel',
  },
  languages: {
    title: 'Languages',
    singular: 'language',
    plural: 'languages',
    groupBy: 'language',
  },
  pages: {
    title: 'Pages',
    singular: 'page',
    plural: 'pages',
    groupBy: 'pathname',
  },
  utm_source: {
    title: 'UTM sources',
    singular: 'source',
    plural: 'sources',
    groupBy: 'utm_source',
  },
  utm_medium: {
    title: 'UTM mediums',
    singular: 'medium',
    plural: 'mediums',
    groupBy: 'utm_medium',
  },
  utm_campaign: {
    title: 'UTM campaigns',
    singular: 'campaign',
    plural: 'campaigns',
    groupBy: 'utm_campaign',
  },
  utm_term: {
    title: 'UTM terms',
    singular: 'term',
    plural: 'terms',
    groupBy: 'utm_term',
  },
  utm_content: {
    title: 'UTM contents',
    singular: 'content',
    plural: 'contents',
    groupBy: 'utm_content',
  },
}

export function isDimensionKey(
  value: string | undefined
): value is DimensionKey {
  return !!value && value in DIMENSIONS
}

export interface DimensionListProps {
  project: ProjectResponse
  dimension: DimensionKey
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
  onBack: () => void
}

export interface Row {
  value: string
  count: number
  percentage: number
}

export type DimensionSortKey = 'value' | 'count'

/**
 * The page a breakdown row opens: an event's detail page, else the visitors
 * in that segment (`/analytics/segments/:dimension/:value`), when the
 * dimension can filter visitors. The date range carries over, so the page
 * opens on the same window. `undefined` leaves the row plain.
 */
export function dimensionRowHref(
  projectSlug: string,
  dimension: DimensionKey,
  value: string,
  searchParams: URLSearchParams
): string | undefined {
  const base = `/projects/${projectSlug}/analytics`
  const path =
    dimension === 'events'
      ? `${base}/events/${encodeURIComponent(value)}`
      : segmentSupportsVisitors(dimension)
        ? `${base}/segments/${dimension}/${encodeURIComponent(value)}`
        : undefined
  if (!path) return undefined
  const params = new URLSearchParams()
  for (const key of ['filter', 'from', 'to']) {
    const current = searchParams.get(key)
    if (current) params.set(key, current)
  }
  const qs = params.toString()
  return qs ? `${path}?${qs}` : path
}
