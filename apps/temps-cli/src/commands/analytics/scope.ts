// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Filters several analytics reports share: one environment or deployment of
 * the project, and the dimension filters of a breakdown.
 */

import type {
  GetAggregatedBucketsData,
  GetAnalyticsGoalBreakdownData,
  GetFunnelMetricsData,
  GetPropertyBreakdownData,
} from '../../api/types.gen.js'
import { fieldFlags } from '../../lib/endpoint.js'

const ENVIRONMENT = { kind: 'int', help: 'Only this environment of the project (an ID)' } as const
const DEPLOYMENT = { kind: 'int', help: 'Only this deployment (an ID)' } as const

/** `--environment-id` and `--deployment-id`. */
export const SCOPE_FILTERS = fieldFlags<NonNullable<GetAggregatedBucketsData['query']>>(
  { environment_id: ENVIRONMENT, deployment_id: DEPLOYMENT },
  'query',
)

/** `--environment-id` alone, for reports that have no deployment filter. */
export const ENVIRONMENT_FILTER = fieldFlags<{ environment_id?: number }>({ environment_id: ENVIRONMENT }, 'query')

/** The scope and dimension filters of a property breakdown (`temps analytics top`). */
export const BREAKDOWN_FILTERS = fieldFlags<NonNullable<GetPropertyBreakdownData['query']>>(
  {
    environment_id: ENVIRONMENT,
    deployment_id: DEPLOYMENT,
    filter_browser: { kind: 'string', help: 'Only visitors on this browser' },
    filter_channel: { kind: 'string', help: 'Only visitors from this channel (e.g. Organic Search)' },
    filter_country: { kind: 'string', help: 'Only visitors from this country' },
    filter_os: { kind: 'string', help: 'Only visitors on this operating system' },
    filter_referrer: { kind: 'string', help: 'Only visitors from this referrer' },
    filter_region: { kind: 'string', help: 'Only visitors from this region' },
    include_crawlers: { kind: 'bool', help: 'Count crawler traffic too' },
  },
  'query',
)

/** Funnel metrics: one environment, one country. */
export const FUNNEL_FILTERS = fieldFlags<NonNullable<GetFunnelMetricsData['query']>>(
  { environment_id: ENVIRONMENT, country_code: { kind: 'string', help: 'Only visitors from this country (ISO code, e.g. DE)' } },
  'query',
)

/** Conversion breakdown: one environment. */
export const GOAL_BREAKDOWN_FILTERS = fieldFlags<NonNullable<GetAnalyticsGoalBreakdownData['query']>>({ environment_id: ENVIRONMENT }, 'query')
