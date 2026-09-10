// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { apiTrafficProxyLogsUrl } from '@/lib/api-traffic-navigation'

describe('API traffic Proxy Logs navigation', () => {
  test('preserves project, route, IP, exact time window, and environment', () => {
    const url = apiTrafficProxyLogsUrl({
      projectId: 42,
      clientIp: '203.0.113.42',
      method: 'GET',
      path: '/api/users/a b?source=admin',
      startDate: '2026-08-14T10:15:00.000Z',
      endDate: '2026-08-15T10:15:00.000Z',
      environmentId: 7,
    })
    const parsed = new URL(url, 'http://localhost')

    expect(parsed.pathname).toBe('/proxy-logs')
    expect(Object.fromEntries(parsed.searchParams)).toEqual({
      project_id: '42',
      client_ip: '203.0.113.42',
      method: 'GET',
      path_exact: '/api/users/a b?source=admin',
      exclude_synthetic: 'true',
      start_date: '2026-08-14T10:15:00.000Z',
      end_date: '2026-08-15T10:15:00.000Z',
      time_range: 'custom',
      filters: 'open',
      environment_id: '7',
    })
  })
})
