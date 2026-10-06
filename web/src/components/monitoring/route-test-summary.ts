// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { NotificationRouteTestResult } from '@/api/client/types.gen'

export interface RouteTestSummary {
  tone: 'success' | 'error'
  title: string
  description: string
}

/** Turns a route test result into one toast: which providers got it, which did not. */
export function summarizeRouteTest(
  result: NotificationRouteTestResult
): RouteTestSummary {
  const sent = result.deliveries.filter((d) => d.status === 'sent')
  const failed = result.deliveries.filter((d) => d.status === 'failed')
  const skipped = result.deliveries.filter(
    (d) => d.status === 'skipped_disabled'
  )
  const lines: string[] = []
  if (sent.length > 0)
    lines.push(`Sent to ${sent.map((d) => d.provider_name).join(', ')}.`)
  for (const delivery of failed) {
    lines.push(
      `${delivery.provider_name}: ${delivery.message ?? 'delivery failed'}`
    )
  }
  if (skipped.length > 0)
    lines.push(
      `Skipped disabled ${skipped.length === 1 ? 'provider' : 'providers'}: ${skipped
        .map((d) => d.provider_name)
        .join(', ')}.`
    )
  if (!result.route_enabled)
    lines.push('This route is disabled, so real alerts do not use it yet.')

  const total = result.deliveries.length
  if (sent.length === 0) {
    return {
      tone: 'error',
      title: `Test through "${result.route_name}" reached no provider`,
      description:
        lines.join(' ') || 'This route has no providers assigned to it.',
    }
  }
  return {
    tone: failed.length > 0 ? 'error' : 'success',
    title:
      failed.length > 0
        ? `Test reached ${sent.length} of ${total} providers`
        : `Test ${result.severity} notification sent through "${result.route_name}"`,
    description: lines.join(' '),
  }
}
