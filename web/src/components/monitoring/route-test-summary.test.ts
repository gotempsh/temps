// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { NotificationRouteTestResult } from '@/api/client/types.gen'
import { summarizeRouteTest } from './route-test-summary'

function result(
  deliveries: NotificationRouteTestResult['deliveries'],
  route_enabled = true
): NotificationRouteTestResult {
  return {
    route_id: 4,
    route_name: 'Critical incidents',
    route_enabled,
    severity: 'critical',
    deliveries,
  }
}

const delivery = (
  provider_id: number,
  provider_name: string,
  status: 'sent' | 'failed' | 'skipped_disabled',
  message: string | null = null
) => ({ provider_id, provider_name, provider_type: 'slack', status, message })

describe('summarizeRouteTest', () => {
  test('reports success when every enabled provider received it', () => {
    const summary = summarizeRouteTest(result([delivery(1, 'On-call', 'sent')]))
    expect(summary.tone).toBe('success')
    expect(summary.title).toContain('Critical incidents')
    expect(summary.description).toBe('Sent to On-call.')
  })

  test('names failing and skipped providers', () => {
    const summary = summarizeRouteTest(
      result([
        delivery(1, 'On-call', 'sent'),
        delivery(2, 'Team email', 'failed', 'Delivery failed: timeout'),
        delivery(3, 'Muted', 'skipped_disabled'),
      ])
    )
    expect(summary.tone).toBe('error')
    expect(summary.title).toBe('Test reached 1 of 3 providers')
    expect(summary.description).toContain(
      'Team email: Delivery failed: timeout'
    )
    expect(summary.description).toContain('Skipped disabled provider: Muted.')
  })

  test('explains a route that delivered nowhere', () => {
    const summary = summarizeRouteTest(result([], false))
    expect(summary.tone).toBe('error')
    expect(summary.title).toContain('reached no provider')
    expect(summary.description).toContain('This route is disabled')
  })
})
