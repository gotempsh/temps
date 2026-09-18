// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client/types.gen'

import type { DimensionKey } from './DimensionList'

/**
 * Map a dimension key to the matching backend query parameter on
 * `GET /analytics/visitors`. Returns `null` when the dimension isn't
 * filterable as a segment (e.g. `pages`).
 */
export function segmentParamFor(dimension: DimensionKey): string | null {
  switch (dimension) {
    case 'events':
      return 'filter_event'
    case 'referrers':
      return 'filter_referrer'
    case 'browsers':
      return 'filter_browser'
    case 'operating_systems':
      return 'filter_os'
    case 'devices':
      return 'filter_device'
    case 'countries':
      return 'filter_country'
    case 'regions':
      return 'filter_region'
    case 'cities':
      return 'filter_city'
    case 'channels':
      return 'filter_channel'
    case 'languages':
      return 'filter_language'
    case 'utm_source':
      return 'filter_utm_source'
    case 'utm_medium':
      return 'filter_utm_medium'
    case 'utm_campaign':
      return 'filter_utm_campaign'
    case 'utm_term':
      return 'filter_utm_term'
    case 'utm_content':
      return 'filter_utm_content'
    case 'pages':
      return null
  }
}

// Pure capability helper shared by analytics routing and this component.
export function segmentSupportsVisitors(dimension: DimensionKey): boolean {
  return segmentParamFor(dimension) !== null
}

export interface SegmentVisitorsProps {
  project: ProjectResponse
  dimension: DimensionKey
  value: string
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
  onBack: () => void
}
