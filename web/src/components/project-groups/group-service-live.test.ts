// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  ProjectDashboardAnalytics,
  ProjectHealthSummary,
  ProjectMonitorHealth,
} from '@/api/client'
import {
  groupServiceLive,
  groupServiceTrafficLabel,
  type GroupServiceLiveSources,
} from './group-service-live'

const hour = (h: number) => `2026-10-28T${String(h).padStart(2, '0')}:00:00Z`

function analytics(
  projectId: number,
  visitors: number,
  hourly: number[]
): ProjectDashboardAnalytics {
  return {
    project_id: projectId,
    unique_visitors: visitors,
    previous_unique_visitors: 0,
    hourly_visits: hourly.map((count, h) => ({ date: hour(h), count })),
  } as ProjectDashboardAnalytics
}

function health(
  projectId: number,
  requests: number,
  hourly: number[],
  status = 'healthy'
): ProjectHealthSummary {
  return {
    project_id: projectId,
    status,
    total_requests: requests,
    total_errors: 0,
    error_rate: 0,
    avg_response_time_ms: 40,
    hourly_requests: hourly.map((request_count, h) => ({
      bucket: hour(h),
      request_count,
    })),
  } as ProjectHealthSummary
}

const settled = { healthLoading: false, healthError: false }

describe('groupServiceLive', () => {
  test('a frontend with visitors reports visitors', () => {
    const sources: GroupServiceLiveSources = {
      ...settled,
      analytics: { '1': analytics(1, 1240, [10, 30, 20]) },
      health: { '1': health(1, 13128, [400, 600, 500]) },
    }
    const live = groupServiceLive(1, sources)
    expect(live.traffic.kind).toBe('visitors')
    expect(live.total).toBe(1240)
    expect(live.traffic.data.map((p) => p.count)).toEqual([10, 30, 20])
    expect(groupServiceTrafficLabel(live)).toEqual({
      short: '1.2K',
      full: '1,240 visitors in the last 24 hours',
    })
  })

  test('an API with no visitors reports requests instead of "0 visitors"', () => {
    const sources: GroupServiceLiveSources = {
      ...settled,
      analytics: { '2': analytics(2, 0, [0, 0, 0]) },
      health: { '2': health(2, 24216, [900, 1100, 1000]) },
    }
    const live = groupServiceLive(2, sources)
    expect(live.traffic.kind).toBe('api-requests')
    expect(live.total).toBe(24216)
    expect(groupServiceTrafficLabel(live)).toEqual({
      short: '24.2K',
      full: '24,216 requests in the last 24 hours',
    })
  })

  test('an uptime monitor decides the dot over quiet traffic', () => {
    const live = groupServiceLive(3, {
      ...settled,
      health: { '3': health(3, 0, [], 'unknown') },
      monitors: { '3': { status: 'down' } as ProjectMonitorHealth },
    })
    expect(live.indicator.tone).toBe('down')
  })

  test('a failed health read shows as unavailable, never as quiet', () => {
    const live = groupServiceLive(4, {
      healthLoading: false,
      healthError: true,
    })
    expect(live.indicator.tone).toBe('unavailable')
    expect(live.total).toBe(0)
  })

  test('a service the batch has not returned yet is pending', () => {
    const live = groupServiceLive(5, {
      healthLoading: true,
      healthError: false,
    })
    expect(live.indicator.tone).toBe('pending')
  })
})
