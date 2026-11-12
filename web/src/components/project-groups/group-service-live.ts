// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ProjectDashboardAnalytics,
  ProjectHealthSummary,
  ProjectMonitorHealth,
} from '@/api/client'
import {
  projectHealthIndicator,
  type ProjectHealthIndicator,
} from '@/components/dashboard/project-card-health'
import {
  projectCardTraffic,
  type ProjectCardTraffic,
} from '@/components/dashboard/project-card-traffic'

/**
 * What a service row in a project card shows about the service right now:
 * the same health dot and 24-hour traffic a standalone service card shows, so
 * grouping services never hides whether they are up.
 */
export interface GroupServiceLive {
  indicator: ProjectHealthIndicator
  traffic: ProjectCardTraffic
  /** Visitors or requests over the window, matching `traffic.kind`. */
  total: number
}

/** The dashboard's batched reads, keyed by service id as the API returns them. */
export interface GroupServiceLiveSources {
  analytics?: Record<string, ProjectDashboardAnalytics>
  health?: Record<string, ProjectHealthSummary>
  monitors?: Record<string, ProjectMonitorHealth>
  healthLoading: boolean
  healthError: boolean
}

export function groupServiceLive(
  serviceId: number,
  sources: GroupServiceLiveSources
): GroupServiceLive {
  const key = String(serviceId)
  const analytics = sources.analytics?.[key]
  const health = sources.health?.[key]
  const requests = health?.total_requests ?? 0
  const traffic = projectCardTraffic(
    analytics?.hourly_visits,
    health?.hourly_requests,
    requests
  )
  return {
    indicator: projectHealthIndicator({
      health,
      monitor: sources.monitors?.[key],
      loading: sources.healthLoading,
      error: sources.healthError,
    }),
    traffic,
    total:
      traffic.kind === 'visitors'
        ? (analytics?.unique_visitors ?? 0)
        : requests,
  }
}

const compact = new Intl.NumberFormat('en-US', {
  notation: 'compact',
  maximumFractionDigits: 1,
})

/** "1.2K" beside a visitors or requests icon, with the full sentence for screen readers and the tooltip. */
export function groupServiceTrafficLabel(live: GroupServiceLive): {
  short: string
  full: string
} {
  const n = compact.format(live.total)
  const exact = live.total.toLocaleString('en-US')
  return live.traffic.kind === 'visitors'
    ? {
        short: n,
        full: `${exact} visitors in the last 24 hours`,
      }
    : { short: n, full: `${exact} requests in the last 24 hours` }
}
